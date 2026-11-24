# Provider authentication

## Release requirements

The first release must support subscription-backed sign-in for both ChatGPT and
Claude, as well as separate API-key connections. An API-key-only implementation
does not meet this requirement.

Subscription support is a product requirement, not a claim of implementation or
provider approval. The current prototype has no sign-in, account management, or
subscription adapter. Any unresolved provider approval or technical restriction
remains a release blocker for that feature; do not silently replace it with API
billing or defer it.

| Connection profile | Required authentication and usage path | Current design readiness |
| --- | --- | --- |
| OpenAI API | User API key; API-account billing | Prototype through the Chat Completions adapter at the fixed endpoint `https://api.openai.com/v1` (ADR-0092); API key only; no request was verified with a real key |
| ChatGPT subscription | Provider-supported browser sign-in and permission to use the user's plan | Prototype ([ChatGPT sign-in](#chatgpt-sign-in-adr-0093)): browser sign-in with PKCE, state, nonce, and ID-token checks, tokens in protected OS storage, and chat through the Responses API, tested only against loopback mock servers; no real sign-in or plan-backed request was made, so plan usage is not verified and the requirement stays open |
| Anthropic API | User API key; API-account billing | Prototype through Anthropic's OpenAI SDK compatibility layer at `https://api.anthropic.com/v1` (ADR-0092), which Anthropic documents for testing, not production; API key only; no request was verified with a real key |
| Claude subscription | Provider-approved subscription sign-in and usage | Required; approval and integration route unresolved |
| Ollama Local | Explicit local server profile; no token | Local loopback chat prototype through Ollama's OpenAI-compatible `/v1` API, tested with Ollama 0.40.16 on Windows (0.44.0, ADR-0064); an `ollama_local` profile records the endpoint, has authentication `none`, and can be chosen in the desktop `settings` window; authenticated Ollama servers and platform qualification pending |
| Ollama Cloud | Provider cloud credentials | Direct connection to the fixed endpoint `https://ollama.com/v1` with a stored API key bound to an `ollama_cloud` profile (ADR-0071); the model list shape was observed, the chat, stream, and tool-call shapes were not verified with a real key |
| OpenRouter | User API key and explicit upstream routing | Prototype at `https://openrouter.ai/api/v1` (ADR-0092); the model list shape was observed without a key; upstream routing preferences are not sent; API key only; no request was verified with a real key |
| Google, Meta, Moonshot, Tencent, Thinking Machines Lab, MiniMax, Z.ai, DeepSeek, xAI | User API key; API-account billing | Prototype at each provider's one documented OpenAI-compatible endpoint ([hosted providers](#hosted-providers-adr-0092)); API key only; no request was verified with a real key |
| OpenAI compatible | User API key for an operator-typed HTTPS base URL | Prototype ([hosted providers](#hosted-providers-adr-0092)); the service must speak the Chat Completions shapes the adapter parses |
| LM Studio | Explicit local server profile; LM Studio API token when required | Local loopback chat prototype tested with Gemma on Windows; a token can be stored as an `api_key` credential, and the adapter reads it through an operator-selected profile (0.30.0), which also supplies the endpoint (0.31.0); full platform/auth qualification pending |

The provider, authentication method, account/workspace, billing mode, and model
capabilities form one connection profile. Do not infer all of these from a
provider name.

## Implemented prototype (v0.1.0)

The current CLI has one local Chat Completions adapter. It serves LM Studio
and, since 0.44.0, Ollama Local, whose `/v1` API returns the same reply,
tool-call, and stream shapes ([Ollama Local](#ollama-local-0440)). It sends
non-streaming requests by default and streamed requests on request
([streamed replies](#streamed-replies-0320)). The
caller supplies an already available model name, which `omgcode models` and
the desktop's model choice can list ([model listing](#model-listing-0460)),
a maximum output budget, and a
context budget with `--context-tokens`. For example, the CLI test fixture passes
131072 as its configured context budget. This value is caller input; the
prototype does not discover or verify the model's actual capacity. The adapter
estimates input use from UTF-8 bytes, message count, fixed overhead, and output
budget. It does not use the model's tokenizer or perform compaction.

The adapter accepts only plain HTTP URLs with a literal loopback IP, an explicit
nonzero port, and a `/v1` path, for example `127.0.0.1` or `[::1]`. It rejects hostnames,
non-loopback addresses, credentials in the URL, query strings, and fragments.
The request uses `/v1/chat/completions`, disables proxy use and redirects, and
does not retry. It sends no tools and rejects tool-call responses. The adapter
does not discover models, start the LM Studio service, download models, or
configure that service.

The CLI accepts an optional `OMGCODE_LM_TOKEN` in its own process
environment. The core keeps the resulting authorization header in memory and
does not add the token to a request manifest. With `--provider-profile` the
adapter reads the stored credential described below instead, and the variable
is not read. The profile also supplies the endpoint (0.31.0). There is no token settings UI. The variable is a prototype-only
developer input. Task execution
and task environments do not exist in the prototype; this does not qualify the
future task-environment credential boundary.

The CLI redacts exact occurrences of that currently configured token from new
user text, preferences, titles, and assistant text before retention or display.
Both chat-created sessions and direct `session new` use the same title redaction.
It also redacts the current token from historical text before sending context.
In tool-enabled chat, a tool call whose identifier or arguments contain the
token is refused before it is recorded, because redacting its arguments could
change the requested path. This is known-token redaction, not general secret
detection or retroactive cleanup of existing journals. Ordinary string storage does not guarantee erasure.

The adapter returns assistant text, `finish_reason`, and optional provider usage
to the CLI, which emits them in JSON output. Tool-enabled chat emits the final
reply's `finish_reason` but no usage. The session journal stores the
assistant text but not provider usage or `finish_reason`. Loopback identifies
the network destination used by the adapter; it does not prove that the selected
LM Studio server or model avoids upstream/cloud services.

On 2026-11-08, a Windows smoke check used the user's loaded
`google/gemma-4-e4b` Q4_K_M model at `http://127.0.0.1:1234/v1`, without a token.
LM Studio reported a loaded context of 131,072 tokens. Two bounded CLI requests
completed: the second resumed the saved history and recalled a harmless test
word. Archive and restore preserved that journal. This checks that one live
profile; it does not qualify token authentication, streaming, Linux inference,
all models, or the full release contract. The adapter accepts empty/null action
fields as no action and still rejects actual tool requests.

These are prototype facts. The rest of this document defines the broader
authentication and provider requirements for release.

## Current provider evidence

Checked against official documentation on 2026-11-08. Recheck before release;
provider availability and requirements can change.

### ChatGPT

OpenAI documents Sign in with ChatGPT plan usage for local and open-source apps.
Identity sign-in and permission to consume plan usage are separate.
[OpenAI integration overview](https://developers.openai.com/siwc/token-sharing-open-source)

Use the documented browser authorization flow with PKCE, state, nonce,
loopback-callback validation, and validation of the returned identity and scopes.
Persist the issued account/workspace registration and installation host identity
as specified by OpenAI. Do not borrow another application's OAuth client
identity or scrape an existing login cache.
[Registration and sign-in](https://developers.openai.com/siwc/token-sharing-open-source/sign-in)

The currently documented plan route has a restricted Responses API surface,
including streamed requests with storage disabled and limits on request fields,
conversation state, and hosted tools. Treat this as a distinct adapter capability
profile from API-key access.
[Preview limitations](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations)

A completed sign-in is not evidence that the selected model request can consume
subscription usage. Verify granted plan permission and a permitted completed
request. Recheck eligibility and limits rather than promising every ChatGPT
plan or model. Review integration eligibility again if distribution changes
from the documented local/open-source model.

### Claude

Anthropic documents subscription login for its own Claude Code product.
That does not by itself establish permission for OMGCode to expose the same
login or usage allowance.
[Claude Code authentication](https://code.claude.com/docs/en/authentication)

The Agent SDK documentation currently requires prior Anthropic approval for a
third-party product to offer claude.ai login or subscription usage limits.
No such approval has been established for OMGCode.
[Agent SDK integration restriction](https://code.claude.com/docs/en/agent-sdk/overview)

Obtain approval or an explicit supported integration route covering OMGCode
before calling this feature available. A CLI or SDK wrapper alone is not
evidence of that approval. Keep the requirement open and the blocker visible;
a separately billed Anthropic API connection remains a different profile.

## Recorded provider profiles (0.24.0)

`workspace provider-profile-add/show/remove` record a name, a provider kind,
and an authentication mode. Kinds are `lm_studio`, `ollama_local`,
`ollama_cloud`, `anthropic`, `openai`, `open_router`, and, since ADR-0092,
`google`, `meta`, `moonshot`, `tencent`, `thinking_machines`, `minimax`,
`z_ai`, `deepseek`, `xai`, and `openai_compatible`. Modes are
`subscription`, `api_key`, and `none`. Subscription is accepted only for
OpenAI and Anthropic. Since ADR-0093 an `openai` subscription profile with
OpenAI's fixed endpoint can sign in with ChatGPT
([below](#chatgpt-sign-in-adr-0093)); no adapter reads an `anthropic` one. API keys are accepted for LM
Studio and every hosted kind. Since ADR-0071 an `ollama_cloud` profile also
accepts its one fixed endpoint, and since ADR-0092 every hosted kind accepts
one ([hosted providers](#hosted-providers-adr-0092)). Ollama Local accepts
only `none`.

A profile stores no account, token, password, certificate, or other
credential. Since 0.31.0 it can record one endpoint (below). An endpoint is not
a secret. Since 0.27.0 its status can name a bound credential's opaque
identifier and backend (below). It is configuration only: no login,
entitlement check, billing mode, or model context uses it. Since 0.30.0 the
local LM Studio adapter can read a bound credential of an `lm_studio` profile
that the operator selects (below). `capabilities` reports
`provider_profiles: true`, `provider_adapters: false`, and
`stored_credential_adapters` (every kind except `ollama_local` since
ADR-0092), and, since 0.31.0, `provider_profile_endpoints: true`. The
provider-specific login approvals remain prerequisites for any subscription
connection. See [ADR-0038](adr/0038-record-provider-profiles-without-credentials.md).

Since 0.31.0 `provider-profile-add` accepts `--endpoint <url>` (ADR-0048). An
`lm_studio` profile accepts one, and since 0.44.0 an `ollama_local` profile
does too (ADR-0064). It must pass the rule the local adapter
applies to every request: plain `http`, a literal loopback IP, an explicit
non-zero port, the path `/v1` or `/v1/`, no user information, query, or
fragment, and at most 256 bytes. Core has one implementation of that rule, and
both the provider client and the profile record use it. Replay of the control
journal applies it again. An `ollama_cloud` profile accepts only the fixed cloud endpoint (Ollama Cloud, below). Since ADR-0092 every other hosted kind
accepts its own rule ([hosted providers](#hosted-providers-adr-0092)). The
text is stored exactly as given, without normalisation. The refusals are
`provider profile refused: a local provider endpoint must be http://<loopback
IP>:<port>/v1 with no user information, query, or fragment`, `provider
profile refused: the provider endpoint is longer than 256 bytes`, `provider
profile refused: this provider accepts only its one documented endpoint`, and
the `openai_compatible` refusal described there.

`provider-profile-show` and `status` print an `endpoint` field only when the
profile records one. A profile without one prints as before. The endpoint is
fixed for the profile's life, and no command changes it. To use another
endpoint, remove the profile and add a new one.

The record's `endpoint` field is written only when present, so a control journal
without endpoints replays and serializes with its exact bytes. A journal that
contains an endpoint cannot be read by 0.30.0 or earlier, which reject the
unknown field. Replay rejects an invalid endpoint, an endpoint on a kind that
does not accept one, and any unknown field. A hand-edited control journal is
checked only by this replay validation. It is not tamper-evident.

## Stored provider credentials (0.27.0)

Trusted `workspace provider-credential-set <name> --revision <n>` reads an API
key from standard input and stores it in protected OS storage: a DPAPI blob for
the current user in a verified-ACL private directory on Windows, or one item in
the Secret Service default collection on Linux. Nothing falls back to plaintext.
A credential is immutable and has a fresh random identifier. The control
journal records `provider_credential_bound` with the profile, that opaque
identifier, and the backend; it never records the secret. Rotation stores a new
credential, records the binding, and then deletes the old item. An interrupted
set either leaves a valid binding or names the identifier to reconcile: when
the binding's journal write fails after it began, the new item is kept and
reported as `credential_binding_uncertain`, so a binding never names an item
OMGCode deleted. On Linux OMGCode never runs a Secret Service prompt; a request
that needs one is dismissed and refused. The secret reaches the service only
through the encrypted session, after an empty placeholder item is created.
`provider-credential-remove` and `provider-profile-remove` record the unbinding
before deleting the native item. Output, `status`, and `capabilities` report
only identifiers, backends, and store state. See
[ADR-0042](adr/0042-store-provider-credentials-in-protected-os-storage.md) and
[development commands](development.md#provider-credentials).

Only `api_key` profiles accept a credential through these commands. Since
ADR-0093 a ChatGPT sign-in stores its own token record
([below](#chatgpt-sign-in-adr-0093)); login codes and ID tokens are never
stored, and Claude subscription sign-in remains blocked as described above. Until 0.30.0 no adapter read a stored credential. Now only
the local LM Studio adapter does (next section). A memory-only session
credential is not implemented. Any process running as the same user can read
these credentials.

## Ollama Local (0.44.0)

[ADR-0064](adr/0064-choose-the-model-connection-in-settings-and-add-ollama-local.md)
connects the local adapter to Ollama. A probe of Ollama 0.40.16 on Windows
with `lfm2.5:latest` and `gemma4:e2b` showed that
`http://127.0.0.1:11434/v1/chat/completions` returns the Chat Completions
shapes the client already parses. The captured replies, tool calls, and
streams are core test fixtures.

- **Profile.** `omgcode workspace provider-profile-add <name> --kind
  ollama_local --auth none --endpoint http://127.0.0.1:11434/v1 --revision
  <n>`, or **Add a local connection** in the desktop `settings` window. The
  endpoint follows the same loopback rule as LM Studio's. An `ollama_local`
  profile accepts only authentication `none`, so no credential is bound and
  no `Authorization` header is sent.
- **Use.** `omgcode chat --provider-profile <name>` and `omgcode-desktop
  --provider-profile <name>` use it for one command or one process. In a
  desktop shell started without a connection argument, the `settings` window,
  the `main` window's model box in the side panel footer (each after a native confirmation), or
  `omgcode workspace provider-profile-select <name>` makes it the active
  connection for later turns; `--default` returns to the default endpoint.
  CLI `chat` does not read the active connection.
- **Context length.** The OpenAI-compatible API cannot set Ollama's context
  length. `--context-tokens` and the composer's context size are a
  client-side budget, so set the server's length in Ollama, for example with
  `OLLAMA_CONTEXT_LENGTH`, to at least that value.
- **Not covered.** Ollama's native `/api` endpoints, model download,
  starting the server, and an Ollama server behind authentication. Ollama
  Cloud is a separate connection (see Ollama Cloud below). Model listing uses `/v1/models`.

## Model listing (0.46.0)

[ADR-0066](adr/0066-choose-the-chat-model-from-the-local-servers-model-list.md)
lets the operator choose a model the server reports instead of typing its id.
LM Studio and Ollama both document `GET /v1/models` in their
OpenAI-compatible APIs, and both returned
`{"object":"list","data":[{"id":...}]}` on the development host on
2026-11-19.

- **Request.** `omgcode models [--endpoint URL | --provider-profile NAME]`
  and the desktop `chat_models` command resolve the connection as `chat`
  does and send one `GET` for `/models` appended to the endpoint's base path.
  For a local server that is `GET /v1/models`, and the endpoint passes the
  same loopback rule. For a hosted profile (since ADR-0092) the endpoint
  passes its hosted rule instead, so DeepSeek is asked at
  `https://api.deepseek.com/models` and Z.ai at
  `https://api.z.ai/api/paas/v4/models`; a hosted request needs the
  profile's stored key. A stored credential is sent as the same bearer
  header. No proxy, redirect, or retry; a five-second connect timeout, a
  ten-second deadline, and a 4 MiB body (1 MiB before ADR-0092).
- **Response.** Parsed strictly: a `data` array of at most 2048 objects (512
  before ADR-0092) with
  string ids, no duplicate key, and no repeated id. An id that is empty, over
  256 bytes, or holds a control or bidirectional formatting character
  refuses the whole list. Other members are ignored. Failures are the
  `provider` errors of a chat request.
- **Cloud tag.** Ollama lists models that run on its hosted service with a
  `:cloud` tag, or a tag ending in `-cloud`. Such an id is reported with
  `cloud_tag: true` and labelled in the desktop. The tag is a naming
  convention: `/v1/models` does not say where a model runs, and Ollama's
  native `/api/tags`, which does, is not read.
- **Not covered.** Loaded state, model type, and context length, which
  `/v1/models` does not report. Embedding models are listed.

`capabilities` reports `streaming_adapters` and `cancellable_adapters` as
every provider kind since ADR-0092 (`["lm_studio", "ollama_local",
"ollama_cloud"]` since ADR-0071), and `stored_credential_adapters` as every
kind except `ollama_local`.

## Ollama Cloud (ADR-0071)

[ADR-0071](adr/0071-choose-the-provider-and-model-per-session-and-add-ollama-cloud.md)
adds a direct connection to Ollama's hosted service. It reuses the local
adapter's Chat Completions client, stream parser, cancellation, and model list.

- **One endpoint.** An `ollama_cloud` profile accepts exactly
  `https://ollama.com/v1`, or the same text with one trailing `/`, and nothing
  else: no other host, port, path, user information, query, fragment, scheme,
  or letter case. It still takes authentication `api_key` only, and a profile
  without an endpoint cannot be used (`endpoint_not_recorded`). Core has one
  implementation of the rule (`OLLAMA_CLOUD_ENDPOINT`, `ModelEndpoint`), which
  the profile record, replay, the client, and the model list share. Since
  ADR-0092 the other hosted kinds have their own rules
  ([hosted providers](#hosted-providers-adr-0092)). `omgcode workspace
  provider-profile-add NAME --kind ollama_cloud --auth api_key --endpoint
  https://ollama.com/v1 --revision N` records one.
- **Credential.** `provider-credential-set` stores the key and binds it as for
  `lm_studio`. `provider_credential_binding` and `load_provider_credential`
  take `ollama_cloud` as the adapter and return the recorded endpoint with
  the credential decision, so the key goes only to the recorded endpoint
  (ADR-0048). `--provider-profile NAME` selects the profile for `chat`,
  `session compact`, `models`, and the desktop launch. `OMGCODE_LM_TOKEN` is
  never read for it, and `--endpoint https://ollama.com/v1` without a profile
  is refused (`cloud_needs_profile`).
- **Desktop key entry.** The desktop `settings` window can add an
  `ollama_cloud` profile and its key in one step (**Add connection**, Ollama
  Cloud) and replace the key later (**Set key…**), as the write-only field of
  ADR-0059 decision 6 describes. The window sends the key once, in
  `settings_add_connection` or `settings_set_api_key`, and clears the field.
  Rust moves it into the zeroing `Secret`, applies the same size bound as the
  CLI (16 KiB, not empty; the desktop also refuses a control character), and
  uses the same core calls: `add_provider_profile`, then
  `set_provider_credential`, which stores the item, binds it, and deletes a
  replaced item. The profile must exist before it can be bound, so Rust opens
  the native store first and, if storing or binding fails, removes the
  profile again. The endpoint is not typed: Rust supplies
  `OLLAMA_CLOUD_ENDPOINT`. No response or error carries the key, its length,
  or a digest. The key passes through the `settings` webview's memory and one
  IPC message, which is a weaker boundary than the CLI's standard input.
  Removing the profile in the window asks a native Yes/No that says the stored
  key is deleted, then removes the profile and deletes the item as `provider-profile-remove`
  does. Replacing a key shows no dialog, because the audience is unchanged.
  See the [desktop interface](desktop-interface.md#choosing-the-model-connection).
- **Transport.** HTTPS only, with the platform's certificate validation
  (`native-tls`); no proxy, redirect, or retry; the same timeouts and body
  limits as a local request. A request to the cloud endpoint without a token
  is refused before any connection (`invalid local model token`).
- **Selectable.** A profile can be the active connection when it is a local
  kind with an endpoint, or a hosted profile (since ADR-0092, any hosted kind)
  with its endpoint and a bound credential
  (`ControlStore::check_provider_selectable`). Replay
  checks the endpoint rule only, because it cannot read the credential store.
- **Off-machine data.** A turn on this connection sends prompts, workspace
  text that tools read, and command output to ollama.com.
- **Not verified.** The model list shape was observed on 2026-11-20 without a
  key. No chat, stream, or tool-call request was made to ollama.com, because
  no key was available; those shapes are assumed to match the local Ollama
  server's. No test makes a network call to ollama.com.
- **Not covered.** Ollama's native `/api` endpoints, account sign-in, usage
  limits and billing.

## Hosted providers (ADR-0092)

[ADR-0092](adr/0092-add-hosted-providers-and-an-openai-compatible-connection.md)
connects the same Chat Completions client to twelve more hosted services and to
any HTTPS service that speaks the same API. Each works as Ollama Cloud does
(above): an `api_key` profile, a stored credential bound to it, the key sent
only to the recorded endpoint, HTTPS with the platform's certificate
validation, no proxy, redirect, or retry, and the same timeouts and limits.

| Kind | Fixed endpoint | Output limit field |
| --- | --- | --- |
| `anthropic` | `https://api.anthropic.com/v1` | `max_tokens` |
| `openai` | `https://api.openai.com/v1` | `max_completion_tokens` |
| `google` | `https://generativelanguage.googleapis.com/v1beta/openai` | `max_tokens` |
| `meta` | `https://api.llama.com/compat/v1` | `max_tokens` |
| `moonshot` | `https://api.moonshot.ai/v1` | `max_completion_tokens` |
| `tencent` | `https://api.hunyuan.cloud.tencent.com/v1` | `max_tokens` |
| `thinking_machines` | `https://tinker.thinkingmachines.dev/services/tinker-prod/oai/api/v1` | `max_completion_tokens` |
| `minimax` | `https://api.minimax.io/v1` | `max_completion_tokens` |
| `open_router` | `https://openrouter.ai/api/v1` | `max_tokens` |
| `z_ai` | `https://api.z.ai/api/paas/v4` | `max_tokens` |
| `deepseek` | `https://api.deepseek.com` | `max_tokens` |
| `xai` | `https://api.x.ai/v1` | `max_completion_tokens` |
| `openai_compatible` | typed by the operator | `max_tokens`, unless the typed address is a named provider's endpoint, which sends that provider's field (below) |

- **Fixed endpoints.** A named kind accepts exactly its endpoint, or that text
  with one trailing `/`. Core has one table (`provider/hosted.rs`) that the
  profile record, replay, the client, and the model list share. Requests
  append `/chat/completions` or `/models` to the base path. A regional or
  proxied endpoint for a named provider must be added as `openai_compatible`.
- **Typed endpoint.** An `openai_compatible` endpoint must be at most 256
  bytes and start with `https://`; contain no whitespace, control character,
  `%`, `@`, `?`, `#`, or `\`; parse with a host; be written exactly as the URL
  parser writes it, apart from one trailing `/`; and have no `//` in its path.
  An upper-case host, a default port, a `.` or `..` segment, or a non-ASCII
  host is refused, not rewritten, so the confirmed text is the URL every
  request uses. Port 0, which no request can reach, is refused too, as the
  loopback rule refuses it. The refusal is `provider profile refused: an
  openai_compatible endpoint must be an https:// base URL with a lower-case
  host and no user information, query, fragment, percent-encoding, default
  port, or empty or dot path segment`. Plain HTTP is refused; a loopback HTTP
  server uses a local kind. A typed endpoint equal to a named provider's
  endpoint uses that provider's output field.
- **CLI.** `omgcode workspace provider-profile-add NAME --kind deepseek --auth
  api_key --endpoint https://api.deepseek.com --revision N`, then
  `provider-credential-set NAME`. `--provider-profile NAME` selects it for
  `chat`, `session compact`, `models`, and the desktop launch. `--endpoint`
  with a hosted URL and no profile is refused (`cloud_needs_profile`).
  `--endpoint` may be left out for a named hosted kind with `--auth api_key`:
  core records that provider's documented endpoint, as the desktop does, so
  no unusable profile is recorded. An `openai_compatible` API-key profile
  without one is refused (`an openai_compatible profile needs an endpoint`).
  A subscription or local profile added without one still records none.
- **Desktop.** **Add connection** in the `settings` window lists every kind.
  A named hosted provider shows its fixed endpoint, which Rust supplies, and
  takes an API key; `openai_compatible` takes an endpoint and an API key.
  The key is sent once and the field cleared, as for Ollama Cloud. The native
  confirmation before choosing a hosted connection names the host, with any
  non-default port, that receives prompts, workspace text read by tools, and
  command output; for `openai_compatible` it adds that the address was typed when the connection
  was added. See the [desktop interface](desktop-interface.md#choosing-the-model-connection).
- **Selectable.** A hosted profile can be chosen only with a bound credential.
- **Compatibility.** A control journal that records a new kind, or an endpoint
  on an `openai`, `anthropic`, or `open_router` profile, cannot be read by
  0.58.0 or earlier.
- **Not verified.** Endpoints and output fields come from each provider's
  documentation, read on 2026-11-24. No request was made to any of these
  providers with a key, and no test makes a network call to one. The reply,
  stream, and tool-call shapes are assumed to match the OpenAI-compatible
  shapes the client parses. A finish reason outside the OpenAI set is refused
  as a malformed reply. Moonshot documents passing `reasoning_content` back
  unchanged in tool loops for its thinking models, without documenting an
  error when it is missing; this client never sends it. Meta's Llama API is a waitlisted preview. xAI's output
  field was not confirmed. Anthropic and Google document their compatibility
  layers as for testing and beta respectively.
- **DeepSeek thinking.** DeepSeek enables thinking by default and documents
  a 400 for a tool request that does not send every earlier turn's
  `reasoning_content` back. This client keeps no reasoning text, so every
  request to the DeepSeek endpoint sends `"thinking": {"type": "disabled"}`.
  DeepSeek turns therefore run without thinking.
- **Not covered.** Native provider APIs (for example Anthropic Messages),
  subscription sign-in (ChatGPT sign-in came later, with ADR-0093),
  provider-specific parameters such as OpenRouter
  routing or reasoning settings, and reasoning text, which is ignored.
  Thinking Machines Lab documents no model list, and the desktop model box
  chooses only a listed model, so a Tinker model can be named only with CLI
  `chat --model`.

## ChatGPT sign-in (ADR-0093)

An `openai` profile with authentication `subscription` and the fixed endpoint
`https://api.openai.com/v1` is a ChatGPT connection. It follows OpenAI's
documented route for local and open-source apps
([ADR-0093](adr/0093-sign-in-with-chatgpt-and-use-the-plan-through-the-responses-api.md)).

- **Sign-in.** `omgcode workspace provider-signin <name> --revision <n>` or
  **Sign in with ChatGPT** in the desktop `settings` window. Rust binds a
  listener on `127.0.0.1`, opens OpenAI's authorization page in the system
  browser with PKCE S256, a random `state` and `nonce`, and a per-host
  `urn:uuid:` identifier, and waits at most five minutes for
  `/auth/callback`. The first sign-in uses OpenAI's dynamic client and records
  the issued `client_id`; a later one reuses it and must return the same
  account. The callback's `state`, client, and parameters are checked; the ID
  token's RS256 signature, issuer, audience, expiry, nonce, and subject are
  validated against OpenAI's published keys; and the granted scopes must
  include `chatgpt.tokens.use.direct`. Without that scope nothing is stored
  (`plan_usage_not_granted`). Ctrl-C on the CLI and **Cancel sign-in** in the
  window end the wait. No other application's client or login cache is used.
- **Storage.** The access and refresh tokens are kept in protected OS storage
  (DPAPI on Windows, the Secret Service on Linux) as a record in one of two
  immutable slots, never in a file, journal, log, error, or the webview. The
  control journal records `provider_subscription_bound` with the client, the
  subject, the account email as a display label, the host identifier, and the
  binding.
- **Refresh.** A request refreshes the token when fewer than five minutes
  remain, under a per-session OS file lock, and stores the rotated record
  before the lock is released. A refused refresh is `signin_required`. A crash
  between OpenAI rotating the token and the store write loses the session.
- **Sign-out.** `omgcode workspace provider-signout <name> --revision <n>` or
  **Sign out** revokes the refresh token once, records the sign-out, and
  deletes both slots. An unconfirmed revocation is reported and not retried.
  A signed-in profile cannot be removed, and `provider-credential-remove`
  refuses every subscription profile.
- **Requests.** Chat, compaction, and tool turns use the Responses API at
  `https://api.openai.com/v1/responses` with `store:false` and `stream:true`.
  The model list reads the plan route's `models` array. Plan failures are
  reported by code, such as `usage_limit_exceeded`, and never fall back to
  API-key billing. API-key `openai` profiles keep the Chat Completions path.
- **Not verified.** No test contacts OpenAI. No real sign-in, refresh,
  revocation, model list, or Responses request was made, and a completed
  sign-in is not evidence that a plan, model, or account may be used. The
  [acceptance evidence](#acceptance-evidence) below has not been gathered, so
  the ChatGPT subscription requirement remains open.

## Stored credential use (0.30.0)

[ADR-0047](adr/0047-read-the-local-provider-token-from-the-credential-store.md)
lets the local LM Studio adapter take its optional server token from a stored
credential. In 0.30.0 it was the only adapter that read one; an
`ollama_cloud` profile (ADR-0071) and every hosted profile (ADR-0092) now
read one too, through the same profile selection.

- **Selection.** The operator names a profile with `omgcode chat
  --provider-profile <name>` or `omgcode session compact --provider-profile
  <name>`. `omgcode-desktop --provider-profile <name>` selects it for the whole
  process at launch. Without a launch argument, the desktop `settings` window
  can choose a profile as the active connection after a native confirmation
  (ADR-0064). The `main` webview cannot choose or see it. With a profile
  selected, `OMGCODE_LM_TOKEN` is not read and the endpoint comes from the
  profile, not from `--endpoint` (see
  [endpoint binding](#endpoint-binding-0310)). Without one, behaviour is
  unchanged: `--endpoint` and its default, and the variable as the prototype
  input. Two sources are never combined for one request.
- **Core checks.** `ControlStore::provider_credential_binding` takes no
  credential store, so it cannot touch native storage. It refuses an unknown
  profile, a profile of another provider kind, a profile with no recorded
  endpoint, a `subscription` profile, and an `api_key` profile with no bound
  credential. A `none` profile yields no token there, so it needs no native
  store. Only when it returns a binding is
  the native store opened. `ControlStore::load_provider_credential` then
  refuses a binding stored by another backend before it reads any secret. The
  LM Studio adapter passes `lm_studio`, so an OpenAI or Anthropic profile's key
  cannot be sent to it.
- **Failure.** Any failure on this path ends the turn with a `credential`
  error before a model request is built. The message has the form `<cause
  code>: <text>`. There is no fallback to no token or to another token. A
  locked Secret Service is an error; OMGCode still never runs a prompt. A
  missing or busy settings store is also a `credential` error, without a cause
  code, and a busy store is refused without a retry. The cause codes are
  `unknown_profile`, `wrong_provider`, `endpoint_not_recorded`,
  `subscription_unsupported`, `not_bound`, `other_backend`, `settings_unusable`, `credential_not_found`,
  `credential_locked`, `credential_unavailable`,
  `credential_storage_not_private`, `credential_corrupt`,
  `credential_prompt_required`, `credential_io`, `credential_refused`, and
  `credential_invalid`. The desktop adds `endpoint_changed` (see the next
  section).
- **Rotation and lifetime.** The secret is loaded while the settings store is
  held, so a rotation cannot delete the item between the binding lookup and the
  load. The store is released before any network request. The desktop resolves
  the token again for each turn and caches nothing, so the next turn uses a
  rotated credential. In the CLI the credential is resolved before the endpoint
  check and before any session or journal exists, so the error JSON carries no
  session locator. A read records nothing: no control-journal entry and no
  revision change. The session journal does not name the profile or the
  credential.
- **Memory.** The token is held in a zeroing buffer with redacted `Debug` and
  no serialization for one turn, and the existing redaction of the token from
  recorded and returned text applies to it. The provider clients' request-header
  copies and transport buffers are not zeroed.
- **Limits.** Any process running as the same user can still read the stored
  credential. A memory-only session credential, subscription
  sign-in, token refresh, and account lifecycle are not implemented. The
  frontend shows nothing about the credential source beyond the endpoint text
  it already displayed. Version 0.30.0 bound the credential to a provider kind
  only; 0.31.0 amends that (next section).

## Endpoint binding (0.31.0)

[ADR-0048](adr/0048-bind-a-stored-credential-to-its-profile-endpoint.md) sends a
stored credential only to the endpoint recorded with its profile. It amends
ADR-0047, whose statement that a credential is bound to a provider kind and not
to an endpoint no longer holds for a usable profile.

- **One source.** A selected profile supplies the endpoint and the token.
  `ControlStore::provider_credential_binding(name, adapter_kind)` returns the
  recorded endpoint together with the credential decision, so the two cannot
  come from different places. It still takes no credential store. It checks, in
  order: the settings store is usable, the profile exists, its kind matches the
  adapter, an endpoint is recorded, and then the authentication mode (`none`
  yields no token, `subscription` is refused, `api_key` must be bound).
- **No endpoint recorded.** A selected profile with no recorded endpoint is
  refused in every authentication mode with a `credential` error that starts
  `endpoint_not_recorded:` and reads `provider profile <name> has no recorded
  endpoint; remove it and add it again with an endpoint`. The refusal comes
  before any native store access and before any request.
- **Exclusive flags.** `--endpoint` and `--provider-profile` together are a
  usage error with exit status 2 in `omgcode chat`, `omgcode session compact`,
  and `omgcode-desktop`. The default value of `--endpoint` does not trigger it.
  There is no case where both are allowed because they are equal.
- **Immutable endpoint.** No command edits a recorded endpoint. Removing a
  profile records the unbinding and deletes its credential, so a stored
  credential cannot be moved to another endpoint.
- **Desktop.** With a profile, the shell resolves the endpoint once at launch.
  A failure exits with status 1 and `omgcode-desktop: <message>` before any
  window opens. That value is the endpoint of the process: the webview is shown
  it, and each chat request is compared with it. Each turn resolves the profile
  again. If the recorded endpoint no longer equals the launch value, because
  the profile was removed and added again, the turn is refused with a
  `credential` error that starts `endpoint_changed:` (the provider profile's
  endpoint changed since launch; restart the application) and no turn starts. A
  replaced profile with the same endpoint and a new credential is accepted and
  uses the new credential. The wire contract is unchanged.
- **Migration.** A profile recorded before 0.31.0 has no endpoint and cannot be
  selected. Remove it, add it again with `--endpoint`, and store the credential
  again. The 0.30.0 behaviour, which took the endpoint from `--endpoint`, was
  never merged or released.
- **Limits.** In this version only `lm_studio` profiles accepted an endpoint. No endpoint rule
  exists for a non-loopback provider, HTTPS, or certificates, and no cloud
  adapter exists. Any process running as the same user can still read a stored
  credential. A hand-edited control journal is checked only by replay
  validation and is not tamper-evident. The frontend shows nothing about the
  credential source beyond the endpoint text it already displayed. The tests
  for this change ran against DPAPI on Windows and the Secret Service on Linux
  under WSL. A chat turn with a stored credential was not driven through the
  desktop shell's windows on either platform.

## Streamed replies (0.32.0)

[ADR-0049](adr/0049-stream-local-model-replies-and-push-chat-events.md) adds an
opt-in streamed request to the LM Studio adapter. The non-streaming methods are
unchanged and remain the default.

- **Request.** `LocalModelClient::complete_streaming` and
  `complete_with_tools_streaming` send the same body as the non-streaming
  methods with `stream` set to `true` and an `Accept: text/event-stream`
  header. `stream_options` is never sent, because LM Studio's documentation
  does not list it. They call a caller-supplied function with each piece of
  assistant text and return the same reply type as the non-streaming methods.
  Usage counters are present only when the server sends them.
- **Stream rules.** The response is read as server-sent events. Any breach fails
  the request, and nothing is repaired. The status must be a success and the
  content type `text/event-stream`. The existing 1 MiB response limit and the
  thirty-second request deadline cover the whole stream. Lines end with LF or
  CRLF. Empty lines, comment lines, and the `event`, `id`, and `retry` fields
  are ignored, and any other line that is not a `data` field is refused.
  `[DONE]` ends the stream, and nothing after it is read. A stream that ends
  without `[DONE]` fails with the error `IncompleteStream` (`local model stream
  ended before completion`). Every other payload goes through the parser that
  refuses duplicate keys and must be an object. A non-null `error` member is
  refused, and its body is never reported. A payload holds zero or one choice
  and one finish reason, and the accepted finish reasons are those of the
  non-streaming methods.
- **Tool calls.** The text method refuses any tool call. The tool method
  assembles calls by `index`: indexes start at 0, appear in order, and stay
  below 64. A call object holds only `index`, `id`, `type`, and `function`, and
  `function` only `name` and `arguments`. `id` is set once, name and argument
  pieces are concatenated, and a null member counts as absent. The assembled
  value holds only the members received, so the existing tool-call validation
  applies to it unchanged.
- **Provisional text.** Text goes to the caller as each payload passes
  validation. It is provisional: if the stream fails later, the request fails
  and the delivered text is not a reply. The journal records only the validated
  whole reply, and does not say whether it was streamed. Sessions stay at schema
  version 3.
- **Redaction.** A turn streams only when `ChatHooks::on_stream` is set, and
  compaction requests are never streamed. `StreamRedactor` removes the known
  server token across piece boundaries by withholding any tail that could be
  the start of the token. If its output would itself contain the token, it
  emits nothing further for that request. The final reply is still redacted as
  a whole before it is recorded. This is known-token redaction, not general
  secret detection.
- **Use.** `omgcode chat --stream` writes the escaped provisional text to
  standard error. The desktop shell streams by default and `--stream off`
  turns it off. See [development](development.md) and the
  [desktop interface](desktop-interface.md).
- **Limits.** The stream rules were checked against local fixtures only. No live
  LM Studio server was used. A streamed text reply has no usage counters unless
  the server sends them. A long generation still fails at the thirty-second
  deadline. Preview text can differ from the recorded reply. Since 0.33.0 a
  cancelled turn interrupts a streamed request, and since 0.34.0 any request,
  as described below.

### Cancellation (0.33.0)

[ADR-0050](adr/0050-interrupt-a-streamed-model-request-on-cancellation.md)
makes a streamed request watch a cancel token. The ADR-0049 stream rules above
are unchanged.

- **Transport.** `complete_streaming` and `complete_with_tools_streaming` take
  a `CancelToken`. The streamed request runs on an async HTTP client inside a
  single-thread runtime created for that one request. The client is built
  inside that runtime and is not reused, so a pooled connection never outlives
  the runtime that drives it. Its settings equal the blocking client's: no
  proxy, no redirect, no retry, the same connect timeout, and the same
  thirty-second deadline, which covers the whole response. The reader is now a
  parser fed bytes in any chunking, and the same stream rules apply to any
  chunking.
- **Cancel rule.** The token is checked before the request is sent, so a turn
  that is already cancelled makes no connection. It is checked while waiting
  for the response head and while waiting for each piece of the body, every 25
  ms, and also before each piece, so a fast stream cannot starve the check.
  When it is set, the request is dropped, which closes the connection, and the
  method returns `ProviderError::Cancelled` (`local model request was
  cancelled`). Text already delivered stays delivered and is still provisional.
  The journal never records it.
- **Runtime.** The turn runtime passes the turn's token to the model with each
  request. A model that returns `Cancelled` while the token is set ends the
  turn with the failure reason `cancelled`. If the token is not set, the
  runtime records a model error. A streamed text-only turn creates a token and
  ends as cancelled in the same way, including when the cancellation is seen
  after the reply arrived and before it is recorded.
- **Not interruptible in 0.33.0.** The non-streaming methods kept the blocking
  client. A non-streamed request, the compaction request before a turn, and a
  tool call in progress were not interrupted, and the CLI had no cancel source.
  Version 0.34.0 closes the first, second, and fourth gaps, below.
- **Limits.** The cancel path was checked against local fixtures only. No live
  LM Studio server was used.

### Cancelling every request (0.34.0)

[ADR-0051](adr/0051-cancel-every-model-request-and-interrupt-cli-turns.md)
applies the cancel rule above to every request.

- **Transport.** `complete` and `complete_with_tools` take a `CancelToken` too.
  All four methods share one transport: each request builds its own async
  client inside a single-thread runtime created for that request, with the
  settings and the deadline above. The blocking client and reqwest's `blocking`
  feature are removed. The wire request is unchanged: reqwest's blocking client
  was the same async client driven from a background thread. A non-streamed
  body is read piece by piece within the 1 MiB bound; a body read in part when
  the token is set is discarded.
- **One token per turn.** Every chat turn creates one token after the session
  checks and before the compaction check, hands a clone to
  `ChatHooks::on_cancel_token` on every path, and uses it for the pre-turn
  compaction requests and for every model request. A front end can supply the
  token through `ChatHooks::cancel`. A token already set when the turn would
  start ends it as `cancelled` before the turn marker, recording nothing.
- **Compaction.** `compact_journal` takes a token. A cancelled compaction
  returns the error kind `cancelled` ("Compaction was cancelled; nothing was
  recorded"). The journal is written only after the last summary, so it is
  unchanged.
- **CLI.** `omgcode chat` and `session compact` install the interrupt handler
  of the trusted `run` command once the journal is open. Ctrl-C or Ctrl-Break
  on Windows and `SIGINT` or `SIGTERM` on Linux then set the token instead of
  ending the process. The command fails with the code `cancelled`; a chat turn
  that had started is recorded as cancelled and the error carries the session
  locator. A handler that cannot be installed fails the command with the code
  `interrupt` before any model request. `capabilities` reports
  `cancellable_adapters: ["lm_studio"]`.
- **Still not interruptible.** A read, list, or edit tool call already in
  progress. A command tool call is cancelled through its run.
- **Limits.** Checked against local fixtures only. No live LM Studio server
  was used, and the CLI interrupt was delivered by a test only on Linux. The
  fixtures bound the time from the cancel until the turn or compaction
  returns, recording included, at five seconds. They do not bound the work
  before the cancel: reading settings and the synced journal appends depend
  on the host's storage.

LM Studio documents the request fields in
[Chat Completions](https://lmstudio.ai/docs/developer/openai-compat/chat-completions)
and tool calls arriving in pieces in
[tool use](https://lmstudio.ai/docs/developer/openai-compat/tools). The line
rules follow the
[HTML Standard on server-sent events](https://html.spec.whatwg.org/multipage/server-sent-events.html),
more strictly.

## Settings and account lifecycle

### LM Studio release connection requirements

LM Studio documents OpenAI-compatible model, Chat Completions, and Responses
endpoints. It also supports API tokens, with server authentication disabled by
default in the current documentation. Use a distinct LM Studio connection
profile; do not send an OpenAI API key or ChatGPT subscription token to it.
[API compatibility](https://lmstudio.ai/docs/developer/openai-compat),
[server authentication](https://lmstudio.ai/docs/developer/core/authentication)

Provide an explicit server URL, model selection/discovery, endpoint mode,
connection test, and token configuration. Suggest loopback
http://localhost:1234/v1 as an editable starting address, not as proof of a running
server. Allow a no-token profile only for the explicitly selected local service.
If authentication is enabled, use its scoped LM Studio token through the
credential service. A separately selected non-loopback service requires
authenticated HTTPS; never silently redirect requests or credentials. The
prototype desktop shell fixes the endpoint and the token source at launch. With
`--provider-profile`, both come from the profile: its recorded endpoint and its
stored credential. Without it, the endpoint is `--endpoint` (default
`http://127.0.0.1:1234/v1`) and the token is `OMGCODE_LM_TOKEN`, read for each
turn. The two flags are mutually exclusive. A webview request cannot change
either, and the shell has no connection test or model discovery.

Qualify Chat Completions and Responses independently for the chosen server
version and model. Reuse wire-format code only where its contract matches.
Model IDs, chat templates, token accounting, context limits, streaming, structured
output, and tool-call reliability are not established by the OpenAI-compatible
label. Resolve the loaded model's safe context budget or require a validated
profile. Malformed tool-like text remains text; do not execute it heuristically.
[Tool use](https://lmstudio.ai/docs/developer/openai-compat/tools)

Keep local session history and portable compaction in OMGCode. Do not assume
that provider-native caching, state, or compaction has the OpenAI API contract.
Do not submit provider-managed Remote MCP or hosted execution tools through this
profile. Route MCP through the [core-owned clients](mcp.md); a provider's own tool
execution could bypass those controls.
[LM Studio Responses features](https://lmstudio.ai/docs/developer/openai-compat/responses)

Starting the service, changing its settings, downloading models, or installing
software are separate explicit operations. Connecting for inference does not
authorize them. If the selected service loads an already installed model on
demand, describe that as part of the selected inference profile and qualify its
resource limits. This grants no general model-management authority.

Test model discovery, absent/unloaded models, token and no-token modes,
authentication failure, streaming/cancellation, validated tool calls, changed
context limits, and absence of unexpected cloud fallback on both supported
platforms. The current prototype does not implement model discovery. Its
streamed requests ([streamed replies](#streamed-replies-0320)) have been checked
against local fixtures only. The Windows Gemma smoke check above covers one live
profile, and it sent no streamed request; full platform and authentication
qualification remains pending.

### Shared settings and lifecycle

- Provide separate subscription and API-key connection choices for OpenAI and
  Anthropic/Claude. Identify the account, workspace where applicable, selected
  model, and billing mode before a session sends project data.
- Show sign-in, reconnect, and sign-out actions. Distinguish connected identity,
  permission to use the plan, model access, expired credentials, quota reached,
  provider restriction, and unavailable integration.
- Open provider login in the system browser. The application must not collect
  the user's ChatGPT or Claude password.
- Let the user cancel login. Reject mismatched, expired, duplicated, or
  unsolicited callbacks. A new attempt must not replace an active connection
  until identity and account binding are validated.
- Refresh credentials through the supported provider flow. Serialize refreshes
  for one credential set and store rotated credentials safely.
- Support account changes and reconnect after revocation. Clear local credentials
  on sign-out and perform remote revocation when the provider supports it.
  Do not imply that local sign-out necessarily revokes every remote token.
- A subscription quota or authentication failure must not trigger paid API
  fallback without an explicit user-selected policy.
- Show available usage/limit information, or state when it is unavailable.
  Do not invent monetary costs or remaining quota from a subscription name.

## Credential and execution boundaries

For release, keep access tokens, refresh tokens, API keys, login codes, and
verifier values in the trusted authentication service. Store durable credentials
in protected OS storage. Do not silently fall back to plaintext when that store
is unavailable; offer an explicit memory-only session if appropriate. Version
0.27.0 implements that protected store for API keys (ADR-0042); since 0.30.0
only the local LM Studio adapter reads it (ADR-0047), and since 0.31.0 only
at the endpoint recorded with its profile (ADR-0048). The prototype's
`OMGCODE_LM_TOKEN` process-environment input does
not meet this credential-service requirement and must not be treated as its
replacement.

Expose opaque connection identifiers and safe account labels to the UI. For
release, never put credentials in model context, workspace files, task or
terminal environments, command arguments, transcripts, or diagnostics. Scope
credentials to the correct provider, account/workspace, and audience. The
prototype has no task processes; its CLI reads the optional LM Studio token
from its own process environment and does not place that value in the journal.

The direct model adapter does not gain filesystem or command permissions.
A future approved provider-runtime integration needs its own reviewed
credential and execution boundary. Its built-in tools must remain constrained
by OMGCode's effective policy and OS containment.

## Acceptance evidence

Before claiming subscription support for either provider, verify on Windows
and Linux:

1. A documented or approved integration route applies to OMGCode.
2. A fresh account connection completes through the supported user flow.
3. Plan consent and model access are validated independently from identity.
4. A bounded, user-authorized request completes using the subscription path.
5. Refresh, expiry, revocation, sign-out, and account switching behave correctly.
6. Refused consent, ineligible plans, quota limits, and network failure produce
   clear results without unexpected API charges.
7. Project tasks and untrusted UI content cannot obtain tokens, alter account
   binding, or expand execution permissions.

Use deterministic mocks for routine tests. Live subscription checks require an
eligible account, explicit opt-in, minimal test data, and recorded results.
No such runtime checks have run yet.

See the [security rules](security-coding-rules.md),
[testing standards](testing-standards.md), and
[subscription decision](adr/0002-require-subscription-authentication.md).
