# 0097: Send a tool result as its call and output on the plan route

- Status: Accepted
- Date: 2026-11-24

## Context

ADR-0096 made ChatGPT text turns work and recorded that a tool turn did not
finish. After its first call the model received the result as
source-labelled user text, without its own call. It repeated the identical
`workspace_list_directory` call 58 times until the 120-second turn limit.

ADR-0093 rejected echoing `function_call` and `function_call_output` items.
It said a second history shape would change replay and compaction for one
provider. The turn runtime kept one shape for every adapter: role-tagged
text, with a tool result as `user` text.

The turn runtime already holds each call it dispatches. `ToolCall::parse`
validates the call, the journal records it as `tool.requested`, and the
dispatcher runs it before the result joins the turn's history. Within a turn,
the call that a result answers is known exactly.

## Decision

- A tool result in the live turn's history carries the call it answers. The
  conversation and model roles gain a `ToolResult` variant that holds an
  `AnsweredCall`: the call identifier, tool name, and argument JSON. The core
  creates one only after it parses, records, and dispatches that call. The
  constructor is crate-private, and the variant is never serialized or
  deserialized, so no text can create one.
- On the plan route, a request with tools sends each tool result as two
  items, in history order. The first is a `function_call` item that repeats
  the model's own call in the `omgcode` namespace. The second is a
  `function_call_output` item with the same `call_id`. Its `output` is the
  same source-labelled, untrusted text that was sent before.
- A plan-route request without tools sends a tool result as a `user` message
  item. No call item names a function that the request does not advertise.
- Chat Completions is unchanged. Its wire role for a tool result is still
  `user`, and the text is the same, so local servers and hosted providers
  receive the same bytes as before.
- Replay and compaction are unchanged. The session journal records no call
  with a tool message, so a replayed earlier turn's result is still `user`
  text. A withheld result is still the fixed withheld text, and the
  summarizer still reads replayed text.
- The plan route's context and request-size estimates count each answered
  call's three strings plus a fixed item overhead. The encoded body is still
  checked against the request byte limit before any token is read.

## Observed on 2026-11-24

One account, `gpt-6.1-sol`, through the trusted CLI, in a workspace with
read and list tools:

- A non-streamed turn asked for one directory listing. The model made one
  `workspace_list_directory` call, received the call and output items, and
  replied `3`, the number of entries the tool returned. The turn took two
  rounds and finished with `stop`.
- A streamed turn asked for a file read and a directory listing. The model
  made both calls in one response. It received two call and output pairs and
  answered correctly in the next round.
- The route accepted `namespace` on an input `function_call` item, and
  interleaved call and output pairs.

## Alternatives considered

- **Record the call identifier with every tool message and rebuild calls on
  replay.** Rejected for now. It changes the session schema and the replay
  and compaction paths for every provider, and a replayed turn has already
  ended with an assistant reply.
- **Group every call of a response before its outputs.** Rejected. The
  runtime adds results one at a time, and interleaved pairs were accepted.
- **Send native tool messages on Chat Completions too.** Rejected here. No
  loop was observed on those adapters, and the change would alter every
  local and hosted provider's request without a verified need.
- **Keep the call in a side list matched to message positions.** Rejected.
  Prompt assembly adds and reorders messages, so a position can drift from
  its result.

## Consequences

- ChatGPT tool turns finish; two were observed.
- On this route the result reaches the model as a tool output, not as `user`
  text. The text is still labelled as untrusted workspace data. Whether the
  model weighs a tool output less than user text was not tested.
- A tool result in the live turn no longer has the `User` role in memory.
  No production code compared a message's role with `User`; the runtime
  tests that did now require the `ToolResult` role and its exact call.
- Not verified: a refresh, a sign-out and its revocation, a failure or
  usage-limit code, a compaction request, a reasoning item, a later turn that
  replays results as `user` text, a write or command tool, and any model but
  `gpt-6.1-sol`.
- Tests cover the call and output items with tools, the `user` item without
  tools, the unchanged Chat Completions wire, refusal of every role spelling
  as input text, the runtime attaching the exact call, and an answered call
  counted against the context estimate. Before the change, the item test sent
  `user` text, the runtime test found no attached call, and the estimate test
  sent the request and failed with `Transport`.

## References

- [ADR-0093](0093-sign-in-with-chatgpt-and-use-the-plan-through-the-responses-api.md)
- [ADR-0096](0096-accept-a-plan-route-stream-without-a-content-type.md)
- [Models and inference](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference)
- [Provider authentication](../provider-authentication.md#chatgpt-sign-in-adr-0093)
