# 0093: Sign in with ChatGPT and use the plan through the Responses API

- Status: Accepted
- Date: 2026-11-24

## Context

The first release must offer subscription sign-in for ChatGPT and Claude
([provider authentication](../provider-authentication.md#release-requirements),
ADR-0002). Claude subscription sign-in stays blocked: Anthropic requires prior
approval for a third-party product to offer claude.ai login, and none exists.
Google's Gemini subscription sign-in is not offered to third-party tools
either. OpenAI documents a supported route for open-source and local apps,
"ChatGPT plan usage" in Sign in with ChatGPT. This record adopts it.

Before this change:

- `openai` and `anthropic` profiles could record authentication
  `subscription`, kept so older journals replay, but every adapter refused one
  (`subscription_unsupported`).
- A stored credential is immutable (ADR-0042): rotation stores a new item,
  records a new binding in the control journal, and deletes the old item.
- A credential is sent only to the endpoint recorded with its profile
  (ADR-0048). Hosted profiles record one fixed endpoint (ADR-0071, ADR-0092).
- The one model adapter, `LocalModelClient`, speaks Chat Completions. The turn
  runtime sends tool results back as user-role text and never echoes the
  model's tool-call items, so the adapter needs only role-tagged text and the
  tool definitions.

Facts read from OpenAI's documentation on 2026-11-24
([overview](https://developers.openai.com/siwc/token-sharing-open-source),
[sign-in](https://developers.openai.com/siwc/token-sharing-open-source/sign-in),
[accounts and sessions](https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions),
[models and inference](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference),
[token reference](https://developers.openai.com/siwc/token-sharing-open-source/token-reference),
[errors](https://developers.openai.com/siwc/token-sharing-open-source/errors-and-recovery),
[preview limitations](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations),
and the issuer's
[OpenID configuration](https://auth.openai.com/.well-known/openid-configuration)):

- Open-source and local apps need no partner approval, client secret, or
  partner key. The first authorization uses `client_id=dynamic_agent_client`
  with `agent_name_hint`; the callback returns an issued `client_id`
  (`oaiapp_...`), which is saved and reused for that account. Each client is
  bound to one user and the workspace chosen at registration.
- Every authorization sends a stable, opaque, per-host `ext_agent_host_id`
  (`urn:ietf:params:oauth:jwk-thumbprint:`, `urn:uuid:`, or `did:key:`),
  chosen before the first sign-in. It is not a credential.
- Issuer `https://auth.openai.com`; authorization
  `https://auth.openai.com/api/accounts/authorize`; token
  `https://auth.openai.com/api/accounts/oauth/token`; revocation
  `https://auth.openai.com/api/accounts/oauth/revoke`; keys
  `https://auth.openai.com/.well-known/jwks.json`. ID tokens are RS256.
- The request is an authorization code with PKCE S256, a fresh `state` and
  `nonce`, `resource=https://api.openai.com/v1`, and the scopes `openid
  profile email offline_access resource.invoke chatgpt.tokens.use.direct`. The
  redirect is `http://127.0.0.1:<port>/auth/callback`; only the port may vary,
  and `localhost` must not be used.
- The code exchange is a form-encoded POST with the issued `client_id`,
  `code`, `code_verifier`, the same `redirect_uri`, and the same `resource`.
  The response holds `access_token`, `refresh_token`, `id_token`,
  `token_type`, `expires_in`, `scope`, and `earliest_refresh_at`.
- The ID token's signature, issuer, audience (the issued client id),
  expiry, and nonce must be checked; `sub` is the identity. Plan usage is
  permitted only when the granted scopes include `chatgpt.tokens.use.direct`;
  a valid ID token alone does not permit it.
- Access tokens last one hour. Refresh tokens last 30 days and rotate on every
  refresh. Refreshes of one session must be serialized "so two processes do
  not race a rotating token". The refresh is a form POST with
  `grant_type=refresh_token`, the issued `client_id`, the `refresh_token`, and
  `resource`, without `scope`.
- Sign-out revokes the refresh token with `token`, `token_type_hint=
  refresh_token`, and `client_id`; an empty 200 is success, even for an
  already invalid token. Revocation does not delete the registered client.
- Inference is `POST https://api.openai.com/v1/responses` with the access
  token as a bearer token, never ChatGPT's `backend-api`. Each request sets
  `store:false` and `stream:true` and sends the whole history in `input`.
  `system` message items are refused; `instructions` is used instead.
  `previous_response_id`, `max_output_tokens`, `temperature`, `top_p`,
  `metadata`, `user`, `truncation`, `prompt`, and other listed fields are
  refused. Function tools must be grouped in a `namespace` tool or supplied
  through `additional_tools`; hosted tools are not supported.
- Success is only `response.completed`. `response.failed` can carry
  `subscription_sharing_usage_limit_exceeded` after text has streamed.
  `response.incomplete`, explicit errors, and an interrupted stream are
  separate outcomes. Admission failures before the stream may be a
  `{"detail": ...}` body.
- `GET https://api.openai.com/v1/models` with the access token returns
  `{"models":[{"slug","display_name","visibility",...}]}`, not the
  `data` list of the API-key route.

## Decision

### Profile and registration

A ChatGPT connection is an `openai` profile with authentication
`subscription` and the fixed endpoint `https://api.openai.com/v1`. One profile
is one registration: one account and workspace. The profile is added with no
credential, as today, and is usable only after sign-in.

A new control-journal event, `provider_subscription_bound`, records a
successful sign-in: the profile, the issued `client_id`, the ID token's
`sub`, the account's email as a display label when the ID token carries one,
the host identifier, and a credential binding (identifier and backend) for the
token store below. The profile keeps `client_id` and `sub` after sign-out, so a
later sign-in reuses the registration and is refused if the returned identity
or client differs. Replay checks that the profile is an `openai` subscription
profile with an endpoint, that the client and subject match an earlier
registration, that the host identifier matches the first one recorded, and
that the binding is unique. The existing `provider_credential_removed` event
records sign-out. `provider_credential_bound` stays `api_key` only.

The host identifier is `urn:uuid:` and a random UUIDv4, created at the first
sign-in and recorded with it. Every later sign-in on this data root sends it.
A JWK thumbprint, which OpenAI recommends, would need a key pair that
OpenAI does not verify; the UUID form is documented as supported.

### Sign-in

Core runs the browser flow; only trusted code starts it:

- `omgcode workspace provider-signin <name> --revision <n>` in the CLI. It
  prints the authorization address on standard error and opens it in the
  system browser.
- **Sign in with ChatGPT** in the desktop `settings` window, after a native
  confirmation that names the browser step and the off-machine data. A name
  that no profile has yet first records an `openai` subscription profile with
  OpenAI's endpoint. Rust opens the browser; the webview never sees the
  address, the callback, or any token. **Cancel sign-in** stays usable during
  the wait and only sets the sign-in's cancel token. A new name is checked
  against the settings store's rules before the confirmation. **Sign out**
  shows the same bar with **Cancel sign-out**, which ends the wait for the
  session lock or for OpenAI.

Steps:

1. Refuse before any network use unless the profile is an `openai`
   subscription profile with its endpoint and is not signed in.
2. Bind a listener to `127.0.0.1` on a port the system chooses, then create a
   32-byte random `state`, `nonce`, and PKCE verifier.
3. Open `https://auth.openai.com/api/accounts/authorize` with
   `response_type=code`, the client (`dynamic_agent_client` with
   `agent_name_hint=OMGCode` for a new registration, the recorded `client_id`
   otherwise), `ext_agent_host_id`, `redirect_uri`, `scope`, `resource`,
   `state`, `nonce`, `code_challenge`, and `code_challenge_method=S256`. No
   `id_token_hint` or `login_hint` is sent, so the address holds no token or
   email.
4. Wait at most five minutes, checking cancellation every 50 ms. Each
   connection gets five seconds and 8 KiB to send one `GET` request line.
   Another path is answered 404 and the wait continues. A request to
   `/auth/callback` ends the wait: its query must hold each parameter at most
   once; a `state` that differs, an `error`, a missing `code`, a new
   registration without an issued `client_id`, `dynamic_agent_client` as the
   issued id, or a returning registration with a different `client_id` ends
   the attempt. The page shown is fixed text and never reflects a parameter.
5. Exchange the code with the issued client, the verifier, the same redirect
   address, and `resource`.
6. Require `token_type` `Bearer`, a positive `expires_in` of at most a day,
   non-empty tokens, and `chatgpt.tokens.use.direct` among the granted
   scopes; otherwise `plan_usage_not_granted` and nothing is stored.
7. Fetch the keys and validate the ID token: three base64url parts, `alg`
   `RS256` and a `kid` that names one RSA key, a PKCS#1 v1.5 SHA-256
   signature verified with the `rsa` crate, `iss`, `aud` (a string or a list
   holding the client id), `exp` in the future with 60 seconds of leeway, the
   nonce, and a non-empty `sub`. A returning registration must return the
   recorded `sub`. When the key fetch, the ID-token check, or the `sub`
   check fails, the issued refresh token is revoked once, best effort, with
   a fresh cancel token so a cancelled attempt still sends it, and the
   refusal is returned whether or not OpenAI confirms it.
8. Store the token record and record `provider_subscription_bound`, with the
   ordering and failure reporting of `set_provider_credential` (ADR-0042).

No ID token is kept after validation: `id_token_hint` is not used.

### Token store

The protected store keeps its immutable items (ADR-0042). A refresh changes
no journal event, because a refresh every hour would exhaust the journal's
4096 events in about six months and bump the settings revision under the
operator's edits.

A sign-in's binding names a pair of slots: the bound identifier and a
companion identifier derived from it with SHA-256 (shaped as a UUIDv4, so the
store accepts it). Each slot item holds one JSON token record: a version, a
generation counter, the client id, the subject, the access token, the refresh
token, the granted scopes, and the expiry time. A reader loads both slots,
checks that each record's client and subject match the registration, and uses
the record with the higher generation. A slot that exists but cannot be read or decoded, or
that holds another registration's record, fails the read with
`token_store_unavailable` rather than falling back to the other slot; only an
absent slot is unused. A writer deletes the slot that does not hold that
record, which holds an older and already rotated token, and stores the next
generation there. An interrupted write leaves the current record in place.

`delete_unbound_credential` is unchanged: it deletes and reports only the
bound identifier, which is all an API key has. A new `delete_unbound_slots`
deletes both the companion and the bound identifier and reports each one it
could not delete, both when the binding names another backend. Only sign-out
uses it: sign-out unbinds and then deletes both slots.

### Refresh and concurrency

Every Responses request asks the token source for an access token first. The
source holds the binding, client, and subject read from the settings store
when the turn or command started; it takes the session's lock, loads the
current record, and refreshes when fewer than five minutes remain. A refresh posts to the token endpoint and stores the rotated
record in the other slot before the lock is released.

The session lock is an exclusive OS file lock on
`<data>/subscription-locks/<binding id>.lock`, taken with `try_lock` every 50
ms for at most 40 seconds and abandoned when the turn is cancelled. It
serializes refresh, sign-out, and token reads across threads and processes.
Lock order: a session lock may be held while the settings store is opened;
the settings store is never held while a session lock is taken. Lock files
are never deleted, so two holders cannot hold different files for one name.

A crash after the token endpoint rotated the refresh token and before the new
record was stored loses the session; the next refresh is refused and the
operator signs in again. OpenAI documents no way to close that window.

A refused refresh (`invalid_grant`, `invalid_refresh_token`, `token_expired`,
`refresh_token_expired`, `refresh_token_invalidated`, `refresh_token_reused`)
is `signin_required`. So is a refresh answer whose stated scopes lack
`chatgpt.tokens.use.direct`: the plan no longer permits use, only a new
sign-in can tell, after a sign-out, and it reports `plan_usage_not_granted`
if the plan still refuses. Other failures keep the record and fail the
request. The token answer is read straight from the zeroing body, not
through a `serde_json::Value` tree: its access, refresh, and ID tokens move
into zeroing buffers as they are read, so an answer refused for a duplicate
key, a wrong type, trailing data, or a later check leaves no unzeroed copy
of a token that contains no escape. `serde_json` unescapes a string with an
escape through its own scratch buffer, which is not zeroed, so a token sent
with an escape, refused or accepted, can leave its text there. No check
refuses an escaped token; that OpenAI's tokens need no escaping is an
assumption about the documented service, not something OMGCode enforces.

### Sign-out and removal

`omgcode workspace provider-signout <name> --revision <n>` and **Sign out**
in the `settings` window take the session lock, write the session's
sign-out marker, revoke the refresh token once, record the unbinding as
`provider_credential_removed`, delete both slots, and report whether
revocation was confirmed. The marker is an empty file named by the binding
identifier beside the session's lock file, written and synced under the
lock. Every token request checks for it under the same lock before reading
a slot and returns `signin_required` if it exists, so a session already
running gives no token and writes no record after a sign-out, even when a
slot could not be deleted. A marker that cannot be written stops the
sign-out before the revocation. An unbinding whose journal write or sync
fails after it began may still be recorded, so its slots are deleted anyway
and it is reported as `signout_uncertain`, never as unrecorded: otherwise a
recorded removal would leave the slots bound to nothing and a second
sign-out would find the profile signed out. A sign-out that fails after the marker
leaves the session unusable until it is signed out again. A new sign-in
gets a new binding identifier, so no marker is ever cleared. A failed revocation is not retried and does not stop the local
sign-out; the report says remote revocation was not confirmed and that the
operator can disconnect OMGCode in ChatGPT settings. Removing a signed-in
subscription profile, and `omgcode workspace provider-credential-remove` on
any subscription profile, are refused, so only sign-out deletes token items,
and always under the session lock.

### Responses adapter

`LocalModelClient` gains a second wire protocol. A subscription profile
builds the client with `LocalModelClient::chatgpt`, which accepts only the
fixed OpenAI endpoint and a token source. Everything the turn runtime, the
compaction requests, and the front ends use stays the same type.

- **Request.** `POST {endpoint}/responses` with `model`, `instructions` (the
  compiled base prompt, which must be the first and only system message),
  `input` (every other message as a `user` or `assistant` message item with
  text content), `store:false`, `stream:true`, and for a tool request one
  `namespace` tool named `omgcode` holding the function definitions, each
  with `strict:false` because the runtime's schemas mark optional arguments
  as optional, and `tool_choice:"auto"`. No output limit, temperature, reasoning, `include`,
  or conversation field is sent. The context estimate and the 256 KiB request
  bound apply as before.
- **Stream.** Every request streams. The non-streaming methods read the
  stream without passing text on. The same server-sent-event line rules
  apply, with a 120-second deadline and an 8 MiB bound for the whole stream,
  because one event per token repeats its envelope and reasoning models take
  longer than thirty seconds. That deadline equals the default turn timeout
  (ADR-0077), which is checked between operations, so one slow request can
  end a default multi-step plan turn. The turn timeout stays the operator's
  limit and is not raised for a plan connection; the documentation tells
  plan users to raise it. Deriving one from the other was rejected because a
  connection choice would then widen a limit the operator set. Each `data` payload is strict JSON with a
  string `type`. `response.output_text.delta` and `response.refusal.delta`
  pass text to the caller. `response.output_item.done` items are the reply:
  `message` items contribute their `output_text` and `refusal` parts,
  `function_call` items become calls, `reasoning` items are ignored, and any
  other item type refuses the reply. `response.completed` ends the stream
  successfully. When it lists a non-empty `output`, the finished items must
  sit at exactly its indices and match each listed item in type, `id`, and
  reply content (a message's text and refusal parts, or a call's `call_id`,
  `namespace`, `name`, and `arguments`), or the reply is refused. An empty or
  absent `output` is not compared, because no real stream was observed. An
  event's `data` lines are joined by LF, one before every line after the
  first, and dispatched at its blank line. `response.failed`, `response.incomplete`, `error`, a body
  that ends first, or a `[DONE]` before completion fail it.
- **Tool calls.** A call must have a non-empty `call_id`, a `name`, string
  `arguments`, and a `namespace` that is absent or `omgcode`. It is handed to
  the runtime as the Chat Completions shape
  `{"id","type":"function","function":{"name","arguments"}}`, so
  `ToolCall::parse` judges it exactly as before. The text-only method refuses
  any call.
- **Errors.** A non-success status reads at most 64 KiB of the body and
  looks only for `error.code`; a known subscription code, from either the
  status body or a failure event, becomes `ProviderError::ResponseFailed`
  with that code (`usage_limit_exceeded`, `usage_unavailable`,
  `user_not_eligible`, `unsupported_capability`, `route_not_supported`,
  `invalid_user`, `not_authorized`, `user_unavailable`); a
  `response.incomplete` is `incomplete` and any other failure event is
  `failed`. A token that cannot be refreshed is `SignInRequired`, and a token
  store or refresh that fails otherwise is `AccessTokenUnavailable`. Any other
  status is the existing `HttpStatus` or `MalformedResponse`. No provider text is reported. There is
  no retry and no fallback to any other billing path.
- **Cancellation.** Unchanged (ADR-0050, ADR-0051). A token wait or refresh
  also stops for a set token.
- **Usage.** `response.usage` input, output, and total tokens fill the
  existing usage counters when present.
- **Model list.** `omgcode models --provider-profile <name>` and the desktop
  model box read the subscription list: the `models` array, keeping entries
  whose `visibility` is `list`, by `slug`, with the existing id checks.
  Hidden, legacy, and non-object entries are skipped, and only kept entries
  count against the 2048-entry cap; the 4 MiB body limit bounds the whole
  array. A kept entry with a bad or repeated slug still refuses the list.

### Journals and context

Session journals are unchanged and do not record that a reply came from a
plan. Tokens never enter a journal, an error, a log, model context, the
webview, or a process environment. Known-token redaction (ADR-0047) is not
applied to access tokens, which change every hour and never reach text the
model or the journal sees.

## Alternatives considered

- **Record every refresh as a new binding.** It keeps ADR-0042's single path
  but fills the 4096-event journal within months and changes the settings
  revision hourly.
- **Add an in-place replace to the platform store.** A replaced DPAPI file or
  Secret Service item is a new write path in reviewed native code, and a
  failure part-way could leave no readable record. Two immutable slots need
  only the existing store, load, and delete.
- **A token file outside protected storage.** OpenAI's example writes a
  0600 file. ADR-0042 forbids a plaintext fallback.
- **The Codex CLI's login cache or OAuth client.** Borrowing another
  application's identity or cache is forbidden by the authentication contract.
- **Chat Completions with the access token.** The plan route accepts only the
  Responses API.
- **Echo `function_call` and `function_call_output` items.** The runtime keeps
  tool results as source-labelled user text for every adapter; a second
  history shape would change replay and compaction for one provider.
- **Send `include: ["reasoning.encrypted_content"]`.** Storing opaque
  encrypted reasoning in journals needs its own record. Reasoning is not
  carried between requests.
- **Check the settings journal before each token use.** It would also stop
  a running session after a sign-out, but opening the control store takes
  its exclusive lock, so a token request would fail while any other
  process held the settings, and each request would replay the journal.
  The marker shares the session lock that the sign-out already holds.
- **Fail the sign-out when a slot remains.** The revocation has already
  happened then, and the profile must not stay bound to dead tokens.
- **Show the authorization address in the webview.** The address carries the
  `state`; a page that knows it and the port could complete a login with
  another account's code. Rust opens the browser itself.

## Consequences

- An operator can sign in with a ChatGPT account and use the plan for chat,
  compaction, and tool turns from the CLI and the desktop shell.
- The control journal gains one event kind. A journal that records it cannot
  be read by 0.59.0 or earlier.
- New dependencies: `rsa` 0.10 (RustCrypto, pure Rust) for RS256
  verification, `base64` 0.23 (already in the tree through reqwest) for
  base64url, `getrandom` 0.4 (already in the tree through `uuid`) for the
  random values, and `bytes` 1.12 (already in the tree through `hyper`) to
  send the token form from a zeroing buffer. Verification uses only the
  public key, so the timing issue reported for RSA decryption does not apply.
  A failed random source is `random_unavailable`, a local error, not a
  transport failure. The host identifier is made from the same source by
  the caller and handed to the settings store, so it fails the same way.
- Opening the browser runs `explorer.exe` from the Windows directory or
  `xdg-open` from `/usr/bin` with the address as one argument, never a shell.
- A sign-in that lacks plan permission records nothing; the issued client then
  stays registered at OpenAI until the operator removes it in ChatGPT
  settings, and the next attempt registers another. A sign-in refused after
  the exchange returned usable tokens (step 7) revokes its refresh token;
  an unconfirmed revocation leaves that session live in the same way.

### Not verified

- No real sign-in, token exchange, refresh, revocation, model list, or
  Responses request was made. Every test uses loopback mock servers and test
  keys. A completed sign-in is not evidence that a plan, model, or account
  is eligible, and nothing here claims plan usage works.
- The stream event shapes come from OpenAI's Responses streaming
  documentation and the plan-route pages; the namespace function-call item
  (`namespace` member) comes from the tool search guide.
- Whether the plan route accepts `tool_choice`, assistant-role input
  messages, or a namespace for every model was not observed.
- Neither platform's browser launch was driven by a test.

## References

- [Provider authentication](../provider-authentication.md)
- [Desktop interface](../desktop-interface.md#choosing-the-model-connection)
- [ADR-0002](0002-require-subscription-authentication.md),
  [ADR-0042](0042-store-provider-credentials-in-protected-os-storage.md),
  [ADR-0048](0048-bind-a-stored-credential-to-its-profile-endpoint.md),
  [ADR-0049](0049-stream-local-model-replies-and-push-chat-events.md),
  [ADR-0051](0051-cancel-every-model-request-and-interrupt-cli-turns.md),
  [ADR-0071](0071-choose-the-provider-and-model-per-session-and-add-ollama-cloud.md),
  [ADR-0092](0092-add-hosted-providers-and-an-openai-compatible-connection.md)
