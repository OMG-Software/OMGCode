//! Trusted per-call approval records.
//!
//! An approval binds a future model-tool call to a session, workspace,
//! operation, exact request digest, and the settings revision in force when it
//! was requested. It stores no argument text. It is single-use and expires.
//! The `workspace_run_command` and `workspace_edit_file` model tools consume
//! records through [`ControlStore::consume_approval`], which every
//! host-changing model tool must call before its operation. A `file_write`
//! approval also records the operator's acknowledgement that other writers are
//! stopped, and consumption refuses one without it. See
//! `docs/agent-instructions.md`, ADR-0039, ADR-0040, and ADR-0043.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Action, ControlError, ControlSnapshot, ControlStore, valid_uuid};
use crate::policy::WorkspaceId;

/// Most approval records retained in one journal.
pub const MAX_APPROVALS: usize = 256;
/// Most undecided approvals at once.
pub const MAX_PENDING_APPROVALS: usize = 64;
/// Shortest accepted approval lifetime.
pub const MIN_APPROVAL_TTL_SECS: u64 = 60;
/// Longest accepted approval lifetime.
pub const MAX_APPROVAL_TTL_SECS: u64 = 60 * 60;
/// Longest approval summary, in bytes.
pub const MAX_APPROVAL_SUMMARY_BYTES: usize = 256;

/// The model-tool operation an approval covers.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOperation {
    /// Start a confined command.
    Command,
    /// Replace or create a workspace file.
    FileWrite,
    /// Read one workspace file or list one directory under a `prompt` read
    /// preference (ADR-0063). Older readers refuse the spelling.
    FileRead,
    /// Perform a Git operation with remote access.
    GitPush,
    /// Fetch one URL for the model `web_fetch` tool (ADR-0098). The digest
    /// binds the exact URL, offset, and size. No writers-stopped
    /// acknowledgement applies.
    Network,
}

impl ApprovalOperation {
    /// The stored spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::FileWrite => "file_write",
            Self::FileRead => "file_read",
            Self::GitPush => "git_push",
            Self::Network => "network",
        }
    }

    /// Parse a stored or command-line spelling, case-insensitively.
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "command" => Some(Self::Command),
            "file_write" | "file-write" => Some(Self::FileWrite),
            "file_read" | "file-read" => Some(Self::FileRead),
            "git_push" | "git-push" => Some(Self::GitPush),
            "network" => Some(Self::Network),
            _ => None,
        }
    }
}

/// The operator's decision.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approved,
    Denied,
}

/// The current state of one approval, computed with the current time.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    Pending,
    Approved,
    Denied,
    Consumed,
    Expired,
}

/// A requested approval, before a decision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequest {
    id: Uuid,
    session_id: Uuid,
    #[serde(deserialize_with = "super::deserialize_workspace_id")]
    workspace_id: WorkspaceId,
    operation: ApprovalOperation,
    request_sha256: String,
    summary: String,
    settings_revision: u64,
    created_unix: u64,
    expires_unix: u64,
}

/// An internal approval record: the request plus its decision and use.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(super) struct ApprovalRecord {
    request: ApprovalRequest,
    decision: Option<ApprovalDecision>,
    /// Whether the approving decision acknowledged that writers are stopped.
    #[serde(default)]
    writers_stopped: bool,
    consumed: bool,
}

/// An operator-facing approval view.
#[derive(Clone, Debug, Serialize)]
pub struct ApprovalStatus {
    pub id: Uuid,
    pub session_id: Uuid,
    pub workspace_id: WorkspaceId,
    pub operation: ApprovalOperation,
    pub request_sha256: String,
    pub summary: String,
    pub settings_revision: u64,
    pub created_unix: u64,
    pub expires_unix: u64,
    pub state: ApprovalState,
    /// Whether the approval acknowledged that other writers are stopped. Only
    /// a `file_write` approval can carry it, and only it requires it.
    pub writers_stopped: bool,
}

/// An operator's approval request, before validation.
#[derive(Clone, Debug)]
pub struct ApprovalDeclaration {
    pub session_id: Uuid,
    pub workspace_id: WorkspaceId,
    pub operation: ApprovalOperation,
    pub request_sha256: String,
    pub summary: String,
    pub ttl_secs: u64,
}

