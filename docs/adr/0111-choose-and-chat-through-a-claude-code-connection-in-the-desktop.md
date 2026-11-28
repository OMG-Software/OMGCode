# 0111: Choose and chat through a Claude Code connection in the desktop

- Status: Accepted
- Date: 2026-11-28
- Amends: [ADR-0109](0109-drive-the-signed-in-claude-code-cli-as-a-model-connection.md)

## Context

[ADR-0109](0109-drive-the-signed-in-claude-code-cli-as-a-model-connection.md)
added a `claude_code` connection: OMGCode starts the operator's own
signed-in, unmodified `claude` program for each model request, confined on
Windows to the connection's own AppContainer, and reaches it with OMGCode's
tools only through a relayed MCP server. Its decision 10 kept the connection
on the trusted CLI. The desktop shell refused it with
`claude_code_cli_only`, and core's selectable rule, which needs a recorded
endpoint, dropped it from every provider list before that refusal was
reached. An operator who added the connection could not see it where the
desktop chooses a provider: the per-session box in the `main` window
([ADR-0071](0071-choose-the-provider-and-model-per-session-and-add-ollama-cloud.md),
[ADR-0089](0089-choose-the-provider-and-model-in-the-side-panel-footer.md)).

A Claude Code connection differs from an HTTP connection in four ways that
the desktop must handle:

- it has no endpoint, so the session record and the manifest need a label
  that is not a URL;
- its MCP relay is an `omgcode` executable, and on Windows the connection's
  container must hold a grant for that executable's folder;
- its process can take up to about 15 seconds to stop after a cancel (the
  exit and worker grace periods of ADR-0109 decision 9), longer than the
  shell's five-second shutdown grace;
- `claude` has no model-list request, and it takes only the efforts `low`
  to `max`.

## Decision

1. **A per-session choice only.** The `main` window's per-session provider
   box lists every recorded `claude_code` connection, labelled Claude Code.
   Choosing one needs the same native confirmation as any other provider
   change. Its text names the `claude` program and the sign-in folder, and
   says that OMGCode starts that program, signed in with that folder's
   account, for each model request of the session. The global connection in
   the `settings` window, the desktop's `--provider-profile` launch argument,
   and `omgcode models` still refuse the kind. A global default would send
   every session that has no choice of its own through a program the operator
   did not confirm for that session.
2. **The recorded label and profile.** The session record keeps the
   profile name, the label `claude-code:<profile>`, the one the CLI writes
   into each request manifest, and the profile's identifier. The label holds
   only the name, and core gives each added profile a new identifier and
   lets no profile's program or folder change, so the identifier names the
   program and sign-in folder the confirmation named. The record grants
   nothing. Every turn opens the settings store again. The turn is refused
   with `session_connection_stale` when the profile is gone, is no longer a
   `claude_code` connection, has another label, or has another identifier.
   A connection removed and added again under the same name is therefore
   stale for every session that chose the first one, and choosing it again
   is a change that the session confirms. Status and reselection apply the
   same rule. It then resolves the connection through core's
   `resolve_claude_code` with the relay of decision 3, which checks the
   recorded program, the sign-in folder, its overlap rules, and on Windows
   every grant. A refusal there fails the turn with `claude_code_unusable`
   and core's reason, before any process starts. The paths core resolves
   for the turn must carry the identifier the session confirmed, because
   they are read in a later hold of the settings store than the label.
3. **The relay is the `omgcode` beside the desktop.** The desktop shell
   passes the `omgcode` executable (`omgcode.exe` on Windows) in the folder of
   its own canonical executable, when that file exists. Every release
   archive ships both executables in one folder. Core identifies the relay
   folder as a directory, so `omgcode workspace claude-code-grant` run from
   the same folder grants what the desktop's turns need. When no such file
   exists, a Claude Code turn fails with `claude_code_unusable` and starts
   nothing.
4. **Turns as on the CLI.** The desktop builds the same clients the CLI
   builds for the connection (ADR-0109 decisions 3 to 9). No token is read or
   kept for the turn. A tool call that needs an approval waits in the turn
   as any desktop turn waits
   ([ADR-0063](0063-deny-prompt-or-allow-each-preference-wait-for-approvals-in-the-turn-and-journal-model-actions.md)),
   while `claude` holds its MCP request open. The process deadline is the
   turn timeout, so the wait counts against it.
