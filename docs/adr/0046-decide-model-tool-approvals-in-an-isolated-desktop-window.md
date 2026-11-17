# 0046: Decide model tool approvals in an isolated desktop window

- Status: Accepted
- Date: 2026-11-16

## Context

ADR-0039 gates the model command and edit tools with bound, single-use
approval records. Until now the only operator surface was the trusted CLI:
`approvals approve <id>`, with `--writers-stopped` for a `file_write` record
(ADR-0043). The desktop shell (ADR-0045) adds chat, so it also needs a way to
decide approvals. The user selected an in-application approval surface.

These constraints shape it:

- **The webview renders hostile text.** Session titles, messages, tool
  arguments, and approval summaries can all be model- or repository-controlled.
  The security rules require permission controls to stay separate from rendered
  untrusted content, and every command exposed to a webview to be validated and
  limited.
- **A record stores a digest, not the arguments.** An approval holds a summary
  and a SHA-256 binding. In the CLI flow the operator reads the recorded tool
  request with `session inspect` and matches it to the approval by eye. A
  window that shows arguments next to an Approve button must not show the wrong
  arguments.
- **A `file_write` digest depends on the live file.** It covers the size and
  SHA-256 of the current and proposed bytes, so it cannot be recomputed from
  the journal alone.
- **Core already enforces the record rules.** Decisions are single, expiry and
  the settings revision are checked at consumption, and a `file_write` record
  needs the writers-stopped acknowledgement. The desktop must add no second
  authority.

## Decision

1. **A separate approvals window.** The shell has two webview windows. `main`
   shows the session tree, transcripts, and chat. `approvals` shows approval
   records and holds the only decision controls. It never renders transcript
   content and loads a separate, smaller bundle.
2. **The decision command exists for one window only.** `approvals_list`,
   `approval_detail`, and `approval_decide` are granted to the `approvals`
   window alone, in two layers:
   - Application commands are declared to the Tauri build so that each needs an
     explicit permission, and the capability file for `main` does not grant
     these three.
   - Every command handler also checks the calling webview's label in Rust
     against one table. A capability file edited by mistake cannot expose a
     decision to `main`.
   The `main` window can ask only for the number of pending approvals and for
   the approvals window to be shown.
3. **Exact review binding in core.** `approval_detail` asks core to find the
   recorded tool request that the approval is bound to, by recomputing the
   approval digest with the same function the tool handler uses:
   - A `command` digest is recomputed from the recorded arguments.
   - A `file_write` digest needs a live read of the file through the audited
     broker, under the same live `read` requirement as the tool. The read
     happens only when the operator opens that approval, never while listing.
   - The result is `exact`, `stale` (a candidate request exists but no digest
     matches, for example because the file changed), or `not_found`.
   The window shows arguments only from an `exact` match.
4. **Approve needs a review token.** For an `exact`, pending approval the
   service issues a random, single-use token that lives for 120 seconds and is
   bound to the approval identifier and digest. `approval_decide` refuses an
   approve without a valid token (`review_expired`, `review_mismatch`). A new
   detail request replaces the previous token for that approval. Tokens live in
   memory only. Deny needs no token.
5. **Writers-stopped stays explicit.** Approving a `file_write` record requires
   `writers_stopped: true` from a dedicated checkbox and calls
   `approve_with_writers_stopped`. Other operations ignore the field and call
   `decide_approval`. Core still refuses the wrong combination.
6. **Display-safe fields.** Core renders the fields the approvals window shows.
   Control characters, bidirectional overrides, and zero-width characters are
   replaced with visible escapes, so a request cannot disguise what it does.
   The frontend writes every value as a text node.
7. **Deliberate friction.** Approve is disabled for one second after the review
   panel renders, is never the default focus, and the window never selects an
   approval by itself.
8. **The CLI commands stay.** `approvals approve`, `deny`, `list`, and `show`
   are unchanged and remain the surface for headless use.

## Alternatives considered

- **Approve from the main window.** Rejected: one script-injection flaw in
  transcript rendering would then reach the decision command.
- **A native operating-system message dialog.** It is rendered outside any
  webview, which is stronger against webview compromise. Rejected for now: a
  message box cannot show a 4 KiB edit legibly, behaves differently on each
  Linux desktop, and needs a dialog plugin. It remains a candidate for a later
  hardening step.
- **Tauri's isolation pattern** (an intermediate sandboxed frame that vets IPC
  messages). Deferred: per-window capabilities plus the Rust label check
  already remove the command from `main`. Isolation adds a moving part without
  changing which window may decide.
- **Showing the latest matching tool request without digest matching.**
  Rejected: the operator could approve one record while reading another
  request's arguments.
- **Approve without a token.** Rejected: any code that can reach the command
  in the approvals window could approve a record the operator never opened.
- **Recomputing the `file_write` digest while listing.** Rejected: every poll
  would read workspace files and add audit records.

## Consequences

- The operator approves only a request that core has just matched to the
  record, and only from a window that renders no transcript content.
- A compromised `main` webview can still do everything `main` is allowed to
  do: create sessions in registered workspaces, send chat turns, archive,
  restore, interrupt, search, and read transcripts. It can cause approval
  requests but cannot decide them. Pending records stay bounded by core.
- The approvals webview is still web content. This design reduces what reaches
  it and what it can call; it does not make the webview a trusted computing
  base. Native dialog confirmation remains possible later.
- Display-safe fields make control, bidirectional, zero-width, filler, tag,
  and variation-selector characters visible. They do not detect look-alike
  letters, mixed scripts, or trailing spaces, so a request can still be
  visually misleading in those ways.
- Opening a `file_write` approval reads the target file and writes up to
  eight audit records, one for each candidate request it has to read. A stale
  or missing binding offers Deny only.
- While a turn runs in the shell, its journal is locked on Windows, so the
  detail request reports `busy` until the turn ends.
- Tokens are lost when the application exits; the operator reviews again.
- Per-window capability enforcement is a Tauri property. The Rust label table
  has its own test; the capability files need a real webview to exercise.

## References

- [ADR-0039](0039-gate-future-model-tools-with-bound-single-use-approval-records.md)
- [ADR-0040](0040-gate-the-model-command-tool-with-approvals-and-the-trusted-runner.md)
- [ADR-0043](0043-gate-the-model-file-edit-tool-with-approvals-and-checkpointed-apply.md)
- [ADR-0045](0045-build-the-desktop-shell-on-tauri-with-a-narrow-command-surface.md)
- [Security coding rules](../security-coding-rules.md#trust-and-permissions)
- [Tauri capabilities](https://tauri.app/security/capabilities/)
