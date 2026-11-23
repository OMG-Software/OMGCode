# 0054: Add multi-edit and create model file tools

- Status: Accepted, amended by [ADR-0079](0079-add-a-model-tool-that-creates-one-directory.md)
- Date: 2026-11-17

## Context

`workspace_edit_file` (ADR-0043) replaces one exact occurrence in an existing
file. A model that must change several places in a file needs one approval and
one checkpoint per replacement, and each later call reads the output of the
earlier one. The model also cannot create a file at all: ADR-0043 left create,
delete, and rename disabled. The broker already provides an exclusive create
that never replaces an existing entry, and the checkpointed apply, the
`file_write` approval operation, the writers-stopped gate, and the desktop
review already exist.

The session and provider layers cap tool names at eight. Six tools are
advertised after this change (read, list, edit, multi-edit, create, command),
and a later increment is expected to add two more.

## Decision

Add two model tools, `workspace_multi_edit_file` and `workspace_create_file`
(tool schema 0.4.0). Both follow the edit tool's contract; this record states
only what differs. The owning guides are the
[model edit route](../file-access.md#model-edit-route-adr-0043) and the
[multi-edit and create routes](../file-access.md#model-multi-edit-and-create-routes-adr-0054).

Common rules:

- Both are advertised and dispatched only under live `model_tools`, `read`, and
  `write`, with the same per-call recheck and the same `edit_disabled` denial.
  They are advertised in the order read, list, edit, multi-edit, create,
  command.
- Both reuse `ApprovalOperation::FileWrite`, so the writers-stopped
  acknowledgement is required and no approval enum, wire type, or TypeScript
  union changes. Each has its own digest domain tag
  (`omgcode-file-multi-edit-v1`, `omgcode-file-create-v1`), so a record for one
  tool, including `workspace_edit_file`, can never be consumed by another.
- Approval lifetime (900 seconds), lookup filters (session, workspace,
  operation, exact digest, current settings revision), the output-fits check
  before consumption, consumption before the write, and the approved, pending,
  and denied handling are shared with the edit tool through one gate.
  `workspace_edit_file` keeps its digest, codes, and messages.
- The desktop review considers candidates from all three tools, at most eight
  distinct candidates in total, newest first, and is exact only for the
  candidate whose tool-specific digest matches. The approvals label becomes
  "File change".

`workspace_multi_edit_file` takes `path` and one to 16 `edits` of
`old_text` (non-empty) and `new_text`, with `additionalProperties: false` at
both levels and the whole call within 4 KiB. Every `old_text` is matched against
the original live bytes and must occur exactly once (`edit_no_match`,
`edit_ambiguous`, naming the 1-based index). The matched ranges must not overlap
(`edit_overlap`); touching ranges are allowed. The replacements apply in
ascending offset order to one proposed buffer, which must differ
(`edit_unchanged`) and fit 1 MiB (`edit_too_large`). One approval and one
checkpointed apply cover the call. The digest covers the session, workspace,
normalized path, and the size and SHA-256 of the current and proposed bytes.
The summary is `multi-edit {path} ({n} edits): {cur} -> {new} bytes, sha256 ...`.

`workspace_create_file` takes `path` and `content` (empty allowed). Before any
approval is recorded, a live audited read must find the path absent
(`file_exists` otherwise) and a one-entry listing of a non-root parent must
succeed (`not_found` otherwise). The digest covers the session, workspace,
normalized path, and the content's size and SHA-256. After approval the handler
checks that the success report fits, consumes the record, and calls the
broker's exclusive `create_file`. A conflict returns `file_exists` and the
approval stays spent. An uncertain outcome is a terminal failure that leaves the
file-operation journal's pending record for the trusted acknowledgement; the
runtime never retries or deletes. Creation is not checkpointed.

A missing name is recognised on Windows from the native status the platform
broker keeps inside an `ErrorKind::Other` error (`omgcode_platform::broker::is_missing_entry`).
`classify_file_error` now maps it to the recoverable `not_found` that Linux
already returned, so reading, editing, or listing a missing path no longer ends
a Windows turn.

## Alternatives considered

- **Extend `workspace_edit_file` with an optional `edits` array.** Rejected: the
  two argument shapes are mutually exclusive, so the schema could not express
  them, and the edit tool's strict parser and digest would change.
- **One digest domain for all file tools.** Rejected: an approval recorded for a
  single edit could then be consumed by an equivalent multi-edit, and the
  reverse. Separate tags keep each approval bound to the tool the operator
  reviewed.
- **A new approval operation.** Rejected for now: `FileWrite` already carries the
  writers-stopped gate and the desktop and CLI flows, and a new operation would
  change the approval enum, wire types, and TypeScript for no new authority.
- **Checkpoint creation.** Not needed: nothing is lost, and the exclusive create
  refuses an existing entry. A checkpoint needs prior content.
- **Sequential single edits.** Rejected: each needs its own approval and sees
  the previous result, so a failure mid-sequence leaves a partial change.

## Consequences

- The 4 KiB argument cap bounds the content of a created file and the size of
  a multi-edit. Larger files need several calls or the trusted CLI.
- A create approval is spent if another writer creates the path between the
  check and the create. The create refuses to replace it, and the model must
  ask again.
- There is no atomicity across files, and neither tool detects a concurrent
  writer. Neither is power-loss qualified.
- The set is six tools of the eight-name cap, so the next increment can add two
  without raising a cap. The schema estimate used for context accounting rises
  from 4 KiB to 5 KiB (the six definitions serialize to 4,546 bytes).
- Windows reads of missing paths become recoverable, which also changes the
  read and edit tools' result for that case from a terminal failure to
  `not_found`.
- Review fields use fixed numbered labels ("Old text 1" to "Old text 16") so the
  label type stays static.

## References

- [ADR-0039](0039-gate-future-model-tools-with-bound-single-use-approval-records.md)
- [ADR-0040](0040-gate-the-model-command-tool-with-approvals-and-the-trusted-runner.md)
- [ADR-0043](0043-gate-the-model-file-edit-tool-with-approvals-and-checkpointed-apply.md)
- [ADR-0046](0046-decide-model-tool-approvals-in-an-isolated-desktop-window.md)
- [File access contract](../file-access.md)
