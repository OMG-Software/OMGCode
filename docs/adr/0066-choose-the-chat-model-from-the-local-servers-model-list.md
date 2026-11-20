# 0066: Choose the chat model from the local server's model list

- Status: Accepted
- Date: 2026-11-19

## Context

The desktop composer asked the operator to type the model id, and the CLI
took it from `chat --model`. A typed id has to match the server's id exactly
(`lfm2.5:latest`, `google/gemma-4-e4b`), and a typing mistake was only found
when the first turn failed. LM Studio and Ollama, the two local servers the one
local adapter serves (ADR-0064), both report their models.

On 2026-11-19 both servers on the development host answered
`GET /v1/models` with the OpenAI-compatible list
`{"object":"list","data":[{"id":...,"object":"model",...}]}`. LM Studio 0.4
listed its downloaded models, including one not loaded and one embedding
model. Ollama listed its local models and two models tagged `:cloud`. Ollama's
native `/api/tags` marks those two with `remote_host: https://ollama.com`:
they run on Ollama's hosted service, so a prompt sent to them leaves the
machine. `/v1/models` carries no such field.

Product priorities are secure operation, prompt injection defence, and
efficiency. A list read from a server is provider data, like a model reply.

## Decision

1. **One list request in core.** `omgcode_core::provider::list_local_models`
   sends one `GET /v1/models` to the endpoint, after the same loopback rule
   that every chat connection passes (ADR-0048). It sends the connection's
   optional bearer token exactly as a chat request does. The request has no
   proxy, no redirect, and no retry, a five-second connect timeout, and a
   ten-second total deadline. The body limit is 1 MiB. The request runs on a
   single-thread runtime made for the call (ADR-0050).
2. **Strict parsing.** The body is parsed with the duplicate-key-refusing
   parser. It must hold a `data` array of at most 512 objects, each with a
   string `id`. Other members are ignored. The whole list is refused, not
   repaired, when an id is empty, longer than 256 bytes, repeated, or holds a
   control character or a bidirectional formatting character. The last rule
   goes beyond the chat client's rule, so a listed id cannot display as a
   different id. Ids are returned sorted.
3. **The cloud tag is shown, not trusted.** An id whose last tag is `cloud` or
   ends in `-cloud` is marked `cloud_tag`. That is Ollama's naming convention,
   not proof. The desktop labels such a model "(Ollama cloud)" and, while it
   is chosen, warns that prompts and workspace text sent to it leave this
   computer. An unmarked model is not shown as local.
4. **The list grants nothing.** Choosing a model only fills the `model` setting.
   `chat_send` and `chat --model` validate it as before, and the server still
   decides whether the model exists. No list is journaled.
5. **Desktop.** A new `main`-only command, `chat_models`, resolves the
   connection exactly as `chat_send` does, stored credential included, and
   returns `{endpoint, models: [{id, cloud_tag}]}`. The composer's Model field
   becomes a choice with a **Refresh** button. It lists models after the
   chat defaults load, after the connection changes, and on Refresh. A list
   for an endpoint other than the one shown is discarded. A saved or chosen
   model the server did not report stays chosen and is marked
   "(not reported by the server)". A failure is shown beside the field. The
   window never offers a free-text model name.
6. **CLI.** `omgcode models [--endpoint URL | --provider-profile NAME]` prints
   `{endpoint, models}` for the connection `chat` would use with the same
   options. `chat --model` stays required, so scripts are unchanged.

## Alternatives considered

- **Use each server's native list.** Ollama's `/api/tags` marks cloud models
  authoritatively, and LM Studio's `/api/v0/models` reports the model type and
  load state. Rejected for now: it needs a provider-specific parser per kind,
  and the default connection does not record which server it reaches. LM
  Studio's `/api/v0` is also a beta interface. This choice is the obvious
  follow-up if the cloud tag proves unreliable.
- **Hide cloud or embedding models.** Rejected: the operator may want an
  Ollama cloud model, and hiding entries on a name heuristic would also hide
  wrongly named local ones. A visible label and warning keep the choice with
  the operator.
- **Keep a free-text box next to the list.** Rejected: it brings back the
  typing mistakes this decision removes. A model the server does not report
  cannot be used for a turn anyway.
- **Choose the first listed model by default.** Rejected: the first id may be a
  cloud model or an embedding model. With no saved model, nothing is chosen
  and Send stays disabled until the operator chooses one.
- **Prompt for a model on the CLI when `--model` is missing.** Rejected: the
  CLI is scripted and reads standard input for nothing else. `omgcode models`
  gives the ids to copy.

## Consequences

- The operator chooses an id the server reported, so a misspelt model no longer
  fails a turn. A model unloaded or deleted after listing still fails at
  `chat_send`, as before.
- Listing makes one extra loopback request when the window starts, when the
  connection changes, and on Refresh. A stored LM Studio credential is read for
  it, as for a turn.
- The list shows embedding models, which cannot answer a chat turn.
- The cloud label depends on Ollama's naming convention. Native-list detection
  remains unimplemented.
- `chat_models` raises the policy to 35 commands, 23 of them for `main`.

## References

- [Desktop interface](../desktop-interface.md)
- [Provider authentication](../provider-authentication.md)
- [ADR-0048: Bind a stored credential to its profile endpoint](0048-bind-a-stored-credential-to-its-profile-endpoint.md)
- [ADR-0050: Interrupt a streamed model request on cancellation](0050-interrupt-a-streamed-model-request-on-cancellation.md)
- [ADR-0064: Choose the model connection in settings and add Ollama Local](0064-choose-the-model-connection-in-settings-and-add-ollama-local.md)
- [OpenAI-compatible endpoints in LM Studio](https://lmstudio.ai/docs/app/api/endpoints/openai)
- [Ollama OpenAI compatibility](https://docs.ollama.com/api/openai-compatibility)