5. **Models and efforts.** The model box offers the fixed Claude Code
   aliases `sonnet`, `opus`, `haiku`, `fable`, `sonnet[1m]`, and `opus[1m]`,
   because `claude` has no model-list request. The aliases `default`,
   `best`, and `opusplan` are left out because they name a plan-dependent or
   mixed choice. The **Effort** box offers `low`, `medium`, `high`, `xhigh`,
   and `max`, the values that `claude --effort` takes. `none` and `minimal`
   are refused before the turn. The program still decides what the account
   may use.
6. **Shutdown waits for the process.** While a Claude Code turn runs, the
   shutdown wait is at least core's `CLAUDE_CODE_TURN_STOP_WAIT` of 30
   seconds instead of the shell's five, so the cancel can stop the process
   tree and observe its end.
7. **Add, grant, and revoke in `settings`.** The `settings` window adds a
   Claude Code connection: Rust opens a native file dialog for the `claude`
   program and a native folder dialog for the sign-in folder, shows one
   native confirmation that names both, and calls core's
   `add_claude_code_profile`, which keeps every check of ADR-0109 decision 2.
   Signing in stays in a terminal (`claude auth login` with
   `CLAUDE_CONFIG_DIR` set to the folder); the window shows that command and
   OMGCode never runs it. On Windows the window grants the connection's
   container its four folders, after a confirmation that names the program,
   sign-in, runtime, and relay folders, through core's `grant_claude_code`
   with the relay folder of decision 3. It revokes them through
   `revoke_claude_code_grants` after a confirmation and waits up to 30
   seconds for the connection's turns to end, as the CLI does. Removing a
   connection is still refused while it holds grants.

## Alternatives considered

- **Keep the connection on the CLI.** Rejected: the operator chooses the
  provider in the desktop, and the CLI-only rule hid a recorded connection
  there without saying why.
- **Let the desktop executable serve as its own relay.** Rejected: the
  desktop is a GUI-subsystem program on Windows, and its standard streams
  inside the AppContainer were not verified. The CLI relay is the tested
  path.
- **Clear a session's Claude Code choice when the connection is removed.**
  Rejected: `omgcode workspace provider-profile-remove` does not touch the
  desktop's session-choice file, and a removal while the desktop is closed
  would be missed. The recorded identifier covers every route.
- **Allow the connection as the global default.** Rejected for decision 1's
  reason. It can be reconsidered once a turn of the real `claude` has been
  observed in the desktop.
- **Ask `claude` for its models.** Rejected: there is no list request, and
  starting the program to learn its models would run it outside a turn.
- **Accept a typed model name.** Not done: the model box chooses from a
  list for every provider (ADR-0066), and the aliases cover the plans.

## Consequences

- The desktop starts the operator's `claude` program, so it reaches
  everything that program can reach inside its container. The ADR-0109
  confinement, overlap checks, and grant checks apply unchanged, and core
  checks them on every turn.
- A Claude Code turn on Linux needs the desktop to run in a delegated cgroup
  v2, as the CLI does. Without one the turn fails with `claude_code_failed`.
  The Linux desktop path is not verified.
- A turn whose process tree could not be confirmed ended keeps its record
  until the operator acknowledges it with the trusted CLI. The desktop keeps
  its own hold on that record until it exits, so the operator restarts the
  application before acknowledging.
- When the settings store stays busy for two seconds at a turn's start or
  end, the turn can be withdrawn, as on the CLI.
- The session-choice file keeps version 1. Only a Claude Code choice writes
  the optional `profile_id`, so other records keep their earlier shape; a
  build before 0.74.0 finds a file with one unreadable and treats it as
  empty, so its sessions follow the default connection there.
- No signed-in turn of the real `claude` has run in the desktop or on the
  CLI. The desktop tests drive the choice, the confirmation, and the
  refusals before a process starts.

## References

- [ADR-0063](0063-deny-prompt-or-allow-each-preference-wait-for-approvals-in-the-turn-and-journal-model-actions.md)
- [ADR-0066](0066-choose-the-chat-model-from-the-local-servers-model-list.md)
- [ADR-0071](0071-choose-the-provider-and-model-per-session-and-add-ollama-cloud.md)
- [ADR-0089](0089-choose-the-provider-and-model-in-the-side-panel-footer.md)
- [ADR-0102](0102-choose-a-reasoning-effort-per-session.md)
- [ADR-0109](0109-drive-the-signed-in-claude-code-cli-as-a-model-connection.md)
- [Desktop interface](../desktop-interface.md)
