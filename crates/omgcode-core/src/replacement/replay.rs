//! Strict bounded decoding and historical state transitions; no filesystem I/O.

use std::collections::{HashMap, HashSet};

use omgcode_platform::broker::RelativePath;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};
use uuid::{Uuid, Variant, Version};

use super::{
    CheckpointRetention, InitializationPhase, InitializationSnapshot, RecordedArtifact,
    RecordedCompaction, RecordedFileIdentity, RecordedReplacement, ReplacementError,
    ReplacementKind, ReplacementOutcome, ReplacementProgress, ReplacementScope,
    ReplacementSnapshot,
};
use crate::policy::WorkspaceId;
use crate::session::NativePath;
use crate::storage::parse_strict_json;

/// Whole-ledger byte bound. Raised from 8 MiB in 0.35.0 (ADR-0053).
pub(super) const MAX_LEDGER_BYTES: usize = 64 * 1024 * 1024;
pub(super) const MAX_RECORD_BYTES: usize = 64 * 1024;
/// Whole-ledger record bound. Raised from 4,096 in 0.35.0 (ADR-0053).
pub(super) const MAX_LEDGER_RECORDS: usize = 65_536;
pub(super) const MAX_OPERATIONS: usize = 64;
const MAX_PAYLOAD_BYTES: u64 = 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
/// SHA-256 of the empty byte string: the proposed artifact of every removal.
pub(super) const EMPTY_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Envelope<A> {
    pub(super) schema_version: u32,
    pub(super) control_store_id: Uuid,
    pub(super) replacement_store_id: Uuid,
    pub(super) event_id: Uuid,
    pub(super) sequence: u64,
    pub(super) timestamp: String,
    pub(super) action: A,
}

#[derive(Deserialize, Serialize)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum InitializationAction {
    Initializing,
    Ready,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArtifactNames {
    pub(super) original: String,
    pub(super) proposed: String,
    pub(super) manifest: String,
    pub(super) stage: String,
}

#[derive(Deserialize, Serialize)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum LedgerAction {
    Created,
    ReplacementStarted {
        operation_id: Uuid,
        /// Absent for a replacement, so earlier records stay byte-identical.
        /// Older readers refuse a record that carries it.
        #[serde(default, skip_serializing_if = "ReplacementKind::is_replace")]
        kind: ReplacementKind,
        session_id: Uuid,
        workspace_id: Uuid,
        revision: u64,
        target: NativePath,
        expected: RecordedArtifact,
        proposed: RecordedArtifact,
        artifacts: ArtifactNames,
    },
    CheckpointPrepared {
        operation_id: Uuid,
        manifest: RecordedArtifact,
    },
    StagePrepared {
        operation_id: Uuid,
        identity: RecordedFileIdentity,
        content: RecordedArtifact,
    },
    CommitIntended {
        operation_id: Uuid,
    },
    ReplacementFinished {
        operation_id: Uuid,
        outcome: ReplacementOutcome,
        code: Option<String>,
    },
    ReplacementAcknowledged {
        operation_id: Uuid,
    },
    /// Durable intent to remove one resolved entry. Older readers refuse it.
    CheckpointDeletionStarted {
        operation_id: Uuid,
    },
    /// The entry and its files were removed and the parent was flushed.
    CheckpointDeleted {
        operation_id: Uuid,
    },
    /// The ledger was rewritten in place. Only the second record may be one.
    /// Older readers refuse it.
    Compacted {
        previous: RecordedArtifact,
        previous_records: u64,
        revision_floor: Option<u64>,
    },
    /// One retained, resolved operation's whole recorded state, carried by a
    /// compaction. It follows the compaction record or another carried record.
    CheckpointCarried {
        operation_id: Uuid,
        /// Absent for a replacement, like the started record.
        #[serde(default, skip_serializing_if = "ReplacementKind::is_replace")]
        kind: ReplacementKind,
        session_id: Uuid,
        workspace_id: Uuid,
        revision: u64,
        target: NativePath,
        expected: RecordedArtifact,
        proposed: RecordedArtifact,
        manifest: Option<RecordedArtifact>,
        stage_identity: Option<RecordedFileIdentity>,
        progress: ReplacementProgress,
        outcome: Option<ReplacementOutcome>,
        code: Option<String>,
        acknowledged: bool,
    },
}

