# 0063: Deny, prompt, or allow each preference, wait for approvals in the turn, and journal model actions

- Status: Accepted; amended by [ADR-0098](0098-add-a-model-web-fetch-tool-behind-a-host-allowlist.md) and [ADR-0099](0099-recover-from-rejected-tool-calls-and-raise-write-tool-argument-limits.md)
- Date: 2026-11-19

## Context

Every workspace preference was a boolean. A model file-write or command call
always needed a trusted single-use approval, even when the operator had turned
`write` on for the workspace. The operator could not let the model create and
change files in a trusted workspace without approving each call.

The approval flow also failed in the desktop shell. A gated call recorded a
pending approval and returned `approval_required`, which ended the call. Only a
later identical call could consume an approved record (ADR-0039, ADR-0043).
After the operator approved in the approvals window, nothing ran unless the
model happened to repeat the exact call in a later turn. To the operator,
approving did nothing.

Model actions were recorded only in places that each covered part of the
picture: the session journal, the file operation journal, the run journal, and
the approval events of the settings journal. No single record listed every
model tool call with how it was authorised and what became of it.

Persisted settings must keep their exact bytes, so settings written by an
earlier version still open and an unchanged workspace is not silently
widened.

## Decision

**Three states.** Each of `read`, `write`, `execute`, `task_network`, and
`host_services` is `deny`, `prompt`, or `allow`
([`Access`](../../crates/omgcode-core/src/policy/access.rs)). `model_tools`
stays a boolean. `prompt` and `allow` both permit an operator-initiated trusted
command, such as `omgcode run`. They differ only for a model tool call:

- `deny` removes the tool and refuses the call.
- `prompt` needs a trusted single-use approval for each call.
- `allow` runs the call without an approval.

**Per-tool write limits.** `write_tools` holds one state for each model
file-write tool: `edit`, `multi_edit`, `create`, `rename`, and `delete`. All
default to `allow`. A limit can only narrow `write`: the effective state of a
tool is the more restrictive of `write` and its limit. A `deny` limit removes
the tool.

**Effective model access.** With `model_tools` off, every model tool is denied.
Otherwise:

- the read and list tools use `read`;
- a file-write tool uses `write` and its limit, and is denied unless `read`
  permits, because every write tool reads the file first. A `prompt` read adds
  no prompt here, because that read returns no file text to the model;
- the command tool uses the most restrictive of `execute`, `read`, and
  `write`. Per-tool limits do not reach it: a permitted command can still
  create, change, or delete files.

A `prompt` read records an approval with the new operation `file_read`. It
needs no writers-stopped acknowledgement. Its digest uses the domain
`omgcode-file-read-v1` and covers the tool, path, and limit.

**Byte-compatible storage.** A stored `false` still means `deny`. A stored
`true` keeps its earlier meaning: `allow` for `read`, whose model calls never
needed an approval, and `prompt` for every other preference. A string is
written only for the state a boolean cannot express. `write_tools` is omitted
while every limit is `allow`, and `host_services` while it is `deny`. Settings
that never use a new state keep their exact bytes. A non-canonical value, such
as the string `"deny"` where `false` belongs, is refused. A new default
workspace starts with `read` allow and `write` prompt, as before.

**Waiting in the turn.** The desktop shell runs each turn with an in-turn
approval wait. When a call records a pending approval, the turn waits for the
decision, polling the settings store every 250 ms without holding it.

- **Approved:** the same call is dispatched again, so every live check, read,
  and digest is repeated before the approval is consumed.
- **Denied:** the call ends with `approval_denied` and the turn continues.
- **Expired:** the call ends with `approval_expired` after the record's
  lifetime.
- **Cancelled:** the turn ends as cancelled.

A call waits at most three times. Time spent waiting does not count against
the turn deadline. The CLI `chat` command does not wait and keeps the earlier
behaviour.

**Reviewing a waiting call.** A turn holds its session journal, so the
approvals window cannot read the recorded request from it. The waiting turn
publishes the exact call it waits on, as the tool name and the recorded
arguments, in a slot shared with the desktop service. The review uses that
call as its only candidate when the turn is waiting on that approval. Core
still recomputes the digest from the call and the live workspace, so the
review is exact only for the recorded request. A turn running for any other
reason still makes the review `busy`. The detail reports `waiting`, and the
window states whether approving runs the call now or only lets a later
identical call run once.

