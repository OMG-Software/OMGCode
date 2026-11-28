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

Adding the connection the way the CLI does meant choosing the `claude`
program and a sign-in folder by hand and signing in from a terminal first.
An operator who chose Claude Code expected the desktop to find the installed
program and let them sign in, as the `settings` window does for ChatGPT
([ADR-0093](0093-sign-in-with-chatgpt-and-use-the-plan-through-the-responses-api.md)).

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
7. **Add finds the program, creates the folder, grants, and starts the
   sign-in.** **Add connection** for Claude Code in the `settings` window
   takes only a name.
   - *The program.* Rust looks for `claude` (`claude.exe` on Windows) in
     each absolute folder of the operator's `PATH`, in order, and then in
     the native installer's folder `~/.local/bin` (and `~/.claude/local`
     outside Windows), through core's `claude_code_program_candidates`,
     which opens and runs nothing. It keeps the first file that core's
     `check_claude_code_profile` accepts, so a `claude` in the development
     root or a workspace is skipped. Only when no such file exists does a
     native file dialog ask for the program. When files exist but core
     refuses each, the first refusal is reported, so an invalid name is not
     hidden behind a dialog.
   - *The sign-in folder.* Rust creates the connection's own folder, new,
     empty, and named at random, under `omgcode-claude-code` in the
     per-user local data folder (`%LOCALAPPDATA%` on Windows), which is not
     application storage. A cancelled or refused add removes it.
   - *One confirmation, then add and grant.* One native confirmation names
     the program, the folder, and on Windows the folders the connection's
     container is granted. Core's `add_claude_code_profile` records the
     connection with every check of ADR-0109 decision 2, and on Windows
     `grant_claude_code` grants the four folders with the relay of decision
     3. On Windows Add is refused before anything is created when there is
     no relay. A grant that fails leaves the connection recorded, and the
     error names the command that signs in from a terminal.
   - *Claude's own sign-in.* On Windows Rust then starts the recorded
     program as `claude auth login` in a console window of its own, outside
     the container, with no inherited handle, the folder as its working
     folder and as `CLAUDE_CONFIG_DIR`, and an environment built from
     nothing: the system and profile folder variables, a `PATH` of the
     system folders, `ComSpec`, `PATHEXT`, `CLAUDE_CODE_SAFE_MODE`,
     `DISABLE_AUTOUPDATER`, and `DISABLE_TELEMETRY`. The operator finishes it in the browser. OMGCode
     waits for nothing, reads nothing in the folder, and never sees the
     credential. This amends ADR-0109 decision 2 for the desktop only in
     that OMGCode starts Claude's own sign-in program. Outside Windows the
     confirmation names the command to run in a terminal instead. A sign-in
     that does not start leaves the connection recorded and names that
     command.
   - *Only a new folder.* The sign-in starts only for the folder Add has
     just created. Once a turn has run, the container could have written the
     folder, and the sign-in runs outside the container. To sign in again,
     the operator revokes the grants, removes the connection, and adds it
     again.
   - *Grant and revoke.* **Grant folders** grants the four folders again
     after a revocation, after a confirmation that names them. **Revoke
     folders** revokes them through `revoke_claude_code_grants` after a
     confirmation and waits up to 30 seconds for the connection's turns to
     end, as the CLI does. Removing a connection is still refused while it
     holds grants.

## Alternatives considered

- **Ask for the program and the sign-in folder in dialogs, and sign in from
  a terminal first.** The first form of decision 7. Rejected: it asked the
  operator for a program the desktop can find and for a terminal step the
  desktop can start.
- **Offer Sign in again on the connection's row.** Rejected: by then the
  connection's container may have written the folder, for example a
  settings file naming a command that a sign-in outside the container would
  run. Safe mode switches off hooks and plugins, but it is not documented to
  ignore every setting.
- **Run the sign-in in the connection's container.** Rejected: the sign-in
  needs the operator's browser, and a browser sign-in that returns to a
  local callback is not received by an AppContainer without a loopback
  exemption.
- **Check `claude auth status` first and skip a signed-in folder.** Not
  done: the folder is new, so it is never signed in, and the check would
  start the program once more.
- **Use the operator's own `~/.claude` sign-in.** Rejected: ADR-0109 keeps
  each connection's sign-in in a folder of its own, because the container
  can read and write it.
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
- Add starts the operator's `claude` once outside every sandbox, for
  Claude's own sign-in, as a terminal would. It is the program the
  confirmation names; a `claude` earlier on `PATH` than the installed one is
  the one chosen, as in a terminal.
- A removed connection's sign-in folder stays under `omgcode-claude-code`,
  still signed in, until the operator deletes it. Nothing records it.
- A development root that contains the per-user local data folder makes
  core refuse every new sign-in folder, and Add reports that refusal.
- `claude auth status --json` of `claude` 2.1.325 on Windows, run with the
  sign-in's environment and a new folder, reported that folder as its
  configuration directory and the connection as not signed in, and wrote
  its own state there. OMGCode's tests record the sign-in instead of
  starting it, and a platform test starts a fixture in a console window of
  its own. No Claude sign-in was completed through OMGCode.
- No signed-in turn of the real `claude` has run in the desktop or on the
  CLI. The desktop tests drive the add, the choice, the confirmations, and
  the refusals before a process starts.

## References

- [ADR-0063](0063-deny-prompt-or-allow-each-preference-wait-for-approvals-in-the-turn-and-journal-model-actions.md)
- [ADR-0066](0066-choose-the-chat-model-from-the-local-servers-model-list.md)
- [ADR-0071](0071-choose-the-provider-and-model-per-session-and-add-ollama-cloud.md)
- [ADR-0089](0089-choose-the-provider-and-model-in-the-side-panel-footer.md)
- [ADR-0093](0093-sign-in-with-chatgpt-and-use-the-plan-through-the-responses-api.md)
- [ADR-0102](0102-choose-a-reasoning-effort-per-session.md)
- [ADR-0109](0109-drive-the-signed-in-claude-code-cli-as-a-model-connection.md)
- [Desktop interface](../desktop-interface.md)
