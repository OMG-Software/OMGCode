//! Shared helpers over the service state: locks, bounded refresh, journal
//! resolution, the settings-store opener, and session summaries.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::MutexGuard;
use std::thread;
use std::time::{Duration, Instant};

use omgcode_core::control::{ControlError, ControlStore};
use omgcode_core::session::{NativePath, SessionInspection};
use omgcode_core::session_index::{IndexedSession, IndexedState, SessionIndex};
use omgcode_core::turn::{AwaitedCall, display_safe_line};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::Inner;
use super::error::DesktopError;
use super::review_tokens::ReviewTokens;
use super::turns::TurnTable;
use super::wire::{SessionStatus, SessionSummary};

/// Journals inspected per `refresh` call.
const REFRESH_CHUNK: usize = 200;
/// Longest a bounded refresh keeps starting new chunks.
const REFRESH_DEADLINE: Duration = Duration::from_secs(2);
/// A completed refresh newer than this makes the next listing skip refreshing.
const REFRESH_REUSE: Duration = Duration::from_secs(1);
/// Longest a settings-store open waits for another holder.
const CONTROL_RETRY_DEADLINE: Duration = Duration::from_secs(2);
/// Pause between settings-store open attempts.
const CONTROL_RETRY_INTERVAL: Duration = Duration::from_millis(20);

/// Folder text for a session whose journal recorded no workspace.
pub(super) const UNKNOWN_FOLDER: &str = "(unknown folder)";
/// Title of a busy session the index never read.
const TITLE_IN_USE: &str = "(session in use)";
/// Title of a session whose journal failed validation.
const TITLE_UNREADABLE: &str = "(unreadable session)";
/// Title of a readable session that somehow has none.
const TITLE_UNTITLED: &str = "(untitled session)";
/// Folder text for a path that another operating system recorded.
const FOLDER_NOT_REPRESENTABLE: &str = "(path not representable on this system)";

/// The index connection with its refresh bookkeeping.
pub(super) struct IndexState {
    pub(super) index: SessionIndex,
    /// When the last refresh that reached `remaining == 0` finished.
    last_refresh: Option<Instant>,
    /// `skipped_links + unrecognized` of the last refresh report.
    pub(super) skipped_entries: u64,
}

impl IndexState {
    pub(super) const fn new(index: SessionIndex) -> Self {
        Self {
            index,
            last_refresh: None,
            skipped_entries: 0,
        }
    }

    /// Bring the index up to date within a time bound.
    ///
    /// Calls `refresh(200)` until nothing remains or the deadline passes. A
    /// refresh that stops at the deadline is partial; the next call continues.
    /// Unless `force` is set, a completed refresh less than a second old is
    /// reused.
    pub(super) fn refresh_bounded(&mut self, force: bool) -> Result<(), DesktopError> {
        if !force
            && self
                .last_refresh
                .is_some_and(|finished| finished.elapsed() < REFRESH_REUSE)
        {
            return Ok(());
        }
        let started = Instant::now();
        loop {
            let report = self
                .index
                .refresh(REFRESH_CHUNK)
                .map_err(|error| DesktopError::from_index(&error))?;
            self.skipped_entries = report.skipped_links.saturating_add(report.unrecognized);
            if report.remaining == 0 {
                self.last_refresh = Some(Instant::now());
                return Ok(());
            }
            if started.elapsed() >= REFRESH_DEADLINE {
                return Ok(());
            }
        }
    }
}