/// The exact request a consumer must match.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalBinding {
    pub session_id: Uuid,
    pub workspace_id: WorkspaceId,
    pub operation: ApprovalOperation,
    pub request_sha256: String,
}

/// The current Unix time in seconds.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl ControlStore {
    /// Record a pending approval. This preserves the settings revision.
    pub fn request_approval(
        &mut self,
        declaration: &ApprovalDeclaration,
        now_unix: u64,
    ) -> Result<ApprovalStatus, ControlError> {
        if !valid_uuid(declaration.session_id) {
            return Err(ControlError::ApprovalRefused(
                "the session identifier must be a version 4 UUID",
            ));
        }
        if self.workspace(declaration.workspace_id).is_none() {
            return Err(ControlError::UnknownWorkspace);
        }
        if !valid_digest(&declaration.request_sha256) {
            return Err(ControlError::ApprovalRefused(
                "the request digest must be 64 lowercase hexadecimal characters",
            ));
        }
        if declaration.summary.is_empty()
            || declaration.summary.len() > MAX_APPROVAL_SUMMARY_BYTES
            || declaration.summary.contains('\0')
            || declaration.summary.chars().any(char::is_control)
        {
            return Err(ControlError::ApprovalRefused(
                "the summary must be 1 to 256 bytes without NUL or control characters",
            ));
        }
        if !(MIN_APPROVAL_TTL_SECS..=MAX_APPROVAL_TTL_SECS).contains(&declaration.ttl_secs) {
            return Err(ControlError::ApprovalRefused(
                "the approval lifetime must be 60 to 3600 seconds",
            ));
        }
        if self.snapshot.approvals.len() >= MAX_APPROVALS {
            return Err(ControlError::ApprovalRefused(
                "too many approval records are retained",
            ));
        }
        if self
            .snapshot
            .approvals
            .iter()
            .filter(|record| pending_at(record, now_unix))
            .count()
            >= MAX_PENDING_APPROVALS
        {
            return Err(ControlError::ApprovalRefused(
                "too many undecided approvals are pending",
            ));
        }
        let request = ApprovalRequest {
            id: Uuid::new_v4(),
            session_id: declaration.session_id,
            workspace_id: declaration.workspace_id,
            operation: declaration.operation,
            request_sha256: declaration.request_sha256.clone(),
            summary: declaration.summary.clone(),
            settings_revision: self.revision(),
            created_unix: now_unix,
            expires_unix: now_unix.saturating_add(declaration.ttl_secs),
        };
        validate_request(&request)?;
        self.append(
            Action::ApprovalRequested {
                request: request.clone(),
            },
            self.revision(),
        )?;
        Ok(status_for(
            &ApprovalRecord {
                request,
                decision: None,
                writers_stopped: false,
                consumed: false,
            },
            now_unix,
        ))
    }

    /// Approve or deny one pending approval. This preserves the settings
    /// revision.
    ///
    /// Approving a `file_write` record is refused here: it needs the
    /// operator's writers-stopped acknowledgement through
    /// [`Self::approve_with_writers_stopped`]. Denial needs no acknowledgement.
    pub fn decide_approval(
        &mut self,
        approval_id: Uuid,
        approved: bool,
        now_unix: u64,
    ) -> Result<(), ControlError> {
        let record = self.decidable_record(approval_id, now_unix)?;
        if approved && record.request.operation == ApprovalOperation::FileWrite {
            return Err(ControlError::ApprovalRefused(
                "approving a file_write request requires the writers-stopped acknowledgement",
            ));
        }
        self.append(
            Action::ApprovalDecided {
                approval_id,
                decision: if approved {
                    ApprovalDecision::Approved
                } else {
                    ApprovalDecision::Denied
                },
                writers_stopped: false,
            },
            self.revision(),
        )
    }

    /// Approve one pending `file_write` record and record the operator's
    /// acknowledgement that every other writer to the target is stopped.
    ///
    /// The acknowledgement is an operating assumption, not enforced isolation:
    /// the checkpointed apply cannot detect a concurrent writer. Other
    /// operations refuse it. This preserves the settings revision.
    pub fn approve_with_writers_stopped(
        &mut self,
        approval_id: Uuid,
        now_unix: u64,
    ) -> Result<(), ControlError> {
        let record = self.decidable_record(approval_id, now_unix)?;
        if record.request.operation != ApprovalOperation::FileWrite {
            return Err(ControlError::ApprovalRefused(
                "the writers-stopped acknowledgement applies only to file_write requests",
            ));
        }
        self.append(
            Action::ApprovalDecided {
                approval_id,
                decision: ApprovalDecision::Approved,
                writers_stopped: true,
            },
            self.revision(),
        )
    }

    /// The undecided, unexpired record a decision may target.
    fn decidable_record(
        &self,
        approval_id: Uuid,
        now_unix: u64,
    ) -> Result<&ApprovalRecord, ControlError> {
        let record = self
            .approval_record(approval_id)
            .ok_or(ControlError::UnknownApproval)?;
        if record.decision.is_some() {
            return Err(ControlError::ApprovalRefused(
                "this approval already has a decision",
            ));
        }
        if record.request.expires_unix <= now_unix {
            return Err(ControlError::ApprovalRefused("this approval has expired"));
        }
        Ok(record)
    }

    /// Consume an approved request once, after checking its exact binding and
    /// that the settings revision has not changed. A host-changing model tool
    /// must call this before the operation. A `file_write` record also needs
    /// the writers-stopped acknowledgement; an earlier journal's approval
    /// without one replays but cannot be consumed. This preserves the settings
    /// revision.
    pub fn consume_approval(
        &mut self,
        approval_id: Uuid,
        binding: &ApprovalBinding,
        now_unix: u64,
    ) -> Result<(), ControlError> {
        let record = self
            .approval_record(approval_id)
            .ok_or(ControlError::UnknownApproval)?;
        if record.decision != Some(ApprovalDecision::Approved) {
            return Err(ControlError::ApprovalRefused(
                "the approval was not granted",
            ));
        }
        if record.consumed {
            return Err(ControlError::ApprovalRefused(
                "the approval was already consumed",
            ));
        }
        if record.request.operation == ApprovalOperation::FileWrite && !record.writers_stopped {
            return Err(ControlError::ApprovalRefused(
                "the file_write approval lacks the writers-stopped acknowledgement",
            ));
        }
        if record.request.expires_unix <= now_unix {
            return Err(ControlError::ApprovalRefused("this approval has expired"));
        }
        if record.request.session_id != binding.session_id
            || record.request.workspace_id != binding.workspace_id
            || record.request.operation != binding.operation
            || record.request.request_sha256 != binding.request_sha256
        {
            return Err(ControlError::ApprovalRefused(
                "the request does not match the approval",
            ));
        }
        if record.request.settings_revision != self.revision() {
            return Err(ControlError::ApprovalRefused(
                "the settings changed after the approval was requested",
            ));
        }
        self.append(Action::ApprovalConsumed { approval_id }, self.revision())
    }

    /// The status of one approval, or `None` when it is unknown.
    pub fn approval_status(&self, approval_id: Uuid, now_unix: u64) -> Option<ApprovalStatus> {
        self.approval_record(approval_id)
            .map(|record| status_for(record, now_unix))
    }

    /// Every retained approval, oldest first, with its current state.
    pub fn approval_statuses(&self, now_unix: u64) -> Vec<ApprovalStatus> {
        self.snapshot
            .approvals
            .iter()
            .map(|record| status_for(record, now_unix))
            .collect()
    }

    fn approval_record(&self, approval_id: Uuid) -> Option<&ApprovalRecord> {
        self.snapshot
            .approvals
            .iter()
            .find(|record| record.request.id == approval_id)
    }
}

