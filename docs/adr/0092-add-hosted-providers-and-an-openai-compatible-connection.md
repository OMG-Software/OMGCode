# 0092: Add hosted providers and an OpenAI-compatible connection

- Status: Accepted
- Date: 2026-11-24

## Context

The user asked for these connections, beside LM Studio, Ollama, and Ollama
Cloud: Claude (Anthropic), OpenAI, Google, Meta, Moonshot, Tencent, Thinking
Machines Lab, MiniMax, OpenRouter, Z.ai, DeepSeek, xAI (Grok), and an "OpenAI
compatible" connection whose API address the operator types, for example
`https://provider-api.example.org/v1`.

Before this change:

- OMGCode had one model adapter, which speaks the OpenAI Chat Completions API.
  It served LM Studio, Ollama Local, and Ollama Cloud.
- `ModelEndpoint` admitted a loopback HTTP endpoint or the single fixed
  endpoint `https://ollama.com/v1` (ADR-0071). No rule admitted any other
  non-loopback address.
- The control journal already recorded the profile kinds `openai`,
  `anthropic`, and `open_router` (ADR-0038). Those kinds refused an endpoint,
  so a profile of those kinds could never be chosen.
- A credential is bound to one profile and sent only to that profile's
  recorded endpoint (ADR-0048).

Facts established on 2026-11-24 from each provider's public documentation and
unauthenticated requests. No API key was used for any provider.

| Provider | Base URL | Output limit field | Notes |
| --- | --- | --- | --- |
| Anthropic | `https://api.anthropic.com/v1` | `max_tokens` | Its OpenAI SDK compatibility layer is documented for testing, not production. Bearer authentication is accepted. |
| OpenAI | `https://api.openai.com/v1` | `max_completion_tokens` | `max_tokens` is deprecated and refused by reasoning models. |
| Google | `https://generativelanguage.googleapis.com/v1beta/openai` | `max_tokens` | The compatibility layer is beta. Model ids may carry a `models/` prefix. |
| Meta | `https://api.llama.com/compat/v1` | `max_tokens` | The Llama API is a preview with a waitlist. |
| Moonshot | `https://api.moonshot.ai/v1` | `max_completion_tokens` | Replies may carry `reasoning_content`, which is ignored. |
| Tencent | `https://api.hunyuan.cloud.tencent.com/v1` | `max_tokens` | No international endpoint is documented. |
| Thinking Machines Lab | `https://tinker.thinkingmachines.dev/services/tinker-prod/oai/api/v1` | `max_completion_tokens` | Beta and documented for testing. The only documented output field; unlisted fields are ignored, so `max_tokens` would leave output unbounded. No model list, tool calls, or streaming are documented. Model ids are checkpoint paths. |
| MiniMax | `https://api.minimax.io/v1` | `max_completion_tokens` | The recommended field. |
| OpenRouter | `https://openrouter.ai/api/v1` | `max_tokens` | Its unauthenticated model list returned 473 models in a 918 KB body. |
| Z.ai | `https://api.z.ai/api/paas/v4` | `max_tokens` | Some finish reasons are outside the OpenAI set. |
| DeepSeek | `https://api.deepseek.com` | `max_tokens` | The base URL has no `/v1`. |
| xAI | `https://api.x.ai/v1` | `max_completion_tokens` (assumed) | The model list returns 401 without a key. `[DONE]` and `tool_choice` are documented. The output field was not confirmed. |

## Decision

1. **New profile kinds.** `ProviderKind` gains `google`, `meta`, `moonshot`,
   `tencent`, `thinking_machines`, `minimax`, `z_ai`, `deepseek`, `xai`, and
   `openai_compatible`. The existing `openai`, `anthropic`, and `open_router`
   kinds now accept an endpoint. There are 16 kinds in total.
   `ProviderKind::ALL` lists them for the CLI: local kinds, named hosted
   providers, then `openai_compatible`. The desktop dropdown keeps its own
   order, with Ollama before LM Studio.
2. **One fixed endpoint per named provider.** Each named hosted kind accepts
   exactly the base URL in the table, or that text with one trailing `/`.
   Core holds one table (`provider/hosted.rs`), which the profile record,
   replay, the client, and the model list share. Any other text is refused
   with `this provider accepts only its one documented endpoint`. A regional
   or proxied endpoint for a named provider must use `openai_compatible`.
3. **A typed endpoint for `openai_compatible`.** The operator types the base
   URL. It must:
   - be at most 256 bytes and start with `https://`;
   - contain no whitespace, control character, `%`, `@`, `?`, `#`, or `\`;
   - parse as an HTTPS URL with a host that can be a base;
   - be written exactly as the URL parser writes it, apart from one trailing
     `/`;
   - contain no `//` in the path.

   An upper-case host, a default port, a `.` or `..` segment, or a non-ASCII
   host is refused rather than rewritten. The text the operator confirms is
   therefore the URL every request uses. Port 0, which no request can reach,
   is refused, as the loopback rule refuses it. A host on this computer (a
   loopback or unspecified address, including IPv4-mapped IPv6, `localhost`,
   or a `.localhost` name) is refused: a local server uses a local kind, and
   the hosted confirmation would otherwise claim that prompts leave this
   computer. An HTTPS-only local server therefore has no connection yet. A typed endpoint that equals a named
   provider's endpoint is treated as that provider's endpoint, including its
   output field.
