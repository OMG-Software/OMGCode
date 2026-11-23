# 0071: Choose the provider and model per session and add Ollama Cloud

- Status: Accepted, amended by [ADR-0080](0080-move-chat-budgets-to-settings-and-add-workspace-instructions.md)
- Date: 2026-11-20

## Context

The user asked for four changes to the desktop shell:

1. A **Workspace settings** button next to the composer's **Chat settings**
   button, opening a window that shows only the open session's workspace.
2. The provider (connection) and model chosen **per session**, so that
   opening another session switches both.
3. A **provider** dropdown and a **model** dropdown in the composer. The model
   dropdown is filled when a provider is chosen. For Ollama and LM Studio "all
   the model options are not being loaded".
4. **Ollama Cloud** as a third provider. It needs an API key and connects to
   Ollama's hosted service directly, not through the local Ollama server.

Facts established before the decision, on 2026-11-20:

- Both local servers on the development host reported every model:
  `GET /v1/models` returned 4 ids from Ollama and 3 from LM Studio. The
  desktop lost them itself. `model_picker.ts` compared the endpoint returned
  by `chat_models` with the endpoint the composer read from `chat_defaults`
  and discarded the whole list when they differed. They differ whenever the
  connection changed outside the `main` window's footer, for example in the
  `settings` window, because `main` is not told, and whenever
  `chat_defaults` fell back to the launch endpoint after a busy settings
  store. The list was then shown as empty, with only the saved model.
- `https://ollama.com/v1/models` answered with the OpenAI-compatible list
  shape without a key (24 models). Ollama documents `https://ollama.com/v1`
  as the OpenAI-compatible base URL for direct cloud access, with the API key
  sent as a bearer token. A chat request to it was not made, because no key
  is available to the developer. The chat, stream, and tool-call shapes are
  therefore assumed to match the local Ollama server's, which ADR-0064
  captured.
- `ollama_cloud` profiles already existed as configuration only, with
  authentication `api_key` only, and refused any endpoint. The only
  endpoint rule was the loopback rule (ADR-0048). No cloud adapter existed.
- ADR-0059 decision 6 already chose a write-only password field in the
  `settings` window for API keys. It was not implemented.
- The workspace HTTP client already links `native-tls`.

Product priorities are secure operation, prompt injection defence, and
efficiency. A cloud connection sends prompts, workspace text read by tools,
and command output off the machine. That is the property the operator must
be told about before a turn uses it.

## Decision

1. **Ollama Cloud is one fixed endpoint.** Core defines
   `OLLAMA_CLOUD_ENDPOINT = "https://ollama.com/v1"`. An `ollama_cloud`
   profile accepts exactly that text as its endpoint (a trailing `/` is also
   accepted) and nothing else: no other host, port, path, user information,
   query, or fragment. It keeps authentication `api_key` only. No rule admits
   any other non-loopback endpoint.
2. **One endpoint type for both rules.** Core validates a provider endpoint
   into a type that is either a loopback endpoint (the ADR-0048 rule,
   unchanged) or the Ollama Cloud endpoint. The chat client, the stream, and
   the model list take that type. For the cloud endpoint the client uses
   HTTPS with the platform's certificate validation, no proxy, no redirect,
   no retry, and the same timeouts and body limits as the local client. A
   request to the cloud endpoint without a token is refused before any
   connection is made.
3. **The credential is bound as before.** `ollama_cloud` joins `lm_studio` as
   a stored-credential adapter. `provider_credential_binding` returns the
   recorded endpoint with the credential decision, exactly as ADR-0048
   requires. `capabilities` reports `stored_credential_adapters:
   ["lm_studio", "ollama_cloud"]`. CLI `chat`, `session compact`, and
   `models` accept an `ollama_cloud` profile through `--provider-profile`.
4. **Selectable connections.** A profile is selectable when it is a local
   kind with a recorded endpoint (unchanged) or an `ollama_cloud` profile with
   the cloud endpoint and a bound credential. Replay checks the same rule
   except the credential, which replay cannot read; a turn without a usable
   credential fails with the existing `credential` errors.
5. **The `settings` window adds Ollama Cloud and its key.** **Add
   connection** offers Ollama Cloud. It takes a name and an API key in a
   password field; the endpoint is fixed by Rust and not typed. The key is
   sent once, in that command, then cleared from the field (ADR-0059
   decision 6). Rust wraps it in the zeroing secret type, applies the CLI's
   checks, records the profile, stores the key through core's credential
   store, and binds it. No command returns a key, its length, or a digest.
   **Set key…** replaces the key of an existing `ollama_cloud` profile the
   same way. Removing a profile with a bound credential is allowed in the
   window after a native Yes/No that says the stored key is deleted; core
   then removes the profile and deletes the credential, as the CLI does.
6. **The connection and model are chosen per session.** A desktop-owned file,
   `desktop-v1/session-connections.json`, maps a session id to the
   connection chosen for it and the model chosen for it. Only Rust writes it,
   atomically. It holds the profile name, or the default, together with the
   endpoint shown in the confirmation, and the model id. It holds no secret.
   It is bounded: at most 4096 sessions and 1 MiB, refusing a new entry
   beyond that. A damaged file is reported and treated as empty, so it never
   blocks chat.
7. **Resolution order for a turn.** A launch argument (`--endpoint` or
   `--provider-profile`) still fixes the connection for every session.
   Otherwise a session's own choice applies. A session without one follows
   the global selection of ADR-0064, which is now the default for sessions
   that have not chosen. Each turn re-resolves the session's profile through
   core. If the profile is gone, is no longer selectable, or its endpoint no
   longer equals the endpoint recorded for the session, the turn is refused
   with `session_connection_stale` and the operator chooses again. A profile
   removed and added again with another endpoint therefore never receives a
   session's turns without a new confirmation.
