# 0051: Cancel every model request and let an interrupt cancel a CLI turn

- Status: Accepted
- Date: 2026-11-17

## Context

ADR-0050 made a streamed model request watch the turn's cancel token. It left
three gaps, and named them as consequences:

- A **non-streamed request** used the blocking HTTP client, so it could not be
  interrupted. A text-only turn that did not stream had no cancel token at
  all, and the desktop with `--stream off` ignored a cancel request.
- The **compaction requests** before a turn, and those of `session compact`,
  had no token. A cancel during pre-turn compaction did nothing until the turn
  itself started.
- The **CLI had no cancel source.** Ctrl-C ended the process, so a turn that
  had started stayed open in its journal until the operator ran `session
  interrupt`.

ADR-0050 rejected moving the non-streaming requests to the async client
because nothing could cancel them and because that path had met a live
server. These constraints shape this decision:

- **The blocking client is the async client.** reqwest's blocking client runs
  the same async client on a background runtime thread. Moving a request to
  the async client inside its own runtime changes how the caller waits, not
  the request on the wire: the same body, headers, timeouts, redirect and
  proxy settings.
- **The interrupt handler already exists.** The trusted `run` command absorbs
  Ctrl-C and Ctrl-Break on Windows and `SIGINT` and `SIGTERM` on Linux, and a
  watcher thread turns the recorded interrupt into a run cancellation
  (ADR-0030). It accepts only the run's `CancelSignal`.
- **Compaction writes once.** Core records the compaction only after the last
  summary is validated, so a compaction stopped earlier leaves the journal
  unchanged.
- **The journal format must not change.** A cancelled turn already has a
  defined record: a turn-failed record with the reason `cancelled` and no
  assistant message.

## Decision

### Adapter: every request watches the token

`complete` and `complete_with_tools` take a `CancelToken`, like the streamed
methods. All four methods share one transport. Each request builds its own
async HTTP client inside a single-thread runtime created for that request, with
the settings ADR-0050 gave the streamed request: no proxy, no redirect, no
retry, the connect timeout, and the thirty-second total deadline. The token is
checked before anything is sent, while the response head is awaited, and
before and while each piece of the body is awaited, at 25 ms intervals. A set
token drops the request, closing the connection, and returns
`ProviderError::Cancelled`. A non-streamed body read in part is discarded. The
1 MiB response bound is applied as the pieces arrive.

The blocking client is removed, and with it reqwest's `blocking` feature. No
dependency is added.

### Application: one token per turn, created before compaction

Every chat turn has one cancel token. It is created after the session checks
and before the compaction check, and `ChatHooks::on_cancel_token` receives a
clone on both the text-only and the tool-enabled path, streamed or not. A
front end can supply the token itself through the new `ChatHooks::cancel`, so
that a cancellation source installed before the turn reaches it. The tool path
hands the token to the runtime through the new
`TurnRuntime::with_cancel_token`.

The same token stops the pre-turn compaction requests. `compact_journal` takes
a token too. A cancelled compaction returns the error kind `cancelled` with the
message "Compaction was cancelled; nothing was recorded", and records nothing.

A token already set when the turn would start ends it as `cancelled` before
the turn marker, so nothing is recorded. This was already the runtime's
behaviour, and the text-only path now matches it. A cancellation after the
turn started is recorded as before.

### CLI: an interrupt cancels the turn or the compaction

The platform interrupt module gains `watch_interrupt`, which runs any closure
once on an interrupt. `cancel_on_interrupt` is now a wrapper around it.
`omgcode chat` creates the turn's token, installs the watcher after the
journal is open and before the turn, and passes the token through
`ChatHooks::cancel`. `session compact` does the same around `compact_journal`.
An interrupt then stops the request in flight. The chat turn is recorded as
cancelled, and the command fails with the code `cancelled` and the session
locator. A cancelled compaction leaves the journal byte for byte unchanged. A
handler that cannot be installed fails the command with the code `interrupt`
before any model request.

The handler stays installed until the process ends, as it does for `run`.
`capabilities` reports `cancellable_adapters: ["lm_studio"]`.

### Desktop

No command or wire type changes. Every turn now hands over its token before
its first model request, so `chat_cancel` and exit stop a non-streamed request
and a pre-turn compaction as they already stopped a streamed request. A cancel
that arrives before the token is acted on when it is handed over, which ends
the turn before it is recorded.

## Alternatives considered

- **Keep the blocking client for non-streamed requests.** It cannot be woken
  while it waits, which is the case ADR-0050 was written for.
- **Add separate cancellable methods beside the old ones.** Two transports
  would have to be kept equal, and a caller could still choose the one that
  cannot be stopped.
- **Let the CLI receive the token through `on_cancel_token` and install the
  watcher there.** The callback cannot fail the command, so a handler that
  cannot be installed would be found only after the turn had started. With
  the token supplied up front the failure happens before any request.
- **Exit with the conventional status 130 after an interrupt.** Every other
  failure exits with status 1 and a JSON error. A caller tells a cancellation
  apart by the code `cancelled`.
- **Bridge the run's `CancelSignal` to the turn's token with another thread.**
  `watch_interrupt` is smaller and keeps one watcher per command.

## Consequences

- Every model request, streamed or not, and every compaction request can be
  cancelled, and stops within about the check interval.
- Ctrl-C during `chat` or `session compact` no longer leaves an open turn.
  Once the watcher is installed, a second interrupt does not end the process:
  the command ends when the request has stopped.
- Still not interruptible: a read, list, or edit tool call already in
  progress. A command tool call was already cancelled through its run.
- Each request now builds a client and a runtime. For a loopback server the
  cost is small next to a model request.
- The wire request is unchanged, but the non-streamed path has not been run
  against a live LM Studio server since this change.
- Not verified: an interrupt delivered to the CLI on Windows. The Windows
  handler is unit-tested by calling it directly, and the cancellation it sets
  is the one the Linux test delivers with `SIGINT`.

## References

- [Provider authentication](../provider-authentication.md)
- [Desktop interface](../desktop-interface.md)
- [ADR-0030](0030-run-trusted-commands-through-recorded-toolchain-profiles.md)
- [ADR-0041](0041-compact-active-context-with-readable-summaries.md)
- [ADR-0050](0050-interrupt-a-streamed-model-request-on-cancellation.md)