impl Inner {
    pub(super) fn lock_index(&self) -> Result<MutexGuard<'_, IndexState>, DesktopError> {
        self.index.lock().map_err(|_| DesktopError::poisoned())
    }

    pub(super) fn lock_turns(&self) -> Result<MutexGuard<'_, TurnTable>, DesktopError> {
        self.turns.lock().map_err(|_| DesktopError::poisoned())
    }

    pub(super) fn lock_reviews(&self) -> Result<MutexGuard<'_, ReviewTokens>, DesktopError> {
        self.reviews.lock().map_err(|_| DesktopError::poisoned())
    }

    /// Run `action` with the index locked. Keep `action` short.
    pub(super) fn with_index<R>(
        &self,
        action: impl FnOnce(&mut IndexState) -> Result<R, DesktopError>,
    ) -> Result<R, DesktopError> {
        let mut state = self.lock_index()?;
        action(&mut state)
    }

    /// The sessions with a chat turn running in this service.
    pub(super) fn running_sessions(&self) -> Result<HashSet<Uuid>, DesktopError> {
        self.reap()?;
        Ok(self.lock_turns()?.running_ids())
    }

    /// Whether a chat turn for `session_id` is running in this service.
    pub(super) fn is_running(&self, session_id: Uuid) -> Result<bool, DesktopError> {
        self.reap()?;
        Ok(self.lock_turns()?.is_running(session_id))
    }

    /// The call the session's running turn waits on, if it is waiting.
    pub(super) fn awaited_call(
        &self,
        session_id: Uuid,
    ) -> Result<Option<AwaitedCall>, DesktopError> {
        self.reap()?;
        Ok(self.lock_turns()?.awaited_call(session_id))
    }

    /// Resolve a session's journal path, only through the index.
    ///
    /// An unknown session triggers one bounded refresh and a retry.
    pub(super) fn resolve_path(&self, session_id: Uuid) -> Result<PathBuf, DesktopError> {
        self.with_index(|state| {
            let lookup = |state: &IndexState| {
                state
                    .index
                    .journal_path(session_id)
                    .map_err(|error| DesktopError::from_index(&error))
            };
            if let Some(path) = lookup(state)? {
                return Ok(path);
            }
            state.refresh_bounded(true)?;
            lookup(state)?.ok_or_else(|| DesktopError::not_found("session"))
        })
    }

    /// Open the existing settings store, waiting briefly for another holder.
    ///
    /// `Ok(None)` means no store exists. No service lock is held while waiting.
    pub(super) fn open_control(&self) -> Result<Option<ControlStore>, DesktopError> {
        retry_busy(|| ControlStore::open_existing(&self.data_root))
    }

    /// Open the settings store, creating an empty one when none exists, and
    /// wait briefly for another holder (ADR-0060).
    ///
    /// Creation records only the store identity at revision zero: no root,
    /// no workspace, and no permission. It is the same store the trusted CLI's
    /// `workspace status` creates. No service lock is held while waiting.
    pub(super) fn open_or_create_control(&self) -> Result<ControlStore, DesktopError> {
        retry_busy(|| ControlStore::open(&self.data_root))
    }
}

/// Run `open` until it returns anything but `Busy`, or the retry deadline
/// passes.
fn retry_busy<T>(mut open: impl FnMut() -> Result<T, ControlError>) -> Result<T, DesktopError> {
    let deadline = Instant::now() + CONTROL_RETRY_DEADLINE;
    loop {
        match open() {
            Ok(value) => return Ok(value),
            Err(ControlError::Busy) if Instant::now() < deadline => {
                thread::sleep(CONTROL_RETRY_INTERVAL);
            }
            Err(error) => return Err(DesktopError::from_control(&error)),
        }
    }
}

/// The status of an indexed session. A turn running here wins over everything,
/// because on Windows the journal is locked while the turn runs.
pub(super) fn status_of(session: &IndexedSession, running: &HashSet<Uuid>) -> SessionStatus {
    if running.contains(&session.session_id) {
        return SessionStatus::Running;
    }
    match session.state {
        IndexedState::Invalid => SessionStatus::Invalid,
        IndexedState::Busy => SessionStatus::Busy,
        IndexedState::Ok if session.active_turn.is_some() => SessionStatus::TurnOpen,
        IndexedState::Ok => SessionStatus::Idle,
    }
}

/// Summarize an indexed session for the interface.
pub(super) fn summary_of(session: &IndexedSession, running: &HashSet<Uuid>) -> SessionSummary {
    let status = status_of(session, running);
    let placeholder = match status {
        SessionStatus::Invalid => TITLE_UNREADABLE,
        SessionStatus::Idle | SessionStatus::TurnOpen => TITLE_UNTITLED,
        SessionStatus::Running | SessionStatus::Busy => TITLE_IN_USE,
    };
    SessionSummary {
        session_id: session.session_id,
        title: session
            .title
            .as_deref()
            .map_or_else(|| placeholder.to_owned(), display_safe_line),
        created_at: session.created_at.and_then(format_time),
        updated_at: session.updated_at.and_then(format_time),
        status,
        archived: session.archived,
        event_count: session.event_count,
        invalid_code: if status == SessionStatus::Invalid {
            session.invalid_code.clone()
        } else {
            None
        },
    }
}

