//! Bounded, rotating audit journal of every model-initiated workspace action.
//!
//! The journal records metadata only: which tool the model called, which
//! workspace-relative paths it named, how the call was authorized or refused,
//! and the outcome. It never stores file contents, edit text, command
//! arguments, or model free text. A record is evidence for an operator, never
//! authorization, and a missing record never grants or denies anything.
//!
//! # Storage
//!
//! The active file is `model-actions-v1.jsonl` in the caller's private data
//! directory. When the next record would exceed the byte or event bound, the
//! writer copies the active file to a durable archive named
//! `model-actions-v1.<first sequence, 20 digits>.jsonl`, then truncates the
//! active file, then drops the oldest archive beyond [`MAX_ARCHIVES`]. The
//! active content is never removed before the archive is synced and renamed
//! into place, and the oldest archive is removed only after that. A crash
//! between the archive rename and the truncation leaves a byte-identical
//! duplicate that the next writer completes; any other overlap is corruption.
//!
//! # Untrusted strings
//!
//! Targets that fail validation are never rejected or stored: the record holds
//! only their SHA-256 and length (`withheld`), so hostile attempts stay auditable.
//! The call identifier and codes originate from the model. They are
//! validated and **rejected** with [`ActionJournalError::InvalidValue`] when
//! they exceed their bounds or contain a control character or a Unicode
//! line-separator, bidirectional-control, or invisible-format character. They
//! are never stored raw and never silently altered. Paths must be relative and
//! must not contain a `..` component, a drive prefix, or a leading separator.
//! The same rules run again when a journal is read, so a hand-edited file
//! cannot smuggle such values into an operator's terminal.
//!
//! # Failure behaviour
//!
//! A write or sync failure poisons the writer. Reopening revalidates the whole
//! journal and fails closed with [`ActionJournalError::Corrupt`] on any
//! malformed, unknown-field, duplicate, or non-consecutive record. Nothing is
//! repaired, truncated, retried, or recovered automatically, and a start that
//! never received a finish is reported as unfinished rather than guessed at.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};
use uuid::{Uuid, Version};

use crate::policy::WorkspaceId;
use crate::storage::{self, StorageError};

const FILE_NAME: &str = "model-actions-v1.jsonl";
const ARCHIVE_PREFIX: &str = "model-actions-v1.";
const ARCHIVE_SUFFIX: &str = ".jsonl";
const TEMP_NAME: &str = "model-actions-v1.rotating.tmp";
const SCHEMA_VERSION: u32 = 1;
/// Maximum size of one journal file, active or archived.
const MAX_BYTES: usize = 4 * 1024 * 1024;
/// Maximum size of one encoded record, including its newline.
const MAX_EVENT_BYTES: usize = 32 * 1024;
/// Maximum number of records in one journal file.
const MAX_EVENTS: usize = 16 * 1024;
/// Number of rotated archives retained beside the active file.
pub const MAX_ARCHIVES: usize = 4;
/// Largest `limit` accepted by [`ActionJournal::recent`].
pub const MAX_RECENT_RECORDS: usize = 1000;
/// Bytes held back per unfinished action so its finish record fits.
const FINISH_RESERVE_BYTES: usize = 1024;
const MAX_CALL_ID_BYTES: usize = 128;
const MAX_TARGET_BYTES: usize = 4096;
const MAX_TARGETS: usize = 2;
const MAX_CODE_BYTES: usize = 64;

/// The model tool that attempted a workspace action.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionTool {
    /// Read one file.
    ReadFile,
    /// List one directory.
    ListDirectory,
    /// Replace one exact text occurrence in a file.
    EditFile,
    /// Replace several exact occurrences in one file.
    MultiEditFile,
    /// Create one new file.
    CreateFile,
    /// Create one new directory.
    CreateDirectory,
    /// Rename one regular file; names both source and destination.
    RenameFile,
    /// Delete one regular file after a checkpoint.
    DeleteFile,
    /// Run one command.
    RunCommand,
    /// A network action. Reserved: no model network tool exists yet.
    Network,
    /// The model named a tool this runtime does not have (ADR-0099). The
    /// name is hostile text and is not stored; the record holds no target.
    Unrecognized,
}

/// The observed result of an executed action.
///
/// `Uncertain` means a side effect may have happened. The journal never retries.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionOutcome {
    /// The action completed.
    Succeeded,
    /// The action failed with no ambiguous side effect.
    Failed,
    /// The action may have changed state.
    Uncertain,
}

/// How an executed action was authorized. Descriptive evidence only.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Authorization {
    /// The workspace preference was Allow; no per-call approval existed.
    StandingAllow {
        /// The settings revision that held the preference.
        settings_revision: u64,
    },
    /// A trusted single-use approval was consumed.
    Approval {
        /// The consumed approval record.
        approval_id: Uuid,
        /// The settings revision in force when it was consumed.
        settings_revision: u64,
    },
}

/// Why the model's call was refused before any action started.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum RefusalReason {
    /// The workspace preference is deny.
    AccessDenied,
    /// A pending approval was recorded and the call did not run.
    ApprovalRequired {
        /// The pending approval record.
        approval_id: Uuid,
    },
    /// The operator denied the approval.
    ApprovalDenied {
        /// The denied approval record.
        approval_id: Uuid,
    },
    /// The approval expired before use.
    ApprovalExpired {
        /// The expired approval record.
        approval_id: Uuid,
    },
    /// The request failed validation.
    InvalidRequest,
    /// The policy or settings could not be read.
    PolicyUnavailable,
    /// The turn was cancelled before the action started.
    Cancelled,
    /// The call named a tool this runtime does not have (ADR-0099). The
    /// tool is recorded as [`ActionTool::Unrecognized`].
    UnsupportedTool,
    /// The call's argument JSON exceeded its tool's limit (ADR-0099). The
    /// request digest is that of the received argument text.
    ArgumentsTooLarge,
    /// The call's arguments were not strict JSON of the tool's shape, or
    /// held a value out of range (ADR-0099). The request digest is that of
    /// the received argument text.
    InvalidArguments,
    /// The call named an absolute, empty, or parent-relative path
    /// (ADR-0099). The request digest is that of the received argument text.
    InvalidPath,
}