/// The recorded fields that start an operation, whether new or carried.
struct Started {
    operation_id: Uuid,
    kind: ReplacementKind,
    session_id: Uuid,
    workspace_id: Uuid,
    revision: u64,
    target: NativePath,
    expected: RecordedArtifact,
    proposed: RecordedArtifact,
}

/// Check framing and all byte/count bounds before parsing or allocating JSON.
fn records(
    bytes: &[u8],
    max_bytes: usize,
    max_records: usize,
    max_record_bytes: usize,
) -> Result<impl Iterator<Item = &[u8]>, ReplacementError> {
    if bytes.len() > max_bytes {
        return Err(ReplacementError::Limit("total bytes"));
    }
    if bytes.is_empty() || !bytes.ends_with(b"\n") {
        return Err(ReplacementError::Invalid("empty or incomplete JSONL"));
    }
    for (index, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        if index >= max_records {
            return Err(ReplacementError::Limit("record count"));
        }
        if line.len() > max_record_bytes {
            return Err(ReplacementError::Limit("record bytes"));
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            return Err(ReplacementError::Invalid("blank JSONL record"));
        }
    }
    Ok(bytes.split_inclusive(|byte| *byte == b'\n'))
}

fn decode<T: DeserializeOwned>(line: &[u8]) -> Result<T, ReplacementError> {
    let value = parse_strict_json(line).map_err(ReplacementError::Json)?;
    serde_json::from_value(value).map_err(ReplacementError::Json)
}

pub(super) fn validate_uuid(value: Uuid) -> Result<(), ReplacementError> {
    if value.is_nil()
        || value.get_version() != Some(Version::Random)
        || value.get_variant() != Variant::RFC4122
    {
        return Err(ReplacementError::Invalid(
            "ID is not an RFC 4122 version-4 UUID",
        ));
    }
    Ok(())
}

fn validate_envelope<A>(
    entry: &Envelope<A>,
    index: usize,
    scope: ReplacementScope,
) -> Result<(), ReplacementError> {
    validate_uuid(entry.event_id)?;
    if entry.schema_version != 1
        || entry.control_store_id != scope.control_store_id
        || entry.replacement_store_id != scope.replacement_store_id
        || entry.sequence
            != u64::try_from(index).map_err(|_| ReplacementError::Limit("sequence"))?
    {
        return Err(ReplacementError::Invalid(
            "schema, scope, or sequence mismatch",
        ));
    }
    if entry.timestamp.len() > 64 {
        return Err(ReplacementError::Invalid("timestamp is too long"));
    }
    let timestamp = OffsetDateTime::parse(&entry.timestamp, &Rfc3339)
        .map_err(|_| ReplacementError::Invalid("invalid timestamp"))?;
    if timestamp.offset() != UtcOffset::UTC {
        return Err(ReplacementError::Invalid("timestamp is not UTC"));
    }
    Ok(())
}

pub(super) fn initialization(
    bytes: &[u8],
    expected_control_store_id: Uuid,
) -> Result<InitializationSnapshot, ReplacementError> {
    validate_uuid(expected_control_store_id)?;
    let mut snapshot: Option<InitializationSnapshot> = None;
    let mut first_id = None;
    for (index, line) in records(bytes, 4096, 2, 4096)?.enumerate() {
        let entry: Envelope<InitializationAction> = decode(line)?;
        let scope = ReplacementScope::new(expected_control_store_id, entry.replacement_store_id)?;
        validate_envelope(&entry, index, scope)?;
        if let Some(previous) = &snapshot
            && (previous.scope != scope || first_id == Some(entry.event_id))
        {
            return Err(ReplacementError::Invalid(
                "initialization identity changed or repeated",
            ));
        }
        let phase = match (index, entry.action) {
            (0, InitializationAction::Initializing) => InitializationPhase::Initializing,
            (1, InitializationAction::Ready) => InitializationPhase::Ready,
            _ => {
                return Err(ReplacementError::Invalid(
                    "invalid initialization transition",
                ));
            }
        };
        if index == 0 {
            first_id = Some(entry.event_id);
        }
        snapshot = Some(InitializationSnapshot {
            scope,
            phase,
            event_count: index + 1,
            encoded_bytes: bytes.len(),
        });
    }
    snapshot.ok_or(ReplacementError::Invalid("missing initialization record"))
}

