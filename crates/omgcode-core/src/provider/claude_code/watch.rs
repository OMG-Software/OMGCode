//! Holds a running Claude Code process to its recorded connection
//! (ADR-0109, as ADR-0061 does for runs).
//!
//! Before the process starts, the turn takes a shared lock on the
//! connection's turn lock file and checks every settings state since the
//! connection was resolved. While it runs, a watcher thread checks each later
//! state and ends the process tree when one withdraws the connection. The
//! session releases the lock only after the tree was observed ended, so a
//! revocation that waits for the exclusive lock sees the process gone.

use std::fs::File;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use omgcode_platform::process::CancelSignal;

use super::ClaudeCodeFailure;
use super::launch::failure;
use crate::control::{ClaudeCodeWatch, ControlError, Recheck, TurnRecord, WatchCursor};
use crate::provider::ProviderError;

/// How often the watcher checks whether the settings changed.
const WATCH_POLL: Duration = Duration::from_millis(100);

/// A turn allowed to start: the connection's shared turn lock, the turn's
/// record, and where its watcher goes on checking the settings.
pub(super) struct HeldTurn {
    pub(super) lock: File,
    pub(super) record: TurnRecord,
    pub(super) cursor: WatchCursor,
}

/// Take the connection's shared turn lock, write the turn's record, and
/// then check every settings state recorded since `watch` was resolved. The
/// lock is taken first, so a change recorded before a revocation's wait
/// found the lock free is seen here. The record is written before the check,
/// so a removal that the check misses already finds the record reserving
/// the sign-in folder; one recorded earlier is seen by the check.
///
/// # Errors
///
/// [`ClaudeCodeFailure::Withdrawn`] when a revocation holds the lock or a
/// state withdrew the connection, [`ClaudeCodeFailure::TurnRecordsFull`]
/// when the turn records are full, and [`ClaudeCodeFailure::Failed`] when the
/// lock file cannot be used or the record cannot be written.
pub(super) fn hold_turn(watch: &ClaudeCodeWatch) -> Result<HeldTurn, ProviderError> {
    hold_turn_observed(watch, || {})
}

/// As [`hold_turn`], running `recorded` between the record and the check.
fn hold_turn_observed(
    watch: &ClaudeCodeWatch,
    recorded: impl FnOnce(),
) -> Result<HeldTurn, ProviderError> {
    let lock = watch
        .lock_turn()
        .map_err(|_| failure(ClaudeCodeFailure::Failed))?
        .ok_or(failure(ClaudeCodeFailure::Withdrawn))?;
    // A full inventory needs an acknowledgement, unlike a failed write.
    let record = watch.record_turn().map_err(|error| match error {
        ControlError::ClaudeCodeTurnRecordsFull => failure(ClaudeCodeFailure::TurnRecordsFull),
        _ => failure(ClaudeCodeFailure::Failed),
    })?;
    recorded();
    let mut cursor = watch.cursor();
    match watch.recheck_waiting(&mut cursor) {
        Recheck::Allowed => Ok(HeldTurn {
            lock,
            record,
            cursor,
        }),
        Recheck::Busy | Recheck::Withdrawn(_) => {
            release_unstarted(lock, record);
            Err(failure(ClaudeCodeFailure::Withdrawn))
        }
    }
}

/// End the hold of a turn that started no process: its record goes first,
/// then the lock, so a revocation that takes the lock finds no record. A
/// record that cannot be removed stays and fails every later wait until it
/// is acknowledged, which is the safe side of a turn that never ran.
pub(super) fn release_unstarted(lock: File, record: TurnRecord) {
    let _kept_if_failed = record.clear();
    drop(lock);
}

/// The thread that ends a process tree once the settings withdraw it.
pub(super) struct Watcher {
    done: Arc<AtomicBool>,
    withdrawn: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Watch the settings from `cursor` on and raise `stop`, the process
    /// tree's cancel signal, when a state withdraws the connection.
    ///
    /// # Errors
    ///
    /// [`ClaudeCodeFailure::Failed`] when the thread cannot start.
    pub(super) fn start(
        watch: ClaudeCodeWatch,
        cursor: WatchCursor,
        stop: CancelSignal,
    ) -> Result<Self, ProviderError> {
        Self::start_observed(watch, cursor, stop, WATCH_POLL, || {})
    }

