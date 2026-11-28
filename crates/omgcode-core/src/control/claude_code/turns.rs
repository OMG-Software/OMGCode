//! Running Claude Code turns, and the settings changes that stop them
//! (ADR-0109, as ADR-0061 does for runs).
//!
//! Each running `claude` process holds a shared lock on its connection's
//! turn lock file, which lies beside the connection's runtime folder and so
//! outside every folder its container is granted. The lock is taken before
//! the process starts and released only after the process tree and the
//! turn's threads were observed ended. A revocation or removal records its
//! change first and then waits for an exclusive lock, which it gets only once
//! no such process remains.
//!
//! A process's watcher rechecks every settings state recorded after its turn
//! resolved the connection, not only the latest, so a change undone before
//! the watcher reads the settings still stops it.
//!
//! Each turn also writes a durable record beside the lock before its process
//! starts, and removes it only once its tree was observed ended. A record
//! left behind, by a tree whose end was not confirmed or by an OMGCode
//! process that exited or failed first, outlives that process: the end of a
//! host proves nothing about the tree. Every wait fails while a record of the
//! connection exists, until the operator acknowledges it, as
//! `runs acknowledge` does for a run. The record names the turn's sign-in
//! folder, which stays reserved while it exists, also once the connection is
//! removed, and the program and relay folders the turn can read, in which no
//! other connection's sign-in folder may then be recorded: on Linux no grant
//! records them. At most [`MAX_TURN_RECORDS`] records are kept: a turn starts no
//! process once that many exist, and a reservation check refuses beyond
//! them, while an acknowledgement still clears its connection's records.
//! A turn counts the records and writes its own under one exclusive lock
//! across every connection, so two turns cannot both take the last free
//! record.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{ClaudeCodeGrantRole, RUNTIME_PARENT, platform_grants_supported};
use crate::control::{CONTROL_BUSY_WAIT, ContainerGrantState, ControlError, ControlStore};
use crate::workspace::DirectoryRecord;

/// How long a revocation or removal waits for the connection's running turns
/// to end.
pub const CLAUDE_CODE_TURN_STOP_WAIT: Duration = Duration::from_secs(30);

/// Poll interval while waiting for a lock or for busy settings.
const POLL: Duration = Duration::from_millis(50);

/// The suffix of a connection's turn lock file name.
const TURN_LOCK_SUFFIX: &str = ".turns";

/// The suffix of a turn's record, named `<profile>.<turn>.unconfirmed`.
const TURN_RECORD_SUFFIX: &str = ".unconfirmed";

/// The longest turn record read back; one holds a profile identifier and
/// three folder records.
const MAX_TURN_RECORD_BYTES: u64 = 64 * 1024;

/// The most turn records kept in application storage, across every
/// connection. Each is a turn whose tree was not observed ended.
pub const MAX_TURN_RECORDS: usize = 256;

/// The lock under which a turn counts the turn records and writes its
/// own, beside the turn lock files and outside every grant.
const RECORDS_LOCK: &str = "turn-records.lock";

/// How long a turn waits for another to count the records and write its
/// own; that takes one count and one synced write.
const RECORDS_LOCK_WAIT: Duration = Duration::from_secs(5);

/// The settings one turn of a Claude Code connection was resolved from: the
/// application storage, the connection, its sign-in folder, the program and
/// relay folders the turn can read, and how many settings events were
/// recorded then. Only [`ControlStore::resolve_claude_code`] makes one, under
/// the settings lock.
#[derive(Clone, Debug)]
pub struct ClaudeCodeWatch {
    data_root: PathBuf,
    profile_id: Uuid,
    folders: TurnFolders,
    events: usize,
}

/// The folders one turn uses: its sign-in folder, and the program and relay
/// folders it can read whole.
#[derive(Clone, Debug)]
pub(super) struct TurnFolders {
    pub(super) config_folder: DirectoryRecord,
    pub(super) program_folder: DirectoryRecord,
    pub(super) relay_folder: DirectoryRecord,
}

impl TurnFolders {
    /// The folders the turn can read whole.
    pub(super) const fn readable(&self) -> [&DirectoryRecord; 2] {
        [&self.program_folder, &self.relay_folder]
    }
}

/// How far a watcher has read the settings.
#[derive(Debug)]
pub(crate) struct WatchCursor {
    seen: Option<(u64, Option<SystemTime>)>,
    events: usize,
}

/// What one settings check found.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Recheck {
    /// Every state since the last check allows the turn.
    Allowed,
    /// Another command holds the settings; check again later.
    Busy,
    /// A state since the last check withdrew the connection.
    Withdrawn(&'static str),
}