/// Rewrite a Windows verbatim path as the text people expect to read.
///
/// `\\?\UNC\server\share\x` becomes `\\server\share\x` and `\\?\C:\x` becomes
/// `C:\x`. Every other text is returned unchanged: other verbatim forms such as
/// `\\?\Volume{...}\`, `\\.\` device paths, a `\\?\` that is not at the start,
/// and every non-Windows path.
///
/// This is display text only. Never use the result to open, compare, group, or
/// authorize a path; those keep the recorded value. Escape the result with
/// [`display_safe_line`] afterwards.
pub(super) fn windows_display_path(text: &str) -> String {
    const VERBATIM: &str = r"\\?\";
    let Some(rest) = text.strip_prefix(VERBATIM) else {
        return text.to_owned();
    };
    // `get(..4)` is a character-boundary-checked slice, so no panic on text.
    if let Some(head) = rest.get(..4)
        && head.eq_ignore_ascii_case(r"UNC\")
        && let Some(share) = rest.get(4..)
        && !share.is_empty()
    {
        return format!(r"\\{share}");
    }
    let mut characters = rest.chars();
    if let (Some(letter), Some(':')) = (characters.next(), characters.next())
        && letter.is_ascii_alphabetic()
        && matches!(characters.next(), None | Some('\\'))
    {
        return rest.to_owned();
    }
    text.to_owned()
}

/// Escape folder text for display after the verbatim prefix is removed.
fn folder_text(raw: &str) -> String {
    display_safe_line(&windows_display_path(raw))
}

/// The display-safe folder text of an indexed session.
pub(super) fn folder_of(session: &IndexedSession) -> String {
    session
        .folder_display
        .as_deref()
        .filter(|text| !text.is_empty())
        .map_or_else(|| UNKNOWN_FOLDER.to_owned(), folder_text)
}

/// The display-safe folder text of a registered workspace directory, or the
/// not-representable text when this system cannot form the path.
pub(super) fn folder_of_directory(directory: Option<PathBuf>) -> String {
    directory.map_or_else(
        || FOLDER_NOT_REPRESENTABLE.to_owned(),
        |path| folder_text(&path.to_string_lossy()),
    )
}

/// Summarize a fresh inspection. No turn of this session runs here.
pub(super) fn summary_of_inspection(inspection: &SessionInspection) -> SessionSummary {
    SessionSummary {
        session_id: inspection.session_id,
        title: display_safe_line(&inspection.title),
        created_at: format_time(inspection.created_at),
        updated_at: format_time(inspection.updated_at),
        status: if inspection.active_turn.is_some() {
            SessionStatus::TurnOpen
        } else {
            SessionStatus::Idle
        },
        archived: inspection.archived,
        event_count: inspection.event_count,
        invalid_code: None,
    }
}

/// The display-safe folder text of a recorded native path.
pub(super) fn folder_of_path(path: &NativePath) -> String {
    folder_of_directory(path.to_path_buf().ok())
}

/// Format a time as RFC 3339, or `None` when it is outside the format's range.
pub(super) fn format_time(time: OffsetDateTime) -> Option<String> {
    time.format(&Rfc3339).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_drive_and_unc_paths_are_rewritten() {
        assert_eq!(
            windows_display_path(r"\\?\C:\Users\me\proj"),
            r"C:\Users\me\proj"
        );
        assert_eq!(windows_display_path(r"\\?\c:\x"), r"c:\x");
        assert_eq!(windows_display_path(r"\\?\Z:"), "Z:");
        assert_eq!(
            windows_display_path(r"\\?\UNC\server\share\x"),
            r"\\server\share\x"
        );
        assert_eq!(
            windows_display_path(r"\\?\unc\server\share"),
            r"\\server\share"
        );
    }

    #[test]
    fn other_text_is_returned_unchanged() {
        for text in [
            "",
            "/home/me/proj",
            r"C:\Users\me",
            r"\\server\share",
            r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\x",
            r"\\.\COM1",
            r"\\.\C:\x",
            r"\\?\UNC\",
            r"\\?\UNC",
            r"\\?\C",
            r"\\?\1:\x",
            r"\\?\C:x",
            r"D:\a\\?\C:\x",
            r"\\?\",
            "\\\\?\\\u{e9}\u{e9}\u{e9}",
        ] {
            assert_eq!(windows_display_path(text), text, "{text:?}");
        }
    }

    #[test]
    fn hostile_text_after_the_prefix_is_still_escaped() {
        assert_eq!(
            folder_text("\\\\?\\C:\\evil\u{202e}dir\nnext"),
            "C:\\evil\\u{202e}dir\\u{000a}next"
        );
        assert_eq!(
            folder_text("\\\\?\\UNC\\srv\u{200b}\\share"),
            "\\\\srv\\u{200b}\\share"
        );
        // A forged escape after the rewrite is still neutralised.
        assert_eq!(folder_text("\\\\?\\C:\\u{202e}"), "C:\\u{5c}u{202e}");
    }

    #[test]
    fn a_directory_that_cannot_be_formed_keeps_its_text() {
        assert_eq!(
            folder_of_directory(None),
            "(path not representable on this system)"
        );
        assert_eq!(
            folder_of_directory(Some(PathBuf::from("/work/a\u{202e}b"))),
            "/work/a\\u{202e}b"
        );
    }
}