    /// [`Self::start`], polling every `poll` and calling `polled` after each
    /// check of the loop, so a test can order a change after a poll.
    fn start_observed(
        watch: ClaudeCodeWatch,
        mut cursor: WatchCursor,
        stop: CancelSignal,
        poll: Duration,
        mut polled: impl FnMut() + Send + 'static,
    ) -> Result<Self, ProviderError> {
        let done = Arc::new(AtomicBool::new(false));
        let withdrawn = Arc::new(AtomicBool::new(false));
        let thread = {
            let done = Arc::clone(&done);
            let withdrawn = Arc::clone(&withdrawn);
            thread::Builder::new()
                .name("claude-code-watch".into())
                .spawn(move || {
                    while !done.load(Ordering::SeqCst) {
                        // Busy settings are checked again on the next poll.
                        if let Recheck::Withdrawn(_) = watch.recheck(&mut cursor) {
                            // Set before the tree ends, so a session that sees
                            // the process gone also sees why.
                            withdrawn.store(true, Ordering::SeqCst);
                            stop.cancel();
                            return;
                        }
                        polled();
                        thread::park_timeout(poll);
                    }
                    // A state recorded after the last poll still counts, so
                    // the turn's result is accepted only once every state up
                    // to the stop was checked. Busy settings fail closed.
                    if !matches!(watch.recheck_waiting(&mut cursor), Recheck::Allowed) {
                        withdrawn.store(true, Ordering::SeqCst);
                        stop.cancel();
                    }
                })
                .map_err(|_| failure(ClaudeCodeFailure::Failed))?
        };
        Ok(Self {
            done,
            withdrawn,
            thread: Some(thread),
        })
    }

    /// Whether the settings withdrew the connection.
    pub(super) fn withdrawn(&self) -> bool {
        self.withdrawn.load(Ordering::SeqCst)
    }

    /// Stop watching and join the thread, after its final check of the
    /// settings. A thread that panicked counts as a withdrawal, so the turn
    /// fails closed.
    pub(super) fn stop(&mut self) {
        self.stop_observed(|| {});
    }

    /// [`Self::stop`], calling `done_set` once the thread was told to stop
    /// and before it is woken and joined, so a test can hold the thread
    /// until then.
    fn stop_observed(&mut self, done_set: impl FnOnce()) {
        self.done.store(true, Ordering::SeqCst);
        done_set();
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            if thread.join().is_err() {
                self.withdrawn.store(true, Ordering::SeqCst);
            }
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::time::Duration;

    use omgcode_platform::process::CancelSignal;

    use uuid::Uuid;

    use super::{ClaudeCodeFailure, Watcher, hold_turn, hold_turn_observed, release_unstarted};
    use crate::control::{
        ClaudeCodeDeclaration, ClaudeCodeWatch, ControlError, ControlStore, MAX_TURN_RECORDS,
        acknowledge_claude_code_cleanup,
    };
    use crate::provider::ProviderError;

    /// A recorded, and on Windows granted, connection named `claude` in
    /// `directory`: its data root, program, sign-in folder, and watch.
    fn scene(directory: &Path) -> (PathBuf, PathBuf, PathBuf, ClaudeCodeWatch) {
        let data = directory.join("data");
        let root = directory.join("projects");
        let tools = directory.join("tools");
        let sign_in = directory.join("sign-in");
        let relay = directory.join("relay");
        for path in [&root, &tools, &sign_in, &relay] {
            fs::create_dir(path).expect("scene folder");
        }
        let program = tools.join(if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        });
        fs::write(&program, b"not a real program").expect("program");
        let relay_program = relay.join(if cfg!(windows) {
            "omgcode.exe"
        } else {
            "omgcode"
        });
        fs::write(&relay_program, b"not a real relay").expect("relay");
        let mut store = ControlStore::open(&data).expect("store");
        store
            .set_development_root(&root, store.revision())
            .expect("root");
        store
            .add_claude_code_profile(
                &ClaudeCodeDeclaration {
                    name: "claude".into(),
                    program: program.clone(),
                    config_folder: sign_in.clone(),
                },
                store.revision(),
            )
            .expect("profile");
        if cfg!(windows) {
            store
                .grant_claude_code("claude", &relay, store.revision())
                .expect("grants");
        }
        let watch = store
            .resolve_claude_code("claude", &relay_program)
            .expect("resolved")
            .watch;
        (data, program, sign_in, watch)
    }

    /// Withdraw the connection: on Windows by revoking its grants and then
    /// removing it, elsewhere by removing it.
    fn withdraw(data: &Path) {
        let mut store = ControlStore::open(data).expect("store");
        if cfg!(windows) {
            let revoked = store.revoke_claude_code_grants("claude").expect("revoked");
            assert!(revoked.profile_deleted);
        }
        let _removed = store
            .remove_provider_profile("claude", store.revision())
            .expect("removed");
    }