/// The durable record of one turn, written by [`ClaudeCodeWatch::record_turn`]
/// before its process starts. Dropping it keeps the file: only
/// [`Self::clear`] removes it, once the turn's tree was observed ended.
#[derive(Debug)]
#[must_use = "keep the record until the turn's tree was observed ended"]
pub(crate) struct TurnRecord {
    path: PathBuf,
}

impl TurnRecord {
    /// Remove the record. A record already gone counts as removed.
    ///
    /// # Errors
    ///
    /// The I/O error that stopped the removal; the record then stays and
    /// fails every later wait until acknowledged.
    pub(crate) fn clear(self) -> io::Result<()> {
        match fs::remove_file(&self.path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

/// What a turn record holds. A record without the program and relay
/// folders, as an earlier build of this change wrote, cannot be read, so it
/// reserves every folder until it is acknowledged.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TurnRecordBody {
    profile_id: Uuid,
    config_folder: DirectoryRecord,
    program_folder: DirectoryRecord,
    relay_folder: DirectoryRecord,
}

impl ClaudeCodeWatch {
    pub(super) const fn new(
        data_root: PathBuf,
        profile_id: Uuid,
        folders: TurnFolders,
        events: usize,
    ) -> Self {
        Self {
            data_root,
            profile_id,
            folders,
            events,
        }
    }

    /// A watch of `profile_id`'s turns in `data_root`, for tests that need
    /// only its lock and records, whose sign-in, program, and relay folders
    /// are all `folder`.
    ///
    /// # Panics
    ///
    /// When `folder` cannot be captured.
    #[cfg(test)]
    pub(crate) fn for_tests(data_root: PathBuf, profile_id: Uuid, folder: &Path) -> Self {
        let folder = DirectoryRecord::capture(folder).expect("turn folder");
        let folders = TurnFolders {
            config_folder: folder.clone(),
            program_folder: folder.clone(),
            relay_folder: folder,
        };
        Self::new(data_root, profile_id, folders, 0)
    }

    /// Record, durably, that a turn of the connection is starting. Call it
    /// while the turn holds its shared lock and before its process starts.
    /// Until [`TurnRecord::clear`] removes it, every wait for the
    /// connection's turns fails, also after this process exits, the turn's
    /// sign-in folder stays reserved, and no other connection's sign-in
    /// folder may be recorded in its program or relay folder.
    ///
    /// # Errors
    ///
    /// [`ControlError::ClaudeCodeTurnRecordsFull`] when [`MAX_TURN_RECORDS`]
    /// records already exist, and [`ControlError::Io`] for the I/O error that
    /// stopped writing or syncing the record or for another turn holding the
    /// records' lock for [`RECORDS_LOCK_WAIT`]. No process may start then.
    pub(crate) fn record_turn(&self) -> Result<TurnRecord, ControlError> {
        self.record_turn_observed(|| {})
    }

    /// [`Self::record_turn`], calling `counted` once the records were
    /// counted and before the new one is written, so a test can start
    /// another turn there.
    pub(super) fn record_turn_observed(
        &self,
        counted: impl FnOnce(),
    ) -> Result<TurnRecord, ControlError> {
        // Held until the record is written and synced, so two turns, also
        // of two connections, cannot both count a free record.
        let _records = lock_records(&self.data_root)?;
        let mut kept = 0;
        each_turn_record(&self.data_root, |_, _| {
            kept += 1;
            Ok(if kept >= MAX_TURN_RECORDS {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            })
        })?;
        if kept >= MAX_TURN_RECORDS {
            return Err(ControlError::ClaudeCodeTurnRecordsFull);
        }
        counted();
        let body = serde_json::to_vec(&TurnRecordBody {
            profile_id: self.profile_id,
            config_folder: self.folders.config_folder.clone(),
            program_folder: self.folders.program_folder.clone(),
            relay_folder: self.folders.relay_folder.clone(),
        })
        .map_err(io::Error::other)?;
        let parent = self.data_root.join(RUNTIME_PARENT);
        let path = parent.join(format!(
            "{}.{}{TURN_RECORD_SUFFIX}",
            self.profile_id.simple(),
            Uuid::new_v4().simple()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        let written = file
            .write_all(&body)
            .and_then(|()| file.sync_all())
            .and_then(|()| sync_directory(&parent));
        if let Err(error) = written {
            drop(file);
            // No process started, so nothing needs the record; one that
            // cannot be removed fails closed, as an unconfirmed end does.
            return Err(ControlError::Io(match fs::remove_file(&path) {
                Ok(()) => error,
                Err(removal) => io::Error::other(format!("{error}; the record stays: {removal}")),
            }));
        }
        Ok(TurnRecord { path })
    }

    /// A cursor at the state the turn was resolved from.
    pub(crate) const fn cursor(&self) -> WatchCursor {
        WatchCursor {
            seen: None,
            events: self.events,
        }
    }

    /// Take a shared lock on the connection's turn lock file, creating it
    /// when missing. `Ok(None)` when a revocation or removal holds it.
    ///
    /// # Errors
    ///
    /// The I/O error that stopped opening or locking the file.
    pub(crate) fn lock_turn(&self) -> io::Result<Option<File>> {
        let file = open_turn_lock(&self.data_root, self.profile_id)?;
        match file.try_lock_shared() {
            Ok(()) => Ok(Some(file)),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }

    /// Check every settings state recorded since `cursor`, when the settings
    /// changed. Settings that cannot be read withdraw the turn.
    pub(crate) fn recheck(&self, cursor: &mut WatchCursor) -> Recheck {
        let Ok(current) = stamp(&ControlStore::journal_path(&self.data_root)) else {
            return Recheck::Withdrawn("the settings could not be read");
        };
        if cursor.seen == Some(current) {
            return Recheck::Allowed;
        }
        match ControlStore::open_existing_since(&self.data_root, cursor.events) {
            Ok(Some((mut control, history))) => {
                cursor.seen = Some(current);
                cursor.events = control.event_count();
                history
                    .into_iter()
                    .find_map(|state| control.as_of(state, |settings| self.withdrawn_by(settings)))
                    .or_else(|| self.withdrawn_by(&control))
                    .map_or(Recheck::Allowed, Recheck::Withdrawn)
            }
            Ok(None) => Recheck::Withdrawn("the settings were removed"),
            Err(ControlError::Busy) => Recheck::Busy,
            Err(_) => Recheck::Withdrawn("the settings could not be read"),
        }
    }

    /// As [`Self::recheck`], retrying busy settings for up to
    /// [`CONTROL_BUSY_WAIT`]; settings that stay busy withdraw the turn.
    pub(crate) fn recheck_waiting(&self, cursor: &mut WatchCursor) -> Recheck {
        let started = Instant::now();
        loop {
            match self.recheck(cursor) {
                Recheck::Busy if started.elapsed() < CONTROL_BUSY_WAIT => thread::sleep(POLL),
                Recheck::Busy => return Recheck::Withdrawn("the settings stayed busy"),
                other => return other,
            }
        }
    }

    /// Why `settings` no longer allow this turn, if they don't: the
    /// connection was withdrawn, or another connection's sign-in folder now
    /// overlaps a folder the turn can read. A sign-in folder recorded after
    /// the turn resolved and before its record existed was admitted without
    /// seeing the record, so this check stops the turn instead.
    fn withdrawn_by(&self, settings: &ControlStore) -> Option<&'static str> {
        settings
            .claude_code_withdrawn(self.profile_id)
            .or_else(|| {
                self.folders.readable().into_iter().find_map(|folder| {
                    match settings.overlaps_claude_code_sign_in(folder, Some(self.profile_id)) {
                        Ok(false) => None,
                        Ok(true) => Some(
                            "another Claude Code connection's sign-in folder overlaps a folder the turn can read",
                        ),
                        // A moved or reserved folder could lie in one.
                        Err(_) => Some(
                            "the Claude Code sign-in folders could not be proven apart from the turn's folders",
                        ),
                    }
                })
            })
    }
}

impl ControlStore {
    /// Why these settings no longer allow a running turn of the Claude Code
    /// connection `profile_id`, if they don't: the connection was removed,
    /// or, where grants exist, one of its roles has no active grant.
    pub(crate) fn claude_code_withdrawn(&self, profile_id: Uuid) -> Option<&'static str> {
        let recorded = self.provider_profiles().iter().any(|status| {
            status.profile().id() == profile_id && status.profile().claude_code_setup().is_some()
        });
        if !recorded {
            return Some("the Claude Code connection was removed");
        }
        let granted = |role: ClaudeCodeGrantRole| {
            self.snapshot.claude_code_grants.iter().any(|status| {
                status.grant.profile_id == profile_id
                    && status.grant.role == role
                    && status.state == ContainerGrantState::Active
            })
        };
        if platform_grants_supported() && !ClaudeCodeGrantRole::ALL.into_iter().all(granted) {
            return Some("a grant of the Claude Code connection was revoked");
        }
        None
    }
}

/// Wait until no running turn of the Claude Code connection `profile_id`
/// holds its turn lock: every process that could use the connection was
/// observed ended. Call it after the change that withdrew the connection is
/// recorded and the settings are released, so the turns' watchers can read
/// it.
///
/// # Errors
///
/// [`ControlError::ClaudeCodeTurnsRunning`] when a turn still holds the lock
/// after `timeout`, [`ControlError::ClaudeCodeCleanupUnconfirmed`] while a
/// turn record of the connection is left and not acknowledged, and
/// [`ControlError::Io`] when the lock file or the records cannot be checked.
pub fn wait_for_claude_code_turns(
    data_root: &Path,
    profile_id: Uuid,
    timeout: Duration,
) -> Result<(), ControlError> {
    wait_for_turns_observed(data_root, profile_id, timeout, || {})
}

/// [`wait_for_claude_code_turns`], calling `contended` after each attempt
/// that found a turn holding the lock, before it waits for the next.
pub(super) fn wait_for_turns_observed(
    data_root: &Path,
    profile_id: Uuid,
    timeout: Duration,
    mut contended: impl FnMut(),
) -> Result<(), ControlError> {
    // A missing file means no turn ever started; one that starts now finds
    // the recorded change when it checks the settings before its launch.
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(turn_lock_path(data_root, profile_id))
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return refuse_unconfirmed(data_root, profile_id);
        }
        Err(error) => return Err(ControlError::Io(error)),
    };
    let until = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    loop {
        match file.try_lock() {
            Ok(()) => {
                let unconfirmed = refuse_unconfirmed(data_root, profile_id);
                // Dropping the handle releases the lock too.
                let _ = file.unlock();
                return unconfirmed;
            }
            Err(TryLockError::WouldBlock) if Instant::now() < until => {
                contended();
                thread::sleep(POLL);
            }
            Err(TryLockError::WouldBlock) => return Err(ControlError::ClaudeCodeTurnsRunning),
            Err(TryLockError::Error(error)) => return Err(ControlError::Io(error)),
        }
    }
}

/// Clear every turn record the Claude Code connection `profile_id` left: the
/// turns whose process tree was not observed ended. The operator first
/// checks that no `claude` process of the connection remains, including its
/// relays and their descendants; this records nothing about that check.
/// `Ok(false)` when nothing was left.
///
/// # Errors
///
/// [`ControlError::ClaudeCodeTurnLockHeld`] while a turn of the connection
/// holds its lock, including one whose end this process could not confirm,
/// and [`ControlError::Io`] when the lock or a record cannot be used.
pub fn acknowledge_claude_code_cleanup(
    data_root: &Path,
    profile_id: Uuid,
) -> Result<bool, ControlError> {
    // The exclusive lock keeps a turn from starting, and so from writing a
    // record, while the records go. With no runtime folder no turn ever ran.
    let lock = match open_turn_lock(data_root, profile_id) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(ControlError::Io(error)),
    };
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(ControlError::ClaudeCodeTurnLockHeld),
        Err(TryLockError::Error(error)) => return Err(ControlError::Io(error)),
    }
    // Visited one at a time, so a full inventory does not stop it.
    let mut removed = false;
    each_turn_record(data_root, |owner, path| {
        if owner == profile_id {
            fs::remove_file(path)?;
            removed = true;
        }
        Ok(ControlFlow::Continue(()))
    })?;
    drop(lock);
    Ok(removed)
}

