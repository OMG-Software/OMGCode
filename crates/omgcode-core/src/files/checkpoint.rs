//! Authorized private preparation and existing-only operator recovery.

use omgcode_platform::broker::{self, FileCapture, RelativePath};
use uuid::Uuid;

use super::{FileBroker, FileError, FileOperation, FileOutcome, FileTarget, validate_target};
use crate::policy::{Operation, PolicyRequest, SessionBinding};
use crate::replacement::store::{self, Store};
use crate::replacement::{
    CheckpointCompaction, CheckpointDeletion, CheckpointError, CheckpointInspection,
    CheckpointReceipt, CheckpointStore, RecordedArtifact,
};
use crate::session::sha256_hex;

impl FileBroker<'_> {
    /// Capture and persist a private checkpoint without replacing the target.
    ///
    /// Requires fresh read/write preferences and a live process-local binding.
    /// The source identity and security metadata come from the same pinned file
    /// as its bytes. Unsupported backends refuse before checkpoint I/O.
    /// Prepared state remains pending until trusted acknowledgement; this is
    /// not an apply token. No payload bytes appear in the returned receipt.
    ///
    /// Each payload is at most 1 MiB. Storage retains at most 64 intents and
    /// 128 MiB of managed file bytes, with capacity reserved before artifacts.
    /// A ledger without room for the operation is compacted in place first,
    /// which drops deleted entries' history (ADR-0053).
    /// A write or sync error can leave uncertain private evidence. Reopen for
    /// inspection; do not automatically retry or remove that evidence.
    pub fn checkpoint_prepare(
        &mut self,
        binding: &SessionBinding,
        target: &FileTarget,
        expected: &RecordedArtifact,
        proposed: &[u8],
    ) -> Result<CheckpointReceipt, FileError> {
        let store = CheckpointStore::Workspace(target.workspace_id);
        self.prepare_in(store, binding, target, expected, proposed)
    }

    /// Prepare into `store`. Product routes pass the target's workspace
    /// store; tests also seed the legacy store, which nothing else writes.
    pub(super) fn prepare_in(
        &mut self,
        store: CheckpointStore,
        binding: &SessionBinding,
        target: &FileTarget,
        expected: &RecordedArtifact,
        proposed: &[u8],
    ) -> Result<CheckpointReceipt, FileError> {
        let path = validate_request(target, expected, proposed)?;
        check_permissions(self.control, binding, target)?;
        if !cfg!(any(target_os = "linux", windows)) {
            return Err(CheckpointError::Io(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "native checkpoint storage is not qualified on this platform",
            ))
            .into());
        }
        let (workspace, mut audit, read_id) =
            self.begin(binding, target, &path, FileOperation::Read, Operation::Read)?;
        let capture = match workspace.capture(&path, broker::MAX_FILE_BYTES) {
            Ok(capture) => capture,
            Err(source) => {
                audit.finish(read_id, FileOutcome::Failed)?;
                return Err(FileError::Native(source));
            }
        };
        audit.finish(read_id, FileOutcome::Succeeded)?;
        // Lock order is control, then one subordinate journal at a time. The
        // read outcome is durable before the checkpoint anchor can be locked.
        drop(audit);
        validate_capture(&capture, expected)?;
        if capture.bytes == proposed {
            return Err(CheckpointError::Unchanged.into());
        }
        // Root setup precedes the callback; initialization cannot start from an
        // old authorization result. The control writer remains borrowed here.
        check_permissions(self.control, binding, target)?;
        if let CheckpointStore::Workspace(workspace_id) = store {
            if workspace_id != target.workspace_id {
                return Err(FileError::Invalid(
                    "checkpoint store belongs to another workspace",
                ));
            }
            store::ensure_workspace_store(self.control, workspace_id)?;
        }
        let mut opened = Store::open_or_initialize(self.control, store, capture.volume, |_| {
            check_permissions(self.control, binding, target)
        })?;
        check_permissions(self.control, binding, target)?;
        Ok(opened.prepare(target, self.control.revision(), &capture, proposed)?)
    }

    /// Inspect existing protected metadata without creating files or opening any
    /// workspace target. No checkpoint contents are returned.
    /// Invalid records or missing required artifacts return an error without
    /// repair. A complete initializing anchor reports incomplete initialization.
    /// An unbound workspace reports unused storage.
    pub fn checkpoint_inspect(
        &mut self,
        store: CheckpointStore,
    ) -> Result<CheckpointInspection, FileError> {
        Ok(store::inspect(self.control, store)?)
    }

    /// Acknowledge one unresolved operation in existing storage. This does not
    /// assert replacement success, restore bytes, delete evidence, or grant access.
    /// Only the current pending operation ID is accepted. The acknowledgement
    /// is appended and synced; a write or sync failure can be uncertain and must
    /// be inspected on reopen without automatic retry.
    pub fn checkpoint_acknowledge(
        &mut self,
        store: CheckpointStore,
        operation_id: Uuid,
    ) -> Result<(), FileError> {
        Ok(store::acknowledge(self.control, store, operation_id)?)
    }

    /// Abandon a workspace's bound `.OMGCode` checkpoint folder (ADR-0070).
    ///
    /// This is a trusted operator action, not a model tool route. Only the
    /// control journal changes; the folder and its contents stay where they
    /// are. The next checkpoint in the workspace creates a new folder, so an
    /// old one must first be moved, deleted, or emptied. Returns whether a
    /// folder was bound.
    pub fn checkpoint_forget_store(
        &mut self,
        workspace_id: crate::policy::WorkspaceId,
    ) -> Result<bool, FileError> {
        Ok(self.control.forget_workspace_store(workspace_id)?)
    }

    /// Permanently delete one resolved checkpoint entry from existing storage.
    ///
    /// This is a trusted operator action, not a model tool route. It needs no
    /// workspace binding and opens no workspace file. The entry's payloads,
    /// manifest, and any retained stage are removed; the ledger keeps the
    /// operation's history and outcome. An unresolved operation must first be
    /// acknowledged. After an interrupted deletion, only the same request is
    /// accepted, and it removes whatever fixed names remain. Removal is never
    /// recursive, and no error path deletes other evidence.
    pub fn checkpoint_delete(
        &mut self,
        store: CheckpointStore,
        operation_id: Uuid,
    ) -> Result<CheckpointDeletion, FileError> {
        Ok(store::delete(self.control, store, operation_id)?)
    }

    /// Rewrite the checkpoint ledger in place, dropping deleted entries' history.
    ///
    /// This is a trusted operator action, not a model tool route. It needs no
    /// workspace binding and opens no workspace file or checkpoint entry.
    /// Retained entries keep their recorded state as one carried record each;
    /// only the replaced ledger's size, digest, and record count remain of the
    /// rest. It is refused while an operation is unresolved or a deletion is
    /// unfinished, and writes nothing when the ledger would not shrink. A
    /// failure during the rewrite is uncertain; reopen to inspect, and do not
    /// retry automatically.
    pub fn checkpoint_compact(
        &mut self,
        store: CheckpointStore,
    ) -> Result<CheckpointCompaction, FileError> {
        Ok(store::compact(self.control, store)?)
    }
}