**Standing allow for commands.** The file tools hold the settings lock from
the decision through the action. The command tool must release it, because
the runner takes its own. A run authorized by a standing `allow` therefore
records `standing_allow` in its run start record. The runner refuses it,
before recording anything, unless the model command state is still `allow`
under its own lock, and the run watcher withdraws it once that state leaves
`allow`. An approved run continues under `prompt`, as before.

**Settings contention.** A waiting turn, other turns, and the approvals window
each hold the settings store briefly. The chat and dispatch paths now wait up
to 2 seconds for another holder before failing, through
`ControlStore::open_existing_waiting`.

**Model action journal.** Every model tool call is recorded in
`<data-dir>/model-actions-v1.jsonl`
([`action_journal`](../../crates/omgcode-core/src/action_journal.rs)):

- An executed call writes `action_started` before it acts. The record holds
  the session, workspace, call ID, tool, targets, request digest, and the
  authorisation: `standing_allow` with the settings revision, or `approval`
  with the consumed approval ID.
- It writes `action_finished` afterwards, with the outcome (`succeeded`,
  `failed`, or `uncertain`), a code, and the broker, checkpoint, or run record
  it produced.
- A refused call writes one `action_refused` with the reason. The reasons are
  access denied, approval required, denied, or expired, invalid request,
  policy unavailable, and cancelled.

The start is synced before the action, and a call whose start cannot be
recorded is refused with `audit_unavailable`, which ends the turn. A target
that fails path validation is stored only as its SHA-256 and length. The
journal rotates at 4 MiB or 16,384 events into at most four archives, and a
record is at most 32 KiB. A finish may be written by a later writer, so the
lock is not held while a command runs. The trusted CLI command
`omgcode actions list` lists recent calls.

## Alternatives considered

- **Two states plus a global "trust" switch.** One switch per workspace would
  make every tool unattended at once. Separate states keep reads, writes, and
  commands apart, and per-tool limits let the operator keep deletes
  prompted while creates run freely.
- **A new persisted enum written for every preference.** Writing strings for
  every value would change the bytes of every settings log on first save and
  break reading by older versions. The mixed encoding changes bytes only when
  a new state is chosen.
- **Treat a stored `true` as `allow` for every preference.** It would silently
  remove the approval that every existing model write and command needed.
  Existing workspaces keep `prompt` until the operator chooses `allow`.
- **Ask the model to repeat the call after approval.** That is the earlier
  behaviour. It depends on the model, wastes a request, and fails silently.
- **Hand the approval to the turn and skip the second dispatch.** The live file
  can change while the operator decides. Dispatching again repeats every check
  and digest before the approval is consumed.
- **Let the approvals window read the session journal while the turn runs.**
  On Windows the running turn's lock blocks that read, and reading a journal
  that is being appended invites torn records. The published call is exact and
  is still checked against the digest.
- **Record model actions in the file operation or run journal.** Each covers
  one kind of action, and neither records refusals or authorisation. One
  journal for every model call is simpler to audit.

## Consequences

- An operator can let the model change a trusted workspace without approving
  each call, and can keep individual tools prompted or denied.
- In the desktop shell, approving a request completes the waiting call in the
  same turn.
- `task_network` and `host_services` have no model tool, so `prompt` and
  `allow` mean the same for them today. `prompt` keeps any future model
  network or host-service tool gated.
- The per-tool limits do not limit the command tool. A permitted command can
  still change files the limits protect.
- Narrowing a command preference from `allow` to `prompt` stops a command
  running under the standing allow. A run start record with `standing_allow`
  cannot be read by version 0.42.0 or earlier.
- An approval consumed just before an audit failure is spent although nothing
  ran. The call ends with `audit_unavailable`.
- A finish whose start was rotated past the newest archive is refused, and
  the turn ends.
- The CLI `chat` command does not wait in the turn. Its approvals still need a
  later identical call.
- The journal records what was called and how it was authorised. It does not
  prove the absence of side effects for an `uncertain` outcome, and it is not
  tamper-evident against a same-user process.
- Linux was not exercised for this change; the in-turn wait and journal were
  verified on Windows only.

## References

- [Workspace permissions](../workspace-permissions.md#deny-prompt-or-allow-adr-0063)
- [Desktop interface](../desktop-interface.md)
- [ADR-0039](0039-gate-future-model-tools-with-bound-single-use-approval-records.md),
  [ADR-0043](0043-gate-the-model-file-edit-tool-with-approvals-and-checkpointed-apply.md),
  [ADR-0046](0046-decide-model-tool-approvals-in-an-isolated-desktop-window.md),
  [ADR-0060](0060-change-workspace-settings-from-the-desktop-with-native-dialogs.md)
