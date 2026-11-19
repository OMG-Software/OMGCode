# 0064: Choose the model connection in settings and add Ollama Local

- Status: Accepted
- Date: 2026-11-19
- Implemented: version 0.44.0 (the connection part of ADR-0059 phase S3)

## Context

The user asked for Ollama support and a dropdown to choose the language-model
provider. After the first version, which put the dropdown only in the
`settings` window, the user asked for the choice to be shown and changeable
from a status bar at the bottom of the `main` window, with the Git branch and
the project workspace beside it. Before this change the only adapter was the local LM Studio client.
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
   because the profile cannot bind a credential. Ollama's buffered replies
   give each tool call a numeric `index`; the adapter drops only that member,
   as the stream assembly already does, so the runtime's strict tool-call
   parser stays unchanged and still refuses any other member. CLI `chat --provider-profile`
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
5. **The `main` window only requests a choice.** Its status bar may call
   `connection_select` with a profile name, or `null` for the default, and
   nothing else. Rust then runs the same checks and the same native Yes/No
   dialog as decision 3, and records nothing unless the operator confirms.
   `connection_status` gives the `main` window the current connection and
   the selectable local profiles: names, kinds, and loopback endpoints, never
   a credential or its binding. A turn still sends back the endpoint text the
   window read from `chat_defaults`. If that no longer equals the endpoint
   core resolves, `chat_send` refuses with `connection_changed`. The window
   then reads the endpoint again and keeps the draft and model name for the
   operator to check before sending again.
6. **The `settings` window may record and remove local connections.**
   `settings_add_local_connection` records an `lm_studio` or `ollama_local`
   profile with authentication `none` and a typed endpoint. Core applies the
   loopback endpoint rule. This amends ADR-0059 decision 7, which said the
   webview never supplies an endpoint: the `settings` window may supply one
   for a new profile, and nothing is sent to it until a selection is
   confirmed natively. Adding needs no native dialog because a profile
   grants nothing until it is selected. `settings_remove_connection` removes
   a profile without a bound credential and deletes no secret. Removing an
   inactive profile changes where no turn goes and needs no confirmation.
   Removing the active profile returns new turns to the default endpoint,
   which receives `OMGCODE_LM_TOKEN` when it is set, so unless a launch
   argument fixed the connection Rust first shows the same native Yes/No as
   choosing the default. A profile with a stored
   credential is refused with `credential_bound` and is removed with the
   trusted CLI. Typing API keys in the window (ADR-0059 decision 6) is not
   implemented.
7. **The status bar shows the workspace and its Git branch.** The `main`
   window's footer shows the open session's workspace folder and, from
   `session_context`, the branch its workspace has checked out. Core reads
   only `HEAD`, at most 4096 bytes, through the anchored broker, opened by the
   recorded root and workspace identities, with no link followed and no Git
   program, configuration, ref, or object read. A `.git` file that names a
   linked worktree's directory is followed only when that directory lies
   inside the development root; the read is then made from the root's handle.
   The branch is display-safe text, never model context, and grants nothing.
   A detached `HEAD` shows seven hexadecimal digits. Anything unusual is shown
   as "Branch unavailable" with a fixed reason, never file content.
8. **The trusted CLI can select too.** `workspace provider-profile-select
   <name>` and `--default` call the same core function. CLI `chat` is not
   affected by the selection: it keeps its explicit `--endpoint` and
   `--provider-profile`.

## Alternatives considered

- **A provider dropdown in the `main` window that switches directly.**
  Rejected. The `main` window renders model and repository text, and a
  compromised `main` webview could then redirect turns. The first version of
  this record rejected any `main` window choice for that reason; the user then
  asked for one. The native dialog keeps the decision with the operator, and
  the window can name only a recorded loopback profile, so a compromised
  window can at most ask.
- **A status bar that only shows the provider and opens `settings` to change
  it.** Rejected after the user chose a choice in the status bar itself. It
  would keep `main` without any connection command, at the cost of a second
  window for each switch.
- **Running `git branch --show-current` or reading Git configuration.**
  Rejected. The security rules forbid a Git process in this path and treat
  Git metadata as untrusted. `HEAD` alone names the branch.
- **Following a linked worktree's `gitdir` anywhere.** Rejected. A pointer
  outside the development root would read a file the workspace contract does
  not cover. Such a branch is shown as unavailable.
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
- **A native confirmation for adding or for removing any connection.**
  Rejected as unnecessary. Adding grants nothing until selected, and
  removing an inactive profile changes no destination.
- **Removing the active connection without a confirmation.** Rejected after
  review. It switches turns to the default endpoint and may send
  `OMGCODE_LM_TOKEN` there, which the explicit choice of the default
  confirms.
- **Saving a model name per connection.** Deferred. The chat preferences keep
  one model name.

## Consequences

- Operators can switch between LM Studio and Ollama without restarting the
  desktop shell, after one native confirmation each time.
- The desktop has three more `settings` commands and three more `main`
  commands (34 in total: 22 `main`, 3 `approvals`, 9 `settings`). Each needs
  its capability permission and its policy entry, and the layer tests count
  them. `connection_select` is the only `main` command that opens a native
  dialog.
- The `main` webview now learns the recorded local profile names and their
  loopback endpoints. A compromised `main` webview can raise the native
  confirmation repeatedly; each one blocks only its own change, and changes
  are serialized.
- The branch display was tested with linked worktree pointers in core tests.
  A pointer outside the development root, such as a worktree whose repository
  lives elsewhere, shows "Branch unavailable".
- The settings webview now carries a typed loopback endpoint. Core validates
  it, and the native dialog shows it before any turn uses it.
- Ollama's context length must be configured in Ollama. A larger
  `--context-tokens` value does not change what the server keeps.
- Desktop selection was tested on Windows with a mock server and the real
  Ollama server through the CLI. The status bar was tested with the frontend
  tests and the desktop service tests, not in a running shell. None of it has
  been run on Linux.
- The rest of phase S3 remains: API-key entry and credential removal in the
  window.

## References

- [ADR-0047](0047-read-the-local-provider-token-from-the-credential-store.md)
- [ADR-0048](0048-bind-a-stored-credential-to-its-profile-endpoint.md)
- [ADR-0059](0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md)
- [ADR-0060](0060-change-workspace-settings-from-the-desktop-with-native-dialogs.md)
- [Provider authentication](../provider-authentication.md)
- [Desktop interface](../desktop-interface.md)