/// Require both permissions from the same live binding while control is locked.
/// Reuse at mutation boundaries so setup time cannot make an old check a grant.
pub(super) fn check_permissions(
    control: &crate::control::ControlStore,
    binding: &SessionBinding,
    target: &FileTarget,
) -> Result<(), CheckpointError> {
    for operation in [Operation::Read, Operation::Write] {
        let decision = control.check_file_policy(
            binding,
            &PolicyRequest {
                session_id: target.session_id,
                workspace_id: target.workspace_id,
                operation,
            },
        )?;
        if !decision.configured_allowed() {
            return Err(CheckpointError::Denied(decision.reason()));
        }
    }
    Ok(())
}

/// Check the complete proposed bytes and exact precondition before opening a target.
pub(super) fn validate_request(
    target: &FileTarget,
    expected: &RecordedArtifact,
    proposed: &[u8],
) -> Result<RelativePath, FileError> {
    if proposed.len() > broker::MAX_FILE_BYTES
        || expected.bytes > 1024 * 1024
        || expected.sha256.len() != 64
        || !expected
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || std::str::from_utf8(proposed).is_err()
    {
        return Err(FileError::Invalid(
            "checkpoint requires bounded UTF-8 and an exact SHA-256/size precondition",
        ));
    }
    validate_target(target, false)
}

/// Compare the bytes from the retained source before creating private evidence.
pub(super) fn validate_capture(
    capture: &FileCapture,
    expected: &RecordedArtifact,
) -> Result<(), FileError> {
    validate_capture_bytes(capture, expected)?;
    if std::str::from_utf8(&capture.bytes).is_err() {
        return Err(FileError::Invalid("checkpoint source is not UTF-8"));
    }
    Ok(())
}

/// Compare the exact size and SHA-256 of the retained source without
/// interpreting its bytes. A removal checkpoint keeps them as they are.
pub(super) fn validate_capture_bytes(
    capture: &FileCapture,
    expected: &RecordedArtifact,
) -> Result<(), FileError> {
    if u64::try_from(capture.bytes.len())
        .map_err(|_| FileError::Invalid("capture size overflow"))?
        != expected.bytes
        || sha256_hex(&capture.bytes) != expected.sha256
    {
        return Err(CheckpointError::Conflict.into());
    }
    Ok(())
}
