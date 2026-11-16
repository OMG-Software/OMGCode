//! Read-only inspection of one session journal without the writer lock.
//!
//! An inspector never calls `try_lock`, `lock`, or `lock_shared`, so scanning
//! cannot make a writer in another process report `Busy`. It validates the
//! journal with the same replay rules as `SessionJournal::open`, never
//! modifies the file, and never executes or resumes recorded work. Everything
//! it returns is untrusted historical data.

use std::fs;
use std::path::Path;

use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    CompactionRecord, MessageRole, MessageSource, NativePath, Replayed, SessionError, ToolOutcome,
    filename, map_storage_error, read_bounded, replay,
};
use crate::storage;

/// A validated, read-only view of one journal.
///
/// Fields mirror the header facts of `SessionSnapshot` and add event times and
/// the ordered transcript. Nothing here grants authority or starts work.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SessionInspection {
    /// The UUID shared by all events in this journal.
    pub session_id: Uuid,
    /// Workspace association recorded at creation. Descriptive only.
    pub workspace_id: Uuid,
    /// Canonical workspace path recorded at creation. Descriptive only.
    pub workspace: NativePath,
    /// Session title recorded at creation.
    pub title: String,
    /// Whether the session is archived after the last complete event.
    pub archived: bool,
    /// The turn left active by the last complete event, if any. Inspecting
    /// never resumes it, and a live writer may already have finished it.
    pub active_turn: Option<Uuid>,
    /// The journal schema version that every event shares.
    pub schema_version: u32,
    /// Number of complete events, including `session.started`.
    pub event_count: u64,
    /// Timestamp of the `session.started` event.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Timestamp of the last complete event.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// True when bytes followed the last LF. A writer may be mid-append, so the
    /// partial record was ignored. The file was not changed.
    pub incomplete_tail: bool,
    /// Every event after `session.started`, in journal order.
    pub entries: Vec<TranscriptEntry>,
}

/// One validated event after `session.started`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TranscriptEntry {
    /// Zero-based journal sequence number. Sequence, not time, orders events.
    pub seq: u64,
    /// The recorded UTC event time.
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    /// The event type and its recorded data.
    #[serde(flatten)]
    pub kind: TranscriptEntryKind,
}

/// The recorded data of one event. All text is untrusted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TranscriptEntryKind {
    /// A user, assistant, or tool message.
    Message {
        /// The recorded display role.
        role: MessageRole,
        /// The recorded origin label.
        source: MessageSource,
        /// The message text, at most 16 KiB.
        text: String,
        /// Tool call identifier. Present only on tool messages.
        #[serde(skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
    },
    /// A turn began.
    TurnStarted {
        /// The turn identifier.
        request_id: Uuid,
    },
    /// A turn completed.
    TurnCompleted {
        /// The turn identifier.
        request_id: Uuid,
    },
    /// A turn failed. Replay requires a code on every failed turn.
    TurnFailed {
        /// The turn identifier.
        request_id: Uuid,
        /// The bounded, non-secret failure code.
        code: String,
    },
    /// The model asked for a tool call. Schema version 2 and later.
    ToolRequested {
        /// The turn identifier.
        request_id: Uuid,
        /// Provider call identifier, unique within its turn.
        call_id: String,
        /// Requested tool name.
        tool: String,
        /// Exact strict-JSON arguments string, at most 4 KiB.
        arguments: String,
    },
    /// Core recorded the outcome of a tool call. Schema version 2 and later.
    ToolCompleted {
        /// The turn identifier.
        request_id: Uuid,
        /// Provider call identifier.
        call_id: String,
        /// The core-observed outcome.
        outcome: ToolOutcome,
        /// Stable error code. Present only when the call failed or was denied.
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        /// Size in bytes of the rendered output.
        output_bytes: u64,
        /// Lowercase SHA-256 of the rendered output.
        output_sha256: String,
    },
    /// The request manifest, reduced to its small descriptive fields. The base
    /// prompt and custom instructions are deliberately not included.
    RequestManifest {
        /// The turn identifier.
        request_id: Uuid,
        /// The model name used for the request.
        model: String,
        /// The version label of the compiled base prompt.
        prompt_version: String,
        /// Tool names advertised to the model. Empty before schema version 2.
        tools: Vec<String>,
    },
    /// A compaction summary replaced earlier messages. Schema version 3 only.
    ContextCompacted {
        /// The recorded compaction. Its summary is untrusted model output.
        record: CompactionRecord,
    },
    /// The session was archived.
    SessionArchived,
    /// The session was restored from the archive.
    SessionRestored,
}

impl SessionInspection {
    /// Inspect the journal at `path` without taking the writer lock.
    ///
    /// The file is opened read-only without following links, checked with the
    /// same platform handle validation as the writer, and read within the
    /// journal size bound. Bytes after the last LF are ignored and reported
    /// through `incomplete_tail`, because a writer may be mid-append. A journal
    /// with no complete record is invalid. The file is never changed.
    ///
    /// # Errors
    ///
    /// Returns `SessionError::Busy` when Windows reports that a writer's
    /// byte-range lock or a sharing rule blocks the read. The call does not
    /// retry. Returns `SessionError::InvalidJournal` or
    /// `SessionError::LimitReached` for a journal that fails validation, and
    /// `SessionError::Io` for any other operating-system failure.
    pub fn inspect(path: &Path) -> Result<Self, SessionError> {
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(SessionError::InvalidJournal("filename is not valid UTF-8"))?;
        let filename_id = filename::lenient_session_id(filename)?;
        let metadata = fs::symlink_metadata(path).map_err(classify_lock_conflict)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SessionError::InvalidJournal(
                "journal is not a regular file",
            ));
        }
        let mut file = storage::open_read_only_file(path)
            .map_err(|error| classify_lock_conflict(map_storage_error(error)))?;
        let contents = read_bounded(&mut file).map_err(classify_lock_conflict)?;
        let complete_len = contents
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .ok_or(SessionError::InvalidJournal(
                "journal has no complete record",
            ))?;
        let complete = contents
            .get(..complete_len)
            .ok_or(SessionError::InvalidJournal(
                "journal has no complete record",
            ))?;
        let mut entries = Vec::new();
        let Replayed {
            snapshot,
            created_at,
            updated_at,
            ..
        } = replay(complete, filename_id, Some(&mut entries))?;
        Ok(Self {
            session_id: snapshot.session_id,
            workspace_id: snapshot.workspace_id,
            workspace: snapshot.workspace,
            title: snapshot.title,
            archived: snapshot.archived,
            active_turn: snapshot.active_turn,
            schema_version: snapshot.schema_version,
            event_count: snapshot.event_count,
            created_at,
            updated_at,
            incomplete_tail: complete_len != contents.len(),
            entries,
        })
    }
}

/// Turn a Windows lock or sharing violation into the distinct `Busy` outcome.
///
/// Raw error 33 is `ERROR_LOCK_VIOLATION` and 32 is `ERROR_SHARING_VIOLATION`.
/// Other platforms and error kinds pass through unchanged.
fn classify_lock_conflict(error: impl Into<SessionError>) -> SessionError {
    let error = error.into();
    #[cfg(windows)]
    if let SessionError::Io(io_error) = &error
        && is_windows_lock_conflict(io_error)
    {
        return SessionError::Busy;
    }
    error
}

#[cfg(windows)]
fn is_windows_lock_conflict(error: &std::io::Error) -> bool {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    matches!(
        error.raw_os_error(),
        Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
    )
}
