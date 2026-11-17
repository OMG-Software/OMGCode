# 0049: Stream local model replies and push chat events to the main window

- Status: Accepted
- Date: 2026-11-16

## Context

The local LM Studio adapter sent one non-streaming request and returned when
the whole reply had arrived. The desktop shell learned that a turn had ended by
calling `chat_status` every 750 ms, and showed nothing until then
(ADR-0045). The plan lists streaming and a push channel as open work in both
Phase D and Phase E.

These constraints shape the decision:

- **The journal is the record.** A reply is recorded only after the whole
  response passed the adapter's strict checks. Text shown earlier must not
  become a second source of truth.
- **Streamed text is untrusted and may hold the server token.** The whole-text
  redaction in `omgcode-app` cannot be applied to pieces as they are: a token
  can be split across two pieces.
- **The webview has no authority and no Tauri core permission** (ADR-0045).
  A push mechanism must not need one.
- **`omgcode-app` is front-end neutral.** It cannot depend on Tauri.
- **LM Studio's documentation** lists `stream` as a supported request field and
  describes tool calls arriving in pieces. It does not list `stream_options`.
- **No live LM Studio server was available** when this was built. The stream
  rules below are checked against local fixtures only.

## Decision

### Adapter: an opt-in streamed request

`LocalModelClient` gains `complete_streaming` and
`complete_with_tools_streaming`. Each sends the same body as its non-streaming
counterpart with `stream` set to `true` and an `Accept: text/event-stream`
header, and calls a caller-supplied function with each piece of assistant text
as it is accepted. It returns the same reply type as the non-streaming method.
`stream_options` is not sent. The non-streaming methods are unchanged and
remain the default.

The response is read as server-sent events under these rules. Any breach fails
the request; nothing is repaired.

- The status must be a success and the content type `text/event-stream`. A
  server that answers a streamed request with one JSON document is refused.
- The whole stream is bounded by the existing 1 MiB response limit and the
  existing thirty-second request deadline, which covers the body.
- Lines end with a line feed, optionally preceded by a carriage return. An
  empty line, a comment line (`:`), and the `event`, `id`, and `retry` fields
  are ignored. Any other line that is not a `data` field is refused.
- Each `data` line holds one payload. `[DONE]` ends the stream, and nothing
  after it is read. A stream that ends without `[DONE]` fails with a new
  `IncompleteStream` error. `[DONE]` before a finish reason is refused.
- Every other payload is parsed with the strict parser that refuses duplicate
  keys. It must be an object. A non-null `error` member is refused and its
  body is never reported.
- `choices` holds zero or one entry. A choice's `index`, when present, is 0.
  `delta.role`, when present, is `assistant`. `delta.content` is a string or
  null. A non-null `finish_reason` may appear once; after it only payloads
  without a choice are accepted. `usage`, when present on any payload, is
  parsed as before; absent usage stays absent.
- The accepted finish reasons are those of the non-streaming methods. The text
  method refuses any tool call. A reply cut short by the token limit cannot
  carry tool calls.
- Tool calls are assembled by `index`. Indexes start at 0, appear in order
  without gaps, and stay below 64. A call object may hold only `index`, `id`,
  `type`, and `function`; `function` only `name` and `arguments`. `id` is set
  once and may be repeated unchanged. `type`, when present, is `function`.
  Name and argument pieces are concatenated. A null `id`, `type`, `function`,
  `name`, `arguments`, or `delta.role` is treated as absent, because a null
  carries no data and servers send it for members a later piece does not
  repeat. The assembled value holds only
  the members that were received, so the existing tool-call validation applies
  to it exactly as to a non-streaming reply.

Text passed to the caller's function is provisional. If the stream fails later,
the request fails and the text already delivered is not a reply.

### Application layer: provisional text, redacted across pieces

`ChatHooks` gains `on_stream`. A turn sends streamed requests only when it is
set. It receives `RequestStarted` before each model request of the turn and
`Text` for each piece. Compaction requests are never streamed.

A stream redactor removes the known token from the pieces. It withholds the
longest tail that could be the start of the token until more text arrives, so
a split token is never emitted. If its output would itself contain the token,
it emits nothing further for that request. The final reply is still redacted
as a whole before it is recorded, as before. Streaming changes no journal
record: the journal does not say whether a reply was streamed.

