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
use crate::control::{ClaudeCodeWatch, Recheck, WatchCursor};
use crate::provider::ProviderError;

/// How often the watcher checks whether the settings changed.
const WATCH_POLL: Duration = Duration::from_millis(100);

/// Take the connection's shared turn lock, then check every settings state
/// recorded since `watch` was resolved. The lock is taken first, so a change
/// recorded before a revocation's wait found the lock free is seen here.
///
/// # Errors
///
/// [`ClaudeCodeFailure::Withdrawn`] when a revocation holds the lock or a
/// state withdrew the connection, and [`ClaudeCodeFailure::Failed`] when the
/// lock file cannot be used.
pub(super) fn hold_turn(watch: &ClaudeCodeWatch) -> Result<(File, WatchCursor), ProviderError> {
    let lock = watch
        .lock_turn()
        .map_err(|_| failure(ClaudeCodeFailure::Failed))?
        .ok_or(failure(ClaudeCodeFailure::Withdrawn))?;
    let mut cursor = watch.cursor();
    match watch.recheck_waiting(&mut cursor) {
        Recheck::Allowed => Ok((lock, cursor)),
        Recheck::Busy | Recheck::Withdrawn(_) => Err(failure(ClaudeCodeFailure::Withdrawn)),
    }
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
        self.done.store(true, Ordering::SeqCst);
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
    use std::sync::mpsc;
    use std::time::Duration;

    use omgcode_platform::process::CancelSignal;

    use super::Watcher;
    use crate::control::{ClaudeCodeDeclaration, ControlStore};

    /// A withdrawal recorded after the watcher's last poll, which the loop
    /// never checks because the watcher stops first, is still seen by the
    /// check at the stop: the turn is withdrawn and its tree stopped.
    #[test]
    fn a_withdrawal_recorded_after_the_last_poll_is_seen_at_the_stop() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let data = directory.path().join("data");
        let root = directory.path().join("projects");
        let tools = directory.path().join("tools");
        let sign_in = directory.path().join("sign-in");
        let relay = directory.path().join("relay");
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
                    program,
                    config_folder: sign_in,
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
        drop(store);

        let tree = CancelSignal::new();
        let (polled, polls) = mpsc::channel();
        let cursor = watch.cursor();
        // The loop polls once and then not again for an hour, so only the
        // check at the stop can see the change below.
        let mut watcher = Watcher::start_observed(
            watch,
            cursor,
            tree.clone(),
            Duration::from_secs(3600),
            move || {
                let _ = polled.send(());
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
        watcher.stop();
        assert!(watcher.withdrawn(), "the change after the last poll");
        assert!(tree.is_cancelled(), "the tree is stopped");
    }
}
