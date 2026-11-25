# 0099: Recover from rejected tool calls and raise write-tool argument limits

- Status: Accepted
- Date: 2026-11-25

## Context

A desktop session on app 0.62.0 and the ChatGPT plan route asked the model to
scaffold a Rust workspace with `workspace_create_file`. The call that created
`crates/quote-core/src/lib.rs` had 4,066 bytes of argument JSON and succeeded.
The next call, almost certainly the `validation.rs` that `lib.rs` declares,
ended the turn with `turn.failed` and the code `invalid_tool_call`, 33 seconds
later. It left no `tool.requested` event in the session journal and no record
in the model action journal. The model received nothing it could recover from.

The causes are in the core:

- `ToolCall::parse` refused argument JSON over `MAX_TOOL_ARGUMENT_BYTES`
  (4 KiB) for every tool. The edit content limit is 1 MiB, so the argument
  limit was the binding limit on what a model could write: no create or edit
  call could carry more than about 4 KiB of file text.
- The turn runtime ended the whole turn with `invalid_tool_call` when any
  call failed to parse, before it recorded an intent or an audit record. AGENTS.md
  says every model tool call, executed or refused, is recorded in the model
  action journal.
- The session journal held at most 4 KiB of arguments and 64 KiB per event.

## Decision

- **Per-tool argument limits.** `workspace_create_file`,
  `workspace_edit_file`, and `workspace_multi_edit_file` accept up to 32 KiB
  of argument JSON (`MAX_WRITE_TOOL_ARGUMENT_BYTES`). Every other tool keeps
  4 KiB (`MAX_TOOL_ARGUMENT_BYTES`). The edit content limits in `edit.rs` are
  unchanged.
- **Consistent bounds.**
  - The session journal accepts up to 32 KiB of arguments in a
    `tool.requested` event. The runtime applies each tool's own limit first.
  - The encoded session event bound rises from 64 KiB to 80 KiB. The worst
    case measured is 66,153 bytes: a 128-byte call identifier of backslashes,
    a 64-byte tool name, and 32,768 bytes of argument JSON that is all
    escaped backslashes, escaped quotes, or newlines, each of which the event
    escapes a second time. Control characters inside a string cost less: a
    `\u0001` escape grows from six to seven bytes. The old bound could not
    hold the worst case.
  - A streamed Chat Completions reply may hold 8 MiB, the bound a Responses
    stream already had. A 32 KiB argument streamed a few bytes per event,
    each with its envelope, is several mebibytes, and the 1 MiB buffered
    bound refused it. The buffered bound is unchanged.
  - Neither provider path caps one call's arguments. The response and stream
    bounds limit memory, and `ToolCall::parse` is the argument bound.
  - Approval review parses a recorded request with the dispatcher's own
    parser, so it takes the per-tool limit. Its candidate bound counts
    requests, not bytes, and a rejected call is not a candidate. The
    approval records, summaries, and desktop approval fields hold no
    argument text.
- **Request and prompt caps.** The encoded request cap (`MAX_REQUEST_BYTES`,
  every route) and the assembled prompt text cap rise from 256 KiB to 4 MiB,
  enough for a 1M-token window of text. The plan route echoes each answered
  call of the live turn (ADR-0097), so about seven 30 KiB creates passed the
  old request cap and ended the turn with `request_too_large`. The assembled
  cap counts history message text, which holds no call arguments. The
  provider's context check estimates one token per byte of message text, plus
  the echoed calls on the plan route, and refuses a request over
  `context_tokens` with `ContextLimit`. It therefore binds at about
  `context_tokens` bytes, before either byte cap for any window up to 4M
  tokens. For a 256K-token window that is about 256 KiB of estimated text, so
  with echoed calls this check, not the byte cap, ends a long scaffold turn;
  real code is about three to four bytes per token, so the estimate is
  conservative. The estimate is unchanged here. Auto-compaction is decided only before a turn
  starts, from the estimate of the saved history plus the new message against
  `min(threshold, safe input budget)`; nothing compacts inside a turn, so a
  long tool turn meets the per-request context-limit check, not compaction.
  The tool-text budget (256 KiB per turn), the 16 KiB message cap, and the
  summarizer chunk cap (128 KiB) are unchanged.
- **Tool descriptions.** The create, edit, and multi-edit descriptions say
  that arguments are limited to 32 KiB and that a large file is a create
  followed by edits. `TOOL_SCHEMA_VERSION` is 0.7.0. The nine definitions
  serialize to 7,425 bytes, inside the app's 8 KiB estimate.
- **Recoverable rejection.** A response element with the supported
  function-call shape and a valid, unique call identifier whose tool name is
  not one the runtime has, or whose arguments are too large, not strict JSON,
  of the wrong shape, out of range, or name an invalid path, is answered with
  a recoverable result and the turn continues. The stable codes are
  `unsupported_tool`, `arguments_too_large`, `invalid_arguments`, and
  `invalid_path`. The result is labelled core-observed text with the code, a
  fixed message, the received argument text's byte length and SHA-256, and
  nothing else the model wrote. `arguments_too_large` adds the tool's limit
  and advises creating a large file with its first part and extending it with
  edit calls. `unsupported_tool` lists the offered tool names. A small result
  limit keeps only the tool name and code. The call counts toward the
  tool-call and tool-byte budgets, and each answer reserves the same prompt
  and byte capacity as a dispatched call.