pub(super) fn ledger(
    bytes: &[u8],
    scope: ReplacementScope,
) -> Result<ReplacementSnapshot, ReplacementError> {
    let mut snapshot = ReplacementSnapshot {
        scope,
        operations: Vec::new(),
        compaction: None,
        event_count: 0,
        encoded_bytes: 0,
        pending_operation_id: None,
        pending_deletion_id: None,
        revision_floor: None,
        carried_revision: None,
        carrying: false,
        event_ids: HashSet::new(),
        positions: HashMap::new(),
        not_deleted: 0,
    };
    for line in records(
        bytes,
        MAX_LEDGER_BYTES,
        MAX_LEDGER_RECORDS,
        MAX_RECORD_BYTES,
    )? {
        push(&mut snapshot, line)?;
    }
    Ok(snapshot)
}

/// Validate one more record against a validated ledger, leaving it unchanged.
///
/// The result equals replaying the whole ledger with `line` appended: the
/// same framing, bounds, and transitions apply, without parsing earlier
/// records again.
pub(super) fn extend(
    snapshot: &ReplacementSnapshot,
    line: &[u8],
) -> Result<ReplacementSnapshot, ReplacementError> {
    if snapshot
        .encoded_bytes
        .checked_add(line.len())
        .is_none_or(|total| total > MAX_LEDGER_BYTES)
    {
        return Err(ReplacementError::Limit("total bytes"));
    }
    if snapshot.event_count >= MAX_LEDGER_RECORDS {
        return Err(ReplacementError::Limit("record count"));
    }
    if line.len() > MAX_RECORD_BYTES {
        return Err(ReplacementError::Limit("record bytes"));
    }
    match line.split_last() {
        Some((b'\n', body)) if !body.contains(&b'\n') => {}
        _ => {
            return Err(ReplacementError::Invalid(
                "record is not one complete JSONL line",
            ));
        }
    }
    if line.iter().all(u8::is_ascii_whitespace) {
        return Err(ReplacementError::Invalid("blank JSONL record"));
    }
    let mut next = snapshot.clone();
    push(&mut next, line)?;
    Ok(next)
}

/// Decode one framed record and apply it at the next sequence number.
fn push(snapshot: &mut ReplacementSnapshot, line: &[u8]) -> Result<(), ReplacementError> {
    let index = snapshot.event_count;
    let entry: Envelope<LedgerAction> = decode(line)?;
    validate_envelope(&entry, index, snapshot.scope)?;
    if !snapshot.event_ids.insert(entry.event_id) {
        return Err(ReplacementError::Invalid("duplicate event ID"));
    }
    if (index == 0) != matches!(entry.action, LedgerAction::Created) {
        return Err(ReplacementError::Invalid(
            "created record must occur exactly first",
        ));
    }
    if index != 1 && matches!(entry.action, LedgerAction::Compacted { .. }) {
        return Err(ReplacementError::Invalid(
            "a compaction record must be the second record",
        ));
    }
    if matches!(entry.action, LedgerAction::CheckpointCarried { .. }) {
        if !snapshot.carrying {
            return Err(ReplacementError::Invalid(
                "a carried operation must follow the compaction record",
            ));
        }
    } else {
        snapshot.carrying = false;
    }
    apply(snapshot, entry.action)?;
    snapshot.event_count = index + 1;
    snapshot.encoded_bytes = snapshot
        .encoded_bytes
        .checked_add(line.len())
        .ok_or(ReplacementError::Limit("total bytes"))?;
    Ok(())
}

fn validate_artifact(
    artifact: &RecordedArtifact,
    max_bytes: u64,
    allow_empty: bool,
) -> Result<(), ReplacementError> {
    if artifact.bytes > max_bytes || (!allow_empty && artifact.bytes == 0) {
        return Err(ReplacementError::Invalid(
            "artifact size is outside its bound",
        ));
    }
    if artifact.sha256.len() != 64
        || !artifact
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ReplacementError::Invalid(
            "artifact digest is not lowercase SHA-256",
        ));
    }
    Ok(())
}

fn validate_target(target: &NativePath) -> Result<(), ReplacementError> {
    let path = target
        .to_path_buf()
        .map_err(|_| ReplacementError::Invalid("target encoding does not match this OS"))?;
    let relative = RelativePath::new(&path)
        .map_err(|_| ReplacementError::Invalid("invalid workspace-relative target"))?;
    if relative.is_empty() {
        return Err(ReplacementError::Invalid(
            "target cannot be the workspace root",
        ));
    }
    Ok(())
}