### CLI: `chat --stream`

`omgcode chat --stream` writes the provisional text to standard error as it
arrives, through a streaming form of core's display-safe escaping, so model
output cannot send control sequences to a terminal. Standard output still
carries the one JSON result. Without the flag the request is not streamed.
`capabilities` reports `streaming_adapters: ["lm_studio"]`.

### Desktop: one caller-scoped channel

A new command, `chat_subscribe`, is allowed for `main` only. Its argument is a
Tauri IPC channel created by the caller. The service keeps at most one
subscriber; a new subscription replaces the old one and first replays the
preview of every running turn. Events carry a session identifier:

| Event | Meaning |
| --- | --- |
| `request_started` | A model request of the turn began. Earlier preview text is superseded |
| `delta` | Provisional assistant text, at most 1,024 bytes |
| `preview_truncated` | The 16 KiB preview bound was reached for this request |
| `finished` | The turn left `running`. The caller reads `chat_status` |

The service holds each running turn's preview (current request only, at most
16 KiB). Preview text is returned as received after token redaction, like
transcript text, and the frontend renders it as text. `finished` is sent by a
guard that also records a panicked turn as failed first, so the event is not
lost on a panic. The desktop streams by default; the launch argument
`--stream off` restores non-streaming requests, and `finished` is still
pushed.

The frontend subscribes once. It no longer polls `chat_status` every 750 ms.
It reads `chat_status` once when `finished` arrives and keeps a five-second
reconcile check while a turn runs, in case an event is lost. If subscribing
fails it falls back to the 750 ms poll. The approvals polls are unchanged.

A channel was chosen because it needs no capability beyond the command's own
permission: Tauri delivers small channel messages by evaluating a callback in
the calling webview, and its channel fetch command is exempt from the access
list. Events stay well under the 8,192-byte size at which Tauri switches to
that fetch.

## Alternatives considered

- **Tauri events (`emit` and `listen`).** They need a `core:event` permission
  in the capability file, which gives the webview a general listen and emit
  surface. ADR-0045 keeps both windows free of core permissions.
- **Return the partial text from `chat_status` and keep polling.** No new
  command, but text would arrive in 750 ms steps, and a faster poll costs a
  round trip per step.
- **Always stream.** A parser that has not met a live server would then gate
  every CLI turn. Streaming stays opt-in for the CLI and can be turned off at
  desktop launch.
- **Send `stream_options.include_usage`.** LM Studio does not document the
  field. Usage is taken when the server includes it and is otherwise absent.
- **Record in the request manifest that a reply was streamed.** It would make
  new journals unreadable by earlier versions and add nothing a replay uses:
  the recorded reply is the same text.
- **Escape preview text in the service.** Transcript text is returned as
  recorded and rendered as text; the preview follows the same rule so the two
  cannot differ in appearance when the turn ends.
- **Interrupt a request when a turn is cancelled.** A streamed read could
  check the cancel token between pieces. It changes what a cancelled turn
  records and is left for its own decision.

## Consequences

- The desktop shows a reply while it is generated, and learns of a turn's end
  without a fast poll. The command count becomes 19.
- A streamed text reply has no usage counters unless the server sends them.
- Preview text can differ from the recorded reply: a later failure discards
  it, and a reply over the 16 KiB session message limit is shown in part and
  then fails as before.
- The request deadline is still thirty seconds for the whole response, so a
  long generation fails as it did before.
- Cancellation is unchanged: cooperative, between rounds and tool calls.
- Not verified: the stream rules against a live LM Studio server, and the
  channel on Linux, where no window has been opened.

## References

- [Desktop interface](../desktop-interface.md)
- [Provider authentication](../provider-authentication.md)
- [ADR-0045](0045-build-the-desktop-shell-on-tauri-with-a-narrow-command-surface.md)
- [LM Studio: Chat Completions](https://lmstudio.ai/docs/developer/openai-compat/chat-completions)
- [LM Studio: tool use, streaming](https://lmstudio.ai/docs/developer/openai-compat/tools)
- [HTML Standard: server-sent events](https://html.spec.whatwg.org/multipage/server-sent-events.html)