8. **Choosing a provider for a session is confirmed natively.**
   `connection_select` now takes the session id with the profile name or
   `null` for the default. Rust refuses an unknown session and an unchanged
   choice, then shows the ADR-0064 native Yes/No naming the provider, the
   profile, and the endpoint. For Ollama Cloud the text also says that
   prompts, workspace text read by tools, and command output leave this
   computer for ollama.com. Only on Yes is the session's choice recorded, and
   its model is cleared, because model ids belong to one server. Opening
   another session shows that session's recorded choice and shows no dialog,
   because that choice was confirmed when it was made.
9. **Choosing a model is recorded without a dialog.** `session_model_select`
   records a model id for a session after the same model-id check as
   `chat_send`. A model grants nothing and the server still decides whether
   it exists. A model of an Ollama Cloud connection, or a cloud-tagged model
   on a local Ollama server, shows the off-machine warning while chosen.
10. **The model list belongs to a session.** `chat_models` takes the session
    id and lists the models of that session's resolved connection. It returns
    the session id with the list. The composer discards an answer only when
    it belongs to a session or request that is no longer shown, never because
    an endpoint string differs. The list loads when a session opens, after
    the provider changes, and on **Refresh**.
11. **The composer shows both dropdowns.** A **Provider** dropdown and a
    **Model** dropdown sit in the composer toolbar, next to **Chat settings**.
    The model dropdown is disabled and empty until the provider's list
    arrives. The footer's provider dropdown changes the open session's
    provider through the same command. Both are disabled when a launch
    argument fixed the connection.
12. **Workspace settings opens the scoped `settings` window.** A **Workspace
    settings** button next to **Chat settings** calls the new `main` command
    `settings_show_workspace` with the open session's id. Rust resolves the
    session's registered workspace, records it as the window's scope, and
    shows the `settings` window. `settings_overview` returns the scope. While
    a scope is set, the window shows only that workspace's settings, without
    the section sidebar, and offers **All settings**, which clears the scope.
    The sidebar **Settings** row shows the window unscoped. The scope is
    display state only: the `settings` window could already change every
    workspace, and the scope grants and removes nothing.

## Alternatives considered

- **Record the session's choice in the session journal.** It would be durable
  with the session, but it needs session schema version 4 and older binaries
  refuse unknown events. Rejected for now: the choice is a desktop preference
  for the next turn, not a record of what happened. Each turn's
  `request.manifest` already records the model and endpoint that were used.
- **Record the choice in the control journal.** Rejected: one entry per
  session would grow the settings journal without bound, and every choice
  would bump the settings revision that the `settings` window checks.
- **Confirm every session switch natively.** Rejected: the choice was
  confirmed when made, and the recorded endpoint is checked again each turn.
  A dialog on every switch would teach the operator to click Yes.
- **Make the most recent choice the default for new sessions.** Rejected: it
  would shadow the global selection made in the `settings` window and give
  two meanings to "default".
- **Accept any HTTPS endpoint for cloud profiles.** Rejected: a key bound to
  a profile could then be sent to any host the profile names. A fixed
  endpoint keeps the credential's audience fixed (ADR-0048).
- **Use Ollama's native `/api/chat` for the cloud.** Rejected for the same
  reason as in ADR-0064: it duplicates the parsing and cancellation that the
  OpenAI-compatible path already has and tests.
- **Route Ollama Cloud through the local Ollama server's `:cloud` models.**
  That already works and stays labelled. The user asked for a direct
  connection that needs no local Ollama.
- **Type the key into a native prompt or keep it CLI-only.** Rejected by the
  user in ADR-0059 decision 6.
- **A separate workspace-settings window.** Rejected: a fourth window needs
  its own capability file and command grants for the same commands the
  `settings` window already has. A scope on that window shows the same view.
- **Keep comparing endpoints in the model picker.** Rejected: it is the bug.
  The session id identifies which list the composer asked for.

## Consequences

- Opening a session restores its provider and model. Sessions that never
  chose follow the global selection.
- The desktop policy gains `session_model_select` and
  `settings_show_workspace` for `main`, and `settings_set_api_key` for
  `settings`. `settings_add_local_connection` becomes
  `settings_add_connection` and accepts Ollama Cloud. `connection_select`,
  `connection_status`, and `chat_models` take a session id.
- The `settings` webview now carries an API key for one IPC message. That is
  the cost ADR-0059 decision 6 accepted.
- OMGCode makes its first non-loopback request, to one fixed HTTPS host,
  only for a profile the operator added with a key and confirmed for a
  session or as the global selection.
- The Ollama Cloud chat, stream, and tool-call shapes were not verified with
  a real key. The model list was verified without one.
- `session-connections.json` is not tamper-evident. A process running as the
  same user can edit it, as it can edit the control journal. The endpoint
  check in decision 7 limits an edit to a profile that is already recorded
  and selectable.

## References

- [ADR-0048: Bind a stored credential to its profile endpoint](0048-bind-a-stored-credential-to-its-profile-endpoint.md)
- [ADR-0059: Configure trusted settings in an isolated desktop settings window](0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md)
- [ADR-0064: Choose the model connection in settings and add Ollama Local](0064-choose-the-model-connection-in-settings-and-add-ollama-local.md)
- [ADR-0066: Choose the chat model from the local server's model list](0066-choose-the-chat-model-from-the-local-servers-model-list.md)
- [Ollama Cloud](https://docs.ollama.com/cloud)
- [Ollama API authentication](https://docs.ollama.com/api/authentication)
- [Ollama OpenAI compatibility](https://docs.ollama.com/api/openai-compatibility)