fn pending(
    snapshot: &mut ReplacementSnapshot,
    id: Uuid,
) -> Result<&mut RecordedReplacement, ReplacementError> {
    if snapshot.pending_operation_id != Some(id) {
        return Err(ReplacementError::Invalid(
            "event does not match the unresolved operation",
        ));
    }
    snapshot
        .operations
        .last_mut()
        .filter(|operation| operation.operation_id == id)
        .ok_or(ReplacementError::Invalid("missing unresolved operation"))
}

fn apply(snapshot: &mut ReplacementSnapshot, action: LedgerAction) -> Result<(), ReplacementError> {
    match action {
        LedgerAction::Created => {}
        LedgerAction::ReplacementStarted {
            operation_id,
            kind,
            session_id,
            workspace_id,
            revision,
            target,
            expected,
            proposed,
            artifacts,
        } => {
            if artifacts.original != "original"
                || artifacts.proposed != "proposed"
                || artifacts.manifest != "manifest.json"
                || artifacts.stage != "stage"
            {
                return Err(ReplacementError::Invalid(
                    "artifact names do not match the fixed private layout",
                ));
            }
            start(
                snapshot,
                Started {
                    operation_id,
                    kind,
                    session_id,
                    workspace_id,
                    revision,
                    target,
                    expected,
                    proposed,
                },
                false,
            )?;
        }
        LedgerAction::CheckpointPrepared {
            operation_id,
            manifest,
        } => {
            let operation = pending(snapshot, operation_id)?;
            if operation.progress != ReplacementProgress::Intent || operation.outcome.is_some() {
                return Err(ReplacementError::Invalid(
                    "checkpoint preparation is out of order",
                ));
            }
            validate_artifact(&manifest, MAX_MANIFEST_BYTES, false)?;
            operation.manifest = Some(manifest);
            operation.progress = ReplacementProgress::CheckpointPrepared;
        }
        LedgerAction::StagePrepared {
            operation_id,
            identity,
            content,
        } => {
            let operation = pending(snapshot, operation_id)?;
            if operation.kind == ReplacementKind::Remove {
                return Err(ReplacementError::Invalid("a removal has no stage"));
            }
            if operation.progress != ReplacementProgress::CheckpointPrepared
                || operation.outcome.is_some()
                || content != operation.proposed
            {
                return Err(ReplacementError::Invalid(
                    "stage is out of order or has different proposed content",
                ));
            }
            operation.stage_identity = Some(identity);
            operation.progress = ReplacementProgress::StagePrepared;
        }
        LedgerAction::CommitIntended { operation_id } => {
            let operation = pending(snapshot, operation_id)?;
            // A removal has no stage, so its intent follows the checkpoint.
            let required = match operation.kind {
                ReplacementKind::Replace => ReplacementProgress::StagePrepared,
                ReplacementKind::Remove => ReplacementProgress::CheckpointPrepared,
            };
            if operation.progress != required || operation.outcome.is_some() {
                return Err(ReplacementError::Invalid("commit intent is out of order"));
            }
            operation.progress = ReplacementProgress::CommitIntended;
        }
        LedgerAction::ReplacementFinished {
            operation_id,
            outcome,
            code,
        } => {
            let operation = pending(snapshot, operation_id)?;
            if operation.outcome.is_some() {
                return Err(ReplacementError::Invalid("duplicate terminal outcome"));
            }
            let committed = operation.progress == ReplacementProgress::CommitIntended;
            if (outcome == ReplacementOutcome::Succeeded && !committed)
                || (committed
                    && matches!(
                        outcome,
                        ReplacementOutcome::Failed | ReplacementOutcome::Conflict
                    ))
            {
                return Err(ReplacementError::Invalid(
                    "outcome is incompatible with commit state",
                ));
            }
            let valid_code = code.as_ref().is_some_and(|value| {
                !value.is_empty()
                    && value.len() <= 128
                    && value.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            });
            if (outcome == ReplacementOutcome::Succeeded && code.is_some())
                || (outcome != ReplacementOutcome::Succeeded && !valid_code)
            {
                return Err(ReplacementError::Invalid("invalid terminal machine code"));
            }
            operation.outcome = Some(outcome);
            operation.code = code;
            if outcome != ReplacementOutcome::Uncertain {
                snapshot.pending_operation_id = None;
            }
        }
        LedgerAction::ReplacementAcknowledged { operation_id } => {
            let operation = pending(snapshot, operation_id)?;
            operation.acknowledged = true;
            snapshot.pending_operation_id = None;
        }
        LedgerAction::CheckpointDeletionStarted { operation_id } => {
            // Only a resolved entry can be deleted, one at a time. Outcome,
            // progress, and acknowledgement are history and stay unchanged.
            if snapshot.pending_operation_id.is_some() || snapshot.pending_deletion_id.is_some() {
                return Err(ReplacementError::Invalid(
                    "deletion requires no unresolved operation or deletion",
                ));
            }
            let operation = snapshot
                .position(operation_id)
                .and_then(|index| snapshot.operations.get_mut(index))
                .ok_or(ReplacementError::Invalid(
                    "deletion names no recorded operation",
                ))?;
            if operation.retention != CheckpointRetention::Retained {
                return Err(ReplacementError::Invalid(
                    "deletion names an entry that is not retained",
                ));
            }
            operation.retention = CheckpointRetention::Deleting;
            snapshot.pending_deletion_id = Some(operation_id);
        }
        LedgerAction::CheckpointDeleted { operation_id } => {
            if snapshot.pending_deletion_id != Some(operation_id) {
                return Err(ReplacementError::Invalid(
                    "deletion outcome does not match the unfinished deletion",
                ));
            }
            let operation = snapshot
                .position(operation_id)
                .and_then(|index| snapshot.operations.get_mut(index))
                .ok_or(ReplacementError::Invalid("missing deleted operation"))?;
            operation.retention = CheckpointRetention::Deleted;
            snapshot.pending_deletion_id = None;
            snapshot.not_deleted = snapshot
                .not_deleted
                .checked_sub(1)
                .ok_or(ReplacementError::Invalid("deleted count exceeds history"))?;
        }
        LedgerAction::Compacted {
            previous,
            previous_records,
            revision_floor,
        } => {
            let maximum = u64::try_from(MAX_LEDGER_BYTES)
                .map_err(|_| ReplacementError::Limit("total bytes"))?;
            validate_artifact(&previous, maximum, false)?;
            if previous_records == 0
                || usize::try_from(previous_records)
                    .map_or(true, |count| count > MAX_LEDGER_RECORDS)
            {
                return Err(ReplacementError::Invalid(
                    "compacted record count is outside its bound",
                ));
            }
            snapshot.compaction = Some(RecordedCompaction {
                ledger: previous,
                records: previous_records,
            });
            snapshot.revision_floor = revision_floor;
            snapshot.carrying = true;
        }
        LedgerAction::CheckpointCarried {
            operation_id,
            kind,
            session_id,
            workspace_id,
            revision,
            target,
            expected,
            proposed,
            manifest,
            stage_identity,
            progress,
            outcome,
            code,
            acknowledged,
        } => carry(
            snapshot,
            Started {
                operation_id,
                kind,
                session_id,
                workspace_id,
                revision,
                target,
                expected,
                proposed,
            },
            Carried {
                manifest,
                stage_identity,
                progress,
                outcome,
                code,
                acknowledged,
            },
        )?,
    }
    Ok(())
}