/// The folders that turn records reserve, with the connection of each,
/// leaving out the connections `skip` accepts.
///
/// # Errors
///
/// [`ControlError::ClaudeCodeTurnRecordUnreadable`] for a record that cannot
/// be read or that names another connection than its file name: it proves
/// nothing about where its folders are. [`ControlError::ClaudeCodeTurnRecordsFull`]
/// beyond [`MAX_TURN_RECORDS`] records, and [`ControlError::Io`] when the
/// records cannot be listed.
pub(super) fn reserved_folders(
    data_root: &Path,
    skip: impl Fn(Uuid) -> bool,
) -> Result<Vec<(Uuid, TurnFolders)>, ControlError> {
    let mut reserved = Vec::new();
    for (owner, path) in turn_records(data_root)? {
        if skip(owner) {
            continue;
        }
        let body = read_turn_record(&path)
            .ok()
            .filter(|body| body.profile_id == owner)
            .ok_or(ControlError::ClaudeCodeTurnRecordUnreadable(owner))?;
        reserved.push((
            owner,
            TurnFolders {
                config_folder: body.config_folder,
                program_folder: body.program_folder,
                relay_folder: body.relay_folder,
            },
        ));
    }
    Ok(reserved)
}

/// [`ControlError::ClaudeCodeCleanupUnconfirmed`] while a record of the
/// connection exists.
fn refuse_unconfirmed(data_root: &Path, profile_id: Uuid) -> Result<(), ControlError> {
    let mut found = false;
    each_turn_record(data_root, |owner, _| {
        found = owner == profile_id;
        Ok(if found {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        })
    })?;
    if found {
        Err(ControlError::ClaudeCodeCleanupUnconfirmed(profile_id))
    } else {
        Ok(())
    }
}

