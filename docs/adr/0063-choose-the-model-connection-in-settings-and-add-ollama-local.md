# 0063: Choose the model connection in settings and add Ollama Local

- Status: Accepted
- Date: 2026-11-19
- Implemented: version 0.43.0 (the connection part of ADR-0059 phase S3)

## Context

The user asked for Ollama support and a dropdown to choose the language-model
provider. Before this change the only adapter was the local LM Studio client.
The desktop endpoint was fixed for the whole process by `--endpoint` or
`--provider-profile`, and no window could change it.
[ADR-0059](0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md)
decision 7 already planned to choose the active connection in the `settings`
window, but only among `lm_studio` profiles, in phase S3.

Facts established before the decision:

- Ollama 0.40.16 on Windows serves OpenAI-compatible Chat Completions at
  `http://127.0.0.1:11434/v1/chat/completions`. A probe with the models
  `lfm2.5:latest` and `gemma4:e2b` returned the same reply, tool-call, and
  server-sent-event shapes the LM Studio client parses. The captured
  responses are fixtures in `omgcode-core` tests.
- Ollama needs no token on loopback. `ollama_local` profiles already existed
  as configuration only, with authentication `none`.
- The OpenAI-compatible API cannot set Ollama's context length (`num_ctx`).
  OMGCode's context size is a client-side budget, so the server's context
  length must be set in Ollama.

## Decision

1. **Ollama Local uses the existing local client.** An `ollama_local`
   profile may record an endpoint under the same rule as `lm_studio`:
   `http://<loopback IP>:<port>/v1`, no user information, query, or
   fragment, at most 256 bytes. `ProviderKind::is_local` names both kinds.
   A turn through an `ollama_local` profile sends no `Authorization` header,
   because the profile cannot bind a credential. CLI `chat --provider-profile`
   and desktop `--provider-profile` accept either kind. `capabilities`
   reports both kinds as streaming and cancellable adapters;
   `stored_credential_adapters` stays `["lm_studio"]`.
2. **Core records one active connection.** The control journal gains
   `ActiveProviderSelected { profile_id }`, with `None` meaning the default.
   Only a local profile with a recorded endpoint is selectable. A selection
   that changes nothing is refused. Replay validates every selection again.
   Removing the selected profile clears the selection in the same event, so
   the journal never points at a missing profile. The snapshot writes
   `active_provider` only when present, so older journals keep their bytes.
3. **The `settings` window chooses it from a dropdown.** The dropdown lists
   the default ("LM Studio: default", the default endpoint) and every
   selectable profile, labelled with its provider name, profile name, and
   endpoint. Choosing one calls `settings_select_connection` with the
   profile name only. Rust checks the choice, then shows a native Yes/No
   dialog naming the provider, profile, and endpoint, and records it only
   if the operator confirms. This applies ADR-0059 decisions 5 and 7 and
   extends decision 7 from `lm_studio` profiles to both local kinds.
4. **A launch argument still wins.** With `--endpoint` or `--provider-profile`
   the connection is fixed for the process, the dropdown is disabled, and
   `settings_select_connection` is refused with `connection_fixed`. Without
   either, each turn resolves the active connection when it starts. With no
   selection it uses `http://127.0.0.1:1234/v1` and `OMGCODE_LM_TOKEN`, as
   before. A running turn keeps the connection it started with.
5. **The `main` window never chooses.** It sends back the endpoint text it read
   from `chat_defaults`. If that no longer equals the endpoint core resolves,
   `chat_send` refuses with `connection_changed`. The window then reads the
   endpoint again and keeps the draft and model name for the operator to
   check before sending again.
6. **The `settings` window may record and remove local connections.**
   `settings_add_local_connection` records an `lm_studio` or `ollama_local`
   profile with authentication `none` and a typed endpoint. Core applies the
   loopback endpoint rule. This amends ADR-0059 decision 7, which said the
   webview never supplies an endpoint: the `settings` window may supply one
   for a new profile, and nothing is sent to it until a selection is
   confirmed natively. Adding needs no native dialog because a profile
   grants nothing until it is selected. `settings_remove_connection` removes
   a profile without a bound credential. Like clearing a package cache root,
   it deletes no secret and needs no confirmation. A profile with a stored
   credential is refused with `credential_bound` and is removed with the
   trusted CLI. Typing API keys in the window (ADR-0059 decision 6) is not
   implemented.
7. **The trusted CLI can select too.** `workspace provider-profile-select
   <name>` and `--default` call the same core function. CLI `chat` is not
   affected by the selection: it keeps its explicit `--endpoint` and
   `--provider-profile`.

## Alternatives considered

- **A provider dropdown in the `main` window.** Rejected. The `main` window
  renders model and repository text. A compromised `main` webview could then
  redirect turns. ADR-0059 keeps that choice in `settings`.
- **A built-in Ollama preset without a profile.** Rejected. It would add a
  second source of truth beside recorded profiles, and the endpoint would not
  be journaled or reviewable.
- **A separate Ollama adapter using `/api/chat`.** Rejected for now. The
  native API can set `num_ctx`, but it would duplicate the streaming,
  cancellation, and tool-call parsing that the OpenAI-compatible path already
  has and tests.
- **Re-resolving silently when the `main` window's endpoint is stale.**
  Rejected. The operator typed the model name for the previous provider. A
  silent switch could send a turn to a server that runs a different model.
- **A native confirmation for adding or removing a connection.** Rejected as
  unnecessary. Adding grants nothing until selected, and removing narrows.
- **Saving a model name per connection.** Deferred. The chat preferences keep
  one model name.

## Consequences

- Operators can switch between LM Studio and Ollama without restarting the
  desktop shell, after one native confirmation each time.
- The desktop has three more `settings` commands (31 in total). Each needs its
  capability permission and its policy entry, and the layer tests count them.
- The settings webview now carries a typed loopback endpoint. Core validates
  it, and the native dialog shows it before any turn uses it.
- Ollama's context length must be configured in Ollama. A larger
  `--context-tokens` value does not change what the server keeps.
- Desktop selection was tested on Windows with a mock server and the real
  Ollama server through the CLI. It has not been run on Linux.
- The rest of phase S3 remains: API-key entry and credential removal in the
  window.

## References

- [ADR-0047](0047-read-the-local-provider-token-from-the-credential-store.md)
- [ADR-0048](0048-bind-a-stored-credential-to-its-profile-endpoint.md)
- [ADR-0059](0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md)
- [ADR-0060](0060-change-workspace-settings-from-the-desktop-with-native-dialogs.md)
- [Provider authentication](../provider-authentication.md)
- [Desktop interface](../desktop-interface.md)
