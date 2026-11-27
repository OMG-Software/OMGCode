# 0108: Report a refused reasoning effort by name

- Status: Accepted
- Date: 2026-11-27
- Amends: [ADR-0102](0102-choose-a-reasoning-effort-per-session.md), the consequence that a refused value arrives as the provider's HTTP status

## Context

ADR-0102 lets the operator choose any of seven reasoning efforts for a
session on a loopback server and leaves it to the server to refuse a value
its model does not take, on the grounds that the server's HTTP error names
the problem. The Chat Completions client never read the body of a
non-success response, so the operator saw only "local model server returned
HTTP status 400" and the journal recorded `model_error`.

On 2026-11-27 two desktop turns with the effort **Max** failed 7 ms and
15 ms after they started. The same request, sent by hand to LM Studio
0.4.28 with `google/gemma-4-e4b` loaded, returned 200 for no effort and for
`none`, `minimal`, `low`, `medium`, `high`, and `xhigh`, and returned this
for `max` within 7 ms:

```text
HTTP 400, Content-Type: application/json
{"error":{"message":"Invalid 'reasoning_effort' value: 'max'. Supported values: none, minimal, low, medium, high, xhigh.","type":"invalid_request_error","param":"reasoning_effort","code":"invalid_value"}}
```

OpenAI's official Python SDK reads `code`, `param`, and `type` from the
same `error` object of a failed response (`APIError` in
`src/openai/_exceptions.py`, read on 2026-11-27). OpenAI's API documentation
of the field was not checked. The ChatGPT plan route already reads at most
64 KiB of a failed body for a documented `error.code` and keeps nothing
else (ADR-0093).

## Decision

1. When a Chat Completions request that sent a reasoning effort gets a
   client-error (4xx) status, core reads the body through the reader the
   plan route uses, within the request's deadline and watching its cancel
   token, and keeps at most 64 KiB of it. The limit is checked as each
   piece arrives: the piece that passes it is dropped unkept and the
   failure keeps its status. Over HTTP/1, the only protocol the client
   speaks, one piece is one socket read, which hyper 1.12.0 caps at its
   417,792-byte read buffer, so that much can be received before the read
   stops.
2. Only `error.param` of one strict JSON value is examined, with duplicate
   keys refused at every depth. When it is exactly `reasoning_effort`, the
   failure is `ProviderError::EffortRefused` with the effort this client
   sent. Its message, "the model server refused the reasoning effort `max`;
   choose another effort or the default", is built from OMGCode's own closed
   effort list. No server text is shown, kept, or journaled.
3. Every other response keeps `ProviderError::HttpStatus`: a request without
   an effort, a server error (5xx), a body that names another parameter, is
   not strict JSON, or exceeds 64 KiB. The first two do not read the body, as
   before.
4. The application reports the failure with the specific code
   `effort_refused` in the `provider` category, beside the plan-route codes,
   on the CLI and in the desktop shell. The turn runtime still records the
   coarse `model_error`, as for every provider failure.

## Alternatives considered

- **Show the server's message.** It names the accepted values, but it is
  untrusted text from the server, and OMGCode does not echo provider text in
  errors (ADR-0093). The fixed message names the value the operator chose,
  which is what they change.
- **Offer only the values each server accepts.** ADR-0102 rejected
  per-provider lists because acceptance is per model. One LM Studio
  observation does not change that: another model, or a later version, may
  accept `max`. The error now says which value to change instead.
- **Map `max` to `xhigh` for loopback servers.** Rejected for ADR-0102's
  reason: a silently changed setting is worse than a refused one.
- **Read the body of every failed response.** It would read up to 64 KiB
  more on failures that cannot be about an effort. The read is limited to
  the case this decision reports.
- **Apply the same rule to the plan route.** Its refusal of an effort was
  not observed, and its failure reader already keys on documented codes.
  Left for an observed case.

## Consequences

- A refused effort on LM Studio, or on any Chat Completions server that
  names the parameter in `error.param` as OpenAI's SDK expects, tells the
  operator which value to change. Other refusals look as they did.
- A request with an effort that fails with 4xx now reads its body before
  returning, keeping at most 64 KiB and receiving at most one more piece of
  up to 417,792 bytes, within the same deadline and cancellation.
- The journal still records only `model_error`; the specific code appears
  only in the CLI and desktop error.
- Not observed: Ollama, Ollama Cloud, and every hosted provider's refusal of
  an effort, and LM Studio versions other than 0.4.28. A server that refuses
  an effort without `error.param` still shows the HTTP status alone.
- Tests: four request methods against a loopback fixture serving the
  recorded LM Studio body; the cases that keep the status; the body parser's
  edge cases; and the application code.

## References

- [ADR-0102](0102-choose-a-reasoning-effort-per-session.md)
- [ADR-0093](0093-sign-in-with-chatgpt-and-use-the-plan-through-the-responses-api.md), for the bounded error-body reader
- [OpenAI Python SDK `APIError`](https://github.com/openai/openai-python/blob/main/src/openai/_exceptions.py)
- [LM Studio 0.4.8 changelog](https://lmstudio.ai/changelog/lmstudio/lmstudio-v0.4.8)