/// Whether a record still occupies a pending slot at `now`. An expired,
/// undecided record keeps its history but cannot be decided, so it must not
/// block a new request.
fn pending_at(record: &ApprovalRecord, now: u64) -> bool {
    record.decision.is_none() && record.request.expires_unix > now
}

/// Validate and apply one approval action during replay or append preflight.
pub(super) fn apply(
    state: &mut ControlSnapshot,
    action: &Action,
    _revision: u64,
) -> Result<(), ControlError> {
    match action {
        Action::ApprovalRequested { request } => {
            validate_request(request)
                .map_err(|_| ControlError::Invalid("invalid approval request"))?;
            if state.approvals.len() >= MAX_APPROVALS
                || state
                    .approvals
                    .iter()
                    .any(|record| record.request.id == request.id)
                || !state
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.id == request.workspace_id)
                || state
                    .approvals
                    .iter()
                    .filter(|record| pending_at(record, request.created_unix))
                    .count()
                    >= MAX_PENDING_APPROVALS
            {
                return Err(ControlError::Invalid(
                    "invalid, duplicate, or excessive approval request",
                ));
            }
            state.approvals.push(ApprovalRecord {
                request: request.clone(),
                decision: None,
                writers_stopped: false,
                consumed: false,
            });
        }
        Action::ApprovalDecided {
            approval_id,
            decision,
            writers_stopped,
        } => {
            let record = state
                .approvals
                .iter_mut()
                .find(|record| record.request.id == *approval_id)
                .ok_or(ControlError::Invalid("decision for an unknown approval"))?;
            if record.decision.is_some() {
                return Err(ControlError::Invalid("duplicate approval decision"));
            }
            // Only an approved file_write decision can carry the
            // acknowledgement. Its absence still replays: earlier journals
            // never recorded it, and consumption refuses such a record.
            if *writers_stopped
                && (*decision != ApprovalDecision::Approved
                    || record.request.operation != ApprovalOperation::FileWrite)
            {
                return Err(ControlError::Invalid(
                    "a writers-stopped acknowledgement needs an approved file_write record",
                ));
            }
            record.decision = Some(*decision);
            record.writers_stopped = *writers_stopped;
        }
        Action::ApprovalConsumed { approval_id } => {
            let record = state
                .approvals
                .iter_mut()
                .find(|record| record.request.id == *approval_id)
                .ok_or(ControlError::Invalid("consumption of an unknown approval"))?;
            if record.decision != Some(ApprovalDecision::Approved) || record.consumed {
                return Err(ControlError::Invalid(
                    "consumption requires an unused approved record",
                ));
            }
            record.consumed = true;
        }
        _ => {}
    }
    Ok(())
}

