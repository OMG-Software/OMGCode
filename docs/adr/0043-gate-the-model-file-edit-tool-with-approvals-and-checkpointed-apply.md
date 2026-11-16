# 0043: Gate the model file edit tool with approvals and checkpointed apply

- Status: Accepted
- Date: 2026-11-16

## Context

The local model can read and list workspace files and, under ADR-0040, ask to
run one approved command. It cannot change a file. The trusted CLI can already
replace an existing file through checkpointed apply (ADR-0018 to ADR-0025),
and single-use approval records (ADR-0039) already declare a `file_write`
operation that nothing uses yet.

These constraints shape a model route:

- **Argument size:** tool-call arguments are limited to 4 KiB. The session
  journal enforces that limit when it reads the stored arguments back, so
  sending a whole replacement file is impractical without a format change.
- **Expected values:** the file-access contract says expected values must not
  be inferred from a saved conversation. A conversation can be stale or
  injected.
- **No compare-and-swap:** apply cannot detect a concurrent writer, so the
  operator must stop other writers.
- **Default `write`:** `write` is enabled by default, so a write tool gated
  only on settings would be offered to every workspace that enables
  `model_tools`.

## Decision

1. **One edit tool:** `workspace_edit_file` takes a workspace-relative `path`,
   a non-empty `old_text`, and a `new_text`. It replaces exactly one
   occurrence of `old_text` in an existing UTF-8 regular file within the 1 MiB
   bound. If `old_text` is missing, occurs more than once, or equals
   `new_text`, the result is a recoverable failure that the model can correct.
   The tool does not create, delete, rename, or change permissions. The
   existing 4 KiB argument limit is unchanged.
2. **Offered under live settings:** the tool is advertised, and dispatched,
   only while `model_tools`, `read`, and `write` are all enabled for the
   workspace. The tool schema version becomes `0.3.0`. Settings never authorize
   the write by themselves; every write needs an approval.
3. **Approval bound to the live file:** on each call the trusted handler reads
   the current file through the audited broker and computes the proposed bytes.
   Its approval digest covers a domain tag, the session, the workspace, the
   path, and the size and SHA-256 of both the current and the proposed bytes.
   The expected current value therefore comes from a live trusted read, never
   from the model or the conversation.
   - No matching record: the handler creates a `file_write` approval with a
     bounded summary (path, sizes, short digests; no file text) and returns
     `approval_required`.
   - Pending: `approval_required`. Denied: `approval_denied`.
   - Approved: the handler consumes the record and then applies.
   - A file that changed since the request produces a different digest, so an
     old approval cannot apply to new content.
4. **Writers stopped is recorded:** approving a `file_write` record requires
   the operator's explicit `--writers-stopped` acknowledgement. The decision
   records it, and core refuses to consume a `file_write` approval without it.
   Other operations do not accept the flag.
5. **Checkpointed apply only:** after consumption the handler calls the
   existing `apply_replacement` with the captured current size and digest as
   the expected value. Every existing gate still applies: live `read` and
   `write`, the qualified filesystem (ext4 on Linux, NTFS on Windows), the
   same-volume checkpoint store, and pending-operation blocking. A conflict,
   pending checkpoint, busy store, or unsupported filesystem returns a
   recoverable failure, and the approval remains consumed. Restore and
   checkpoint management stay trusted CLI commands.
6. **Review:** the operator reviews the exact `old_text` and `new_text` in the
   session journal's recorded tool request with `session inspect` before
   approving. The approval summary never contains file text.
7. **Model-visible result:** a successful edit returns the path, the new size
   and SHA-256, and the checkpoint operation identifier. It never returns file
   contents.

## Alternatives considered

- **Whole-file replacement content in the arguments.** Rejected: it would need
  a larger argument limit, and with it a journal format change. It also costs
  far more context than a targeted edit.
- **The model supplies the expected SHA-256.** Rejected: the expected value
  would come from conversation state. A live trusted read is stronger and
  simpler.
- **A separate `model_writes` preference.** Rejected for now: every write
  already needs a per-call approval, and a new preference would change the
  settings schema. The tool is still offered only under live `model_tools`,
  `read`, and `write`.
- **Applying without consuming first.** Rejected: ADR-0039 consumes a record
  before the operation, so a crash cannot reuse the approval.
- **Multi-edit, create, delete, or rename tools.** Deferred: each needs its own
  journal, checkpoint, and review design.

## Consequences

- Workspaces that enable `model_tools` with the default `write` now advertise
  the edit tool. It does nothing without an approval, and the operator can
  disable `write` to remove it.
- An approval that is consumed before a failed apply is spent; the model must
  ask again. This is safe, but it costs the operator another decision.
- The model can edit only text that fits in one 4 KiB call. Larger changes need
  several approved edits.
- The trusted CLI apply route keeps its own contract, which `--writers-stopped`
  in this route mirrors. Neither route detects a concurrent writer.

## References

- [ADR-0018](0018-define-checkpointed-replacement-before-enabling-overwrites.md)
- [ADR-0026](0026-offer-read-list-tools-through-a-workspace-preference.md)
- [ADR-0039](0039-gate-future-model-tools-with-bound-single-use-approval-records.md)
- [ADR-0040](0040-gate-the-model-command-tool-with-approvals-and-the-trusted-runner.md)
- [Workspace file access](../file-access.md)
