# 0096: Accept a plan-route stream without a Content-Type

- Status: Accepted, amended by [ADR-0097](0097-send-a-tool-result-as-its-call-and-output-on-the-plan-route.md)
- Date: 2026-11-24

## Context

ADR-0093 was built from OpenAI's documentation alone; no real Responses
request had been made. On 2026-11-24 an operator signed in with ChatGPT from
the desktop `settings` window, and the model box listed the plan's models
from `GET https://api.openai.com/v1/models`. Every chat turn of that session
then failed, and its journal recorded only `turn.failed` with `model_error`.

`omgcode chat --provider-profile` reproduced it with `gpt-6.1-sol` and
reported `invalid local model response` (`MalformedResponse`). A local
diagnostic build, not committed, copied the response head and body to a
file. The plan route answered `200` with **no `Content-Type` header**, and
its body was a well-formed server-sent-event stream: `event:` and `data:`
lines ending in LF, from `response.created` to `response.completed`. The
Responses exchange shared the Chat Completions head check (ADR-0049), which
accepts only `text/event-stream`, so it refused every plan-route reply before
reading its first byte. OpenAI's plan-route pages show a streamed request but
do not state the response's media type.

With that check skipped, the same build completed the turn, so nothing else
in the stream reader refused the real stream.

## Decision

- The plan route gets its own head check, `check_responses_head`. Its caller
  has already turned a non-success status into an error. An absent
  `Content-Type` is accepted. A value that names `text/event-stream`, in any
  case and with parameters such as `charset`, is accepted. Any other value,
  including an empty one, is still `MalformedResponse`.
- The Chat Completions streaming check is unchanged and still refuses an
  absent header, so local servers and hosted Chat Completions providers keep
  the ADR-0049 rule. Both checks share one media-type comparison.
- Nothing is repaired. The body must still pass the strict Responses reader,
  which refuses any line outside the server-sent-event fields and any ending
  but `response.completed`. A JSON document with no line ending ends as
  `IncompleteStream`, and a page as `MalformedResponse`.

## Observed on 2026-11-24

One account, `gpt-6.1-sol`, through the trusted CLI:

- A request with `instructions`, user message items, the `omgcode` namespace
  holding nine functions with `strict:false`, `tool_choice:"auto"`,
  `store:false`, and `stream:true` was accepted.
- The text reply's events were, in order: `response.created`,
  `response.in_progress`, `response.output_item.added`,
  `response.content_part.added`, `response.output_text.delta`,
  `response.metadata`, `response.output_text.done`,
  `response.content_part.done`, `response.output_item.done`, and
  `response.completed`. A message item carries `phase` (`final_answer`); a
  delta carries `logprobs`, `safety_buffering`, and `obfuscation`; usage
  carries `input_tokens_details`, `output_tokens_details`, and `attribution`.
  The completed response listed its output, and it matched the finished
  items. The existing reader accepts all of these; `response.metadata` is
  validated as JSON and skipped.
- A finished function call carried `type`, `id`, `call_id`, `namespace`
  (`omgcode`), `name`, `arguments`, and `status`, and the existing reader
  turned it into a call that `ToolCall::parse` accepted.
- **A tool turn did not finish.** After the first call the model received the
  result as source-labelled user text, as every adapter sends it, without its
  own call. It repeated the identical `workspace_list_directory` call 58 times
  until the 120-second turn limit ended the turn. The likely cause, inferred
  and not proven, is that the route's models expect a call and its output as
  `function_call` and `function_call_output` items, which ADR-0093 chose not
  to send. This record does not change that.

## Alternatives considered

- **Accept any `Content-Type` on the plan route.** Rejected. A header that
  names JSON or HTML says the body is not the stream the caller asked for.
- **Relax the shared check for every streamed request.** Rejected. Only the
  plan route was observed without the header; elsewhere an absent header
  remains a sign of a reply that is not a stream (ADR-0049).
- **Inspect the body's first bytes to decide.** Rejected. The strict reader
  already decides, and a second sniffing parser would have its own rules.

## Consequences

- A ChatGPT connection now completes text turns; this was observed.
- Tool turns on a ChatGPT connection can loop until the turn limit, as above.
  Fixing that needs a change to how the history carries tool calls and
  results, and a record of its own.
- Not verified: a refresh, a sign-out and its revocation, a failure event or
  usage-limit code, a compaction request, assistant-role input items, a
  reasoning item, and any model but `gpt-6.1-sol`.
- OpenAI asks clients to keep a failure's HTTP status, body shape, error
  code, and request ID. The client still keeps only a documented code, so the
  journal and the front ends report this kind of failure only as
  `model_error` and its provider category.
- Tests cover a stream with the observed event shapes and no header, refusal
  of a JSON, plain-text, or empty `Content-Type`, and a JSON or HTML body
  without the header. The new no-header test failed before this change with
  `MalformedResponse`.

## References

- [ADR-0049](0049-stream-local-model-replies-and-push-chat-events.md)
- [ADR-0093](0093-sign-in-with-chatgpt-and-use-the-plan-through-the-responses-api.md)
- [Models and inference](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference)
- [Errors and recovery](https://developers.openai.com/siwc/token-sharing-open-source/errors-and-recovery)
- [Provider authentication](../provider-authentication.md#chatgpt-sign-in-adr-0093)