/// A target as stored: a validated path, or a digest of a value that failed
/// validation. The journal never stores hostile or oversize text.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum StoredTarget {
    /// A workspace-relative path that passed validation.
    Path(String),
    /// A value that failed validation, identified by digest and length.
    Withheld(WithheldTarget),
}

/// Identity of a withheld target without its text.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WithheldTarget {
    /// Lowercase SHA-256 of the raw UTF-8 bytes.
    pub sha256: String,
    /// Length of the raw value in bytes.
    pub bytes: u64,
}

/// Arguments for [`ActionJournal::record_started`].
#[derive(Clone, Copy, Debug)]
pub struct StartedAction<'a> {
    /// The session making the call.
    pub session_id: Uuid,
    /// The workspace the call targets.
    pub workspace_id: WorkspaceId,
    /// The model's call identifier. Validated, never stored raw.
    pub call_id: &'a str,
    /// The tool called.
    pub tool: ActionTool,
    /// Workspace-relative paths: exactly two for a rename, otherwise at most one.
    pub targets: &'a [&'a str],
    /// Lowercase SHA-256 of the canonical request, when the caller computed one.
    pub request_sha256: Option<&'a str>,
    /// How the call was authorized.
    pub authorization: Authorization,
}

/// Arguments for [`ActionJournal::record_refused`].
#[derive(Clone, Copy, Debug)]
pub struct RefusedAction<'a> {
    /// The session making the call.
    pub session_id: Uuid,
    /// The workspace the call targets.
    pub workspace_id: WorkspaceId,
    /// The model's call identifier. Validated, never stored raw.
    pub call_id: &'a str,
    /// The tool called.
    pub tool: ActionTool,
    /// Workspace-relative paths, bounded as for [`StartedAction`].
    pub targets: &'a [&'a str],
    /// Lowercase SHA-256 of the canonical request, when known.
    pub request_sha256: Option<&'a str>,
    /// Why the call was refused.
    pub reason: RefusalReason,
}