fn status_for(record: &ApprovalRecord, now_unix: u64) -> ApprovalStatus {
    let state = if record.consumed {
        ApprovalState::Consumed
    } else {
        match record.decision {
            Some(ApprovalDecision::Denied) => ApprovalState::Denied,
            Some(ApprovalDecision::Approved) if record.request.expires_unix <= now_unix => {
                ApprovalState::Expired
            }
            Some(ApprovalDecision::Approved) => ApprovalState::Approved,
            None if record.request.expires_unix <= now_unix => ApprovalState::Expired,
            None => ApprovalState::Pending,
        }
    };
    ApprovalStatus {
        id: record.request.id,
        session_id: record.request.session_id,
        workspace_id: record.request.workspace_id,
        operation: record.request.operation,
        request_sha256: record.request.request_sha256.clone(),
        summary: record.request.summary.clone(),
        settings_revision: record.request.settings_revision,
        created_unix: record.request.created_unix,
        expires_unix: record.request.expires_unix,
        state,
        writers_stopped: record.writers_stopped,
    }
}

fn validate_request(request: &ApprovalRequest) -> Result<(), ControlError> {
    if !valid_uuid(request.id)
        || !valid_uuid(request.session_id)
        || !valid_digest(&request.request_sha256)
        || request.summary.is_empty()
        || request.summary.len() > MAX_APPROVAL_SUMMARY_BYTES
        || request.summary.contains('\0')
        || request.summary.chars().any(char::is_control)
        || request.expires_unix <= request.created_unix
        || request.expires_unix - request.created_unix > MAX_APPROVAL_TTL_SECS
    {
        return Err(ControlError::ApprovalRefused(
            "the approval request is invalid",
        ));
    }
    Ok(())
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests;