    /// A withdrawal recorded after the watcher's last poll, which the loop
    /// never checks because the watcher stops first, is still seen by the
    /// check at the stop: the turn is withdrawn and its tree stopped. The
    /// watcher waits after its first poll until it was told to stop, on a
    /// channel rather than in its timed park, so no poll of the loop, also
    /// after a spurious wake, can see the withdrawal first.
    #[test]
    fn a_withdrawal_recorded_after_the_last_poll_is_seen_at_the_stop() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (data, _program, _sign_in, watch) = scene(directory.path());

        let tree = CancelSignal::new();
        let (polled, polls) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let (waited, waits) = mpsc::channel();
        let cursor = watch.cursor();
        // The hook holds the loop, so its poll can be short. A long one could
        // outlast the test: the channel's own wait can take the wake that
        // `stop` sends, and the loop's park would then wait the whole poll.
        let mut watcher = Watcher::start_observed(
            watch,
            cursor,
            tree.clone(),
            Duration::from_millis(10),
            move || {
                let _ = polled.send(());
                // Bounded, so a failed test cannot leave the thread behind;
                // a timeout is reported rather than raised, since a watcher
                // that panicked counts as withdrawn.
                let _ = waited.send(released.recv_timeout(Duration::from_secs(30)).is_ok());
            },
        )
        .expect("watcher");
        polls
            .recv_timeout(Duration::from_secs(30))
            .expect("the first poll allowed the turn");