- **Still ending the turn.** A response element that is not an object, has an
  unsupported shape (including a non-string tool name), or has no usable call
  identifier leaves no call to answer. A duplicate call identifier is
  ambiguous. Each ends the turn with `invalid_tool_call` before any call is
  recorded or dispatched, as before. A call to a tool the runtime has but the
  workspace's live preferences do not offer reaches the dispatcher and is
  still denied and ends the turn (`edit_disabled`, `command_disabled`,
  `model_tools_disabled`), as before. The offered list is a snapshot, and
  ADR-0063 keeps the live recheck terminal.
- **Pairing and storage.** A rejected call is one `tool.requested`, one
  `tool.completed` (outcome `failed` with the code), and one tool message, in
  that order, like any call. The request records `{}` as its arguments. The
  received text is never stored. The tool name is the known tool's name; for
  an unknown tool it is the model's own name when that is at most 64 bytes of
  letters, digits, `_`, `.`, and `-`, and otherwise `unsupported_tool`. In the
  live turn the result carries that name and `{}` as its answered call
  (ADR-0097), so the plan route sends a `function_call` item and a
  `function_call_output` item with the same call identifier. Replay is
  unchanged: a saved result is user text, as ADR-0097 states. No schema
  version changed.
- **Audit.** Each rejected call is recorded in the model action journal as
  refused, with the tool when it is a known one, no target, and the SHA-256
  of the received argument text as the request digest. Four refusal reasons
  are added: `unsupported_tool`, `arguments_too_large`, `invalid_arguments`,
  and `invalid_path`. A call to a tool the runtime does not have has no
  `ActionTool`, and inventing one would put a false tool in the evidence, so
  a new tool value `unrecognized` records it with no name. The name is hostile
  text, and the session journal and the result already hold its bounded
  spelling or its absence. If the refusal cannot be recorded, the turn ends
  with `audit_unavailable`, as for any call.
- **Compatibility.** A build before 0.63.0 refuses to open a session journal
  that holds a `tool.requested` event with more than 4 KiB of arguments or any
  event over 64 KiB, as an invalid journal, and repairs nothing. It also
  reads a model action journal that holds a new reason or `unrecognized` as
  corrupt, and the core then refuses every model tool call with
  `audit_unavailable` until the operator moves that file aside. These
  statements come from reading the earlier code and were not tried with an
  older build. Journals that hold no such call or record open in both.

## Alternatives considered

- **Raise every tool's argument limit to 32 KiB.** Rejected. Only the three
  file-text tools carry file text. A larger limit for the command, read,
  list, rename, delete, and directory tools widens the journals, the review,
  and the echo of calls to the plan route for no use.
- **Keep ending the turn but report a clearer error.** Rejected. The model
  cannot act on a failed turn, and the operator loses the work already done.
- **Store the rejected arguments, truncated.** Rejected. The text is
  untrusted and was refused; a digest and a length identify it without
  keeping it in two journals.
- **Record a rejected call only in the action journal.** Rejected. A result
  without a recorded request breaks the pairing replay and the plan route
  rely on.
- **Make a known tool that is not offered recoverable too.** Rejected for
  now. Several tests and ADR-0063 treat a live denial as terminal, and a
  model that calls a tool it was not offered is a policy event rather than a
  formatting mistake. Revisit if models do this in practice.
- **Add an append tool for large files.** Not done. A create followed by edit
  calls already works within the limits, and a new tool is a new approval
  surface.
- **Reuse `invalid_request` and an existing tool for every rejection.**
  Rejected. The audit would not say why a call was refused, and an unknown
  tool would be recorded as a tool it is not.

## Consequences

- A model can create or edit a file of nearly 32 KiB in one call. A larger
  file takes a create and edits.
- One malformed, oversized, or misnamed call no longer ends the turn or
  disappears from the audit trail.
- With a 256K-token window the byte-based context estimate still stops a turn
  whose echoed calls and history estimate to more than about 256 KiB. Making
  that estimate less conservative is separate work.
- A request can now reach 4 MiB. The 4 MiB body is built in memory once per
  request; the stream and response bounds are unchanged. Ten 30 KiB creates
  in one turn finish in a scripted turn, and a plan-route request echoing
  them (about 600 KiB) and a Chat Completions request over 256 KiB are sent
  to mock servers. A real route was not tried.
- Not verified: a real model or the real plan route with a call of this
  size, and a real Chat Completions server streaming one.
- Tests cover the boundary of each limit; 5, 30, and 40 KiB create calls
  under `allow` and `prompt`; an exact approval review of a 30 KiB create;
  each rejection code with its result text, session records, paired call,
  and action record; a hostile tool name; calls mixed with valid ones; both
  budgets; a small result limit; the shape errors and duplicate identifier
  that still end the turn; replay on the next turn; the Responses items; the
  worst-case event; a 32 KiB streamed argument; and the new action journal
  values. Before the change, the 5 KiB create test ended the turn with
  `InvalidToolCall(InvalidArguments)`.

## References

- [ADR-0043](0043-gate-the-model-file-edit-tool-with-approvals-and-checkpointed-apply.md)
- [ADR-0054](0054-add-multi-edit-and-create-model-file-tools.md)
- [ADR-0063](0063-deny-prompt-or-allow-each-preference-wait-for-approvals-in-the-turn-and-journal-model-actions.md)
- [ADR-0097](0097-send-a-tool-result-as-its-call-and-output-on-the-plan-route.md)
- [Session storage](../session-storage.md)
- [Workspace settings](../workspace-permissions.md#model-action-journal)
