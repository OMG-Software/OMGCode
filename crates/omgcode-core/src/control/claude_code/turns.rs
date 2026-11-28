//! Running Claude Code turns, and the settings changes that stop them
//! (ADR-0109, as ADR-0061 does for runs).
//!
//! Each running `claude` process holds a shared lock on its connection's
//! turn lock file, which lies beside the connection's runtime folder and so
//! outside every folder its container is granted. The lock is taken before
//! the process starts and released only after the process tree was observed
//! ended. A revocation or removal records its change first and then waits for
//! an exclusive lock, which it gets only once no such process remains.
//!
//! A process's watcher rechecks every settings state recorded after its turn
//! resolved the connection, not only the latest, so a change undone before
//! the watcher reads the settings still stops it.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use uuid::Uuid;

use super::{ClaudeCodeGrantRole, RUNTIME_PARENT, platform_grants_supported};
use crate::control::{CONTROL_BUSY_WAIT, ContainerGrantState, ControlError, ControlStore};

/// How long a revocation or removal waits for the connection's running turns
/// to end.
pub const CLAUDE_CODE_TURN_STOP_WAIT: Duration = Duration::from_secs(30);

/// Poll interval while waiting for a lock or for busy settings.
const POLL: Duration = Duration::from_millis(50);

/// The suffix of a connection's turn lock file name.
const TURN_LOCK_SUFFIX: &str = ".turns";

/// The settings one turn of a Claude Code connection was resolved from: the
/// application storage, the connection, and how many settings events were
/// recorded then. Only [`ControlStore::resolve_claude_code`] makes one, under
/// the settings lock.
#[derive(Clone, Debug)]
pub struct ClaudeCodeWatch {
    data_root: PathBuf,
    profile_id: Uuid,
    events: usize,
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

impl ClaudeCodeWatch {
    pub(super) const fn new(data_root: PathBuf, profile_id: Uuid, events: usize) -> Self {
        Self {
            data_root,
            profile_id,
            events,
        }
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
        let file = open_turn_lock(&self.data_root, self.profile_id, true)?;
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
                let profile_id = self.profile_id;
                history
                    .into_iter()
                    .find_map(|state| {
                        control.as_of(state, |settings| settings.claude_code_withdrawn(profile_id))
                    })
                    .or_else(|| control.claude_code_withdrawn(profile_id))
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
/// after `timeout`, and [`ControlError::Io`] when the lock file cannot be
/// opened or locked.
pub fn wait_for_claude_code_turns(
    data_root: &Path,
    profile_id: Uuid,
    timeout: Duration,
) -> Result<(), ControlError> {
    // A missing file means no turn ever started; one that starts now finds
    // the recorded change when it checks the settings before its launch.
    let file = match open_turn_lock(data_root, profile_id, false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(ControlError::Io(error)),
    };
    let until = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    loop {
        match file.try_lock() {
            Ok(()) => {
                // Dropping the handle releases the lock too.
                let _ = file.unlock();
                return Ok(());
            }
            Err(TryLockError::WouldBlock) if Instant::now() < until => thread::sleep(POLL),
            Err(TryLockError::WouldBlock) => return Err(ControlError::ClaudeCodeTurnsRunning),
            Err(TryLockError::Error(error)) => return Err(ControlError::Io(error)),
        }
    }
}

/// The turn lock file of one connection, beside its runtime folder.
pub(super) fn turn_lock_path(data_root: &Path, profile_id: Uuid) -> PathBuf {
    data_root
        .join(RUNTIME_PARENT)
        .join(format!("{}{TURN_LOCK_SUFFIX}", profile_id.simple()))
}

fn open_turn_lock(data_root: &Path, profile_id: Uuid, create: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(turn_lock_path(data_root, profile_id))
}

/// The size and modification time of the settings journal.
fn stamp(path: &Path) -> io::Result<(u64, Option<SystemTime>)> {
    let metadata = std::fs::symlink_metadata(path)?;
    Ok((metadata.len(), metadata.modified().ok()))
}