        let mut store = ControlStore::open(&data).expect("store");
        if cfg!(windows) {
            let revoked = store.revoke_claude_code_grants("claude").expect("revoked");
            assert!(revoked.profile_deleted);
        } else {
            let _removed = store
                .remove_provider_profile("claude", store.revision())
                .expect("removed");
        }
        drop(store);
        watcher.stop_observed(|| {
            let _ = release.send(());
        });
        assert_eq!(
            waits.try_iter().collect::<Vec<_>>(),
            [true],
            "the loop polled once and was held until the stop"
        );
        assert!(watcher.withdrawn(), "the change after the last poll");
        assert!(tree.is_cancelled(), "the tree is stopped");
    }

    /// A removal recorded after the turn wrote its record and before its
    /// settings check: the record already reserves the sign-in folder, so a
    /// new connection cannot take it while the turn may still start, and the
    /// check then withdraws the turn, which leaves no record and frees the
    /// lock. With the check before the record, the new connection would take
    /// the folder before any record existed.
    #[test]
    fn a_removal_between_the_turn_record_and_its_check_never_frees_the_folder() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (data, program, sign_in, watch) = scene(directory.path());
        let profile_id = ControlStore::open(&data)
            .expect("store")
            .claude_code_setup("claude")
            .expect("connection")
            .0;
        let again = ClaudeCodeDeclaration {
            name: "again".into(),
            program,
            config_folder: sign_in,
        };
        let held = hold_turn_observed(&watch, || {
            withdraw(&data);
            let mut store = ControlStore::open(&data).expect("store");
            let refused = store.add_claude_code_profile(&again, store.revision());
            assert!(
                matches!(refused, Err(ControlError::ClaudeCodeSignInReserved(id)) if id == profile_id),
                "the record reserves the folder: {refused:?}"
            );
        });
        assert!(
            matches!(
                held,
                Err(ProviderError::ClaudeCode(ClaudeCodeFailure::Withdrawn))
            ),
            "the check sees the removal"
        );
        // Nothing started: no record is left and the lock is free.
        assert!(!acknowledge_claude_code_cleanup(&data, profile_id).expect("free lock"));
        let mut store = ControlStore::open(&data).expect("store");
        store
            .add_claude_code_profile(&again, store.revision())
            .expect("the folder is free once the turn is gone");
    }

    /// A full turn-record inventory fails the turn with its own failure,
    /// whose description names the acknowledgement and the process-tree
    /// check before it, while a record that cannot be written for another
    /// reason fails it as `claude_code_failed`. Neither leaves a record.
    #[test]
    fn a_full_inventory_fails_the_turn_apart_from_other_record_failures() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (data, _program, _sign_in, watch) = scene(directory.path());
        let profile_id = Uuid::new_v4();
        let others = ClaudeCodeWatch::for_tests(data.clone(), profile_id, directory.path());
        for _ in 0..MAX_TURN_RECORDS {
            let _left = others.record_turn().expect("record below the limit");
        }
        let full = hold_turn(&watch).err();
        assert!(
            matches!(
                full,
                Some(ProviderError::ClaudeCode(
                    ClaudeCodeFailure::TurnRecordsFull
                ))
            ),
            "{full:?}"
        );
        let description = ClaudeCodeFailure::TurnRecordsFull.description();
        assert!(
            description.contains("workspace claude-code-acknowledge")
                && description.contains("MCP relays and their descendants"),
            "{description}"
        );
        assert!(acknowledge_claude_code_cleanup(&data, profile_id).expect("acknowledged"));

        // The records' lock cannot be opened once a folder takes its name.
        let lock = data.join("claude-code").join("turn-records.lock");
        fs::remove_file(&lock).expect("the records' lock file");
        fs::create_dir(&lock).expect("a folder in its place");
        let failed = hold_turn(&watch).err();
        assert!(
            matches!(
                failed,
                Some(ProviderError::ClaudeCode(ClaudeCodeFailure::Failed))
            ),
            "{failed:?}"
        );
        fs::remove_dir(&lock).expect("the folder");
        let Ok(held) = hold_turn(&watch) else {
            panic!("the turn records again");
        };
        release_unstarted(held.lock, held.record);
        withdraw(&data);
    }

    /// A running turn's record reserves its relay folder: while the turn
    /// holds it, no other connection's sign-in folder can be recorded there,
    /// though a separate one can. Once the turn ends, its record is gone;
    /// on Linux, where no grant records the relay folder, the folder is then
    /// accepted.
    #[test]
    fn a_running_turn_reserves_its_relay_folder() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (data, program, _sign_in, watch) = scene(directory.path());
        let profile_id = ControlStore::open(&data)
            .expect("store")
            .claude_code_setup("claude")
            .expect("connection")
            .0;
        let inside = directory.path().join("relay").join("sign-in");
        let apart = directory.path().join("apart-sign-in");
        for folder in [&inside, &apart] {
            fs::create_dir(folder).expect("sign-in folder");
        }
        let declaration = |name: &str, config_folder: &Path| ClaudeCodeDeclaration {
            name: name.into(),
            program: program.clone(),
            config_folder: config_folder.to_owned(),
        };
        let Ok(held) = hold_turn(&watch) else {
            panic!("the turn starts");
        };
        let mut store = ControlStore::open(&data).expect("store");
        let refused =
            store.add_claude_code_profile(&declaration("inside", &inside), store.revision());
        assert!(
            matches!(refused, Err(ControlError::ClaudeCodeScopeReserved(id)) if id == profile_id),
            "the record reserves the relay folder: {refused:?}"
        );
        store
            .add_claude_code_profile(&declaration("apart", &apart), store.revision())
            .expect("a separate folder is accepted");
        release_unstarted(held.lock, held.record);
        let after =
            store.add_claude_code_profile(&declaration("inside", &inside), store.revision());
        if cfg!(windows) {
            assert!(
                matches!(
                    after,
                    Err(ControlError::ProviderRefused(
                        "the Claude sign-in folder must not overlap another Claude Code connection's program or relay grant"
                    ))
                ),
                "the relay grant still refuses it: {after:?}"
            );
        } else {
            after.expect("no record or grant reserves the folder");
        }
        drop(store);
        withdraw(&data);
    }

    /// A sign-in folder recorded inside the turn's relay folder after the
    /// turn resolved and before its record existed: no record refused it,
    /// and on Linux no grant records the relay folder, so the turn's check
    /// withdraws the turn before any process starts, and leaves no record.
    /// On Windows the relay grant refuses the folder.
    #[cfg(not(windows))]
    #[test]
    fn a_sign_in_folder_recorded_in_the_relay_folder_before_the_record_withdraws_the_turn() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (data, program, _sign_in, watch) = scene(directory.path());
        let inside = directory.path().join("relay").join("sign-in");
        fs::create_dir(&inside).expect("sign-in folder in the relay folder");
        let mut store = ControlStore::open(&data).expect("store");
        store
            .add_claude_code_profile(
                &ClaudeCodeDeclaration {
                    name: "inside".into(),
                    program,
                    config_folder: inside,
                },
                store.revision(),
            )
            .expect("no record or grant refuses it yet");
        drop(store);
        let held = hold_turn(&watch).err();
        assert!(
            matches!(
                held,
                Some(ProviderError::ClaudeCode(ClaudeCodeFailure::Withdrawn))
            ),
            "the check sees the folder: {held:?}"
        );
        let profile_id = ControlStore::open(&data)
            .expect("store")
            .claude_code_setup("claude")
            .expect("connection")
            .0;
        assert!(!acknowledge_claude_code_cleanup(&data, profile_id).expect("free lock"));
    }
}
