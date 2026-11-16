# 0050: Interrupt a streamed model request when a turn is cancelled

- Status: Accepted
- Date: 2026-11-16

## Context

Cancellation of a chat turn was cooperative and coarse. The turn runtime
checked a cancel token between model rounds and tool calls. A model request
already in flight ran to its end or to the thirty-second request deadline. A
text-only turn had no cancellation point at all, so cancelling it changed
nothing. On exit the desktop shell waited five seconds and then abandoned any
turn still waiting on the model, leaving its journal with an open turn that
the operator had to interrupt later (ADR-0045).

ADR-0049 added streamed requests and left this open, because interrupting a
request changes what a cancelled turn records.

These constraints shape the decision:

- **A blocking read cannot be woken.** The streamed request used the blocking
  HTTP client. A read that waits for the next piece returns only when bytes
  arrive or the deadline passes, so a check between pieces would not stop a
  server that has gone quiet, and would not cover the wait for the response
  head.
- **Core already has the needed pattern.** The package fetch runs an async
  client inside a single-thread runtime and bounds each wait
  (ADR-0035).
- **A cancelled turn must be recorded as cancelled**, not as a provider
  failure, and must not leave an open turn.
- **The journal format must not change.** The runtime already records the
  failure reason `cancelled` for a turn stopped between rounds.

## Decision

### Adapter: the streamed request watches a cancel token

`complete_streaming` and `complete_with_tools_streaming` take a
`CancelToken`. The streamed request now runs on an async client inside a
single-thread runtime created for that one request. The client is built inside
that runtime and is not reused, because a pooled connection must not outlive
the runtime that drives it. Its settings equal the blocking client's: no
proxy, no redirect, no retry, the same connect timeout, and the same total
request deadline, which still covers the whole response.

The token is checked before the request is sent, while waiting for the
response head, and while waiting for each piece of the body, at an interval of
25 ms. When it is set, the request is dropped, which closes the connection,
and the method returns the new error `ProviderError::Cancelled`. Text already
passed to the caller stays delivered and is still provisional.

The event-stream rules of ADR-0049 are unchanged. The reader becomes a parser
that is fed bytes as they arrive, so the same rules apply to any chunking.

The non-streaming methods keep the blocking client and cannot be interrupted.

### Runtime and application layer: one recorded outcome

- `ToolModelRequest` carries the turn's `CancelToken`, and `ModelError` gains
  `Cancelled`. When a model returns it and the turn's token is set, the
  runtime ends the turn exactly as it does for a cancellation between rounds:
  failure reason `cancelled`, error `TurnError::Cancelled`. A model that
  returns it while the token is not set is treated as a model error.
- A streamed text-only turn now creates a cancel token and hands it to the
  front end through the existing hook. A cancelled request ends the turn with
  the failure reason `cancelled`. As in the runtime, a cancellation seen after
  the reply arrived but before it is recorded also ends the turn as cancelled.
- A cancelled turn is reported with the new application error kind
  `cancelled` (message `turn was cancelled`). Before, the tool path reported
  it with the general kind `turn`.

No journal record changes shape. A cancelled turn has a `turn failed` record
with the reason `cancelled` and no assistant message.

### Desktop

`chat_cancel` now stops a streamed request. The turn ends as failed with the
error code `cancelled`, the `finished` event is pushed, and the frontend shows
a plain notice that the turn was cancelled. Exit cancels every running turn,
so a streamed turn ends within the grace period and leaves no open turn.

With `--stream off`, and for the CLI without `--stream`, nothing changes: a
request in flight is not interrupted.

## Alternatives considered

- **Check the token between pieces on the blocking client.** Small, but it
  does not stop a quiet server or the wait for the response head, which are
  the cases an operator most wants to cancel.
- **Read on a helper thread and abandon it.** The turn thread could return at
  once, but the helper would stay blocked until the deadline, with the
  connection open, and nothing would supervise it.
- **Move the non-streaming requests to the async client too.** It would make
  every request interruptible, but the CLI and compaction have no cancel
  source, and it would change a path that has met a live server.
- **Report a cancelled request as a provider error.** The journal and the
  window would then show a failure the operator did not have.
- **Keep the application error kind `turn`.** The window could tell a
  cancellation from another turn failure only by matching message text.
- **Record the partial text of a cancelled reply.** The text is provisional
  and was never validated as a whole reply (ADR-0049).

## Consequences

- A streamed turn, text-only or tool-enabled, can be cancelled at any point of
  a model request, and ends within about the check interval.
- The desktop's bounded exit no longer abandons a streamed turn.
- Still not interruptible: a non-streamed request, the compaction request
  before a turn, and a tool call already in progress. The CLI has no cancel
  source; stopping its process still leaves an open turn.
- The error kind of a cancelled tool-enabled turn changes from `turn` to
  `cancelled`. No released version reported the old value from a window.
- The streamed request no longer shares the blocking client, so a streamed
  and a non-streamed request never share a pooled connection.
- Not verified: a live LM Studio server, and any window on Linux.

## References

- [Desktop interface](../desktop-interface.md)
- [Provider authentication](../provider-authentication.md)
- [ADR-0035](0035-fetch-packages-through-a-trusted-allowlisted-cache.md)
- [ADR-0045](0045-build-the-desktop-shell-on-tauri-with-a-narrow-command-surface.md)
- [ADR-0049](0049-stream-local-model-replies-and-push-chat-events.md)
