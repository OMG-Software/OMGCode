# 0055: Add checkpointed delete and no-replace rename model tools

- Status: Accepted
- Date: 2026-11-17

## Context

After ADR-0054 the model can edit, multi-edit, and create files, each after a
trusted single-use approval. It still cannot remove or rename one. Both are
useful during a refactor, and both can destroy work: a removal loses the file's
content, and a rename that replaces an existing name loses that file's content.
The tool-name cap is eight, and six names are in use. This record uses the last
two slots.

The platform already has a handle-relative replacement commit (ADR-0022,
ADR-0024), a private checkpoint store with a ledger (ADR-0021, ADR-0023,
ADR-0025, ADR-0053), and the audited broker with an exclusive create (ADR-0043,
ADR-0054). Nothing removes a workspace file or renames one without replacing.

## Decision

Add two model tools, `workspace_rename_file` and `workspace_delete_file` (tool
schema 0.5.0). Both follow the edit tool's contract; this record states only
what differs. The owning guide is
[model rename and delete routes](../file-access.md#model-rename-and-delete-routes-adr-0055).

Common rules:

- Both are advertised and dispatched only under live `model_tools`, `read`, and
  `write`, with the edit tool's per-call recheck and `edit_disabled` denial.
  They are advertised in the order read, list, edit, multi-edit, create,
  rename, delete, command. That is eight names, the existing cap; no cap rose.
- Both reuse `ApprovalOperation::FileWrite`, so the writers-stopped
  acknowledgement is required and no approval enum, wire type, or TypeScript
  union changes. Each has its own digest domain tag
  (`omgcode-file-rename-v1`, `omgcode-file-delete-v1`), so a record for one
  tool, or for any of the three earlier file-write tools, is never consumed by
  another. Approval lifetime, lookup filters, the output-fits check, and
  consumption before the write come from the shared gate.
- Before any approval is recorded, a live audited read of the source supplies
  its exact size and SHA-256. A source over 1 MiB is refused as the recoverable
  `edit_too_large`. Neither tool interprets the bytes, so a non-UTF-8 source is
  accepted.
- The desktop review considers candidates from all five file-write tools, at
  most eight distinct candidates in total, newest first, and is exact only for
  the candidate whose tool-specific digest matches.

`workspace_rename_file` takes `from` and `to`, with `additionalProperties:
false`. Both are validated as relative paths and must differ after
normalization. Before an approval is recorded, the source read must succeed
(`not_found`), the destination must not exist (`file_exists`), and its parent
must (`not_found`). The digest covers the session, workspace, both normalized
names, and the source's size and SHA-256. After approval, the broker's new
`rename_file` records the intent with both names in the file-operation audit
journal and calls the platform's `rename_no_replace`. Rename is not
checkpointed: no content is destroyed, and renaming back reverses it. An
uncertain outcome leaves the audit journal's pending record for the trusted
acknowledgement and is never retried.

`workspace_delete_file` takes `path`. The digest covers the session, workspace,
normalized path, and the file's size and SHA-256. After approval, the broker's
new `apply_removal` runs the same sequence as the checkpointed apply: the same
live-binding, permission, admission, and qualified-filesystem gates, a durable
checkpoint of the original bytes, a recheck, commit intent, one native removal
attempt, and the same uncertain-outcome recovery. The success message names the
checkpoint's operation ID, from which the trusted `file restore` recreates the
file.

Ledger change. Every operation record gains a `kind` of `replace` or `remove`.
It is omitted for `replace`, so every existing record, carried record, and
manifest stays byte-identical and replays unchanged. For `remove` the proposed
artifact is exactly the empty artifact, no stage exists, and commit intent
follows the prepared checkpoint directly. Replay refuses a removal that
proposes bytes, that has a stage record, or whose carried form has a stage
identity. Compaction carries `kind`. Older binaries refuse a ledger that holds a
`remove` record, because they reject the unknown field. The checkpoint payload
rule that the original is UTF-8 applies only to a `replace`; a `remove` keeps
its original bytes as they are.

Restoring a `remove` checkpoint is not an apply: there is nothing to
checkpoint. The target must not exist, and the retained original is written
through the broker's audited exclusive create. An existing entry is
`checkpoint_conflict`. The new file takes the broker's default creation
metadata; mode, owner, access list, and timestamps are not restored. The
trusted CLI `file restore` omits the expected digest and size for this case.

Native primitives:

- `rename_no_replace` opens both parents handle-relative beneath the workspace
  without links or mount crossings, pins the source as a regular single-link
  file, reads it through the pinned handle, and requires its size and
  SHA-256 to match. On Linux it calls `renameat2` with `RENAME_NOREPLACE`,
  re-statting the source name against the pinned inode immediately before and
  the destination name against it afterwards. On Windows the source handle
  carries DELETE access with sharing that denies every other writer and
  deleter, and the rename is `FileRenameInformationEx` with POSIX semantics and
  without the replace flag, so an existing name fails with a collision status.
- The removal on Linux is `unlinkat` by name after the recheck, then a check
  that the pinned inode's link count is zero. On Windows it opens the name with
  DELETE, verifies that same handle (file ID, volume, name, security, and every
  byte), and marks that handle for POSIX-semantics deletion.

## Alternatives considered

- **Delete without a checkpoint.** Rejected: it destroys the content, and the
  operator could not recover from a wrong approval.
- **Delete by moving into a workspace trash directory.** Rejected: it adds
  entries to the workspace, and the model could read the file back.
- **Delete by moving into the private store.** Rejected: the data root can be
  on another volume, so the move is not an atomic rename. Copy-then-delete
  gives the same sequence as the checkpoint plus removal without the new
  failure modes.
- **Rename that replaces an existing name.** Rejected: it destroys the replaced
  file without a checkpoint.
- **Checkpoint a rename.** Not needed: nothing is destroyed, and renaming back
  reverses it.
- **A new approval operation or a shared digest domain.** Rejected for the
  reasons recorded in ADR-0054.

## Consequences

- Both tools refuse a source over 1 MiB, because the approval-bound read and the
  checkpoint are bounded by it. Larger files cannot be renamed or deleted by the
  model.
- On Linux the unlink is by name, so a swap between the recheck and the unlink
  cannot be prevented. It is detected afterwards through the pinned inode's link
  count and reported as uncertain; a different file may then be gone and the
  operator must inspect it. On Windows the removal is by the verified handle, so
  exactly the verified file is deleted.
- A rename of a file whose name is swapped between the last check and the call
  is caught on Linux by the destination identity check, reported uncertain. On
  Windows the retained source handle denies other deleters, so the swap cannot
  happen.
- Restoring a deleted file does not restore its mode, owner, access list, or
  timestamps.
- Older binaries refuse a checkpoint ledger that holds a removal record.
- All eight tool slots are used. A further tool needs a cap decision.
- The schema estimate for context accounting rises from 5 KiB to 6 KiB (the
  eight definitions serialize to 5,968 bytes).
- Neither primitive is power-loss qualified, and neither detects a concurrent
  writer. Windows directory flushes carry no power-loss claim (ADR-0023). The
  Linux paths are not qualified on a Linux host.

## References

- [ADR-0021](0021-store-checkpoints-with-native-private-handles.md)
- [ADR-0023](0023-store-windows-checkpoints-with-verified-acls.md)
- [ADR-0024](0024-publish-one-verified-stage-on-windows-ntfs.md)
- [ADR-0025](0025-restore-and-delete-checkpoints-explicitly.md)
- [ADR-0043](0043-gate-the-model-file-edit-tool-with-approvals-and-checkpointed-apply.md)
- [ADR-0053](0053-compact-the-checkpoint-ledger-in-place.md)
- [ADR-0054](0054-add-multi-edit-and-create-model-file-tools.md)
- [File access contract](../file-access.md)