/// The recorded state a carried operation reached after it started.
struct Carried {
    manifest: Option<RecordedArtifact>,
    stage_identity: Option<RecordedFileIdentity>,
    progress: ReplacementProgress,
    outcome: Option<ReplacementOutcome>,
    code: Option<String>,
    acknowledged: bool,
}

/// Rebuild one carried operation through the same transitions that its
/// original records made, so a carried state can only be one those records
/// could reach. The result must be resolved. Carried revisions keep their
/// recorded order and never exceed the compaction's floor.
fn carry(
    snapshot: &mut ReplacementSnapshot,
    started: Started,
    carried: Carried,
) -> Result<(), ReplacementError> {
    let operation_id = started.operation_id;
    let revision = started.revision;
    if snapshot.revision_floor.is_none_or(|floor| revision > floor)
        || snapshot
            .carried_revision
            .is_some_and(|last| revision < last)
    {
        return Err(ReplacementError::Invalid(
            "carried revision is out of order or above the floor",
        ));
    }
    let content = started.proposed.clone();
    let kind = started.kind;
    start(snapshot, started, true)?;
    let Carried {
        manifest,
        stage_identity,
        progress,
        outcome,
        code,
        acknowledged,
    } = carried;
    let prepared = progress != ReplacementProgress::Intent;
    if kind == ReplacementKind::Remove && progress == ReplacementProgress::StagePrepared {
        return Err(ReplacementError::Invalid("a removal has no stage"));
    }
    let staged = kind == ReplacementKind::Replace
        && matches!(
            progress,
            ReplacementProgress::StagePrepared | ReplacementProgress::CommitIntended
        );
    match (prepared, manifest) {
        (true, Some(manifest)) => apply(
            snapshot,
            LedgerAction::CheckpointPrepared {
                operation_id,
                manifest,
            },
        )?,
        (false, None) => {}
        _ => {
            return Err(ReplacementError::Invalid(
                "carried manifest does not match its progress",
            ));
        }
    }
    match (staged, stage_identity) {
        (true, Some(identity)) => apply(
            snapshot,
            LedgerAction::StagePrepared {
                operation_id,
                identity,
                content,
            },
        )?,
        (false, None) => {}
        _ => {
            return Err(ReplacementError::Invalid(
                "carried stage does not match its progress",
            ));
        }
    }
    if progress == ReplacementProgress::CommitIntended {
        apply(snapshot, LedgerAction::CommitIntended { operation_id })?;
    }
    match outcome {
        Some(outcome) => apply(
            snapshot,
            LedgerAction::ReplacementFinished {
                operation_id,
                outcome,
                code,
            },
        )?,
        None if code.is_none() => {}
        None => {
            return Err(ReplacementError::Invalid(
                "carried machine code has no outcome",
            ));
        }
    }
    if acknowledged {
        apply(
            snapshot,
            LedgerAction::ReplacementAcknowledged { operation_id },
        )?;
    }
    if snapshot.pending_operation_id.is_some() {
        return Err(ReplacementError::Invalid(
            "a carried operation must be resolved",
        ));
    }
    snapshot.carried_revision = Some(revision);
    Ok(())
}