/// Every turn record in `data_root`, with the connection its name gives.
///
/// # Errors
///
/// [`ControlError::ClaudeCodeTurnRecordsFull`] beyond [`MAX_TURN_RECORDS`]
/// records, before any more is kept, and [`ControlError::Io`] when the
/// records cannot be listed.
fn turn_records(data_root: &Path) -> Result<Vec<(Uuid, PathBuf)>, ControlError> {
    let mut records = Vec::new();
    let mut full = false;
    each_turn_record(data_root, |owner, path| {
        if records.len() == MAX_TURN_RECORDS {
            full = true;
            return Ok(ControlFlow::Break(()));
        }
        records.push((owner, path));
        Ok(ControlFlow::Continue(()))
    })?;
    if full {
        Err(ControlError::ClaudeCodeTurnRecordsFull)
    } else {
        Ok(records)
    }
}

/// Visit each turn record in `data_root`, with the connection its name
/// gives, until `visit` breaks or fails. A name of another shape is no
/// record. Only the current entry is held, so the visit costs no memory
/// per record.
fn each_turn_record(
    data_root: &Path,
    mut visit: impl FnMut(Uuid, PathBuf) -> io::Result<ControlFlow<()>>,
) -> io::Result<()> {
    let entries = match fs::read_dir(data_root.join(RUNTIME_PARENT)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some((profile, turn)) = name
            .to_str()
            .and_then(|name| name.strip_suffix(TURN_RECORD_SUFFIX))
            .and_then(|stem| stem.split_once('.'))
        else {
            continue;
        };
        if let (Ok(profile), Ok(_)) = (Uuid::try_parse(profile), Uuid::try_parse(turn))
            && visit(profile, entry.path())?.is_break()
        {
            break;
        }
    }
    Ok(())
}