/// A failure while opening, reading, or appending to the action journal.
#[derive(Debug, Error)]
pub enum ActionJournalError {
    /// Another writer owns the journal, or a writer's lock blocked a read.
    #[error("model action journal is busy")]
    Busy,
    /// Existing journal content is malformed or inconsistent. It is not repaired.
    #[error("model action journal is corrupt: {0}")]
    Corrupt(&'static str),
    /// A record cannot fit even after rotation, or rotation itself failed.
    #[error("model action journal capacity reached")]
    LimitReached,
    /// A write or sync failed earlier; this handle cannot be used again.
    #[error("model action journal writer is poisoned")]
    Poisoned,
    /// An operating-system failure. The original cause is retained.
    #[error("model action journal I/O failed: {0}")]
    Io(#[from] io::Error),
    /// A caller-supplied value is out of bounds or unsafe to store.
    #[error("invalid model action value: {0}")]
    InvalidValue(&'static str),
}

impl From<StorageError> for ActionJournalError {
    /// Preserve I/O causes and lock conflicts across the storage boundary.
    fn from(value: StorageError) -> Self {
        match value {
            StorageError::Io(error) => Self::Io(error),
            StorageError::Invalid(reason) => Self::Corrupt(reason),
            StorageError::Busy => Self::Busy,
        }
    }
}

/// The finish of an executed action, joined from its `action_finished` record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionFinish {
    /// RFC 3339 UTC time of the finish record.
    pub time: String,
    /// The outcome.
    pub outcome: ActionOutcome,
    /// A short `[a-z0-9_]` code, when the caller supplied one.
    pub code: Option<String>,
    /// The broker, checkpoint, or run journal record this action produced.
    pub operation_id: Option<Uuid>,
}

/// What became of one model call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActionDisposition {
    /// The action started. `finish` is `None` when no finish record exists,
    /// for example after a crash; the outcome is then unknown, not failed.
    Executed {
        /// Identifier shared by the start and finish records.
        action_id: Uuid,
        /// How the call was authorized.
        authorization: Authorization,
        /// The finish, if one was recorded.
        finish: Option<ActionFinish>,
    },
    /// The call was refused and nothing started.
    Refused {
        /// Why.
        reason: RefusalReason,
    },
}

/// One model call as listed by [`ActionJournal::recent`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionRecord {
    /// Sequence number of the start or refusal record.
    pub sequence: u64,
    /// RFC 3339 UTC time of the start or refusal record.
    pub time: String,
    /// The session making the call.
    pub session_id: Uuid,
    /// The workspace the call targeted.
    pub workspace_id: WorkspaceId,
    /// The validated call identifier.
    pub call_id: String,
    /// The tool called.
    pub tool: ActionTool,
    /// Validated workspace-relative paths, or digests of withheld values.
    pub targets: Vec<StoredTarget>,
    /// Lowercase SHA-256 of the request, when recorded.
    pub request_sha256: Option<String>,
    /// Executed or refused.
    pub disposition: ActionDisposition,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JournalLine {
    schema_version: u32,
    sequence: u64,
    time: String,
    event: JournalEvent,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum JournalEvent {
    #[serde(rename = "action_started")]
    Started(StartedPayload),
    #[serde(rename = "action_finished")]
    Finished(FinishedPayload),
    #[serde(rename = "action_refused")]
    Refused(RefusedPayload),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StartedPayload {
    action_id: Uuid,
    session_id: Uuid,
    #[serde(deserialize_with = "deserialize_workspace_id")]
    workspace_id: WorkspaceId,
    call_id: String,
    tool: ActionTool,
    targets: Vec<StoredTarget>,
    request_sha256: Option<String>,
    authorization: Authorization,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FinishedPayload {
    action_id: Uuid,
    outcome: ActionOutcome,
    code: Option<String>,
    operation_id: Option<Uuid>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RefusedPayload {
    session_id: Uuid,
    #[serde(deserialize_with = "deserialize_workspace_id")]
    workspace_id: WorkspaceId,
    call_id: String,
    tool: ActionTool,
    targets: Vec<StoredTarget>,
    request_sha256: Option<String>,
    reason: RefusalReason,
}

/// Reject malformed persisted IDs before constructing typed workspace metadata.
fn deserialize_workspace_id<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<WorkspaceId, D::Error> {
    WorkspaceId::from_uuid(Uuid::deserialize(deserializer)?).map_err(serde::de::Error::custom)
}

/// Configurable bounds, so tests can force rotation with small files.
#[derive(Clone, Copy, Debug)]
struct Limits {
    max_bytes: usize,
    max_events: usize,
    max_archives: usize,
}

const DEFAULT_LIMITS: Limits = Limits {
    max_bytes: MAX_BYTES,
    max_events: MAX_EVENTS,
    max_archives: MAX_ARCHIVES,
};

/// A locked, append-only writer for the model action journal.
///
/// The exclusive lock on the active file lasts until this value is dropped.
/// Each record is written and synced before the call returns, so a start
/// record is durable before the caller performs the action. The caller must
/// treat a failed `record_started` as a refusal to act.
pub struct ActionJournal {
    directory: PathBuf,
    file: File,
    limits: Limits,
    next_sequence: u64,
    /// Records in the active file.
    events: usize,
    /// Bytes in the active file.
    bytes: usize,
    /// First sequence of each retained archive, oldest first.
    archives: Vec<u64>,
    /// Unfinished starts in the active file. They are finishable.
    active_open: HashSet<Uuid>,
    /// Unfinished starts in the newest archive. They stay finishable until the
    /// next rotation; after that they are unfinished forever and uncounted.
    archive_open: HashSet<Uuid>,
    poisoned: bool,
    #[cfg(test)]
    fail_next_write: bool,
    #[cfg(test)]
    fail_next_archive: bool,
}

impl ActionJournal {
    /// Open or create the journal under `data_directory` and take the writer lock.
    ///
    /// Missing directories are created through the shared private-directory
    /// helper, which must be given a trusted path. Every retained file is
    /// strictly validated. An unparseable or inconsistent journal fails with
    /// [`ActionJournalError::Corrupt`] and is left untouched. Another writer
    /// yields [`ActionJournalError::Busy`]. Actions left unfinished by an
    /// earlier process stay unfinished: this handle cannot finish them.
    pub fn open(data_directory: &Path) -> Result<Self, ActionJournalError> {
        Self::open_with(data_directory, DEFAULT_LIMITS)
    }

    fn open_with(data_directory: &Path, limits: Limits) -> Result<Self, ActionJournalError> {
        storage::create_private_directories(data_directory)?;
        let path = data_directory.join(FILE_NAME);
        let (mut file, created) = match storage::open_locked_file(&path, true) {
            Ok(file) => (file, true),
            Err(StorageError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists => {
                (storage::open_locked_file(&path, false)?, false)
            }
            Err(error) => return Err(error.into()),
        };
        if created {
            storage::sync_parent_directory(data_directory)?;
        }
        // A temporary left by a crash holds an incomplete copy of content that
        // is still in the active file, and only the lock holder may remove it.
        match fs::remove_file(data_directory.join(TEMP_NAME)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let archives = load_archives(data_directory, false)?;
        let mut active_bytes = read_bounded(&mut file)?;
        let mut active_lines = parse_lines(&active_bytes)?;
        if let (Some(newest), Some(first)) = (archives.last(), active_lines.first())
            && first.sequence <= newest.last_sequence
        {
            if active_bytes != newest.bytes {
                return Err(ActionJournalError::Corrupt(
                    "active file overlaps an archive but differs",
                ));
            }
            // The archive is durable and identical: finish the interrupted rotation.
            file.set_len(0)?;
            file.sync_all()?;
            active_bytes.clear();
            active_lines.clear();
        }
        let mut chain = Chain::default();
        for archive in &archives {
            chain.accept(&archive.lines)?;
        }
        chain.accept(&active_lines)?;
        // A start is finishable while it is in the active file or the newest
        // archive and no finish record exists in either.
        let mut archive_open = HashSet::new();
        if let Some(newest) = archives.last() {
            collect_open(&newest.lines, &mut archive_open, None);
        }
        let mut active_open = HashSet::new();
        collect_open(&active_lines, &mut active_open, Some(&mut archive_open));
        let mut journal = Self {
            directory: data_directory.to_path_buf(),
            file,
            limits,
            next_sequence: chain
                .last_sequence
                .map_or(Some(1), |last| last.checked_add(1))
                .ok_or(ActionJournalError::LimitReached)?,
            events: active_lines.len(),
            bytes: active_bytes.len(),
            archives: archives
                .iter()
                .map(|archive| archive.first_sequence)
                .collect(),
            active_open,
            archive_open,
            poisoned: false,
            #[cfg(test)]
            fail_next_write: false,
            #[cfg(test)]
            fail_next_archive: false,
        };
        journal.prune()?;
        Ok(journal)
    }

    /// Durably record that a model-initiated action is about to start.
    ///
    /// Returns the action identifier to pass to [`Self::record_finished`]. The
    /// record is synced before this returns. Capacity for a finish record is
    /// reserved, rotating the journal first when needed. Values are validated
    /// and rejected, never stored raw; see the module documentation. At most
    /// 256 actions may be unfinished on one writer.
    pub fn record_started(
        &mut self,
        action: &StartedAction<'_>,
    ) -> Result<Uuid, ActionJournalError> {
        self.ensure_healthy()?;
        validate_scope(action.session_id, &action.workspace_id)?;
        validate_call_id(action.call_id)?;
        let targets = classify_targets(action.tool, action.targets, true)?;
        validate_sha256(action.request_sha256)?;
        validate_authorization(&action.authorization)?;
        let action_id = Uuid::new_v4();
        let event = JournalEvent::Started(StartedPayload {
            action_id,
            session_id: action.session_id,
            workspace_id: action.workspace_id,
            call_id: action.call_id.to_owned(),
            tool: action.tool,
            targets,
            request_sha256: action.request_sha256.map(str::to_owned),
            authorization: action.authorization,
        });
        // Reserve room for every finishable start, capped at half a file so
        // crash leftovers cannot make a start unfittable. A finish that still
        // does not fit rotates into an empty file, where it always fits.
        let pending = self
            .active_open
            .len()
            .saturating_add(self.archive_open.len())
            .saturating_add(1);
        let reserved_events = pending.min(self.limits.max_events / 2);
        let reserved_bytes = pending
            .saturating_mul(FINISH_RESERVE_BYTES)
            .min(self.limits.max_bytes / 2);
        self.append(event, reserved_events, reserved_bytes)?;
        self.active_open.insert(action_id);
        Ok(action_id)
    }

    /// Durably record the outcome of an action this writer started.
    ///
    /// `code` must be 1 to 64 bytes of `[a-z0-9_]`. The start may come from
    /// any earlier writer, so a caller can drop the writer while the action
    /// runs. It must still be in the active file or the newest archive and
    /// unfinished there; an unknown, already finished, or rotated-out id is
    /// refused. The finish fits by reservation, and the journal rotates rather
    /// than refusing when it does not.
    pub fn record_finished(
        &mut self,
        action_id: Uuid,
        outcome: ActionOutcome,
        code: Option<&str>,
        operation_id: Option<Uuid>,
    ) -> Result<(), ActionJournalError> {
        self.ensure_healthy()?;
        if !self.active_open.contains(&action_id) && !self.archive_open.contains(&action_id) {
            return Err(ActionJournalError::InvalidValue(
                "action is unknown, already finished, or no longer finishable",
            ));
        }
        validate_code(code)?;
        validate_operation_id(operation_id)?;
        let event = JournalEvent::Finished(FinishedPayload {
            action_id,
            outcome,
            code: code.map(str::to_owned),
            operation_id,
        });
        self.append(event, 0, 0)?;
        self.active_open.remove(&action_id);
        self.archive_open.remove(&action_id);
        Ok(())
    }

    /// Durably record a refused call. One record; no start is written.
    ///
    /// Refusals are recorded even when the call never ran, so an operator can
    /// audit what the model attempted. Validation matches
    /// [`Self::record_started`].
    pub fn record_refused(&mut self, action: &RefusedAction<'_>) -> Result<(), ActionJournalError> {
        self.ensure_healthy()?;
        validate_scope(action.session_id, &action.workspace_id)?;
        validate_call_id(action.call_id)?;
        let targets = classify_targets(action.tool, action.targets, false)?;
        validate_sha256(action.request_sha256)?;
        validate_reason(&action.reason)?;
        let event = JournalEvent::Refused(RefusedPayload {
            session_id: action.session_id,
            workspace_id: action.workspace_id,
            call_id: action.call_id.to_owned(),
            tool: action.tool,
            targets,
            request_sha256: action.request_sha256.map(str::to_owned),
            reason: action.reason,
        });
        self.append(event, 0, 0)
    }

    /// List the newest `limit` model calls across the active file and archives.
    ///
    /// Read-only: it opens each file without a lock, never creates or changes
    /// anything, and returns an empty list when no journal exists. `limit`
    /// must be 1 to 1000. Records are joined by action identifier and listed
    /// newest first. An executed action with `finish: None` is unfinished. A
    /// finish whose start was rotated out is omitted. A concurrent writer is
    /// retried once; on Windows its byte-range lock can still yield
    /// [`ActionJournalError::Busy`]. Any invalid content is
    /// [`ActionJournalError::Corrupt`], including a torn last line.
    pub fn recent(
        data_directory: &Path,
        limit: usize,
    ) -> Result<Vec<ActionRecord>, ActionJournalError> {
        if !(1..=MAX_RECENT_RECORDS).contains(&limit) {
            return Err(ActionJournalError::InvalidValue(
                "limit must be between 1 and 1000",
            ));
        }
        // A rotation between listing the archives and reading the active file
        // can look like a gap. One retry sees the settled state.
        let lines = match read_all(data_directory) {
            Err(ActionJournalError::Corrupt(_)) => read_all(data_directory)?,
            other => other?,
        };
        let mut records: Vec<ActionRecord> = Vec::new();
        let mut index: HashMap<Uuid, usize> = HashMap::new();
        for line in lines {
            match line.event {
                JournalEvent::Started(start) => {
                    index.insert(start.action_id, records.len());
                    records.push(ActionRecord {
                        sequence: line.sequence,
                        time: line.time,
                        session_id: start.session_id,
                        workspace_id: start.workspace_id,
                        call_id: start.call_id,
                        tool: start.tool,
                        targets: start.targets,
                        request_sha256: start.request_sha256,
                        disposition: ActionDisposition::Executed {
                            action_id: start.action_id,
                            authorization: start.authorization,
                            finish: None,
                        },
                    });
                }
                JournalEvent::Finished(finish) => {
                    if let Some(position) = index.get(&finish.action_id).copied()
                        && let Some(record) = records.get_mut(position)
                        && let ActionDisposition::Executed { finish: slot, .. } =
                            &mut record.disposition
                    {
                        *slot = Some(ActionFinish {
                            time: line.time,
                            outcome: finish.outcome,
                            code: finish.code,
                            operation_id: finish.operation_id,
                        });
                    }
                }
                JournalEvent::Refused(refusal) => records.push(ActionRecord {
                    sequence: line.sequence,
                    time: line.time,
                    session_id: refusal.session_id,
                    workspace_id: refusal.workspace_id,
                    call_id: refusal.call_id,
                    tool: refusal.tool,
                    targets: refusal.targets,
                    request_sha256: refusal.request_sha256,
                    disposition: ActionDisposition::Refused {
                        reason: refusal.reason,
                    },
                }),
            }
        }
        records.reverse();
        records.truncate(limit);
        Ok(records)
    }

    fn ensure_healthy(&self) -> Result<(), ActionJournalError> {
        if self.poisoned {
            Err(ActionJournalError::Poisoned)
        } else {
            Ok(())
        }
    }

    /// Append one record, rotating first when it (plus the reserve) does not fit.
    fn append(
        &mut self,
        event: JournalEvent,
        reserved_events: usize,
        reserved_bytes: usize,
    ) -> Result<(), ActionJournalError> {
        self.ensure_healthy()?;
        let line = JournalLine {
            schema_version: SCHEMA_VERSION,
            sequence: self.next_sequence,
            time: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .map_err(|_| ActionJournalError::InvalidValue("clock cannot be encoded"))?,
            event,
        };
        let encoded = encode_line(&line)?;
        if encoded.len() > MAX_EVENT_BYTES {
            return Err(ActionJournalError::InvalidValue(
                "record exceeds size bound",
            ));
        }
        if !self.fits(encoded.len(), reserved_events, reserved_bytes) {
            if self.events == 0 {
                return Err(ActionJournalError::LimitReached);
            }
            self.rotate()?;
            if !self.fits(encoded.len(), reserved_events, reserved_bytes) {
                return Err(ActionJournalError::LimitReached);
            }
        }
        let next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(ActionJournalError::LimitReached)?;
        if let Err(error) = self.write_and_sync(&encoded) {
            self.poisoned = true;
            return Err(ActionJournalError::Io(error));
        }
        self.bytes += encoded.len();
        self.events += 1;
        self.next_sequence = next_sequence;
        Ok(())
    }

    fn fits(&self, encoded: usize, reserved_events: usize, reserved_bytes: usize) -> bool {
        let bytes = self
            .bytes
            .checked_add(encoded)
            .and_then(|total| total.checked_add(reserved_bytes));
        let events = self
            .events
            .checked_add(1)
            .and_then(|total| total.checked_add(reserved_events));
        matches!((bytes, events), (Some(bytes), Some(events))
            if bytes <= self.limits.max_bytes && events <= self.limits.max_events)
    }

    fn write_and_sync(&mut self, encoded: &[u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::End(0))?;
        #[cfg(test)]
        if self.fail_next_write {
            self.fail_next_write = false;
            return Err(io::Error::other("injected action journal write failure"));
        }
        self.file.write_all(encoded)?;
        self.file.sync_all()
    }

    /// Move the active content to a durable archive, then empty the active file.
    ///
    /// A failure before the archive is renamed into place leaves the active
    /// file untouched and the writer usable. A failure afterwards poisons the
    /// writer; reopening completes or rejects the rotation. Pruning the oldest
    /// archive happens last and its failure is not fatal.
    fn rotate(&mut self) -> Result<(), ActionJournalError> {
        let first_sequence = self
            .next_sequence
            .checked_sub(u64::try_from(self.events).map_err(|_| ActionJournalError::LimitReached)?)
            .ok_or(ActionJournalError::Corrupt("event count exceeds sequence"))?;
        self.file.seek(SeekFrom::Start(0))?;
        let bytes = read_bounded(&mut self.file)?;
        let temp = self.directory.join(TEMP_NAME);
        let final_path = archive_path(&self.directory, first_sequence);
        if let Err(error) = self.stage_archive(&temp, &final_path, &bytes) {
            // Best effort: the staged temporary is incomplete or redundant.
            if fs::remove_file(&temp).is_err() {
                // The next open removes a stale temporary under the lock.
            }
            return Err(error);
        }
        // The archive is durable from here on.
        self.archives.push(first_sequence);
        if let Err(error) = self.empty_active() {
            self.poisoned = true;
            return Err(error);
        }
        // The rotated file is now the newest archive. Starts in the previous
        // newest archive stop being finishable and stop counting.
        self.archive_open = std::mem::take(&mut self.active_open);
        self.events = 0;
        self.bytes = 0;
        if self.prune().is_err() {
            // Retention only. The journal is consistent; the next rotation or
            // open retries, and until then one extra archive may remain.
        }
        Ok(())
    }

    fn stage_archive(
        &mut self,
        temp: &Path,
        final_path: &Path,
        bytes: &[u8],
    ) -> Result<(), ActionJournalError> {
        #[cfg(test)]
        if self.fail_next_archive {
            self.fail_next_archive = false;
            return Err(ActionJournalError::Io(io::Error::other(
                "injected archive failure",
            )));
        }
        let mut staged = storage::open_locked_file(temp, true)?;
        staged.write_all(bytes)?;
        staged.sync_all()?;
        // Windows denies renaming a file with an open handle.
        drop(staged);
        omgcode_platform::journal::rename_noreplace(temp, final_path)?;
        storage::sync_parent_directory(&self.directory)?;
        Ok(())
    }

    fn empty_active(&mut self) -> Result<(), ActionJournalError> {
        self.file.set_len(0)?;
        self.file.sync_all()?;
        Ok(())
    }

    /// Remove archives beyond the retention bound, oldest first.
    fn prune(&mut self) -> Result<(), ActionJournalError> {
        while self.archives.len() > self.limits.max_archives {
            let Some(oldest) = self.archives.first().copied() else {
                break;
            };
            match fs::remove_file(archive_path(&self.directory, oldest)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            self.archives.remove(0);
            storage::sync_parent_directory(&self.directory)?;
        }
        Ok(())
    }

    /// Inject one write failure to exercise poisoning.
    #[cfg(test)]
    fn fail_next_write_for_test(&mut self) {
        self.fail_next_write = true;
    }

    /// Inject one archive staging failure to exercise rotation safety.
    #[cfg(test)]
    fn fail_next_archive_for_test(&mut self) {
        self.fail_next_archive = true;
    }
}

struct ArchiveFile {
    first_sequence: u64,
    last_sequence: u64,
    bytes: Vec<u8>,
    lines: Vec<JournalLine>,
}

fn archive_path(directory: &Path, first_sequence: u64) -> PathBuf {
    directory.join(format!(
        "{ARCHIVE_PREFIX}{first_sequence:020}{ARCHIVE_SUFFIX}"
    ))
}

/// Parse `model-actions-v1.<20 digits>.jsonl`. Other names are not archives.
fn parse_archive_name(name: &str) -> Option<u64> {
    let digits = name
        .strip_prefix(ARCHIVE_PREFIX)?
        .strip_suffix(ARCHIVE_SUFFIX)?;
    if digits.len() != 20 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Read and strictly validate every archive, oldest first.
///
/// `tolerate_vanished` skips an archive pruned during a concurrent read.
fn load_archives(
    directory: &Path,
    tolerate_vanished: bool,
) -> Result<Vec<ArchiveFile>, ActionJournalError> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound && tolerate_vanished => {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error.into()),
    };
    let mut names: Vec<u64> = Vec::new();
    for entry in entries {
        let entry = entry?;
        if let Some(first) = entry.file_name().to_str().and_then(parse_archive_name) {
            names.push(first);
        }
    }
    names.sort_unstable();
    let mut archives = Vec::new();
    for first_sequence in names {
        let mut file = match storage::open_read_only_file(&archive_path(directory, first_sequence))
        {
            Ok(file) => file,
            Err(StorageError::Io(error))
                if error.kind() == io::ErrorKind::NotFound && tolerate_vanished =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let bytes = read_bounded(&mut file).map_err(classify_read_error)?;
        let lines = parse_lines(&bytes)?;
        let (Some(first), Some(last)) = (lines.first(), lines.last()) else {
            return Err(ActionJournalError::Corrupt("archive is empty"));
        };
        if first.sequence != first_sequence {
            return Err(ActionJournalError::Corrupt(
                "archive name does not match its first sequence",
            ));
        }
        archives.push(ArchiveFile {
            first_sequence,
            last_sequence: last.sequence,
            bytes,
            lines,
        });
    }
    Ok(archives)
}

/// Read every retained record, oldest first, without locking or writing.
fn read_all(directory: &Path) -> Result<Vec<JournalLine>, ActionJournalError> {
    let archives = load_archives(directory, true)?;
    let mut active_lines = match storage::open_read_only_file(&directory.join(FILE_NAME)) {
        Ok(mut file) => parse_lines(&read_bounded(&mut file).map_err(classify_read_error)?)?,
        Err(StorageError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if let Some(newest) = archives.last() {
        // An interrupted rotation leaves the active file as a copy of the newest
        // archive. Its records are already counted there.
        active_lines.retain(|line| line.sequence > newest.last_sequence);
    }
    let mut chain = Chain::default();
    let mut lines = Vec::new();
    for archive in archives {
        chain.accept(&archive.lines)?;
        lines.extend(archive.lines);
    }
    chain.accept(&active_lines)?;
    lines.extend(active_lines);
    Ok(lines)
}

/// Add unfinished starts from `lines` to `open`. A finish also clears `older`,
/// the set for the preceding file.
fn collect_open(
    lines: &[JournalLine],
    open: &mut HashSet<Uuid>,
    mut older: Option<&mut HashSet<Uuid>>,
) {
    for line in lines {
        match &line.event {
            JournalEvent::Started(start) => {
                open.insert(start.action_id);
            }
            JournalEvent::Finished(finish) => {
                open.remove(&finish.action_id);
                if let Some(older) = older.as_deref_mut() {
                    older.remove(&finish.action_id);
                }
            }
            JournalEvent::Refused(_) => {}
        }
    }
}

/// A Windows writer's byte-range lock blocks other handles from reading.
fn classify_read_error(error: ActionJournalError) -> ActionJournalError {
    #[cfg(windows)]
    if let ActionJournalError::Io(io_error) = &error
        && io_error.raw_os_error() == Some(33)
    {
        return ActionJournalError::Busy;
    }
    error
}

/// Read at most one file's bound plus one byte, so an oversize file is detected.
fn read_bounded(file: &mut File) -> Result<Vec<u8>, ActionJournalError> {
    let mut bytes = Vec::new();
    let bound = u64::try_from(MAX_BYTES + 1).map_err(|_| ActionJournalError::LimitReached)?;
    Read::by_ref(file).take(bound).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BYTES {
        return Err(ActionJournalError::Corrupt(
            "journal file exceeds size bound",
        ));
    }
    Ok(bytes)
}

/// Strictly parse one file. Empty input is an empty file, never an error.
fn parse_lines(bytes: &[u8]) -> Result<Vec<JournalLine>, ActionJournalError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let complete = bytes
        .strip_suffix(b"\n")
        .ok_or(ActionJournalError::Corrupt("incomplete final line"))?;
    let mut lines: Vec<JournalLine> = Vec::new();
    for raw in complete.split(|byte| *byte == b'\n') {
        if raw.is_empty() || raw.len() >= MAX_EVENT_BYTES || lines.len() >= MAX_EVENTS {
            return Err(ActionJournalError::Corrupt("invalid record size or count"));
        }
        let value = storage::parse_strict_json(raw)
            .map_err(|_| ActionJournalError::Corrupt("invalid or duplicate JSON keys"))?;
        let line: JournalLine = serde_json::from_value(value)
            .map_err(|_| ActionJournalError::Corrupt("unsupported record shape"))?;
        validate_line(&line)?;
        lines.push(line);
    }
    Ok(lines)
}

/// Cross-file consistency: consecutive sequences and one start/finish per action.
#[derive(Default)]
struct Chain {
    last_sequence: Option<u64>,
    starts: HashSet<Uuid>,
    finishes: HashSet<Uuid>,
}

impl Chain {
    fn accept(&mut self, lines: &[JournalLine]) -> Result<(), ActionJournalError> {
        for line in lines {
            if let Some(last) = self.last_sequence
                && last.checked_add(1) != Some(line.sequence)
            {
                return Err(ActionJournalError::Corrupt("sequence is not consecutive"));
            }
            self.last_sequence = Some(line.sequence);
            match &line.event {
                JournalEvent::Started(start) => {
                    if self.finishes.contains(&start.action_id)
                        || !self.starts.insert(start.action_id)
                    {
                        return Err(ActionJournalError::Corrupt("duplicate action start"));
                    }
                }
                JournalEvent::Finished(finish) => {
                    // A start rotated out of retention may legitimately be absent.
                    if !self.finishes.insert(finish.action_id) {
                        return Err(ActionJournalError::Corrupt("duplicate action finish"));
                    }
                }
                JournalEvent::Refused(_) => {}
            }
        }
        Ok(())
    }
}

fn encode_line(line: &JournalLine) -> Result<Vec<u8>, ActionJournalError> {
    let mut encoded = serde_json::to_vec(line)
        .map_err(|_| ActionJournalError::InvalidValue("record cannot be encoded"))?;
    encoded.push(b'\n');
    Ok(encoded)
}

fn valid_uuid(value: Uuid) -> bool {
    !value.is_nil() && value.get_version() == Some(Version::Random)
}

fn corrupt<T>(reason: &'static str) -> Result<T, ActionJournalError> {
    Err(ActionJournalError::Corrupt(reason))
}

/// Validate a record read from disk with the same rules used for writing.
fn validate_line(line: &JournalLine) -> Result<(), ActionJournalError> {
    let time = OffsetDateTime::parse(&line.time, &Rfc3339)
        .map_err(|_| ActionJournalError::Corrupt("invalid record time"))?;
    if line.schema_version != SCHEMA_VERSION || time.offset() != UtcOffset::UTC {
        return corrupt("unsupported schema version or non-UTC time");
    }
    if line.sequence == 0 {
        return corrupt("sequence starts at one");
    }
    let result = match &line.event {
        JournalEvent::Started(start) => {
            if !valid_uuid(start.action_id) {
                return corrupt("invalid action identifier");
            }
            validate_scope(start.session_id, &start.workspace_id)
                .and_then(|()| validate_call_id(&start.call_id))
                .and_then(|()| validate_stored_targets(start.tool, &start.targets, true))
                .and_then(|()| validate_sha256(start.request_sha256.as_deref()))
                .and_then(|()| validate_authorization(&start.authorization))
        }
        JournalEvent::Finished(finish) => {
            if !valid_uuid(finish.action_id) {
                return corrupt("invalid action identifier");
            }
            validate_code(finish.code.as_deref())
                .and_then(|()| validate_operation_id(finish.operation_id))
        }
        JournalEvent::Refused(refusal) => validate_scope(refusal.session_id, &refusal.workspace_id)
            .and_then(|()| validate_call_id(&refusal.call_id))
            .and_then(|()| validate_stored_targets(refusal.tool, &refusal.targets, false))
            .and_then(|()| validate_sha256(refusal.request_sha256.as_deref()))
            .and_then(|()| validate_reason(&refusal.reason)),
    };
    // A stored value that fails validation means the file was not written by
    // this module, which is corruption, not a caller mistake.
    result.or(corrupt("stored value fails validation"))
}

fn invalid<T>(reason: &'static str) -> Result<T, ActionJournalError> {
    Err(ActionJournalError::InvalidValue(reason))
}

fn validate_scope(session_id: Uuid, workspace_id: &WorkspaceId) -> Result<(), ActionJournalError> {
    if valid_uuid(session_id) && valid_uuid(*workspace_id.as_uuid()) {
        Ok(())
    } else {
        invalid("session and workspace identifiers must be non-nil version 4 UUIDs")
    }
}

fn validate_operation_id(operation_id: Option<Uuid>) -> Result<(), ActionJournalError> {
    match operation_id {
        Some(id) if id.is_nil() => invalid("operation identifier must not be nil"),
        _ => Ok(()),
    }
}

fn validate_authorization(authorization: &Authorization) -> Result<(), ActionJournalError> {
    match authorization {
        Authorization::Approval { approval_id, .. } if !valid_uuid(*approval_id) => {
            invalid("approval identifier must be a non-nil version 4 UUID")
        }
        _ => Ok(()),
    }
}

fn validate_reason(reason: &RefusalReason) -> Result<(), ActionJournalError> {
    match reason {
        RefusalReason::ApprovalRequired { approval_id }
        | RefusalReason::ApprovalDenied { approval_id }
        | RefusalReason::ApprovalExpired { approval_id }
            if !valid_uuid(*approval_id) =>
        {
            invalid("approval identifier must be a non-nil version 4 UUID")
        }
        _ => Ok(()),
    }
}

/// True for characters that can hide or reorder text in a terminal.
fn is_display_unsafe(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
        )
}

fn validate_text(value: &str, max_bytes: usize) -> Result<(), ActionJournalError> {
    if value.len() > max_bytes {
        return invalid("value exceeds its size bound");
    }
    if value.chars().any(is_display_unsafe) {
        return invalid("value contains a control or invisible formatting character");
    }
    Ok(())
}

fn validate_call_id(call_id: &str) -> Result<(), ActionJournalError> {
    if call_id.is_empty() {
        return invalid("call identifier must not be empty");
    }
    validate_text(call_id, MAX_CALL_ID_BYTES)
}

/// Check the number of targets. A started action follows the tool's arity; a
/// refusal may carry 0 to 2 so a malformed call can still be refused on record.
fn validate_target_count(
    tool: ActionTool,
    count: usize,
    strict: bool,
) -> Result<(), ActionJournalError> {
    if count > MAX_TARGETS {
        return invalid("too many targets");
    }
    if strict {
        let exact = matches!(tool, ActionTool::RenameFile);
        if exact && count != MAX_TARGETS {
            return invalid("a rename needs a source and a destination");
        }
        if !exact && count > 1 {
            return invalid("only a rename may name two targets");
        }
    }
    Ok(())
}

/// Classify raw model-supplied targets. A value that fails path validation is
/// stored only as a digest and length, so hostile text never reaches the file.
fn classify_targets(
    tool: ActionTool,
    targets: &[&str],
    strict: bool,
) -> Result<Vec<StoredTarget>, ActionJournalError> {
    validate_target_count(tool, targets.len(), strict)?;
    Ok(targets
        .iter()
        .map(|target| {
            if validate_target(tool, target).is_ok() {
                StoredTarget::Path((*target).to_owned())
            } else {
                StoredTarget::Withheld(WithheldTarget {
                    sha256: Sha256::digest(target.as_bytes())
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect(),
                    bytes: u64::try_from(target.len()).unwrap_or(u64::MAX),
                })
            }
        })
        .collect())
}

/// Strictly validate stored targets read from disk.
fn validate_stored_targets(
    tool: ActionTool,
    targets: &[StoredTarget],
    strict: bool,
) -> Result<(), ActionJournalError> {
    validate_target_count(tool, targets.len(), strict)?;
    for target in targets {
        match target {
            StoredTarget::Path(path) => validate_target(tool, path)?,
            StoredTarget::Withheld(withheld) => {
                if withheld.sha256.len() != 64
                    || !withheld
                        .sha256
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return invalid("withheld digest must be 64 lowercase hexadecimal characters");
                }
            }
        }
    }
    Ok(())
}

/// A target is a relative path as the model wrote it, not a resolved path.
/// Only listing may name the workspace root with an empty string.
fn validate_target(tool: ActionTool, target: &str) -> Result<(), ActionJournalError> {
    validate_text(target, MAX_TARGET_BYTES)?;
    if target.is_empty() {
        return if tool == ActionTool::ListDirectory {
            Ok(())
        } else {
            invalid("an empty target is valid only for listing")
        };
    }
    let bytes = target.as_bytes();
    let drive_prefix = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if target.starts_with(['/', '\\']) || drive_prefix {
        return invalid("target must be workspace-relative");
    }
    if target.split(['/', '\\']).any(|part| part == "..") {
        return invalid("target must not contain a parent component");
    }
    Ok(())
}

fn validate_sha256(digest: Option<&str>) -> Result<(), ActionJournalError> {
    match digest {
        Some(value)
            if value.len() != 64
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) =>
        {
            invalid("request digest must be 64 lowercase hexadecimal characters")
        }
        _ => Ok(()),
    }
}

fn validate_code(code: Option<&str>) -> Result<(), ActionJournalError> {
    match code {
        Some(value)
            if value.is_empty()
                || value.len() > MAX_CODE_BYTES
                || !value.bytes().all(|byte| {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                }) =>
        {
            invalid("code must be 1 to 64 bytes of lowercase letters, digits, or underscores")
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests;