/// Record a new or carried operation. A new start must not lower the
/// revision floor and raises it; carried revisions follow their own rule.
fn start(
    snapshot: &mut ReplacementSnapshot,
    started: Started,
    carried: bool,
) -> Result<(), ReplacementError> {
    let Started {
        operation_id,
        kind,
        session_id,
        workspace_id,
        revision,
        target,
        expected,
        proposed,
    } = started;
    validate_uuid(operation_id)?;
    validate_uuid(session_id)?;
    validate_uuid(workspace_id)?;
    if snapshot.pending_operation_id.is_some()
        || snapshot.pending_deletion_id.is_some()
        || snapshot.positions.contains_key(&operation_id)
    {
        return Err(ReplacementError::Invalid("parallel or reused operation ID"));
    }
    if snapshot.not_deleted >= MAX_OPERATIONS {
        return Err(ReplacementError::Limit("retained operations"));
    }
    if !carried
        && snapshot
            .revision_floor
            .is_some_and(|floor| revision < floor)
    {
        return Err(ReplacementError::Invalid("policy revision moved backwards"));
    }
    validate_target(&target)?;
    validate_artifact(&expected, MAX_PAYLOAD_BYTES, true)?;
    validate_artifact(&proposed, MAX_PAYLOAD_BYTES, true)?;
    if kind == ReplacementKind::Remove && (proposed.bytes != 0 || proposed.sha256 != EMPTY_SHA256) {
        return Err(ReplacementError::Invalid(
            "a removal proposes the empty artifact",
        ));
    }
    let workspace_id = WorkspaceId::from_uuid(workspace_id)
        .map_err(|_| ReplacementError::Invalid("invalid workspace ID"))?;
    snapshot
        .positions
        .insert(operation_id, snapshot.operations.len());
    snapshot.not_deleted = snapshot
        .not_deleted
        .checked_add(1)
        .ok_or(ReplacementError::Limit("retained operations"))?;
    snapshot.operations.push(RecordedReplacement {
        operation_id,
        kind,
        session_id,
        workspace_id,
        revision,
        target,
        expected,
        proposed,
        manifest: None,
        stage_identity: None,
        progress: ReplacementProgress::Intent,
        outcome: None,
        code: None,
        acknowledged: false,
        retention: CheckpointRetention::Retained,
    });
    snapshot.pending_operation_id = Some(operation_id);
    if !carried {
        snapshot.revision_floor = Some(revision);
    }
    Ok(())
}