fn read_turn_record(path: &Path) -> io::Result<TurnRecordBody> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_TURN_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_TURN_RECORD_BYTES {
        return Err(io::Error::other("the turn record is too large"));
    }
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

/// Make a new entry in `directory` durable. Windows file systems record
/// directory changes in their own journal, and a directory cannot be opened
/// for syncing there.
fn sync_directory(directory: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(directory)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        Ok(())
    }
}

/// The turn lock file of one connection, beside its runtime folder.
pub(super) fn turn_lock_path(data_root: &Path, profile_id: Uuid) -> PathBuf {
    data_root
        .join(RUNTIME_PARENT)
        .join(format!("{}{TURN_LOCK_SUFFIX}", profile_id.simple()))
}

/// Open the connection's turn lock file, creating it when missing.
fn open_turn_lock(data_root: &Path, profile_id: Uuid) -> io::Result<File> {
    open_lock(&turn_lock_path(data_root, profile_id))
}

/// Take the exclusive lock under which a turn counts the turn records and
/// writes its own, waiting at most [`RECORDS_LOCK_WAIT`] for another turn.
///
/// # Errors
///
/// The I/O error that stopped opening or locking the file, and one that
/// says another turn held it for the whole wait.
fn lock_records(data_root: &Path) -> io::Result<File> {
    let file = open_lock(&data_root.join(RUNTIME_PARENT).join(RECORDS_LOCK))?;
    let until = Instant::now()
        .checked_add(RECORDS_LOCK_WAIT)
        .unwrap_or_else(Instant::now);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if Instant::now() < until => thread::sleep(POLL),
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "another Claude Code turn held the turn records' lock",
                ));
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
}

/// Open a lock file, creating it, private, when missing.
fn open_lock(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// The size and modification time of the settings journal.
fn stamp(path: &Path) -> io::Result<(u64, Option<SystemTime>)> {
    let metadata = std::fs::symlink_metadata(path)?;
    Ok((metadata.len(), metadata.modified().ok()))
}