4. **Every hosted kind needs an API key.** Hosted kinds take authentication
   `api_key` only. `openai` and `anthropic` still accept `subscription` so
   that older journals replay, but no adapter reads a subscription. A hosted
   request without a token is refused before any connection. A hosted
   profile can be chosen only with a bound credential
   (`a hosted provider profile needs a bound credential before it can be
   chosen`). `--endpoint` with a hosted URL and no profile is refused
   (`cloud_needs_profile`). A named hosted kind's API-key profile added
   without an endpoint records the provider's documented endpoint, as the
   desktop does, because a hosted profile without one could never be used and
   its endpoint cannot be added later. An `openai_compatible` API-key profile
   without one is refused. A subscription profile still records none, so its
   record stays readable by older versions.
5. **The output field is chosen per endpoint.** Requests send either
   `max_tokens` or `max_completion_tokens`, as the table says. A loopback
   endpoint, and a typed endpoint that is not a named provider's endpoint,
   send `max_tokens`; a typed endpoint equal to a named provider's endpoint
   sends that provider's field (decision 3).
6. **Paths are appended to the base.** `/chat/completions` and `/models` are
   appended to the base path, so DeepSeek's base without `/v1` and Google's
   `/v1beta/openai` base both work.
7. **Larger model lists.** A model list may hold 2048 models in a 4 MiB body,
   up from 512 models in 1 MiB, to admit OpenRouter's list.
8. **Desktop.** **Add connection** in the `settings` window lists every kind.
   A named hosted provider shows its fixed endpoint, which Rust supplies, and
   takes a key. `openai_compatible` takes an endpoint and a key. The window
   sends the key once and clears the field, as for Ollama Cloud (ADR-0071).
   The native confirmation before a hosted connection is chosen names the
   host, with any non-default port, that will receive prompts, workspace
   text read by tools, and command output. For `openai_compatible` it also says that the address was typed
   by the operator.
9. **CLI.** `capabilities` lists every kind except `ollama_local` in
   `stored_credential_adapters`, and every kind in `streaming_adapters` and
   `cancellable_adapters`. `provider_adapters` stays `false`.
10. **DeepSeek runs without thinking.** DeepSeek's documentation says
    thinking is enabled by default and that a request with `tools` must send
    the `reasoning_content` of all previous turns back, or the API answers
    400. The client discards reasoning text and the journals do not keep it,
    so every request to the DeepSeek endpoint, or to a typed endpoint equal
    to it, sends DeepSeek's documented `"thinking": {"type": "disabled"}`. No
    other endpoint is sent the field.

## Alternatives considered

- **Use Anthropic's native Messages API.** Rejected for this change. It would
  add a second adapter with its own request, stream, and tool-call shapes and
  their own parsers. The compatibility layer reuses the tested client. A
  native adapter can be a later decision if the compatibility layer proves
  insufficient.
- **Send both output fields in every request.** Rejected. DeepSeek and Z.ai
  document only `max_tokens`, and OpenAI's reasoning models refuse it, so no
  single request shape was documented as accepted by all of them.
- **Allow plain HTTP for a typed endpoint.** Rejected. The key and every
  prompt would cross the network in clear text. A loopback HTTP server uses a
  local kind.
- **Accept each provider's regional or alternative endpoints as fixed.**
  Rejected. Each extra fixed endpoint widens the set of hosts a named kind can
  reach without a typed address. `openai_compatible` covers them, and its
  confirmation says that the address was typed.
- **Normalise a typed endpoint.** Rejected. The URL a request uses would then
  differ from the text the operator confirmed.
- **Keep DeepSeek's reasoning and send it back.** Deferred. It would put model
  reasoning text into the session journals and every later request, which
  changes the session schema, the context budget, and what replay and
  compaction must treat as untrusted model output. That needs its own
  decision. Disabling thinking only for tool requests was also rejected: with
  tools, DeepSeek wants the reasoning of every earlier turn, including turns
  that had no tools.

## Consequences

- An operator can connect to 13 named hosted providers and any HTTPS
  OpenAI-compatible service through the one existing adapter.
- A turn on a hosted connection sends prompts, workspace text that tools read,
  and command output to that provider's host.
- A control journal that records a new kind, or an endpoint on an `openai`,
  `anthropic`, or `open_router` profile, cannot be read by 0.58.0 or earlier.
- Not verified:
  - No chat, stream, tool-call, or model-list request was made to any of these
    providers with a key. The reply shapes are assumed to match the
    OpenAI-compatible shapes the client already parses. No test makes a
    network call to a hosted provider.
  - Meta's Llama API is a waitlisted preview.
  - xAI's output field was not confirmed.
  - Moonshot documents passing `reasoning_content` back unchanged in tool
    loops for its thinking models, without documenting an error when it is
    missing. This client never sends it; a Moonshot tool turn may reason
    less well or fail, and only a request with a real key would show which.
  - Google model ids may carry a `models/` prefix; the list reports them as
    the server sends them.
  - A finish reason outside the OpenAI set is refused as a malformed reply,
    which ends the turn with a controlled error.
- Thinking Machines Lab documents no model list. The desktop model box chooses
  only from a listed model, so the desktop cannot choose a Tinker model until
  it accepts a typed model id. The CLI `chat --model` can name one.
- Anthropic's compatibility layer is documented for testing, not production.

## References

- [Provider authentication](../provider-authentication.md#hosted-providers-adr-0092)
- [Desktop interface](../desktop-interface.md#choosing-the-model-connection)
- [ADR-0038](0038-record-provider-profiles-without-credentials.md),
  [ADR-0048](0048-bind-a-stored-credential-to-its-profile-endpoint.md),
  [ADR-0066](0066-choose-the-chat-model-from-the-local-servers-model-list.md),
  [ADR-0071](0071-choose-the-provider-and-model-per-session-and-add-ollama-cloud.md)
