# Desktop interface

This guide owns the desktop shell: what it is, how to start it, what each
window may call, how the session tree, search, chat, and approvals behave, and
what the design does and does not defend. It is a prototype. It is not
release-ready, and it has no installer. CI publishes unsigned archives of
the binaries as a rolling `latest` prerelease
([ADR-0052](adr/0052-publish-rolling-testing-builds-from-main.md)).

The decisions behind it are recorded in
[ADR-0044](adr/0044-observe-journals-without-the-writer-lock-and-index-them-in-sqlite.md),
[ADR-0045](adr/0045-build-the-desktop-shell-on-tauri-with-a-narrow-command-surface.md),
[ADR-0046](adr/0046-decide-model-tool-approvals-in-an-isolated-desktop-window.md),
[ADR-0049](adr/0049-stream-local-model-replies-and-push-chat-events.md), and
[ADR-0050](adr/0050-interrupt-a-streamed-model-request-on-cancellation.md).
The trusted CLI and the contracts it enforces are unchanged. Other guides own
the rules this shell relies on: [session storage](session-storage.md),
[approval records](agent-instructions.md#approval-records),
[execution](execution.md), [workspace file access](file-access.md), and the
[security coding rules](security-coding-rules.md).

## Status and scope

Implemented (Phase E, version 0.29.0, branch `codex/desktop-interface`; not
merged to `main` at the time of writing):

- A Tauri 2 shell with two windows, `main` and `approvals`. Version 0.40.0
  adds a third, `settings`.
- A session tree grouped by workspace folder, with archived and active views.
- Full-text search over session titles and transcript text.
- A paged transcript view that reads a journal without taking the writer lock.
- Chat turns against the local model endpoint, using the same chat turn as the
  trusted CLI.
- Approval review and decision in the isolated `approvals` window.
- Archive, restore, and interrupt for sessions.
- A derived SQLite index of the sessions.
- Since version 0.30.0 (Phase D2, ADR-0047), an optional launch argument,
  `--provider-profile`, makes the service read the local server token from a
  stored credential instead of `OMGCODE_LM_TOKEN`.
- Since version 0.31.0 (Phase D3, ADR-0048), a selected profile also supplies
  the endpoint. `--endpoint` and `--provider-profile` are mutually exclusive.
- Since version 0.44.0 (ADR-0064), the `settings` window chooses the model
  connection from a dropdown: the default LM Studio endpoint or a recorded
  `lm_studio` or `ollama_local` profile, after a native confirmation. The
  `settings` window can also add and remove local connections without a
  credential. The status bar at the bottom of the `main` window shows the
  open session's workspace and its Git branch. It offered the same
  dropdown until ADR-0084 removed it; the model box in the
  right-hand panel's footer chooses a session's provider (ADR-0089). A launch argument still fixes the connection for the
  process.
- Since version 0.32.0 (Phase D4 and E6, ADR-0049), a chat turn streams the
  local model's reply and the shell pushes chat events to the `main` window
  through one caller-scoped channel. The window shows the provisional text
  while the turn runs, and no longer polls for the end of a turn. The launch
  argument `--stream off` restores non-streaming requests.
- Since version 0.33.0 (Phase D5, ADR-0050), cancelling a streamed turn
  interrupts its model request, and the turn ends as cancelled. Exit cancels
  every running streamed turn within the grace period.
- Since version 0.34.0 (Phase D6, ADR-0051), cancelling also interrupts a
  non-streamed request (`--stream off`) and the compaction request before a
  turn. No command or wire type changed.
- Since version 0.40.0 (Phase S1, ADR-0059), an isolated `settings` window
  shows a read-only view of the trusted configuration. The `main` window
  saves its chat preferences and can select up to four enabled skills for
  one turn. See [settings window](#settings-window).
- Since version 0.41.0 (Phase S2, ADR-0060), the `settings` window changes
  the development root, workspace registrations, all six workspace
  preferences, and package cache roots. Rust opens a native folder dialog for
  every folder and a native confirmation before every change that widens
  access. The settings store is created empty the first time the window
  reads it, so no CLI command is needed first.
- Since version 0.42.0 (ADR-0062), Enter sends the composer's message and
  Shift+Enter adds a newline. An **Enter to send** chat preference, on by
  default, can be turned off to restore Ctrl+Enter. See [chat](#chat).
- Since version 0.43.0 (ADR-0063), each preference except `model_tools` is
  Deny, Prompt, or Allow, and each model file-write tool has its own limit. A
  chat turn waits for each approval it needs, and an approved call completes
  in the same turn. Every model tool call is recorded in the model action
  journal.
- Since version 0.45.0 (ADR-0065), all three windows use one dark theme and
  new layouts. The `approvals` window shows its queue beside a fixed review
  pane, the `settings` window is split into Model, Workspaces, and Advanced
  sections, and the `main` window moves its view options, skills, and chat
  settings into menus. No command, IPC shape, or permission changed. See
  [appearance](#appearance).
- Since version 0.51.0 (ADR-0072), the `main` window's sessions panel and a
  new terminal panel can be resized and collapsed to a rail of icons, chat
  entries are centred and follow the window's width, and the transcript
  follows its end after a send until the operator scrolls up. The terminal
  panel holds no terminal. No command, IPC shape, or permission changed. See
  [appearance](#appearance) and [transcript view](#transcript-view).
- Since version 0.56.0 (ADR-0083, ADR-0085 to ADR-0087), the `main`
  window's right-hand panel has tabs. The Terminal tab keeps its note;
  **Files** shows a lazily expanded tree of the open session's workspace
  through `workspace_entries`; **Git** shows the branch and changed paths
  through `workspace_git_status`; **Processes** lists the session's model
  command-tool calls through `session_processes`; and **Context** estimates
  the next turn by part through `session_context_breakdown`. A fixed footer
  sits under every tab. See [right-hand panel](#right-hand-panel).
- Since version 0.64.0 (ADR-0098), the right-hand panel has a **Web** tab
  that lists the open session's `web_fetch` calls and every model action
  made after its first fetched web content, through `session_web_audit`.
  The approvals window reviews a `network` approval as **Web fetch**, and
  the `settings` window sets each workspace's `web_fetch` preference and
  edits its host allowlist. See [Web tab](#web-tab).
- Since version 0.66.0 (ADR-0101), every section divider moves: the
  `approvals` queue's, the `settings` section list's, the line above the
  `main` window's composer, which sets the message box's height, and the
  line above the right-hand panel's footer. Each is a window splitter like
  the column separators, without collapse. No command, IPC shape, or
  permission changed. See [section dividers](#section-dividers).
- Since version 0.56.0 (ADR-0089, ADR-0084), the provider and model choice
  moved from the composer to a box in the right-hand panel's fixed footer,
  with a **Model** rail icon when the panel is collapsed, and the status bar
  offers no connection choice.
- Since version 0.56.0 (ADR-0081, ADR-0082, ADR-0088), the `settings` window
  lists workspaces under **Workspaces** in its navigation, unregisters a
  workspace, manages skills, and adds and removes toolchain profiles,
  package-network profiles, and, on Windows, container grants. See
  [settings window](#settings-window).
- Since version 0.68.0 (ADR-0103), the **Toolchain profiles** group generates
  profile proposals from the toolchains installed on this computer. Since
  0.70.0 (ADR-0107), adding them with a workspace chosen also assigns them to
  it and, on Windows, grants that workspace's container the folders it needs
  that this account can change, in the same native confirmation; folders that
  need an administrator, or whose access-control list could not be read, are
  named and skipped. Discovery runs no program.
- Since version 0.70.0 (ADR-0107), each workspace's page has a switch per
  toolchain profile of this platform. Turning one on assigns the profile to
  the workspace and, on Windows, grants its container the profile's folders,
  after a native confirmation that names them; turning one off revokes the
  grants no other assigned profile needs. On Windows, registering a workspace
  grants its container read and write on its folder, and removing one revokes
  its grants first.

Not implemented:

- Installers, signing, and Tauri bundles. `bundle.active` is `false`. The
  `latest` archives hold the bare binaries only.
- Interrupting a read, list, or edit tool call in progress. Cancelling such a
  turn waits for the call to end.
- Binding a credential to a local
  LM Studio profile, which changes only through the trusted CLI. The
  `settings` window adds a hosted connection (Ollama Cloud since ADR-0071, the
  other hosted providers and an OpenAI-compatible service since ADR-0092) with
  its API key, replaces the key, and removes the connection, through the
  write-only key field. The rest of phase
  S3 (a local profile's credential) and phases S4 and S5 of
  [ADR-0059](adr/0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md)
  are not implemented: credentials and the trusted package fetch and run. Skills
  are imported, enabled, and removed in the `settings` window since ADR-0082,
  and toolchain profiles, package-network profiles, and container grants are
  added and removed there since ADR-0088 (none can be edited).
- Development panels: a working terminal, task logs, and diffs. The Terminal
  tab (ADR-0072) only says that no terminal is available; it starts no shell
  and runs no command. The Files tab (ADR-0083) shows file and folder names
  only, with no file content, search, or editing.
- Git actions (the Git tab only reads the status: no diff, staging, commit,
  or branch change), MCP, and memory interfaces. The `main` window
  can only select an already enabled skill for one turn; the `settings`
  window manages skills (ADR-0082). Skill updates, Git sources, and the
  per-skill description and body view are not implemented.
- A recovery interface for damaged journals or an incomplete tail.
- A provider API other than OpenAI-compatible Chat Completions and the
  ChatGPT plan's Responses route (ADR-0093), such as Anthropic's native
  Messages API, and any subscription sign-in other than ChatGPT. Stored API keys
  are used only for an LM Studio server's token and a hosted connection's key.
  The `main` window shows nothing about the credential source beyond the
  endpoint text and the hosted label. A model the server does not list, such
  as a Thinking Machines Lab checkpoint, cannot be chosen in the model box.
- A native operating-system confirmation dialog for approvals.

Platform status. The shell was built, tested, and driven end to end on Windows
11 (build 26300) with WebView2 on 2026-11-16. On Linux (WSL 6.6, Ubuntu 24.04)
the whole workspace, including this crate, compiled against WebKitGTK and
passed formatting, Clippy, and every test on the same day. The Linux binary was
started only far enough to check its launch arguments: no window has been
opened on Linux, so the webview, IPC, capability enforcement, and navigation
guard are unverified there. The CI workflows now build and test this crate on
Ubuntu and Windows Server 2025; that compiles and tests it but opens no
window ([repository checks](repository-checks.md#rust-and-frontend-workflows)).
See [platform support](platform-support.md).

## Architecture

| Layer | Crate or directory | Owns |
| --- | --- | --- |
| Core | `crates/omgcode-core` | Journal inspection, discovery, the session index, approval review, and every authorization decision |
| Application | `crates/omgcode-app` | The chat turn, session creation, compaction, the data-root resolver, token redaction, and `DesktopService` |
| Shell | `crates/omgcode-desktop` | Windows, capability files, the window policy, and one handler per command |
| Frontend | `crates/omgcode-desktop/ui` | The strict TypeScript interface that renders the results |

`omgcode-app` is front-end neutral. The trusted CLI and the shell both call it,
so they run the same chat turn. It grants no authority: workspace
preferences, approvals, checkpointed edits, confinement, and journal
validation are still decided in `omgcode-core`. The CLI is a thin adapter over
it; `model_tools.rs` and `context_cli.rs` moved from `omgcode-cli` into it.

The shell is a thin adapter. Each handler checks the calling window, runs the
service in a blocking task, and returns the result. It holds no policy of its
own beyond the window table.

The webview is untrusted. It renders text that models, repositories, and other
users can influence. The Rust side is the only authority.

## Start the desktop shell

Build the frontend first. The shell embeds `ui/dist` at compile time, and any
Cargo command that includes the crate needs it, including `check`, `clippy`,
`build`, and `test`:

```text
cd crates/omgcode-desktop/ui
npm ci
npm run build
cd ../../..
cargo build -p omgcode-desktop --locked
```

Without `ui/dist`, Cargo fails with a message that contains:

```text
proc macro panicked ... The `frontendDist` configuration is set to "ui/dist"
but this path doesn't exist
```

The frontend needs Node 24. A C compiler is needed for the bundled SQLite. On
Linux, the Tauri build also needs the WebKitGTK development packages listed in
[platform support](platform-support.md#desktop-shell).

Launch:

```text
omgcode-desktop [--data-dir <path>] [--endpoint <url> | --provider-profile <name>]
                [--stream <on|off>]
```

| Argument | Default | Meaning |
| --- | --- | --- |
| `--data-dir <path>` | The per-user OMGCode data directory | The application data root, the same one the CLI uses |
| `--endpoint <url>` | None: the connection chosen in the `settings` window, else `http://127.0.0.1:1234/v1` | The loopback model endpoint, fixed for the process. Not allowed with `--provider-profile` |
| `--provider-profile <name>` | None: as for `--endpoint`, with the token from `OMGCODE_LM_TOKEN` for the default endpoint | A recorded `lm_studio` or `ollama_local` provider profile that supplies the endpoint and, for `lm_studio`, the stored credential used as the server token, fixed for the process. Not allowed with `--endpoint` |
| `--stream <on\|off>` | `on` | Whether chat turns send streamed model requests and push their text to the window. `off` sends non-streaming requests. The `finished` event is still pushed |

An unknown argument, a missing value, a repeated flag, an empty value, a value
that starts with `--`, a `--stream` value other than `on` or `off`, or both
`--endpoint` and `--provider-profile` prints the usage text to stderr and exits with status 2. An invalid endpoint, an unusable data
root, or a service that cannot open prints `omgcode-desktop: <message>` to
stderr and exits with status 1.
With `--provider-profile`, the shell resolves the profile once at launch. The
profile's recorded endpoint becomes the endpoint of the process, and the shell
also resolves the credential once. An unknown profile, a profile of another
provider kind, a profile with no recorded endpoint, an unbound profile, or an
unreadable credential prints `omgcode-desktop: credential: <cause>: <text>` and
exits with status 1. Both kinds of failure happen before any window opens. A
normal close exits with status 0.

With neither `--endpoint` nor `--provider-profile`, the connection is not
fixed. Each chat turn resolves the active connection recorded in the
settings store when it starts: a profile chosen in the `settings` window
(ADR-0064), or, with none chosen, `http://127.0.0.1:1234/v1` and
`OMGCODE_LM_TOKEN`. An unusable selection fails the turn, not the launch, so
the `settings` window stays reachable to change it.

The command line and that recorded selection are the only configuration. The
shell reads no other environment variable and no configuration file for these
values. The local server token has two sources. For the default endpoint or
`--endpoint`, if `OMGCODE_LM_TOKEN` is set, the service reads it for each chat
turn. For a profile, from `--provider-profile` or chosen in settings, the
service reads the credential bound to it from protected OS storage for each
chat turn and never reads the variable; an `ollama_local` profile has no
credential and sends no token. The flag carries a profile name, not a secret.
The endpoint comes from the same source as the token. No webview receives the
token. The `main` window cannot choose or see the profile. The service caches nothing, so a rotated credential is used by
the next turn. An unreadable credential makes `chat_send` fail at once with a
`credential` error, and no turn thread starts. See
[stored credential use](provider-authentication.md#stored-credential-use-0300)
and [endpoint binding](provider-authentication.md#endpoint-binding-0310).

A Windows release build has no console window. Stderr is visible only when the
process is started with redirection.

Use a scratch `--data-dir` for development. The default is the real per-user
data root, which holds real sessions and workspace registrations.

Chat tools follow the workspace settings, as in the CLI. A command tool turn
runs a process through the trusted runner, which on Linux requires the process
to run inside a delegated cgroup v2 and refuses otherwise
([execution](execution.md)). That behaviour is inherited. The shell on Linux
has not been run, so how it behaves in a delegated cgroup is unverified.

## Windows and commands

The shell has three webview windows and 67 commands (31 in `main`, 3 in `approvals`, and 33 in `settings`). No other command exists.
No command accepts a host path. Sessions are addressed by UUID, workspaces by
their registered identifier, and approvals by identifier.

| Command | Window | Purpose |
| --- | --- | --- |
| `app_info` | main | Version, platform, and whether the build supports model tools |
| `session_tree` | main | List sessions grouped by workspace folder |
| `session_search` | main | Search titles and transcript text |
| `session_transcript` | main | Read one page of a session transcript |
| `session_archive` | main | Archive an idle session |
| `session_restore` | main | Restore an archived session |
| `session_interrupt` | main | Close a session's unfinished turn as `operator_interrupted` |
| `workspace_list` | main | List the registered workspaces a session can be created in |
| `session_create` | main | Create a session in a registered workspace |
| `chat_defaults` | main | Return the endpoint the next turn uses and the saved or built-in chat preferences |
| `chat_models` | main | List the model ids a session's connection reports, for the composer's model choice; takes the session id |
| `chat_preferences_save` | main | Save the composer's own chat defaults: compaction threshold, auto-compact, and the send key (ADR-0080) |
| `session_skills` | main | List the names of the skills enabled for a session's workspace |
| `session_context_breakdown` | main | Estimate, by part, what a session's next turn would send; takes the session id, sends nothing to a model, records nothing, and returns names and numbers only (ADR-0087) |
| `session_context` | main | Report whether a session's workspace is registered and the Git branch it has checked out, for display |
| `workspace_entries` | main | List the names and kinds in one directory of a session's registered workspace, for the Files tab; takes the session id and relative name components (ADR-0083) |
| `session_processes` | main | List a session's model command-tool calls, newest first, at most 100, read-only (ADR-0086) |
| `session_web_audit` | main | List a session's `web_fetch` calls and every model action marked `after_web_content`, newest first, at most 100, read-only (ADR-0098) |
| `workspace_git_status` | main | Report the Git branch and a bounded list of changed paths for a session's registered workspace, read in process by `gix` with no program run (ADR-0085); takes the session id |
| `connection_status` | main | Return a session's connection, where it comes from, its model, its reasoning effort and the efforts its connection takes, and the connections that may be chosen; takes the session id or `null` |
| `connection_select` | main | Request the default or a recorded profile as one session's provider; Rust shows a native confirmation |
| `session_model_select` | main | Record the model chosen for one session; no dialog |
| `session_effort_select` | main | Record the reasoning effort chosen for one session, or no effort (Default) for `null`; no dialog; an effort the session's connection does not take is refused; `null` is accepted on any connection (ADR-0102) |
| `chat_send` | main | Start one chat turn on a supervised thread |
| `chat_status` | main | Report the state and outcome of a session's turn |
| `chat_cancel` | main | Request cooperative cancellation of a running turn |
| `chat_subscribe` | main | Subscribe a caller-created channel to chat events |
| `approvals_summary` | main | Return the advisory count of pending approvals |
| `approvals_show_window` | main | Show the existing approvals window |
| `settings_show_window` | main | Clear the settings window's workspace scope and show the existing settings window |
| `settings_show_workspace` | main | Scope the settings window to a session's registered workspace, then show it |
| `approvals_list` | approvals | List pending approvals and recent decisions |
| `approval_detail` | approvals | Review one approval and obtain a review token |
| `approval_decide` | approvals | Approve or deny one approval |
| `settings_overview` | settings | Return the trusted configuration, creating an empty settings store when none exists |
| `settings_save_chat_defaults` | settings | Save the chat budgets and the global custom instructions; no dialog (ADR-0080) |
| `settings_set_workspace_instructions` | settings | Replace one registered workspace's custom instructions; empty removes them; no dialog (ADR-0080) |
| `settings_choose_root` | settings | Choose the development root in a native folder dialog, after a native confirmation |
| `settings_register_workspace` | settings | Register a folder below the root, chosen in a native dialog, after a native confirmation; on Windows the confirmation also says the workspace's sandbox gets read and write on the folder, which registering grants (ADR-0107) |
| `settings_set_permissions` | settings | Set one workspace's preferences and per-tool write limits; widening any needs a native confirmation |
| `settings_choose_package_cache` | settings | Choose one workspace's package cache root in a native dialog, after a native confirmation |
| `settings_clear_package_cache` | settings | Clear one workspace's package cache root; files stay in place |
| `settings_add_web_fetch_host` | settings | Add one host pattern to a workspace's `web_fetch` allowlist after a native confirmation that names the folder and the canonical pattern (ADR-0098) |
| `settings_remove_web_fetch_host` | settings | Remove one host pattern from a workspace's `web_fetch` allowlist after a native confirmation (ADR-0098) |
| `settings_unregister_workspace` | settings | Remove one workspace's registration after a native confirmation that names its folder and the container grants revoked first; its settings and instructions file go, its files and sessions stay (ADR-0081, ADR-0107) |
| `settings_select_connection` | settings | Choose the default or a recorded profile with an endpoint (a hosted one also needs a bound credential) as the model connection, after a native confirmation |
| `settings_add_connection` | settings | Record an `lm_studio` or `ollama_local` profile with a loopback endpoint and no credential, a named hosted profile with its fixed endpoint and a write-only API key, or an `openai_compatible` profile with a typed HTTPS endpoint and a write-only API key (ADR-0092) |
| `settings_set_api_key` | settings | Replace the stored API key of a hosted `api_key` profile; the key is write-only and no dialog is shown |
| `settings_remove_connection` | settings | Remove a provider profile; one with a stored API key, or the active one, needs a native confirmation, and the key is deleted with the profile |
| `settings_sign_in_chatgpt` | settings | After a native confirmation, sign a connection in with ChatGPT, recording an `openai` subscription profile first when the name has none; Rust opens the browser and waits at most five minutes (ADR-0093) |
| `settings_cancel_chatgpt_sign_in` | settings | End the running ChatGPT sign-in's or sign-out's wait; takes nothing and grants nothing |
| `settings_sign_out_chatgpt` | settings | After a native confirmation, revoke a ChatGPT session once, record the sign-out, and delete its tokens; reports whether OpenAI confirmed the revocation |
| `settings_clear_scope` | settings | Clear the window's workspace scope and return the unscoped overview |
| `settings_repair_data_folder` | settings | Remove the data folder's extra grants, listed by Rust, after a native confirmation that names each account (ADR-0073) |
| `settings_import_skill` | settings | Import a skill folder, chosen in a native dialog, through core as `omgcode skills add` does; the skill arrives disabled and no confirmation follows (ADR-0082) |
| `settings_set_skill_state` | settings | Set a skill's application setting or one workspace's override (`enabled`, `disabled`, `inherit`); a change that leaves it enabled needs a native confirmation that names it (ADR-0082) |
| `settings_remove_skill` | settings | Remove a skill and delete its stored copy after a native confirmation that names it; the source folder is kept (ADR-0082) |
| `settings_add_toolchain_profile` | settings | Open a native folder dialog for each read-only root and `PATH` entry, then, after a native confirmation that lists them and the variable names, record a toolchain profile as `workspace profile-add` does (ADR-0088) |
| `settings_remove_toolchain_profile` | settings | Remove a toolchain profile after a native confirmation that names each workspace it is assigned to and the grants revoked there; the runs the settings then withdraw are stopped (ADR-0088, ADR-0107) |
| `settings_assign_toolchain_profile` | settings | Assign a toolchain profile to one workspace after a native confirmation that names the workspace, the profile, each folder the workspace's container is granted read and execute, and the folders it cannot grant (ADR-0107) |
| `settings_unassign_toolchain_profile` | settings | End one assignment after a native confirmation that names the grants it revokes and says the workspace's runs stop (ADR-0107) |
| `settings_add_package_profile` | settings | Record a package-network profile (configuration only) after a native confirmation that lists the origins core parsed (ADR-0088) |
| `settings_remove_package_profile` | settings | Remove a package-network profile, which also unbinds and deletes the credentials bound to its origins, after a native confirmation (ADR-0088) |
| `settings_grant_container_folder` | settings | Windows only. Open a native folder dialog and, after a native confirmation that names the workspace, folder, and access and says the folder's access-control list changes, grant it to the workspace's container (ADR-0088) |
| `settings_revoke_container_grant` | settings | Windows only. Revoke one recorded grant after a native confirmation and stop the workspace's earlier runs (ADR-0088) |
| `settings_discover_toolchains` | settings | Read the standard install folders and the operator's `PATH` and return toolchain profile proposals with core's refusal reason for each; with a workspace, on Windows, each root's container access. Runs no program and records nothing (ADR-0103) |
| `settings_add_discovered_profiles` | settings | Discover again and, after one native confirmation that lists every root, `PATH` entry, and variable, record the proposals whose digests the window sent as `workspace profile-add` does; with a workspace, also assign them to it, granting on Windows the folders its container needs, named in the same confirmation (ADR-0103, ADR-0107) |

`main` shows the session tree, search, transcripts, and chat. It renders
transcript text. `approvals` lists approval records, shows the reviewed
request, and holds the only decision controls. It starts hidden and never
renders transcript content. `settings` shows the read-only configuration
view. It starts hidden, never renders transcript text, tool output, or a
skill's description or body, and has no decision control. Closing
`approvals` or `settings` hides it. Closing `main` hides every window, runs a
bounded shutdown, and exits.

## Appearance

Since 0.45.0 ([ADR-0065](adr/0065-redesign-the-desktop-windows-with-one-dark-theme-and-split-layouts.md))
every window uses one dark theme defined in `ui/src/styles/tokens.css`:
near-black greys with an orange accent, and `color-scheme: dark`. Since 0.61.0
([ADR-0095](adr/0095-use-the-omg-software-palette-and-omgcode-icons-in-the-desktop.md))
those are the OMG Software brand colours: neutral greys around charcoal
`#151515` and switch orange `#ff761f`, with charcoal text on orange. There is
no light theme. Rust opens each window with the same background colour, and a
unit test keeps it equal to `--bg`, so a window does not flash white before
its page paints. Native folder and confirmation dialogs follow the operating
system's theme.

- **Brand.** The sessions panel, the `settings` navigation, and the
  `approvals` header lead their heading with the OMGCode knob, an 18-pixel
  SVG built through the DOM from one fixed path and hidden from assistive
  technology. The app icons in `crates/omgcode-desktop/icons/` are the
  OMGCode project icons.

- **Orange marks action.** It is used for primary buttons, focus, selection,
  and "waiting for you", never for a permission at Allow. A permission control
  draws Deny muted, Prompt outlined in orange, and Allow solid neutral, and
  every state also shows its word.
- **Contrast.** `tests/tokens.test.ts` checks that each text colour reaches
  WCAG AA (4.5:1) on every surface, that text on tinted and filled backgrounds
  reaches 4.5:1, and that control borders and the focus colour reach 3:1 on
  the page and panel surfaces. It checks token pairs, not every rendered
  combination.
- **Shared components.** `ui/src/ui/` holds a segmented control (radio-group
  semantics: one tab stop, arrow keys move, Space or Enter chooses), a tab
  strip (tablist semantics: arrow keys select), a `<details>` popover that
  closes on Escape or a click outside it, and a splitter (see
  [section dividers](#section-dividers)). They import no IPC code, so every
  bundle can use them. The segmented control never changes itself; its window
  draws it again from the state the core returns.
- **Window sizes.** `main` opens at 1200 by 800 pixels with a minimum of 800
  by 560. `approvals` and `settings` open at 960 by 720 with a minimum of 600
  by 480, wide enough for their two columns.

The `main` window is laid out as follows.

- **Columns.** The sessions panel, the conversation, and the right-hand panel,
  with a one-pixel separator between each pair (ADR-0072). A separator is a
  focusable window splitter: drag it, or use the arrow keys (16 pixels, 64
  with Shift), Home and End for the limits, and Enter or a double click to
  collapse or expand its panel. Dragging a panel more than 80 pixels below
  its minimum collapses it. The sessions panel is 200 to 480 pixels and opens
  at 260; the terminal panel is 240 to 720, opens at 360, and starts
  collapsed. Neither may take the conversation below 360 pixels: when the
  window narrows the terminal panel gives way first. A panel dragged or keyed
  wider grows into the other, which narrows down to its minimum; a panel
  expanding from its rail takes the free room first. When both minimums do not fit,
  as at the 800-pixel minimum window width, the other panel collapses to its
  rail (on narrowing, the terminal panel first) and stays collapsed until the
  operator expands it. Widths and collapsed
  states last until the window closes. The shell's two width properties are
  written through `ui/src/ui/size_property.ts`, the frontend's only style
  write (ADR-0101).
- **Conversation header.** The title clamps to two lines and the workspace
  folder to one. Each carries its full text, neutralised, as a `title`
  tooltip; there is no separate disclosure that repeats them.
- **Session header actions.** **Archive** and **Restore** are icon buttons
  named in their tooltip and accessible name; **Interrupt**, **Confirm
  interrupt**, **Keep**, and **Cancel** stay text buttons.
- **Collapsed panels.** A collapsed panel is a 44-pixel rail of icon buttons,
  each with an accessible name and a tooltip. The sessions rail holds Expand,
  **New session**, Search, Sessions, **Approvals** with the waiting count, and
  **Settings**; Search and Sessions expand the panel and focus the search box
  or the open session. The right-hand rail holds one icon per tab, each
  expanding the panel on its tab, and, at its bottom, a **Model** icon button
  that expands the panel to the model box (ADR-0089).
- **Right-hand panel.** Tabs, a scrolling content area, and a fixed footer;
  see [right-hand panel](#right-hand-panel). The Terminal tab holds a note
  that no terminal is available and that the panel starts no shell and runs
  no command. It calls no command and has no input.
- **Sidebar.** A title row with **New session** and a collapse button, then
  the search field with a **View** menu beside it. The menu holds Active or
  Archived as a segmented control, the sort field, and the direction toggle.
  The session tree follows, with quiet group labels and an accent bar on the
  open session. Each session row is one line: the title, shortened with an
  ellipsis when it does not fit, then any status chip, then a muted age of the
  last update at the right ("now", "5m", "3h", "2d", "4w", then a short date;
  a missing or unreadable time reads "unknown"). The ages are drawn again in
  place every 30 seconds, and the timer stops when the panel is disposed. A
  row's tooltip holds the full title and the full update time. A group label
  is also one line. An unreadable session adds its reason as a second line.
  Search hits keep their own layout. **Approvals**, with the pending-count badge, and **Settings**
  are rows at the bottom.
- **Status bar.** The workspace, the branch, and an approvals segment.
  It has no model or provider control and shows no connection message
  (ADR-0084). The segment reads "N approvals waiting" with a
  **Review** button while the advisory count is above zero, and is hidden at
  zero. **Review** and the sidebar row only call `approvals_show_window`;
  Rust also shows the window when a turn starts waiting (ADR-0105). The
  notice line above the status bar now only reports a failure to open a
  window, with **Dismiss**; the earlier "An approval is waiting." banner is
  gone.
- **Transcript.** Every entry is centred and takes 96% of the transcript's
  available width, so it follows the window and the panels (ADR-0074).
  Operator messages and live and recorded replies use that width, with
  left-aligned text. A reply sits in a response block one shade lighter than
  the transcript. A recorded reply is
  rendered as a Markdown subset (ADR-0067): paragraphs, headings, rules, block
  quotes, nested lists, tables, inline code, strong, emphasis, and
  strikethrough. A fenced code block is a darker monospace block with its
  language label and a **Copy** button that writes the block's shown text to
  the clipboard. Links and images are inert text that show their destination.
  A reply over 100,000 UTF-16 code units is shown as plain text. A reply is
  labelled with the model id from its turn's request manifest, or "Assistant"
  when none is loaded (ADR-0068). Under it are three icon buttons, each named
  in its tooltip and accessible name (ADR-0080): **Copy Markdown** copies the
  recorded text, **Copy text** copies the rendered reply without Markdown
  symbols, and **Copy image** copies a PNG of the whole reply without its
  buttons. After a click a tick or a cross replaces the icon, and the
  tooltip says "Copied" or "Copy failed", for 1.5 seconds. The image is repainted on a canvas from the computed layout and bounded at
  8,192 pixels on a side, 4096 × 4096 in area, and 50,000 text runs. Each
  writes to the clipboard on a click and nothing reads it. A
  tool request is a collapsed row whose summary shows the tool name and a
  one-line preview of its arguments (whitespace collapsed, cut at 160 UTF-16
  code units without splitting a surrogate pair); opening it shows the full
  arguments. A tool result is a compact muted row. Turn notes, the request
  manifest, and archive or restore notes are thin divider lines. A tool
  request and its result stay separate rows, because paging can put them on
  different pages.
- **Composer.** A movable top line that sets the message box's height
  (see [section dividers](#section-dividers)), the message box, then one
  toolbar on the right. Left to
  right it holds the byte counter, the running label, a **Skills** icon button
  (a puzzle piece) that opens the skills menu, hidden when no skill is
  enabled, a **Chat settings** icon button (a gear) that opens the chat
  settings menu, the **Workspace settings** icon button (see "Settings
  window"), **Cancel**, and **Send**. Each menu button is named and tooltipped
  by `aria-label` and `title`, and reports `aria-expanded` and `aria-controls`.
  The Skills tooltip is "Skills for this turn", or "Skills for this turn (n
  selected)" when n skills are chosen, and a small badge with n shows on the
  button then (hidden from assistive technology). The gear is distinct from the
  three-slider **Settings** icon. The chat settings menu holds the read-only
  endpoint, **Auto-compact**, the compaction slider, **Enter to send**, and
  **Save as defaults**. Panels open upward and end at the toolbar's right edge.
  The counter appears only above 80% of the
  message limit or when the message is over it. The composer holds no
  provider or model control since ADR-0089: a send reads the chosen model
  from the model box and the endpoint from the shared connection.
- **No waiting marker.** The `main` window cannot tell which session an
  approval belongs to: `chat_status`, the chat events, and `approvals_summary`
  carry no such signal, and the approval detail is not available to its
  bundle. It therefore shows only the advisory count.

### Section dividers

Since 0.66.0
([ADR-0101](adr/0101-make-every-section-divider-movable.md)) every line
between two sections of content moves. The `main` window's columns use the
separators described above, which also collapse. Every other divider is built
by `ui/src/ui/splitter.ts`, sizes one section against one neighbour, and
never collapses either:

| Window | Section | Neighbour | Pixels | Starts at | Neighbour keeps |
| --- | --- | --- | --- | --- | --- |
| `approvals` | Queue width | Review pane | 200 to 640 | 300 | 280 |
| `approvals`, under 720 wide | Queue height | Review pane | 96 to 640 | 240 | 160 |
| `settings` | Section list width | Content | 150 to 420 | 190 | 320 |
| `main` | Message box height | Transcript | 64 to 1200 | three rows | 120 |
| `main` | Right-hand panel footer height | Tab content | 48 to 720 | its content | 96 |

- **Use.** Drag the line, or focus it and use the arrow keys along its
  movement (16 pixels, 64 with Shift), Home for the section's minimum, and End
  for the largest size its neighbour allows. A double click restores the
  starting size, or the stylesheet's own size for the message box and the
  footer. Enter does nothing. A drag ends on release, even over another
  window. Only the pointer that starts a drag moves, ends, or restarts it;
  another pointer cannot take it over. Each line is a focusable `separator`
  with an accessible name ("Resize the approval list", "Resize the settings
  sections", "Resize the message box", "Resize the side panel footer"),
  `aria-controls`, `aria-orientation`, and its current, least, and greatest
  values.
- **Giving way.** The neighbour shrinks as the section grows, down to the
  size it keeps. When the window or the neighbour shrinks, the section
  narrows until the neighbour has that size again, even when the window
  shrinks past the neighbour in one step, and does not grow back by itself.
  This holds for a section still at its stylesheet size too, such as the
  footer at its content's height, which then keeps the narrowed size.
  A hidden section, such as the footer of a collapsed panel, keeps its size.
- **Orientation.** Below 720 pixels wide the `approvals` panes stack and the
  queue's divider turns horizontal; each orientation keeps its own size.
- **Where the lines are.** The message box's divider is the composer's top
  line, and it replaces the box's own resize grip. The footer's divider sits
  between the tab content and the model box; a footer shorter than the box
  scrolls. The `settings` divider hides with the section list when the
  window is scoped to one workspace.
- **Not dividers.** Headers, tab strips, toolbars, the status bar, the
  sidebar's two-button footer, and the review pane's sticky decision row hold
  fixed controls and do not move.
- **Not kept.** Sizes last until the window closes.

### Right-hand panel

The right-hand column (ADR-0072, ADR-0083) is one panel with tabs built by
`ui/src/main/side_panel.ts`. Its layout, resizing, and collapsing are those of
[the columns](#appearance). It holds:

- **A tab strip and a collapse button.** The strip follows the WAI-ARIA tabs
  pattern through `ui/src/ui/tabs.ts`: arrow keys move focus and select, and
  each tab controls a `tabpanel`. The selected tab is remembered until the
  window closes; no storage is used. The tabs are **Terminal**, **Files**,
  **Git**, **Processes**, **Web**, and **Context**. A new tab is one entry in the list `app.ts` gives
  `createSidePanel`: an id, a label, a rail icon, its content, and an optional
  callback that says when it becomes visible or hidden.
- **A rail.** Collapsed, the panel shows one icon button per tab. A click
  expands the panel on that tab.
- **A scrolling content area** with the selected tab's content.
- **A fixed footer** under the content area, outside its scrolling and shown
  on every tab. The panel's API exposes it as `footer`. It holds the model
  box (ADR-0089). Since 0.66.0 the line above it moves, so the footer can be
  taller or shorter than the box, which then scrolls
  (see [section dividers](#section-dividers)).
- **The model box** (`ui/src/main/model_box.ts`, ADR-0089): a "Model" heading,
  the **Provider** dropdown, the **Model** dropdown with its **Refresh**
  button, the **Effort** dropdown (`ui/src/main/effort_select.ts`,
  ADR-0102), and the status notes (list status, off-machine warning, no
  effort setting, connection message, unreadable saved choices), stacked
  vertically. It does not scroll
  with the tab content and stays when the tab changes. A collapsed panel
  hides it, so the rail's **Model** icon button (named and tooltipped
  "Model") expands the panel. The controls are the ones ADR-0071 placed in
  the composer, over the same shared connection state, with the same
  commands, native confirmation, and disabled states: disabled while no
  session accepts a message or a turn runs. Only the place changed.

**Terminal tab.** The note from ADR-0072; no command is called.

**Git, Processes, Web, and Context tabs.** Each wraps one session view
(`createGitTab`, `createProcessesTab`, `createWebTab`, `createContextTab`) through
`ui/src/main/panel_tab.ts`, which reads only while the tab is visible: a
hidden or collapsed tab is cleared, so it reads nothing, polls nothing, and
keeps no stale data, and showing it again reads the open session afresh.
Opening a session passes it to every tab; a visible tab reads it at once.
When a turn settles, or the session tree changes after an action, the visible
tab reads again. The Processes tab's own two-second poll therefore runs only
while it is visible; the Web tab does not poll. See [Git status tab](#git-status-tab),
[Processes tab](#processes-tab), [Web tab](#web-tab), and [Context tab](#context-tab).

**Files tab.** A lazily expanded tree of the open session's workspace. It
loads nothing while the tab is hidden or collapsed. Opening a session clears
the tree and loads the workspace's top level once the tab is visible; opening
a folder lists just that folder, and opening it again lists it again. A
chevron shows each folder's state, **Refresh** lists the top level again and
keeps open folders open, and loading, empty, truncated, and error states are
text. A top level that failed to list keeps its error until the tab is shown
again, which lists it again. At most 5,000 rows are drawn across the open folders; closing a folder
drops its rows, and a listing that arrives after its folder closed is
discarded. A name that is not valid Unicode is shown
but its folder is not opened, nor is a folder Rust says it would not list
(too deep, or a name the path rules refuse), and a link or special entry is
shown but not opened. Names are text nodes, never markup. The tab shows names and kinds
only: no content, no size, no date.

`workspace_entries` takes the session id and `path`, an array of relative name
components for the folder to list (empty lists the workspace). Rust finds the
workspace from the session's recorded, registered workspace; the webview never
supplies a host path. Each component must be non-empty, not `.` or `..`, free
of separators, NUL and the `.OMGCode` store name, at most 255 characters, and
at most 32 components are allowed. Rust takes the recorded folders while the
settings store is open and releases that lock before the listing touches the
file system. The listing goes through the platform
broker's directory handle, which does not follow links out of the workspace,
and returns at most 512 entries (the broker's bound) with a `truncated` flag,
directories first and then by name. Entries are `directory`, `file`, or `other`
(the broker does not tell a link from a special entry); a name that is not valid
Unicode is shown lossily with `lossy` set. Each entry's `openable` is true only
for a directory whose name, joined to the listed path, passes the component
checks above, and the tab opens no other folder. `.OMGCode` is hidden. The command
does not need the workspace's `read` preference, is not a model action, and is
not journaled; it needs a registered workspace and refuses an unregistered one
with `unknown_workspace`. It grants nothing: a listing never authorizes a
read, edit, or run.

## Session tree

The tree lists sessions from the derived index. Sessions are grouped by their
recorded workspace folder. A session whose journal records no workspace is
listed under `(unknown folder)`. Groups appear in the order their first session
appears in the sorted listing. A group identifier is the first 16 hexadecimal
characters of a SHA-256 over the recorded path encoding. It is an opaque key,
not a path.

- **Views.** Active or archived. A session whose archive state is unknown
  counts as active.
- **Sorting.** By updated time, created time, or title, ascending or
  descending. Titles sort by a Unicode-lowercased key.
- **Limit.** One listing returns at most 5,000 sessions. The response also
  carries the total for the view.
- **Skipped entries.** Discovery skips symbolic links and reparse points and
  counts other unexpected entries. The tree reports that count so the operator
  knows the listing is not a complete view of the directory.
- **Placeholders.** A session with no recorded title shows a fixed
  placeholder by status: `(untitled session)` for `idle` and `turn_open`,
  `(session in use)` for `running` and `busy`, and `(unreadable session)` for
  `invalid`.

Each session has one of five statuses:

| Status | Meaning |
| --- | --- |
| `running` | A chat turn is running in this shell process |
| `invalid` | The journal failed validation. The tree shows a stable code and offers no transcript |
| `busy` | Another process holds the journal (Windows only, see below). The tree shows the last known metadata |
| `turn_open` | The journal records a turn that never finished |
| `idle` | Nothing is running and no turn is open |

The shell derives the status in that order: a turn running here wins, then
index state `invalid`, then `busy`, then an open turn, then `idle`.

`busy` is a Windows state. The writer lock is mandatory there, so inspection
cannot read a journal that a running writer holds, and it does not retry. On
Linux the writer lock is advisory, so inspection beside a live writer succeeds,
and a just-created empty journal can show as `invalid` until its first record
is written. That Linux behaviour is designed but has not been observed.

Archive, restore, and interrupt open the journal as the writer, apply one
change, and release it. Archive refuses a running session (`turn_running`) and
a session with an open turn (`turn_open`). Restore refuses a running session.
Interrupt records the failure code `operator_interrupted` for the open turn. It
starts no work and replays nothing. The trusted CLI offers the same
operations on an explicit journal path.

## Search

The index holds, for each session, its title and the text of user, assistant,
and tool messages and of compaction summaries.

Not indexed: tool-call arguments, request manifests, custom instructions, tool
outcomes, and failure codes. A search cannot find text that appears only
there.

Rules for a query:

- Search text is never query syntax. Each whitespace-separated term is quoted,
  the terms are combined with AND, and the last term is a prefix match.
- At most 256 characters and 16 terms. A query with more terms is refused, not
  truncated. A query with a NUL character is refused.
- A request returns at most 200 hits, ordered by relevance, then updated time,
  then session identifier, then sequence number.
- Hits carry a snippet of about 24 tokens. The snippet is returned as parts,
  each marked matched or not, so the interface never parses markers out of
  text. The two private-use code points that mark a match are replaced in the
  text before indexing, so a transcript cannot forge a highlight.
- Search covers the active or the archived view, not both at once.
- Only `ok` and `busy` index rows are searched. An `invalid` session has no
  searchable text.

The index is a cache that the shell refreshes before each listing and search.
See [session storage](session-storage.md#session-index) for its files and
limits.

## Transcript view

A transcript page returns entries after a given sequence number, at most 200
entries and at most 1 MiB of text per page, and always at least one entry. When
more remain, the response gives the last returned sequence number to continue
from.

Entry kinds: message, turn started, turn completed, turn failed, tool
requested, tool completed, request manifest, context compacted, session
archived, and session restored. A request manifest entry shows the model, the
prompt version, and tool names only. It never shows the base prompt or custom
instructions.

Reading a transcript uses the lock-free inspection path:

- A journal with bytes after its last line feed is shown with an incomplete
  tail notice. The file is not changed. The writer open still rejects such a
  file, so the shell offers no repair for it.
- While a turn runs here for the session, the request reports `busy`.
- A journal that fails validation reports `invalid_journal` with a stable code.

Transcript text, tool arguments, and snippets are returned as recorded. The
frontend renders them as text and marks bidirectional override characters. They
are not escaped by the service.

Since 0.51.0 (ADR-0072) the transcript follows its end:

- Sending a message scrolls to the end and pins the transcript there. While
  it is pinned, the live preview, the reload at a turn's end, and a change in
  the transcript's size keep the end in view.
- A scroll that moves up and leaves the end unpins it; the place is kept
  while content arrives. Scrolling back to the end pins it again. Content
  growth alone fires no scroll event, so it never unpins.
- Opening another session, or a search hit, starts unpinned; the transcript
  pins once it is at its end. **Load more** unpins it when pressed, so the
  next page is read from where the operator is, and a send made while the
  page loads still follows the end.

## Chat

A new session is created in a workspace the operator has already registered
in the `settings` window or with the trusted CLI. The webview cannot name a
folder. `workspace_list`
returns each registered workspace with its folder display text and its
`model_tools`, `read`, `write`, and `execute` preferences. A workspace whose
folder was recorded by another operating system cannot be used to create a
session here. When no workspace is registered, the new-session dialog offers
**Open settings**, which calls `settings_show_window`. The `main` window
registers and changes no workspace itself.

A turn takes the model name, the context size, the output reserve, optional
user instructions, and the compaction settings. The composer starts from the
saved chat preferences, or from built-in values when none are saved, and
each can be changed before a turn. The model is chosen from a list, not
typed (see **Model choice**). The server must be able to load it, and the
context size must match what the server loaded. A context of
8,192 tokens was too small for a tool-enabled turn in testing, because the
estimate reached the compaction trigger. A context of 32,768 tokens worked.

**Tool calls per turn**, under **Chat defaults** in the settings window's
**Model** section (ADR-0080), accepts a whole number from 1 to 256 and
defaults to 128. **Save chat defaults** keeps the value.
If the next batch would exceed the cap, the turn stops and reports the cap,
calls used, and calls requested. Calls from that batch are skipped.

**Tool-enabled turn timeout (seconds)**, beside it, defaults to 120 and
accepts whole numbers from 1 to 3600.
The runtime checks the deadline between operations and excludes approval waits.
Each model request still has a 30-second timeout of its own (120 seconds on
the ChatGPT plan route, ADR-0093).
The settings window gets the maximum output reserve (4,294,967,295, the
largest 32-bit count the request holds), tool-call, and timeout values from
the overview's `chat` block, and refuses a value outside them before sending.
The composer reads the saved context size, output reserve, tool-call limit,
timeout, and global instructions from `chat_defaults` when it starts, when
the `main` window gains focus, and again just before each send, and sends
them in `chat_send`; it has no control for them. Send is disabled while no
context size is saved, and the composer says to set it in Settings, under
Model. When `chat_defaults` cannot be read, the composer says so and keeps
**Send** enabled: a click reads them again, and nothing is sent until a
read succeeds, even with values read earlier. The message and skills are
taken at the click. After a successful
send the composer removes the sent text from the draft only when the draft
still starts with it, and unticks only sent skills left unchanged since the
click, so text typed, or skills ticked or unticked, while the defaults or the
turn were awaited are kept.

**Auto-compact** and the compaction slider stay in **Chat settings**. The
slider runs from 128K to 1M tokens (128,000 to 1,000,000) in steps of 8,000
and shows its value, such as "400K tokens"; `chat_defaults` reports the
range and, since 0.58.0, the step (`compact_threshold_step`), and the slider
uses them once they arrive. When no chat preferences are saved, **Auto-compact** is on and the
threshold is 400K tokens (since 0.58.0, ADR-0091). A saved preferences file
keeps its own values. The CLI keeps a 200,000-token default and an opt-in
`--auto-compact`. Compaction still triggers earlier when the model's safe input is
smaller (`min(threshold, safe input)`). A saved threshold outside the range,
such as one saved before 0.54.0, is shown and sent at the nearest end of
the range. When defaults arrive after the operator has begun editing, only
untouched controls take the returned values.

- **Send key.** With **Enter to send** on, the default, Enter sends the
  message and Shift+Enter or Alt+Enter adds a newline. With it off, Enter
  adds a newline. Ctrl+Enter (Cmd+Enter) sends in both modes. An Enter that
  confirms an input-method composition never sends. A send key never adds a
  newline, so Enter on an empty message shows why it cannot be sent. The
  check box is in **Chat settings** and applies at once; **Save as defaults**
  keeps it for the next run. A change made before the saved defaults load is
  kept; the loaded value does not replace it. `chat_defaults` returns it as `enter_to_send`,
  outside the turn settings, because it never reaches a turn (ADR-0062).
- **Model choice.** Since 0.46.0 (ADR-0066) the model is a dropdown with a
  **Refresh** button, in the model box in the side panel footer since ADR-0089
  (the composer toolbar from ADR-0071 until then). `chat_models`
  takes the session id, resolves that session's connection exactly as
  `chat_send` does, stored credential included, and sends one
  `GET /models` under its endpoint's base path, which is a loopback endpoint,
  a hosted provider's fixed endpoint, or a typed `openai_compatible` endpoint
  (ADR-0092). Core refuses the whole list when it is not a `data` array of at
  most 2048 entries (512 before ADR-0092) with string ids, or when an id is empty, over
  256 bytes, repeated, or holds a control or bidirectional formatting
  character. The deadline is ten seconds. A signed-in ChatGPT connection
  first gets its access token, which can wait up to 40 seconds for the
  session lock and run a refresh of up to 30 seconds, so its list can take
  about 80 seconds. The answer carries the session id,
  the endpoint it asked (display only), and the provider kind. **The list is
  keyed on the session and the request, never on endpoint text.** The
  composer keeps an answer only when it is for the session still shown and no
  later request or reset began; it never compares the endpoint of an answer
  with another endpoint. It once did, and discarded the whole list
  whenever the connection had changed outside this window (a regression test
  covers it). The list loads when a session opens, after a provider is
  chosen for it, when the connection read for it changes, and on
  **Refresh**, which is disabled while a list is being read. While it loads,
  the dropdown is empty and disabled, so no model of another server is
  offered. The ids are sorted and shown as text. A chosen or saved model the
  server did not report stays chosen and is marked "(not reported by the
  server)". An id whose last tag is `cloud` or ends in `-cloud` is marked
  "(Ollama cloud)", and while it is chosen a warning says that prompts and
  workspace text sent to it leave this computer. On a hosted connection
  (any kind but LM Studio and Ollama) every model is marked "(hosted)" and a
  warning says that
  prompts, workspace text read by tools, and command output leave this
  computer. The warning is read from the chosen id itself and from the
  connection, so it also shows while the list loads or after listing fails. A
  chosen id is sent exactly as listed, never trimmed. The mark follows
  Ollama's naming convention and is not proof; an unmarked model is not shown
  to be local. A failure is shown beside the toolbar with its code. Nothing
  is chosen automatically: with no saved model, Send stays disabled until a
  model is chosen. Choosing a model grants nothing; `chat_send` validates it
  as before. Embedding models are listed too and cannot answer a turn.
- **Provider and model per session** (ADR-0071 decisions 6 to 11). The
  provider and the model belong to the open session. The model box's
  (ADR-0089; the composer's until then) **Provider** dropdown renders a state read from `connection_status` for
  the session; the status bar no longer has a provider dropdown (ADR-0084). Opening a session reads its connection and model and lists its
  models; switching sessions switches both and shows no dialog, because the
  choice was confirmed when it was made. Choosing a provider calls
  `connection_select` with the session id and the profile name or `null`;
  Rust shows the native Yes/No dialog, and only on Yes records the choice and
  clears the session's model, because model ids belong to one server. The
  model dropdown is then empty and disabled until the session's list
  arrives. Choosing a model calls `session_model_select`, which shows no
  dialog. A status or list that arrives for a session that is no longer open
  is ignored. A session that never chose follows the global default chosen in
  the `settings` window. **Chat settings** keeps the endpoint as read-only text
  (the open session's endpoint). A launch argument
  (`--endpoint` or `--provider-profile`) fixes the connection of every
  session: the model box's **Provider** dropdown is disabled and shows "Set
  at launch", and the model is still chosen from that connection's list. `chat_send` keeps
  sending the endpoint of the shared connection and the model the model box shows; Send is disabled without a model
  and while the session's provider is being read or has gone stale.
- **Reasoning effort per session** (ADR-0102). Under **Model**, the
  **Effort** dropdown offers "Default", which sends no effort field, and the
  efforts `connection_status` reports in `reasoning_efforts`: None, Minimal,
  Low, Medium, High, Extra high (`xhigh`), and Max. Loopback connections, an
  operator-typed `openai_compatible` address, and the fixed Ollama Cloud,
  OpenAI, Google, Moonshot, MiniMax, OpenRouter, Z.ai, and xAI endpoints take
  all seven; the fixed Anthropic, DeepSeek, Tencent, Meta, and Thinking
  Machines Lab endpoints take none, and there the dropdown is disabled with
  the note "This connection does not take an effort setting." The endpoint
  decides, not the kind: an `openai_compatible` address typed as exactly one
  of the fixed endpoints takes that endpoint's rule. Choosing an
  effort calls `session_effort_select`, which shows no dialog and shares the
  model choice's serialized writes. A provider choice clears the effort with
  the model; a model choice keeps it. It is disabled with the other dropdowns
  (no session that accepts a message, a turn running, or a provider choice in
  flight), but not by a launch argument that fixes the connection.
  `chat_send` sends the effort the box shows in its settings as
  `reasoning_effort`, and Rust refuses one the turn's connection does not
  take. The server still decides whether its model accepts the value; a
  refusal arrives as the provider's error.
- **Saved preferences.** Two windows save one file, each its own fields,
  under one service lock, so neither overwrites the other's (ADR-0080). The
  composer's **Save as defaults** sends the compaction threshold, the
  automatic compaction switch, and the **Enter to send** choice to
  `chat_preferences_save`, which refuses a threshold outside 128,000 to
  1,000,000 and any other field. The settings window's **Save chat
  defaults** sends the context size, output reserve, tool-call limit,
  tool-enabled turn timeout, and global custom instructions to
  `settings_save_chat_defaults`, which refuses a zero context size or output
  reserve, a tool-call limit or timeout out of range, and global
  instructions longer than the room the overview reports (see "Settings
  window"). Neither saves the model name; a file's saved
  name is kept. A file without `max_tool_calls` reads as 128, and one
  without `turn_timeout_seconds` reads as 120. A file or request
  without `enter_to_send`, such
  as a file saved before version 0.42.0, reads it as on. An older build
  refuses a file that has the field as unknown, reports it as `unreadable`,
  and uses its built-in values until the next save. They carry no
  endpoint and grant nothing. The request refuses an unknown field, so an
  endpoint cannot be saved. A saved model name over 256 bytes or with a
  control character, or instructions over 64 KiB, make the file damaged
  when it is read: `chat_defaults` reports it as `unreadable` and gives the
  built-in values until the next save replaces it. No save writes such
  values. A file saved before 0.54.0 may hold global instructions up to
  64 KiB, which a turn refuses. The tool-call limit and turn
  timeout are validated on save, load, and send.
  `chat_send` validates the other budgets before starting a turn. The file is
  `<data_root>/desktop-v1/chat-preferences.json`, at most 512 KiB, read only
  as a regular file and parsed strictly. A save writes a new temporary file,
  syncs it, and renames it over the old one; on Unix it is created with mode
  0600 and the directory is synced. A missing file gives the built-in
  values. A file that cannot be read or parsed also gives the built-in values,
  `chat_defaults` reports it as `unreadable`, and the composer shows a note
  in its **Chat settings** menu. Since the built-in values set no context
  size, the composer's reason beside **Send** then says the saved defaults
  could not be read and asks for them to be saved in Settings, instead of
  only asking for a context size.
  Because each save keeps the other window's fields, it reads the file first.
  The next save replaces a damaged regular file (one too large, not valid
  JSON, or holding a value over its bound), starting from the built-in
  values. A file that exists but cannot be read, such as one another program
  holds, a link, or a directory, is left unchanged: the save fails with
  `preferences`, since its fields are unknown, and the operator must close the
  program, remove the entry, or correct its access before saving again.
- **Skills.** When a session opens, `main` calls `session_skills`. It returns
  the names of the skills effectively enabled for the session's registered
  workspace, and nothing for a workspace that is not registered here or when
  no settings store exists. The composer lists them as check boxes in its
  **Skills** menu and allows at most four. `chat_send` carries the selected names in `skills`, and the
  composer clears them after a send. The service refuses more than four, or
  a name that is not a valid skill name, with `invalid_request`, and a
  repeated name with `skill_selection_invalid`, before a turn starts. Core
  then loads each skill as the CLI's `chat --skill` does (ADR-0057): a skill
  that is no longer enabled fails the turn before any model request. A
  selected skill reaches only that turn's first request, as a labelled
  untrusted message, and is never journaled. The window shows names only.
- **Endpoint.** `chat_defaults` returns the endpoint the next turn would
  use, and the composer shows it as text, not as an input. A launch argument
  fixes it. A request whose endpoint is not byte for byte the launch endpoint
  is then refused with `invalid_request`. Without a launch argument, each
  turn resolves the active connection, and a request whose endpoint differs
  from it is refused with `connection_changed`: the `settings` window chose
  another connection after `main` read the endpoint. No turn starts. The
  composer then reads the endpoint again, lists that connection's models,
  and keeps the draft and the chosen model, marked when the new server does
  not report it, so the operator can check the model before sending again
  (ADR-0064, ADR-0066). With `--provider-profile`, each turn also resolves the profile
  again. If its recorded endpoint no longer equals the
  launch value, because the profile was removed and added again, `chat_send`
  fails at once with a `credential` error that starts `endpoint_changed:` and
  tells the operator to restart the application. No turn starts. A profile
  replaced with the same endpoint and a new credential is accepted, and the
  turn uses the new credential.
- **Token.** Resolved for each turn from the same source as the endpoint:
  the stored credential of the profile, none for an `ollama_local` profile,
  or else `OMGCODE_LM_TOKEN`. It is held in
  a zeroing type with a redacted `Debug` and no serializer, and removed from any
  recorded or returned text. The provider clients' header copies are not
  zeroed. With a profile selected, a failure to read the credential ends the
  turn with a `credential` error before any model request, with no fallback.
- **Tools.** Offered only as in the CLI: a workspace with `model_tools`
  enabled, plus the permissions each tool needs. Otherwise the turn runs
  text-only and the response says why.
- **Concurrency.** At most two turns run at once. A third request is refused
  with `too_many_turns`. Finished results stay until the session's next turn;
  the oldest of more than 64 is dropped.
- **Validation.** `chat_send` checks the settings and builds the provider
  clients before it starts the thread, so a bad setting fails at once. The
  message and instructions are first bounded at 64 KiB each; core then applies
  its own limits.
- **Status.** `chat_status` reports the state and outcome of a turn. The
  frontend reads it once when the `finished` event arrives, and keeps a
  five-second reconcile check while a turn runs in case an event is lost. If
  subscribing fails, it polls every 750 ms instead. The result reports the finish reason, whether tools were
  offered, rounds, tool calls, and the context meter line: estimated tokens,
  context size, and the compaction trigger.
- **Live preview.** With streaming on (the default), `chat_subscribe` gives the
  `main` window one channel for chat events. They are `request_started`,
  `delta`, `preview_truncated`, and `finished`, tagged by `kind`, each with a
  session identifier. A `delta` carries at most 1,024 bytes of text. The
  service keeps the preview of each running turn for the current request only,
  at most 16 KiB; at the bound it sends `preview_truncated` and no more deltas
  for that request. A new subscription replaces the old one and first replays
  the preview of every running turn. The open session shows one live entry
  labelled "<model> (generating, not saved yet)", with the model this window
  sent or the loaded last turn's, else "Assistant", updated at most once per
  animation frame. The preview is plain text; Markdown is rendered only once
  the recorded reply replaces it. The frontend removes it when the turn
  settles and shows the recorded transcript, and shows a note when the
  preview was cut off. It keeps
  previews for up to 4 sessions it is not tracking, each bounded at 64 KiB of
  UTF-16 code units. The preview is provisional: a later failure discards it,
  and a reply over the 16 KiB session message limit is shown in part and then
  fails as before. The journal never holds preview text. `finished` is sent by
  a guard after the result is recorded, and a panicked turn is recorded as
  failed first, so `chat_status` read after `finished` never reports
  `running`. With `--stream off` no preview exists, and `finished` is still
  pushed. See
  [streamed replies](provider-authentication.md#streamed-replies-0320).
- **Cancellation.** Cooperative. Every turn, text-only or tool-enabled,
  streamed or not, hands its cancel token to the service before the
  compaction check and its first model request (ADR-0051). `chat_cancel` sets
  it, the state becomes `cancelling`, and the model or compaction request in
  flight stops within about 25 ms (see
  [cancellation](provider-authentication.md#cancelling-every-request-0340)).
  The turn ends as `failed` with the error code `cancelled`, and `finished` is
  pushed. A turn that had started leaves a turn-failed record with the reason
  `cancelled`, no final assistant reply, and no open turn; an assistant
  message recorded with an earlier round's tool calls stays. A turn cancelled
  during its compaction, or before its token was handed over, records nothing;
  a compaction that had already finished stays recorded. The
  preview text is never recorded. The window shows the plain notice "Turn
  cancelled." with no error styling. It decides by the error code only. A
  tool-enabled turn also checks the token between model rounds and tool
  calls. A read, list, or edit tool call already in progress is not
  interrupted.
- **Archived or open sessions.** A chat request for an archived session fails
  with `archived`, and for a session with an open turn with `turn_open`.
- **Exit.** Closing `main` asks every running turn to cancel and waits up to
  five seconds. A turn waiting on a model or compaction request ends within
  that period and leaves no open turn. A turn in a step that cannot be
  interrupted, a read, list, or edit tool call, is abandoned if it is still
  running when the period ends. Its journal keeps an open turn. The tree shows
  it as `turn_open`, and the operator can interrupt it.

A panicked turn thread is recorded as a failed turn with the code `internal`.

## Context tab

The **Context** tab of the right-hand panel shows `createContextTab()` from
`ui/src/main/context_tab.ts`, which the `session_context_breakdown` command
serves (ADR-0087). The tab reads when it is shown, when the open session
changes while it is shown, and again when a turn settles; see
[right-hand panel](#right-hand-panel).

- **Command.** `session_context_breakdown { session_id }` is `main`-only. Rust
  resolves everything from trusted state by session id: the journal through the
  index, the saved chat budgets and global instructions, the session
  workspace's saved instructions (read under the preferences lock, as
  `chat_send` reads them), the session's recorded reasoning effort, and live
  settings for the tool decision. It
  sends nothing to a model, appends nothing to any journal, and returns no
  instruction or message text, only numbers and fixed codes. It runs on a
  blocking task.
- **Parts.** In display order: the system prompt (compiled base prompt and its
  message overhead), global instructions, workspace instructions (including the
  headings that join the two texts), tool definitions, the compaction summary,
  the conversation history after the compaction boundary (with its message
  count; the boundary is named only when a summary is sent), selected skills,
  and message framing. Framing is the fixed overhead
  no part owns (the instructions label, the instruction message's
  overhead, and the reasoning effort field when the session records an
  effort, ADR-0102), reported as its own part so the parts add up to the
  estimate.
  Tool definitions count only when live settings would offer tools (the same
  rule a turn applies); otherwise the part is not included and carries a fixed
  reason such as `preference_disabled`. Skills are loaded only when selected
  for one turn, so the part is always not included (`selected_per_turn`).
- **One estimator.** The parts come from the arithmetic the pre-turn trigger
  uses (`estimate_history_parts` in core, whose total is
  `estimate_history_input_tokens`), applied to the same history the next
  request would send. The estimate leaves out the operator's next message,
  which is not known; the trigger counts it. Tests show that the parts sum to
  the preflight total for the same session state, less that message.
- **Totals.** The estimate, the output reserve, the effective compaction
  trigger, the saved context capacity (`null` when none is saved; the
  trigger is then the saved threshold alone), and whether auto-compaction is
  on. They come from the saved chat budgets and composer preferences, the
  values the composer sends with a turn.
- **Errors.** `not_found` for an unknown session; `busy` while a turn runs
  for the session or another process holds its journal; `invalid_journal`;
  `instructions_unreadable` when a turn would also refuse. A session whose
  workspace is no longer registered is not an error: as in `chat_send`, its
  workspace instructions are not sent even when a saved file remains, so that
  part and the tool definitions part are not included
  (`workspace_unregistered`, ADR-0081).
- **View.** A horizontal stacked bar of the included parts scaled to the
  context capacity (or to the larger of the estimate and the trigger when the
  capacity is unknown) with the compaction trigger marked, then a table of
  every part with its tokens, its share of the estimate, and a note (the reason
  a part is not included, or the history's message count), then the totals and
  the plain statement "Conservative estimate before the next turn". The bar is
  an SVG drawn through the DOM with sizes in attributes, so the view sets no
  inline style. Each part has a colour from the shared tokens and a text label,
  so colour is never the only cue. With no session open the view shows an empty
  state; a failure shows as text, and a failed reload keeps the session's last
  estimate below the error until a reload succeeds; **Refresh** reloads. A slow
  answer for a session that is no longer open is ignored, and opening another
  session clears the previous one's estimate.
- **Limits.** The estimate counts one byte as one token, so it overstates a
  real tokenizer's count. It is not provider-reported usage and is not
  recomputed while a turn runs.

## Processes tab

The **Processes** tab of the right-hand panel shows `createProcessesTab()`
([ADR-0086](adr/0086-show-the-sessions-subprocesses-in-the-main-window.md)).
It reads only while it is visible; see [right-hand panel](#right-hand-panel).

The only subprocess a session starts is a model `workspace_run_command` call.
`session_processes { session_id }` lists those calls for one session, newest
first, at most 100, with `truncated` set when older calls may exist: more
than 100 matched, or the 1,000-record window was full. It is
read-only: it cancels, restarts, and runs nothing, and the webview names only
the session identifier. Rust resolves every source from trusted state, on the
blocking pool, with the same bounded readers the trusted CLI uses.

| Source | Gives |
| --- | --- |
| Model action journal (`ActionJournal::recent`, the 1,000 newest records of all sessions) | The call identifier, start and refusal time, how the call was authorised or why it was refused, the finish time and outcome, and the run identifier a finished call recorded |
| Run journal (`run::audit_existing`, the newest 100 events; one attempt, no wait, nothing created) | Through that run identifier: the resolved program, the profile name, the result (exit code, signal, deadline, cancellation, or the limit that stopped it: memory, processes, CPU time, or disk growth), and the elapsed time |
| Session journal (`SessionInspection`) | The program and argument text the model sent, matched to the action records by call identifier |

The action journal records no argument text and the run journal records only
an argument count and digest, so the command text comes from the session
journal. A call identifier is unique within one turn only, so records and
requests that share one are paired in order from the newest end. A call
with no matching request, or a session journal that cannot be read (for
example while a turn holds it on Windows), shows the run journal's program
when it has one, and otherwise "(command unavailable)". A run journal that
cannot be read sets `run_details` to `unavailable`, and one that another holder
has locked sets it to `busy`: results and durations are then absent and the
action journal data is still listed. The run journal read never creates the
settings store or the journal, and never waits. A missing store or journal
lists no runs and is not an error. A run holds the settings and journal locks
only while it records its start and its finish, not while the program runs.
The action journal read waits at most 250 ms for a writer, well under the
two-second poll, and then fails with `busy`; the shared poller never starts a
read while one is in flight.

Each entry has a `state`:

- `running`: started, no finish recorded, and this desktop has a chat turn
  active for the session.
- `unfinished`: started, no finish recorded, and no turn is active here, for
  example after a crash. The outcome is unknown, never failed.
- `finished`: started and finished. `result` and `limit` come from the run
  journal; a finished call with no run result shows its failure code.
- `refused`: never started, with the refusal reason.

The reply returns, per entry, only: the call identifier, the state, the start
and finish times, the run's elapsed milliseconds, the program, the arguments,
whether text was shortened, the argument count, the profile name, the result
kind with exit code or signal, the limit, and a reason code. It never returns
captured output, environment values, credentials, working directories, or
digests. Program, arguments, and the profile name are untrusted model-chosen
text. Rust escapes each to one line with `display_safe_line`, renders a
non-Unicode program path lossily, and bounds them: 256 characters per item, 32
arguments, 2,048 characters per call, charged to the program, then the profile
name, then the arguments. The view builds text nodes only and shows
the full command line in a tooltip.

`createProcessesTab()` in `ui/src/main/processes_tab.ts` returns a
`PanelView` (`element`, `showSession`, `refresh`, `dispose`). It shows each
call on one ellipsized line with a state badge, start time, duration, and
outcome, the empty state "No subprocess has run in this session.", errors as
text, and a **Refresh** button. A `truncated` answer adds "Older calls may not
be shown."; with no call listed it can only mean the record window was full,
so the empty state reads "No recent subprocess was found in this session."
instead. While the list holds a `running` call it reads
again every two seconds, through the shared poller, which also pauses while the
page is hidden; it stops when nothing runs, on `showSession(null)`, and on
`dispose()`. An answer for a session that is no longer shown is dropped.

## Web tab

The **Web** tab of the right-hand panel shows `createWebTab()`
([ADR-0098](adr/0098-add-a-model-web-fetch-tool-behind-a-host-allowlist.md)).
The operator chose to keep `allow` unchanged after a fetch and to review
what the model did instead, so this tab lists, for the open session, each
`web_fetch` call and every model action recorded with `after_web_content`,
newest first. It reads only while it is visible, on **Refresh**, and when a
turn ends; it never polls, so a fetch in progress shows as unfinished until
the next read. See [right-hand panel](#right-hand-panel).

`session_web_audit { session_id }` reads the 1,000 newest records of the
model action journal, keeps the session's records whose tool is `network`
or that carry `after_web_content`, and returns at most 100, with
`truncated` set when more matched or the window was full. It creates no
file, takes no settings lock, runs nothing, and fails with the same
`not_found`, `busy`, `invalid_journal`, and `internal` errors as
`session_processes`. Each entry returns only the call identifier, the time,
the tool's journal name, whether it is a fetch, whether it carries
`after_web_content`, its targets, its outcome (`succeeded`, `failed`,
`uncertain`, `unfinished`, or `refused`), the refusal reason or finish code,
and whether it ran under the settings (`standing_allow`) or an approval. A
target is a validated workspace path or fetch URL as text, up to 4,096
characters, or a withheld value shown only as withheld with its byte
length; no digest is sent. A `run_command` record has no target, so its
program and arguments come from the session journal under the Processes
tab's bounds and pairing, and are absent when that journal cannot be read.
No entry names the files a command touched.

Each row shows the time since the call, the tool, its whole target as
inert, wrapping text (never a link), the outcome, and how it was
authorised. Fetch rows have an accent edge and a filled chip, so they stand
apart from the actions after them. The tab says "This session has not
fetched web content." when nothing matches, notes truncation, keeps the
list with a note when the journal is busy, and clears it with an error when
the journal fails validation. The trusted CLI's
`omgcode actions list --after-web-content --session <id>` lists the same
records, with `truncated` true when its 1,000-record search window was full
or more calls matched than `--limit`.

## Settings window

**Settings** in the session sidebar calls `settings_show_window`, which shows
the hidden `settings` window. The window calls `settings_overview` when it
starts, when it becomes visible again or gains focus, and when **Refresh** is
pressed. When
no settings store exists, that call creates an empty one, as the CLI's
`workspace status` does (ADR-0060). The window loads hidden at launch, so the
store exists from the first launch.

Since 0.45.0 (ADR-0065) a sidebar chooses one of three sections: **Model**,
**Workspaces** (shown first), and **Advanced**. Since 0.66.0 (ADR-0101) the
line beside the sidebar moves, so a long workspace path can be read in full
(see [section dividers](#section-dividers)). **Refresh** and the settings
revision sit at the bottom of the sidebar, and errors and notices at the top
of the section. **Workspaces** is a collapsible group: a chevron button
beside it (`aria-expanded`, named "Workspaces list") opens or closes a list
with one entry for each registered workspace, each showing its folder with an
ellipsis and the full text as a tooltip. The group starts open and is hidden
when no workspace is registered. The chosen section, the chosen workspace,
whether the group is open, and which disclosures are open are kept in memory
only and survive a refresh and every change. The sidebar, the chevron, and
the workspace entries are disabled with every other control while a change
waits. The window shows:

- **Model.** The connection the next turn uses, with a **Model provider**
  dropdown, and its provider, endpoint, and source. The dropdown lists the
  default LM Studio endpoint and every selectable profile; when a launch
  argument fixed the connection, it is disabled and says so. Then one row per
  provider profile, with its name, an **Active** chip, its provider,
  endpoint, and authentication, whether a credential is bound, and
  **Remove**. A hosted profile also has a **Hosted** chip and a line saying
  that prompts, workspace text read by tools, and command output leave this
  computer for its endpoint's host. A hosted `api_key` profile with a
  recorded endpoint also has a **Set key…** disclosure with a password field
  and **Save key**. A `subscription` profile has no key field, and a profile
  recorded without an endpoint has none either; its line says instead that it
  cannot be used and must be removed and added again. Then **Add connection**, a disclosure with
  labelled provider, name, and endpoint fields. The provider dropdown lists
  `Ollama`, `LM Studio`, and every hosted kind with "(hosted)" after its name,
  ending with `OpenAI compatible (hosted)` (ADR-0092). For a local provider
  the endpoint is prefilled with its usual address
  (`http://127.0.0.1:11434/v1` or `http://127.0.0.1:1234/v1`). For a named
  hosted provider the endpoint field is hidden and its fixed endpoint is
  shown as read-only text. For `OpenAI compatible` the endpoint field is
  empty with the hint `https://llm.example.org/v1`. Every hosted kind shows a
  labelled **API key** password field (`autocomplete="off"`) and a line that
  names the host prompts go to, or "the address you enter".
  Between the active connection and the connection list, **Chat defaults**
  (ADR-0080) holds number fields for **Context tokens**, **Max output
  tokens**, **Tool calls per turn**, and **Tool-enabled turn timeout
  (seconds)**, a **Global custom instructions** box with a UTF-8 byte
  counter against the room left beside the longest workspace text, and **Save chat defaults**, which calls
  `settings_save_chat_defaults` with no native dialog and no settings
  revision, because none of it is a permission. What the operator types is
  kept in window memory until a save succeeds, so a redraw or a section
  change keeps it. A note says when the saved file could not be read.
- **Workspaces.** The meaning of Deny, Prompt, and Allow, stated once. The
  development root, with **Choose folder…** or **Change…**, and
  **Register workspace…**, which is disabled until a root is set, and a line
  saying when no workspace is registered. That overview is what the
  **Workspaces** entry shows. Choosing a workspace's entry under it shows that
  workspace instead and marks only that entry as the current page; choosing
  sends nothing, and the window shows the first workspace when the chosen one
  is gone, or the overview when none is left. The shown workspace has its folder, a **Model tools** checkbox for
  `model_tools`, and one row each for `read`, `write`, `execute`,
  `task_network`, `host_services`, and, since 0.64.0 (ADR-0098), `web_fetch`,
  each with a short description and a Deny, Prompt, or Allow segmented
  control. Under Web fetch, **Allowed web hosts** lists the workspace's host
  patterns, each with a remove button, and a field and **Add** button add one,
  up to 32. Rust checks the pattern before any dialog, then adds or removes it
  through `settings_add_web_fetch_host` or `settings_remove_web_fetch_host`
  after a native confirmation, with the revision the window read. With no
  pattern it says that every fetch asks first. Under Write, the disclosure
  **File-write tool limits** holds a row for each model file-write tool's limit
  (edit, multi-edit, create, rename, and delete since 0.43.0, ADR-0063; create
  directory since 0.55.0, ADR-0079). A tool whose limit is wider than `write`
  says "Limited by Write:" and the `write` state. On the disclosure's summary
  line, outside the summary so it works while the disclosure is closed,
  **Set all** is one more Deny, Prompt, or Allow control named "All file-write
  tools" (since 0.57.0, ADR-0090). Choosing a state sends the whole
  preference set once with all six limits at that state and `write`
  unchanged, so a widening change shows one native confirmation that names
  each widened limit. It marks a state only while all six limits share it,
  and none while they differ. Under Execute, **Toolchain profiles**
  (ADR-0107) has one switch per recorded profile of this platform, named
  "Use NAME in this workspace" and on while the profile is assigned, with
  the profile's read-only roots beside it; with no profile of this platform
  it says to add one under **Advanced**. Turning a switch on calls
  `settings_assign_toolchain_profile` with the revision, the workspace
  identifier, and the profile name. Rust plans the grants and shows a native
  confirmation that names the workspace folder, the profile, each folder the
  workspace's sandbox is granted read and execute, and the folders that need
  an administrator or whose access could not be read, then grants and assigns
  with the confirmed plan. Turning one off calls
  `settings_unassign_toolchain_profile`, whose confirmation names the grants
  it revokes and says the workspace's running commands stop. A declined
  confirmation or a refusal shows the recorded state again. Then the
  workspace identifier and its package cache root with
  **Choose…** and, when one is set, **Clear**, and one line of help: the root
  is where the trusted CLI command `omgcode packages fetch` stores downloads
  for that workspace, a trusted run exposes it read-only, and the app never
  fetches packages. Then **Custom instructions**
  (ADR-0080): a box with a byte counter and **Save instructions**, which
  calls `settings_set_workspace_instructions` with the workspace identifier
  and the text, with no dialog. Rust refuses a workspace that is not
  registered and text longer than the workspace's `instructions_max_bytes`;
  saving empty text removes the workspace's file. The counter counts against
  that room, and the window refuses longer text before sending it.

  A turn sends the global and workspace texts together under headings,
  within 16 KiB (ADR-0080). So that every accepted save can be sent, each
  save checks the pair: a workspace's room is 16 KiB less its 24-byte heading
  when no global text is saved, or less the global text and 47 bytes of both
  headings and the blank line otherwise; the global room is 16 KiB when no
  workspace has text, or 16 KiB less 47 bytes and the longest workspace
  text. Both saves hold the service's preferences lock, so neither can
  overtake the other's check, and `chat_send` reads the workspace's file
  under the same lock, so a turn sees one whole save. For the room, a damaged
  other text counts as empty. A turn sends a damaged global file as the
  built-in empty text, but refuses a workspace whose own file is damaged
  with `instructions_unreadable` until it is saved again, and that save
  checks the room. An other text that exists but cannot be read
  now leaves the room unknown: a save of non-empty text is refused with
  `preferences`, but saving empty text needs no room and always proceeds.
  The overview applies the same rule without the lock and reports an
  unknown room as a `null` `instructions_max_bytes`; the counter then says
  the room is unknown, and the window leaves the decision to Rust's save,
  which reads the other text again. Each workspace's typed draft is kept in window memory
  until its save succeeds. A note says when the saved file could not be
  read; until it is saved again, that workspace's sessions cannot send.

  **Remove workspace…** (ADR-0081) ends each workspace's page under
  **Workspaces**; the scoped view has none. It calls
  `settings_unregister_workspace` with the workspace identifier and the
  revision the window read, never a path. Rust refuses an unknown workspace, a
  stale revision, and a workspace in which a chat turn is running, all
  before any dialog. A native Yes/No confirmation then names the folder,
  lists the folders its container was granted, which are revoked first and
  whose runs stop (ADR-0107), and states that its preferences, toolchain
  profile assignments, instructions, package cache selection, skill
  overrides, and checkpoint record are removed, that its files are not
  touched, and that its sessions stay and can chat without tools only. On
  Yes, the grants are revoked; when every revocation is confirmed, one
  `workspace_unregistered` event is recorded and the workspace's
  instructions file is deleted under the preferences lock. When a grant
  cannot be revoked, the workspace stays registered with the grants not yet
  revoked, the runs the revocations withdrew are stopped and waited for, and
  the command fails with `container_access_failed` or
  `revocation_incomplete`, saying how many grants were not revoked, to retry
  each one's **Revoke**, and to remove the workspace again; a grant revoked
  whose AppContainer profile could not be deleted is reported apart, because
  it is no longer listed. When every grant was revoked but the removal
  cannot be recorded, the workspace stays registered and the command fails
  with `revocation_incomplete` after the same wait. If that file cannot
  be deleted, the command still stops and waits for the runs the removal
  withdraws, then fails with `instructions_orphaned` (a run that was not
  stopped is reported first); no turn sends the file, because `chat_send`
  checks registration and reads a workspace's instructions under the same
  lock, only while that workspace is registered. A turn that starts
  while the dialog is open is found again before the change; a turn that
  starts in the instant after that loses its tools, because every tool call
  reads the live settings. A `settings` window opened for the removed
  workspace shows the full view with the existing notice.
- **Advanced.** Four groups, each with controls (ADR-0082, ADR-0088). The
  window sends identifiers, counts, typed names, origins, and variables, and
  never a path; Rust opens every folder dialog and composes every
  confirmation. Core has no edit operation for a toolchain profile, a
  package-network profile, or a grant, so there is none here: remove and add
  again. Adding is not atomic with removing, which is why no edit is offered.

  **Toolchain profiles** list each profile's platform, read-only roots, path
  entries, and environment variable names, without values, and a **Remove**
  button. **Add toolchain profile** takes a name, how many read-only roots
  (1 to 16) and `PATH` entries (0 to 16) to choose, and the variables as
  `NAME=VALUE` lines, typed in the webview because they are stored in plain
  text by design; a line says never to put a secret in one. **Add profile…**
  calls `settings_add_toolchain_profile`, and Rust then opens one native folder
  dialog per root and per entry (the entry dialogs start at the first chosen
  root), shows one native confirmation with the name, platform, every root and
  `PATH` entry, and the variable names (never the values), and applies the
  change with the revision the window read. A closed dialog or a declined
  confirmation changes nothing; core's refusals (name, variables, overlap
  with the development root or application storage, a `PATH` entry outside a
  root, a name in use) come back as controlled errors after the dialogs.
  Removing is confirmed natively and stops the runs the settings then
  withdraw, as `workspace profile-remove` does; a run already started keeps
  its recorded profile. Since ADR-0107 the confirmation says the profile is
  assigned to no workspace, or names each workspace it is assigned to and
  the grants revoked there, which no profile still assigned needs.

  **Generate toolchain profiles from local packages** (ADR-0103) sits above
  the add form. It asks Rust for proposals, which core makes by reading the
  standard install folders, the operator's `PATH`, `JAVA_HOME`, `GOROOT`, and
  `RUSTUP_HOME`, and small installer-written files; it runs no program and
  never reads repository configuration such as `rust-toolchain.toml`. Each
  proposal shows its name, family, version, roots, `PATH` entries, variables
  with their values, notes, and, when core would refuse it now, the reason,
  with its checkbox disabled. A line says how many installations the list
  left out when a bound was reached. An **Assign to workspace** choice
  names a workspace on every platform (ADR-0107). On Windows each root then
  says whether that workspace's container can read it, needs a grant, needs
  an administrator to grant, or could not have its access-control list
  read; the add names and skips a folder in either of the last two states.
  Changing the choice generates a list on screen again, keeping the
  selection. A line says when the workspace lacks the active read-write
  grant on its own folder, and another that the profiles added are also
  assigned to it. **Add selected profiles…** sends the revision, the list's
  workspace or none, and the digests of 1 to 8 selected proposals; Rust
  discovers again, refuses a list that no longer matches as
  `discovery_changed`, checks every folder to grant with core's grant rules,
  shows one native confirmation that lists everything and, with a
  workspace, the folders it grants, records each profile, and then grants
  those folders and assigns the profiles to the workspace. Only folders this
  account can change are granted. The separate grant button of 0.68.0 is
  gone. A declined confirmation or a refusal keeps the list and the
  selection.

  **Package-network profiles** list each profile's origins and how many
  credentials are bound, and a **Remove** button. A line says a profile is
  configuration only: no run uses it and it grants no network access.
  **Add package profile…** sends a name and 1 to 8 origins typed one per line;
  Rust has core parse them first (a bad origin is refused before any dialog)
  and the native confirmation lists the canonical origins. Removing is
  confirmed natively; when credentials are bound to the profile's origins the
  confirmation says they are unbound and their stored keys deleted, as
  `workspace package-profile-remove` does, and `credential_orphaned` reports a
  key that could not be deleted. Binding a credential stays CLI-only.

  **Container grants** are Windows only. Elsewhere the group says so and offers
  no control, the overview's `container_grants_supported` is false, and the two
  commands return `unsupported_platform`. On Windows each grant lists its
  folder, workspace, access, and state (`pending`, `active`, `revoking`) with a
  **Revoke** button. The group's help says that each workspace's commands run
  in its own AppContainer, a Windows sandbox with no access to the operator's
  files until a grant adds it, that registering a folder grants it read and
  write there, and that turning a profile on for a workspace grants its
  folders (ADR-0107).
  **Grant a folder** has a **Workspace whose commands get access** select, an
  access select (**Read and execute a folder you choose**, or **Read and write
  the workspace's own folder**), and a hint that follows the access. Both
  buttons call `settings_grant_container_folder` with the workspace and the
  access. For read and execute, **Grant folder…** has Rust open the folder
  dialog. For read and write, **Grant the workspace folder…** opens no
  dialog: core accepts that access only on the workspace's own directory, so
  Rust uses the recorded one, and core still checks that the directory at
  that path is the recorded directory
  ([ADR-0104](adr/0104-grant-the-workspace-folder-read-write-without-a-folder-dialog.md)).
  A recorded path this system cannot form is refused as `invalid_request`.
  Either way Rust shows a confirmation that names the workspace, the folder
  (once, for read and write), and the access, says that the access goes to
  the workspace's sandbox, and says that the folder's access-control list
  changes on disk until the grant is revoked. A failed list
  change leaves the grant `pending` (the command fails with
  `container_access_failed`); **Revoke** tries again, and a revocation that
  fails stays `revoking`. Revoking checks the revision under the lock, stops
  the workspace's earlier runs, and needs no revision from core.

  The **Skills** group (ADR-0082): **Import skill…** calls
  `settings_import_skill` with only the revision; Rust opens a native folder
  dialog and imports the folder through the call `omgcode skills add` uses,
  so the skill arrives disabled, nothing in it runs, and core's checks apply
  (a folder named as the skill, `SKILL.md`, the size and shape bounds, at
  most 64 skills). A refusal comes back as a controlled error with core's
  code and fixed text that names no path. Each skill then has one select
  for its **Application** setting (enabled or disabled; "Not set (disabled)"
  while unset) and one select per registered workspace, shown by its folder,
  with **Inherit the application setting**, **Enabled**, and **Disabled**,
  which call `settings_set_skill_state` (a `null` workspace is the
  application scope; `skill_state` is `enabled`, `disabled`, or `inherit`),
  and a **Remove** button, which calls `settings_remove_skill`. Enabling lets
  the `main` window select the skill for a turn, which sends its text to the
  model as a labelled untrusted message, so Rust shows a native confirmation
  that names the skill, its scope, and a bounded display-safe description
  before any change that leaves the skill enabled at that scope, including an
  inherit that ends enabled; a change that leaves it as it was, and every
  disable, asks nothing. Removing is always confirmed: the text says the
  registration, its settings, and its stored copy in private storage are
  deleted, the imported folder is not touched, and the skill must be imported
  again, disabled, to return. When the registration is removed but the stored
  copy cannot be fully deleted, the command fails with `package_orphaned`;
  the copy is inert, and the window reads the settings again and shows the
  skill gone. Each select shows the recorded state again after a declined
  dialog or a refusal. The window shows a skill's name only; its description
  and body stay unrendered.
- **Workspace scope** (ADR-0071). **Workspace settings**, an icon button
  with a folder beside **Send** (ADR-0080), calls `settings_show_workspace` with
  the open session's identifier and nothing else; it is disabled until a
  session is open. Rust resolves the workspace the session was recorded in
  through the session index and the settings store, never from the webview.
  A session that is unknown, or whose workspace is not registered, is
  refused with `not_found` or `unknown_workspace`, and the composer shows the
  message. Otherwise Rust records that workspace as the window's scope in
  memory, one short mutex-protected write, and shows the `settings` window.
  Every overview carries `scope`, either `{ workspace_id }` or `null`. While
  the scope names a registered workspace, the window hides the section
  sidebar and shows only **Workspace settings** for that workspace: the
  Deny, Prompt, and Allow help, its folder, **Model tools**, the permission
  rows, **File-write tool limits** with **Set all**, its identifier, its
  package cache, and
  its **Custom instructions**.
  It shows no development root or **Register workspace…**, and the hidden
  sidebar lists only that workspace. **All settings** calls `settings_clear_scope`, a `settings`
  command, and the window then shows the sections as before. **Settings** in
  the sidebar calls `settings_show_window`, which clears the scope first. If
  the scoped workspace is no longer registered, the window shows the full
  view with a note. The window also reads the overview when it gains focus,
  so a window that was already visible picks up a new scope; this uses no
  `core:` event permission. The scope is display state: the `settings` window
  could already change every workspace, and the scope grants and removes
  nothing. A poisoned scope lock reads as no scope.
- **Status bar.** A footer along the bottom of `main` shows the open
  session's workspace folder, its Git branch, and the approvals segment
  (ADR-0064). Until ADR-0084 it also held a **Model** provider dropdown and
  a connection message; both are removed because the model box in the side
  panel footer chooses the provider and model per session (ADR-0071, ADR-0089)
  and shows the failure or
  "The model provider was not changed." note beside its own dropdowns, so no
  connection message is lost. The footer calls no connection command, but
  its focus and tree-change refresh still asks the shared connection state
  to read `connection_status` again, so the model box follows a connection
  changed in the `settings` window. `connection_select` stays in the `main`
  capability only for the model box's **Provider** dropdown, with the native
  Yes/No dialog described there. The branch comes from
  `session_context`, which reads only `HEAD` through the anchored broker (see
  "Git branch display"). The footer reads the branch again when the window
  regains focus, when a session opens, and after a turn settles. The footer
  uses no `title` tooltips; the reason a branch is unavailable is shown as
  text. Elsewhere in `main` a tooltip holds fixed text, or untrusted text
  that is neutralised: the session row, the conversation header, a
  transcript label, and the Processes command line.
- **Git branch display.** `session_context` opens the session's registered
  workspace by its recorded root and workspace identities and reads
  `.git/HEAD`, at most 4096 bytes, without following a link. No Git program
  runs, and no Git configuration, ref, or object is read. When `.git` is a
  file of the form `gitdir: <path>`, the named directory's `HEAD` is read
  only when it lies inside the development root, from the root's own handle.
  The check compares names with the recorded root, so a pointer that spells
  the root another way (an 8.3 short name, a junction, or a `subst` drive)
  shows "Branch unavailable".
  `ref: refs/heads/<name>` shows the name, another ref shows in full, and a
  40- or 64-digit commit shows as "Detached at" its first seven digits. A
  missing `.git` shows "Not a Git repository". Anything else, a name over
  255 bytes, or a name with whitespace or a control character shows "Branch
  unavailable" with a fixed reason. The name is display-safe text and is
  never sent to the model.

Only this window receives the configuration view. The `main` window receives
only the workspace summaries of `workspace_list` (each registered
workspace's identifier, folder display text, and `model_tools`, `read`,
`write`, and `execute` states), the endpoint text of
`chat_defaults`, the model ids of `chat_models`, the enabled skill names of `session_skills`, the branch
display of `session_context`, and the connection view, the session's model,
and the profile names, kinds, and endpoints of `connection_status`. None
carries a credential.

The view reads the settings store through the same core functions as the
trusted CLI and releases it before returning. It reads no stored secret and
returns no secret value. Every path and recorded name is passed through
core's display-safe functions; a path that this system cannot represent is
shown as a fixed note. A skill's description and body are never returned.
An error is shown with its code, and **Refresh** stays available.

### Changing settings

The five change commands call the same `ControlStore` functions as the CLI's
`workspace set-root`, `register`, `permissions`, `set-package-cache`, and
`clear-package-cache`. They use the same expected-revision check, validation,
and audit events. Each takes only identifiers, the revision the window last
read, and for `settings_set_permissions` the complete preference set:
`model_tools`, the six three-state preferences, and `write_tools`. None
takes a path. Each returns the new overview.

- **Folders come from a native dialog.** Rust opens the operating system's
  folder picker through `rfd`, modal to the settings window. The chosen path
  goes from the dialog to core and never reaches a webview. Registration
  starts the picker at the development root. No Tauri plugin and no dialog
  command exists, so no webview can open, answer, or read a dialog.
- **Widening needs a native confirmation.** These changes are applied only
  after the operator answers Yes in a native message box: setting the root,
  registering a workspace, widening any preference or tool limit, and
  choosing a package cache root. A state widens when it moves towards Allow:
  from Deny to Prompt or Allow, or from Prompt to Allow. The service composes
  the text from fixed words and display-safe values. It names each widened
  preference with its old and new states, what it allows, and whether model
  calls will ask first or run without asking. Narrowing and clearing a cache
  root show no dialog.
- **No lock while the operator decides.** The service checks the revision,
  releases the store, shows the dialogs, and then reopens the store and
  applies the change with the same revision. A change made meanwhile, for
  example by the CLI, makes the request fail with `stale_revision`, and
  nothing is applied.
- **Revocation is observed.** The runs a change withdraws are listed while
  the settings lock is still held, then they and any others the settings
  withdraw are stopped and observed. A run stops even if a later change
  restores its access before its watcher reads the settings, because the
  watcher checks every recorded settings state (ADR-0061). A run that is not
  observed to stop, or a run journal that cannot be read, fails the command
  with `runs_not_stopped`, and the change stays recorded.
- **One change at a time.** While one change waits for the operator, another
  fails with `settings_change_running`. The window disables its controls
  until the answer arrives.

A closed dialog or a No answer returns `cancelled`. The window shows "No
change was made." and reads the configuration again, so every control shows
the recorded value. Any other failure is shown with its code and is followed
by the same re-read.

### Choosing the model connection

The `main` window's `connection_select` chooses the provider of **one
session** (ADR-0071). It takes the session id and a profile name, or `null` for
the default, and nothing else. Rust refuses, before any dialog, a launch
argument that fixed the connection (`connection_fixed`), an unknown session
(`not_found`), a profile core would not accept (`control`), and a choice the
session already has (`control`). It then shows the native Yes/No dialog:
"Send this session's chat turns to `<provider>` through the connection
`<name>`?" with the endpoint, and, for a hosted provider, a paragraph saying
that prompts, workspace text read by tools, and command output leave this
computer for the endpoint's host. For an `openai_compatible` profile it adds
that the address was typed when the connection was added (ADR-0092). The command takes the one settings change at a time, so a
second change is refused with `settings_change_running`. After Yes the
settings are read again, and a profile removed or given another endpoint while
the dialog was open records nothing (`stale_revision`). Only then is the
choice recorded with the endpoint that was confirmed, and the session's model
is cleared. It does not change the global default or the settings revision.
`session_model_select` records a model for a session with no dialog; it
applies the rule `chat_send` applies (at most 256 bytes, no control
character; an empty id clears the choice). A session with no record gets one
that follows the default connection, so choosing a model never pins a
provider the operator did not confirm.
`session_effort_select` records a session's reasoning effort the same way,
with no dialog (ADR-0102). It deserializes only the seven names and `null`,
refuses an effort the session's connection does not take with
`invalid_request`, and keeps the session's model. `null` records no effort
(Default, which is not the `none` effort) and is accepted on any connection.
It runs as one settings change, so
a provider choice cannot land between its check and its save; while another
change runs, such as a provider choice awaiting its confirmation, it is
refused with `settings_change_running`. The per-session record stores it as
an optional `reasoning_effort`, omitted when none is chosen, so
`session-connections.json` keeps version 1. `connection_select` clears it.
The `settings` window's `settings_select_connection` keeps choosing the
**global default**, which sessions without their own choice follow; it uses
the same checks and dialog. The `settings` connection commands call
the same `ControlStore` functions as the
CLI's `workspace provider-profile-select`, `provider-profile-add`,
`provider-credential-set`, and `provider-profile-remove`, with the same
revision check and validation (ADR-0064, ADR-0071).

- **Selecting needs a native confirmation.** `settings_select_connection`
  takes the revision and a profile name, or none for the default. The
  service refuses an unknown profile, a profile that cannot be chosen (core's
  selectable rule: a local kind with a recorded endpoint, or a hosted
  profile with its endpoint and a bound credential), and an unchanged choice
  before any dialog. It then asks "Send chat turns of sessions that have no
  connection of their own to Ollama through the connection `<name>`?" (or
  LM Studio, a hosted provider, or the default), with the endpoint,
  and records the choice only on Yes. It applies to turns of sessions
  without their own choice that start afterwards. When a launch argument fixed the connection, the command is
  refused with `connection_fixed`.
- **Adding needs no dialog.** `settings_add_connection` takes a name of at
  most 256 bytes and a kind. For `lm_studio` or `ollama_local` it takes an
  endpoint and no key, and records a profile with authentication `none`;
  core refuses an endpoint that is not `http://<loopback IP>:<port>/v1`. For
  a named hosted kind (`ollama_cloud` and, since ADR-0092, `anthropic`,
  `openai`, `google`, `meta`, `moonshot`, `tencent`, `thinking_machines`,
  `minimax`, `open_router`, `z_ai`, `deepseek`, and `xai`) it takes an
  `api_key` and no endpoint: Rust supplies the kind's one fixed endpoint and
  records authentication `api_key`. For `openai_compatible` it takes both an
  endpoint and an `api_key`; core applies the typed-endpoint rule of
  [provider authentication](provider-authentication.md#hosted-providers-adr-0092).
  A request that gives a local kind a key, gives a named hosted kind an
  endpoint, or omits the field its kind needs is refused as
  `invalid_request` before anything is recorded. A new profile is not used
  until it is selected.
- **The API key is write-only** (ADR-0059 decision 6, ADR-0071 decision 5).
  The key is typed into a password field, sent once in `settings_add_connection` or
  `settings_set_api_key`, and the field is emptied as the button is pressed,
  whether the answer is success or failure. The window keeps it in no state
  object, log, or message. In Rust the request type moves it into a zeroing
  buffer on arrival, its `Debug` form is fixed text, and core's secret type
  refuses an empty key, one over 16 KiB, and (in the desktop) one with a
  control character, as one fixed `invalid_request` message that never
  repeats the key or its length. Core's credential store keeps the key in
  DPAPI or the Secret Service and binds it to the profile; no command returns
  the key, its length, or a digest of it. Because a profile cannot be bound
  before it exists, Rust opens the native store first (an unavailable or locked
  store refuses the request before anything is recorded), records the profile,
  then stores and binds the key. If storing or binding fails, the profile is
  removed again in the same locked store and a binding it gained is deleted,
  so a failure leaves no profile without a key; the settings revision still
  advances by the two recorded events. If the profile cannot be removed,
  the error is `credential_orphaned`, with the opaque identifier of an item
  that could not be deleted. A binding whose journal write failed may still be
  recorded, so then neither the profile nor the new item is removed: the
  error is `credential_binding_uncertain` and names the kept item's
  identifier (and any item it replaced), as the CLI does, for the operator to
  check with `provider-profile-show`. No error shows the key or native error
  text. `settings_set_api_key` replaces the key
  of a hosted `api_key` profile (and refuses a local kind, a `subscription`
  profile, which never holds a key and shows no **Set key…**, a profile
  recorded without an endpoint, which can never be used and whose row says
  so, or an unknown name) the way the CLI's `provider-credential-set` does: the new item is
  stored, then bound, then the old item is deleted. It shows no dialog,
  because the key goes only to the same host the profile already names
  and choosing the connection stays confirmed separately; a failure to delete
  the old item is reported as `credential_orphaned`, which says the new key is
  set and names the old item's identifier, since no profile names it any more.
- **A data folder that is not private can be repaired** (ADR-0073). On
  Windows a data folder that grants another account access refuses every key
  change with `credential_storage_not_private`. The error shows **Remove extra
  permissions…**, which calls `settings_repair_data_folder` with no arguments.
  Rust lists the folder's allow entries for principals other than the current
  user, SYSTEM, and Administrators and shows the folder and each account, by
  name and SID, in a native Yes/No dialog. On Yes it removes every allow
  entry for those accounts, refusing with `data_folder_changed` if the set of
  accounts differs when it looks again (a changed access mask for a listed
  account does not). It keeps every other entry, makes inherited entries explicit, and
  stops the folder inheriting. A folder with nothing to remove succeeds with
  no dialog. A link, an untrusted owner, an unknown entry type, or a result
  that would not give the user the rights the store needs on the folder is
  `data_folder_not_repairable`. Both the user's own entries and Windows'
  `AccessCheck` must grant them. After No, `data_folder_changed`,
  `data_folder_unreadable` (the folder could not be opened, read, or
  checked before the dialog), or `settings_change_running` (another
  settings change held the guard) nothing has changed, so the button stays
  on offer. After a repair the
  window says that only the user, SYSTEM, and Administrators have access,
  and asks the operator to try the change again. When reading the settings
  again then fails (`data_folder_repaired`) it says the same about access,
  and the error alone gives the next step: close and reopen the window.
  The key field was already emptied, so the operator types the key again.
- **Removing a connection with a stored key, or the active one, is
  confirmed.** `settings_remove_connection` removes a profile and, through
  the same core path as the CLI's `provider-profile-remove`, deletes its
  bound credential. A profile with a stored key first asks "Remove the
  connection `<name>` and delete its stored API key?", and, when it is the
  selected profile and no launch argument fixed the connection, adds that new
  turns for sessions without their own connection go to the default
  connection and its endpoint. Removing the selected profile without a key asks
  the ADR-0064 question about the default endpoint, which receives
  `OMGCODE_LM_TOKEN` when it is set. Removing any other profile shows no
  dialog. No keeps the profile, its key, and its selection. The native store is
  opened before anything is removed, so an unavailable one refuses the
  removal; a native item that cannot be deleted after the profile is gone is
  reported as `credential_orphaned` with that item's identifier. A session that chose the removed profile
  refuses its next turn with `session_connection_stale` until the provider is
  chosen again.
- **Signing in with ChatGPT** (ADR-0093). The **Model** section's **Sign in
  with ChatGPT** disclosure takes a connection name (default `chatgpt`), and a
  signed-out ChatGPT row offers the same button. `settings_sign_in_chatgpt`
  takes the revision and the name. Rust refuses a stale revision, a profile
  that is not an `openai` subscription profile, one already signed in, and a
  new name the settings store would not record (for example a blank one),
  then asks "Open OpenAI's sign-in page in your browser to connect `<name>`
  to your ChatGPT plan?", which names what goes to OpenAI and says that a
  completed sign-in does not show the plan will accept OMGCode's requests.
  After Yes a new name is recorded as an `openai` subscription profile with
  OpenAI's endpoint, and Rust opens the system browser itself: the webview
  never receives the authorization address, the callback, or a token. From
  the confirmation to the end of the browser step, the window shows a bar
  reading "Confirm the sign-in, then finish it in your browser." with
  **Cancel sign-in** outside the disabled body; it calls
  `settings_cancel_chatgpt_sign_in`, which only sets the sign-in's cancel
  token, so a cancel sent while the confirmation is open is kept. A cancel
  that arrives after the browser step but before the tokens are recorded
  revokes them (best effort) and returns `cancelled`; once the one local
  recording write starts, the sign-in completes. A failed
  sign-in shows its stable code, such
  as `plan_usage_not_granted`, and stores no token, except in two cases. When
  the write that binds the new tokens has an uncertain result, the tokens are
  kept and not revoked, and the error (`signin_binding_uncertain`) names their
  identifier and says to check it with `provider-profile-show`; if the
  profile does not name it, the operator deletes the item and disconnects
  OMGCode in ChatGPT's settings. When the
  binding certainly failed but the new token item could not be deleted, the
  session is revoked (best effort) and the error (`signin_not_recorded`) names
  the item's identifier; the item may remain in the operating system's
  credential store, and the operator deletes that named item there. A
  settings change during the browser step that makes the recording stale or
  refused stores nothing and returns the `control` conflict or refusal; the
  operator reads the settings again and signs in again. A sign-in or
  sign-out that was recorded but whose settings could not be read again
  for the window is `signed_in_unread` or `signed_out_unread`, which says
  the change took effect and to reopen the window. A signed-in
  row reads
  "Signed in with ChatGPT as `<email>`", carries a **ChatGPT plan** chip and
  no API key field, and offers **Sign out**, which asks "Sign `<name>` out of
  ChatGPT?" and calls `settings_sign_out_chatgpt`. From the confirmation to
  the end of the revocation the same bar reads "Confirm the sign-out. OMGCode
  then asks OpenAI to end the session, which can take a minute." with
  **Cancel sign-out**, which calls the same cancel command. A cancel before
  the sign-out holds the session lock returns `cancelled` and changes
  nothing; a later cancel stops the wait for OpenAI, and the sign-out is
  still recorded here as unconfirmed. When OpenAI did not
  confirm the revocation the window says so and points to ChatGPT's
  settings. A token slot that could not be deleted makes a recorded
  sign-out `credential_orphaned`, naming each slot's identifier and saying
  whether OpenAI confirmed the revocation; a sign-out whose record is
  uncertain is a `credential` error whose `signout_uncertain` message names
  them instead. A signed-in connection cannot be removed. A ChatGPT profile
  recorded without OpenAI's endpoint shows the no-endpoint note and offers no
  sign-in, because core would refuse it. No real sign-in was
  made from the shell, and the browser launch is not driven by a test.
- **Ollama context length.** OMGCode cannot set Ollama's context length over
  this API. Set it in Ollama, for example with `OLLAMA_CONTEXT_LENGTH`, and
  give the composer the same context size.

## Approvals

A model tool call needs a trusted single-use approval when its effective
state is Prompt: the file-write tools (edit, multi-edit, create, rename,
delete, and create directory) and the command tool under the default `write`
and `execute` state, and, since 0.43.0, the read and list tools when `read` is
Prompt
([agent instructions](agent-instructions.md#approval-records),
[deny, prompt, or allow](workspace-permissions.md#deny-prompt-or-allow-adr-0063)).
Since 0.64.0 (ADR-0098) the `web_fetch` tool needs one under Prompt, and under
Allow for a host that is not on the workspace's allowlist. A call whose state
is Allow needs none otherwise. The CLI
commands `approvals list`, `show`, `approve`, and `deny` still work and remain
the surface for headless use. The approvals window is a second surface for the
same records. It adds no authority. Core still enforces single decisions,
expiry, the settings revision, and the writers-stopped acknowledgement.

The flow:

1. A chat turn makes a call whose state is Prompt. Core records a pending
   approval, and the turn waits for the decision (ADR-0063). It publishes the
   exact call it waits on so that the approvals window can review it.
2. The `main` window polls `approvals_summary` every 3 seconds and shows the
   pending count. It can show the approvals window and nothing more.
   Since 0.69.0 (ADR-0105) Rust also shows the approvals window as soon as a
   desktop turn starts waiting, before the wait begins, through an observer
   the shell registers with the service at startup. The observer posts one
   closure to the event loop, which checks the foreground and acts in the
   same callback. When OMGCode owns the foreground window (on Windows, read
   with `GetForegroundWindow`), the window is unminimized, shown, and
   activated with `SetForegroundWindow` alone, not tao's `set_focus`. When
   another application owns it, or Windows refuses, the window is shown and
   its taskbar button flashes until the operator comes to it, and the
   foreground is not taken: on Windows that would send the other
   application a simulated key press. No
   webview can trigger this, and the window decides as below. An approval
   recorded by a CLI turn shows only in the count.
3. The operator opens the `approvals` window, or it opens itself (step 2).
   It polls `approvals_list` every 3
   seconds. Polling stops while a window is hidden. Since 0.69.0 (ADR-0105)
   it also lists again when the window gains focus, so a request that
   brought it forward is listed without waiting for the next poll; a new
   request never replaces the one under review. Since 0.45.0 (ADR-0065)
   the window shows a queue beside a review pane; each column scrolls on its
   own, and below 720 pixels wide they stack. Since 0.66.0 (ADR-0101) the
   line between them moves, to widen the queue or, stacked, make it taller
   (see [section dividers](#section-dividers)). The queue has two tabs:
   **Pending (n)**, the pending records soonest-expiring first as selectable
   rows with a countdown, and **History (n)**, the 50 most recent decided
   records as rows with a state chip and no control. Decided records
   therefore never push the review or its buttons down.
4. The window selects a record: the soonest-expiring pending one when it
   opens, and the next one whenever the selected record stops being pending
   (decided, expired, or gone). A poll that only adds a record keeps the
   selection. The operator can select another row by clicking it or with the
   up and down arrow keys in the pending list; Escape in the review returns
   focus to the selected row. Each selection calls `approval_detail`, and
   only the answer to the newest request is shown, because a newer request
   replaces the review token. Core
   looks for the recorded tool request that the approval is bound to by
   recomputing the approval digest with the same function the tool handler
   uses. The result is one of three bindings:
   - `exact`: a recorded request reproduces the digest. The window shows the
     request fields and a review token.
   - `stale`: a request of that kind exists but no digest matches, for example
     because the file or the settings changed. The window shows no arguments
     and offers Deny only.
   - `not_found`: no recorded request exists to review. Deny only.
5. For a file change, the operator must tick the writers-stopped checkbox. This
   is the operator's statement that no other process is writing the file.
   Neither this window nor the CLI detects a concurrent writer.
6. Approve is enabled only for an `exact` binding with a token, after the
   checkbox for a file edit, and one second after the review panel renders.
   It is never the default focus. Every newly shown review, including the one
   the window moves to after a decision and one loaded again after an expired
   token, starts with the checkbox unticked and its own one-second delay. When
   the operator selects a record or the window moves on after a decision,
   focus goes to the review heading. No key approves or denies. The effect
   line, the checkbox, and **Deny** and **Approve** stay in a bar at the
   bottom of the review pane; the request digest, tool identifier, and
   binding are in a collapsed **Technical details** section.
7. `approval_decide` redeems the token against the record's current digest in
   the same critical section as the decision, then calls core to approve. A
   token is random, single use, bound to the approval identifier and digest,
   valid for 120 seconds, and kept in memory only. A new detail request for the
   same approval replaces it. At most 64 tokens are outstanding. Deny needs no
   token.
8. The waiting turn sees the decision within about 250 ms. An approved call
   is dispatched again, so every live check and digest is repeated, then it
   consumes the record and runs through the broker, the trusted runner, or
   checkpointed apply, in the same turn. A denied call ends with
   `approval_denied` and the turn continues; cancelling the turn ends the
   wait and leaves the record pending. The time spent waiting does not count
   against the turn deadline.

The detail reports `waiting`, which is true only when a turn waits on that
approval now. Above the decision buttons, an exact pending review states the
effect: "Approving runs this request now" when its turn is waiting, or that
approving runs nothing now and only lets the model's next identical request
run once, for example after the turn was cancelled or from the CLI. The
window's introduction says the same.

Reviewed fields:

- **Command:** Profile, Program, Arguments (numbered, one per line), Working
  directory, Network (`off`), Host services (`off`), Time limit (`20 seconds`),
  and Output limit (`4096 bytes per stream`).
- **Edit:** File, Old text, New text, Size, Current SHA-256, and Proposed
  SHA-256. The multi-edit tool numbers its texts (`Old text 1`, `New text 1`,
  and so on), and the create tool shows File, Size, SHA-256, and Content.
  Since 0.45.0 the window draws each old and new pair as a line difference,
  labelled **Changes** or "Edit 1 of N", with `-`, `+`, or a space before
  each line and runs of more than six unchanged lines collapsed to "N
  unchanged lines" with three lines of context. The difference is computed in
  the window from the display-safe text, over at most 2,000 lines a side and
  an edit distance of at most 2,000; past either bound it shows the two blocks
  and "Too large to compare line by line.". A create shows its content as
  added lines, or one block past 2,000 lines.
- **Create directory:** Directory (ADR-0079).
- **Rename:** From, To, Size, and SHA-256 of the source. The review is stale
  once the source changed or the destination exists.
- **Delete:** File, Size, and SHA-256. The review is stale once the file
  changed or is gone.
- **Read:** Action (`Read a file` or `List a directory`), Path, and `Most bytes` or
  `Most entries`. Reviewing it reads no file.
- **Web fetch** (since 0.64.0, ADR-0098): URL, Host, Host on the allowlist
  now (`yes` or `no`), Offset, and Maximum bytes. The URL is shown whole,
  escaped to one line. An exact pending review adds "Approving sends this
  exact URL, including its path and query, to that host.", and, when the
  host is not listed, "This host is not on the workspace allowlist." A web
  fetch needs no writers-stopped checkbox, and its decision sends
  `writers_stopped: false`. Reviewing it fetches nothing and reads no file.

Review reads only what the approval needs:

- Only `command`, `file_read`, `file_write`, and `network` approvals can be reviewed.
- Opening a command approval changes nothing.
- Opening a file-write approval makes one audited broker read of the target
  file (two for a rename, which also reads the destination), under the same
  live `read` requirement as the tool, and writes one audit record per read.
  It tries at most eight distinct candidate requests, newest first.
  It happens only when the window shows that approval's review, never while
  listing. Since 0.45.0 the window shows a review when the operator selects a
  record and also when it selects one itself: on opening, and when the
  selected record stops being pending. Each of those is one detail request,
  so each shown review makes the reads of one detail request: one, or two for
  a rename. If a detail request fails, the error stays on screen until the
  operator selects a request or a review is shown; the window does not retry
  it, because each retry would be another audited read.
- Opening a record that is no longer pending reads nothing.
- Review does not check that the approval is still unexpired. Core checks that
  when the record is consumed.
- While a turn waits on an approval, the review uses the call the turn
  published instead of reading the locked journal. Core still recomputes the
  digest from that call and the live workspace, so only the recorded request
  can be `exact`. While a turn runs for any other reason, its journal is
  locked on Windows, so the detail request reports `busy` until the turn
  ends.
- Tokens are lost when the application exits. The operator reviews again.

Display-safe text. Core renders the fields the window shows, plus session
titles, folders, and approval summaries. Control characters other than the line
feed, the soft hyphen, bidirectional and zero-width characters, variation
selectors, tag characters, and several other invisible format characters become
visible `\u{xxxx}` escapes. A backslash that could forge such an escape is
escaped too. The character list is `is_hidden_format` in
`crates/omgcode-core/src/turn.rs`. It does not cover look-alike letters
(homoglyphs), mixed scripts, or ordinary spaces. An operator can still be
misled by a request that uses such text.

## Git status tab

`workspace_git_status` and the view in `ui/src/main/git_tab.ts` show the open
session's Git branch and changed files, read-only
([ADR-0085](adr/0085-read-git-status-with-gix-without-running-programs.md)).
The **Git** tab of the right-hand panel shows `createGitTab()` with the
`gitStatus` rail icon, and its styles are part of `ui/src/styles/main.css`. It
reads when it is shown, when the open session changes while it is shown, and
when a turn settles, so the status run never starts for a hidden tab; see
[right-hand panel](#right-hand-panel).

- **Scope.** The command takes only a session id. Rust resolves the session's
  recorded workspace, opens it by its recorded identities as the branch reader
  does, and refuses an unregistered workspace with `unknown_workspace`. A
  folder with no `.git` is the `not_repository` state, shown as "Not a Git
  repository.", not an error. When `.git/HEAD` cannot be read, the anchored
  handle names what `.git` is, and nothing is followed: a `.git` file (a
  linked worktree or submodule pointer) shows "Git status unavailable: the
  .git entry is a file, so linked worktrees and submodules are not shown"; a
  `.git` folder without a readable `HEAD`, and a `.git` link or special entry,
  each show their own reason.
- **No program runs.** The repository is read in process with the `gix` crate,
  opened with isolated permissions (no system, global, user, or environment
  configuration, no includes). Every section of the repository's own
  `.git/config` is rejected, so a `filter.<name>.*` or `diff.<name>.*` command
  is never known, and `core.fsmonitor` is read by nothing. No hook runs.
  Submodules are ignored. The status run writes nothing, not even the index.
  A test sets each of those keys to a command that would create a marker file
  and asserts that none appears.
- **Result.** The branch is the same view as the status bar's. Up to 1,000
  changed paths are sorted by path, each with a `HEAD`-to-index state (added,
  modified, deleted, renamed, type change) and an index-to-worktree state
  (modified, deleted, untracked, conflicted). An untracked folder is one entry
  with a trailing `/`. `truncated` is true when more paths changed. A run that
  takes over 10 seconds is interrupted and reported as unavailable. A path is
  display-safe text, lossy for non-Unicode names, and never used as a path.
  The command runs on the blocking pool.
- **View.** A branch line and a **Refresh** button, then the paths grouped as
  **Staged**, **Changed**, **Untracked**, and **Conflicted**, each row a
  state letter (`A`, `M`, `D`, `R`, `T`, `?`, `U`) and the path. A path both
  staged and changed appears in both groups. The letter's tooltip is fixed
  text such as "Staged: added"; a path is never placed in a tooltip. "No
  changes." shows for a clean repository, a note shows when the list is
  truncated, and an error shows as text. `showSession(null)` clears the view
  and a new session reads again; an older answer is dropped.
- **Mock.** `npm run dev` answers `workspace_git_status` with a small fixed
  status that includes a markup-like name.

## Security properties and limits

What the design does:

- **Two enforcement layers.** Every command is declared to the Tauri build, so
  each needs an explicit permission in a capability file. The `main` file does
  not grant the three approval commands or any of the thirty-two settings-window
  commands. The `settings` file grants only those thirty-two. Every handler also calls the Rust
  window policy as its first statement. A capability file edited by mistake
  does not expose a decision to `main`.
- **No `core:` permissions.** Neither window is granted any Tauri core
  permission. Adding one needs an ADR.
- **One style write.** `ui/src/ui/size_property.ts` sets one of a fixed list
  of size custom properties to a whole number of pixels from 0 to 16 384, or
  removes one, through the CSSOM, and throws for any other name or value
  (ADR-0072, ADR-0101). `ui/src/main/layout.ts` passes the column widths and
  `ui/src/ui/splitter.ts` the section sizes, each computed and clamped. No
  other frontend code sets a style, and no text reaches one;
  `tests/security.test.ts` checks both.
- **A names-only file view.** `workspace_entries` lets the `main` webview learn
  the file and folder names of the open session's registered workspace without
  the `read` preference (ADR-0083). It returns no content, size, or date,
  hides the `.OMGCode` store, never follows a link out of the workspace, takes
  no host path, and grants nothing.
- **No host paths.** No command accepts a host path; `workspace_entries` takes
  only relative name components that Rust validates and resolves below the
  session's workspace. The service returns no journal
  path, no data root, no token, and no tool output digest. The path-like
  text the `main` window receives is: workspace folder display text; the
  Files tab's names, which are names and kinds only, at most 512 per folder,
  with `.OMGCode` hidden and no link followed (ADR-0083); the Git tab's
  changed paths relative to the workspace, as lossy text, at most 1,000
  (ADR-0085); and the Processes tab's program text, escaped to one line and
  at most 256 characters, which can be the run's resolved program path
  (ADR-0086). The view shows all of it as text nodes, and only
  `workspace_entries` takes listed names back, as relative components. The
  `settings` window also receives the recorded development root, profile
  roots and path entries, package cache roots, and grant folders, all as
  display-safe text. No command takes any of them back as input. A settings
  change gets its folder from a native dialog that Rust opens.
- **Endpoint and token outside `main`.** The `main` webview cannot supply an
  endpoint, choose the credential source, or read the token. It may only
  request a recorded profile, or the default, for one session with
  `connection_select`; Rust records it only after the operator confirms a
  native dialog, and not at all when a launch argument fixed the connection
  (ADR-0064, ADR-0071). The `settings` window chooses the global default
  through the same check and dialog. No webview reads a token. A stored credential goes only to the endpoint
  recorded with its profile (ADR-0048).
- **An API key crosses the `settings` webview once.** The operator types an
  hosted connection's key into a password field of the `settings` window
  (ADR-0059 decision 6, ADR-0071 decision 5, ADR-0092). The key is in that webview's memory
  while it is typed and in one IPC message (`settings_add_connection` or
  `settings_set_api_key`); the field is emptied as the button is pressed,
  whether the command succeeds or fails, and the window holds the key in no
  state object, storage, log, or message. That is a weaker boundary than the
  CLI's standard input, and it is the cost the operator chose over a native
  prompt. The window renders no model- or repository-controlled text, which
  narrows who can reach it. Rust moves the key into a zeroing buffer on
  arrival (zeroing reduces, and cannot guarantee, the removal of copies the IPC layer
  or the operating system make), applies the CLI's checks, stores it in the
  native credential store, and returns nothing derived from it: no key, length,
  or digest, and no error text that repeats it. The `main` window has no
  command that carries or reads a key. A key replaced by a compromised
  `settings` webview still reaches only the host that profile already
  records, which a replaced key cannot change; choosing
  that connection for a session or as the default stays behind a native
  confirmation, which is why replacing a key shows none.
- **Per-session choices are re-checked and not tamper-evident.**
  `<data root>/desktop-v1/session-connections.json` maps a session id to the
  provider chosen for it and its model. Only Rust writes it, atomically, under
  one in-process lock and an exclusive file lock on the neighbouring
  `session-connections.lock`, so two desktop processes on one data folder do not
  drop each other's choices; a write waits at most two seconds for that lock and
  then fails with `busy`. Reads take no lock, because a write is an atomic
  rename. Neither lock is ever held across a dialog or a request. It holds a profile
  name, the confirmed endpoint, and a model name, and no credential. It is
  limited to 4,096 sessions and 1 MiB; a new entry beyond that is refused with
  `session_choices`. It is parsed strictly (a version, no unknown field); a
  damaged file is reported by `connection_status` as unreadable, treated as
  empty, and replaced by the next choice, so it never blocks chat. While it is
  reported unreadable, the composer shows a warning that the session follows
  the default connection and that the next choice replaces every session's
  saved choice. It is not
  tamper-evident: a process running as the same user can edit it, as it can
  edit the settings journal. Every turn, model list, and status resolves the
  order launch argument, then the session's record, then the global default.
  A recorded profile is resolved through core each time. If it is gone, is
  not selectable, or its endpoint no longer equals the recorded one, the turn
  and the list are refused with `session_connection_stale`, so an edit can
  only choose a profile that is already recorded and selectable, and a profile
  removed and added again with another endpoint receives nothing until the
  operator confirms it again. The existing `connection_changed` check still
  compares the endpoint the window sent with the resolved one.
- **Channel, no `core:` permission.** `chat_subscribe` takes a Tauri IPC
  channel that the `main` webview creates. It needs no capability beyond the
  command's own permission. Tauri events would need a `core:event` permission,
  and both windows still have none. Events are sent while the hub lock is
  held, so replay and live events stay in order, and the sink must not block.
  The `turns` lock and the `events` lock are never held together.
- **Content policy.** The content security policy is `default-src 'none'`, with
  `'self'` for scripts, styles, images, and fonts, and IPC as the only
  connection target. Tauri appends three hashes for its own bootstrap scripts.
  The global Tauri object is disabled and the object prototype is frozen. File
  drop is disabled.
- **Navigation guard.** A window may navigate only to the application origin
  (`http://tauri.localhost` on Windows, `tauri://localhost` elsewhere) and, in a
  debug build, `http://localhost:1420`. New-window requests are denied.
  `about:blank` is denied.
- **Text-only rendering.** The frontend builds its document from elements and
  text nodes. Tests scan the source for markup-injecting interfaces, inline
  script or style, remote resources, and browser storage. The Markdown
  renderer for recorded replies (ADR-0067) builds elements the same way: raw
  HTML stays literal text, no element gets an `href` or `src`, and a link
  always shows its destination. Its parse is bounded in input size, nesting
  depth, and delimiter search, and tests cover hostile and pathological
  replies.
- **Decisions only from `approvals`.** The approvals window loads a separate,
  smaller bundle. The main bundle contains neither `approval_decide` nor
  `approval_detail`. The command name `chat_subscribe` lives in
  `src/ipc/chat_events.ts`, which only the main bundle imports, and a bundle
  test checks that no approvals asset contains it.
- **The configuration view and changes only in `settings`.** The settings
  window loads its own bundle. The thirty-two settings-window command names appear
  only in `src/ipc/settings_client.ts`. Bundle tests check that no main or
  approvals asset contains any of them, and that no settings asset contains
  `approval_decide`, `approval_detail`, or `chat_subscribe`.
- **No dialog surface for webviews.** The manifest has no Tauri plugin, and
  only the settings handlers open dialogs. A test checks both.
- **The index is a cache.** It authorizes nothing. A journal path from the
  index is revalidated against the strict layout, and the writer open
  validates the journal again by replay.

What it does not do:

- A compromised `main` webview can use every `main` command. It can read
  transcripts, search, create sessions in registered workspaces, send chat
  turns, archive, restore, and interrupt. It can subscribe to chat events and
  read the provisional text of running turns, which it could already read from
  transcripts once recorded. It cannot name a host path, choose
  the endpoint, read the token, or decide an approval. It can cause approval
  requests; it cannot decide them. It can overwrite the saved chat
  preferences, which grant nothing, and select enabled skills for a turn,
  which load as untrusted text and grant no permission. It can show the
  settings window. It cannot call any settings-window command.
- A compromised `settings` webview can read the configuration view: recorded
  paths, profile names, endpoints, origins, and skill names. It can turn
  preferences off, clear package cache roots, disable a skill, remove an
  unselected connection that holds no API key, and open the native dialogs
  again and again, one at a time. It can also ask to delete, each behind a
  native confirmation it cannot answer: a workspace registration with its
  settings and instructions file, a skill's stored copy, a connection with
  its stored API key, a toolchain profile, a package-network profile with
  the credentials bound to its origins, and a Windows container grant. It
  cannot choose a folder, answer a dialog, widen access or enable a skill
  without the native confirmation, read a secret, or call any other window's
  command. It can ask to import a
  folder only through the native folder dialog, so it cannot name one.
- On Windows the native confirmation's default button is Yes, because `rfd`
  offers no way to change it. A keyboard Enter confirms.
- The approvals webview is still web content. The design reduces what reaches
  it and what it can call. It does not make the webview a trusted computing
  base. A native dialog for the final confirmation remains a possible later
  step.
- Display-safe escaping does not detect homoglyphs, mixed scripts, or every
  invisible code point.
- The default WebView2 context menu stays enabled. Developer tools are enabled
  in debug builds only.
- Transcript text is shown as recorded. Only text rendering and the content
  security policy stand between it and the page. Markdown formatting is chosen
  by the model: a reply can make text look like a heading, a quote, or a code
  block that the operator did not write. A code block's **Copy** button writes
  what the block shows, including any command the model chose; it is
  user-initiated and write-only. The DOM builder neutralises the
  attributes a person reads or hears (`title`, `aria-label`, and the like) as
  it does text, and so do the icon-button and popover helpers, so recorded
  text such as a workspace folder in a tooltip or an accessible name cannot
  reorder what is shown or announced. Visible markers replace bidirectional
  overrides, but zero-width and other invisible characters are neither marked
  nor removed, so they are copied too. A reply's **Copy Markdown**, **Copy
  text**, and **Copy image** buttons follow the same rule (ADR-0068). A
  reply's label is the model id from the turn's request manifest, which the
  model server chooses; it names the model asked, not an identity. Copying has
  been checked under jsdom with stubbed clipboard, canvas, and layout, and as
  a refused write in a Chromium pane; a successful copy of text or an image in
  WebView2 or WebKitGTK has not been exercised. Preview text is returned as
  received after token redaction, like transcript text. The frontend renders
  it as text and marks bidirectional overrides.
- Core's approval checks, the trusted runner, and checkpointed apply apply
  unchanged. They do not detect a concurrent writer and are not process or
  workspace isolation.
- The index file holds transcript text. It is protected like the journals and
  is not encrypted.
- The per-window capability enforcement is a Tauri property. The Rust layer has
  unit tests. A live call that reaches the Rust check, past a capability
  refusal, has not been exercised.

## Limits and constants

| Item | Value |
| --- | --- |
| Commands | 67 |
| Git status changed paths | 1,000, and a 10-second deadline |
| Sessions per listing | 5,000 |
| Search terms and length | 16 terms, 256 characters |
| Search hits per request | 200 |
| Transcript entries per page | 200 |
| Transcript text per page | 1 MiB (at least one entry) |
| Running chat turns | 2 |
| Chat preview per request | 16 KiB |
| Chat delta text | 1,024 bytes |
| Sessions with a recorded provider or model | 4,096 |
| `session-connections.json` | 1 MiB |
| Previews kept for untracked sessions | 4 |
| Client preview bound | 64 KiB of UTF-16 code units |
| Finished chat results kept | 64 |
| Message and instructions prefilter | 64 KiB each |
| Skills selected per turn | 4 |
| Saved model name | 256 bytes, no control characters |
| Listed models | 2,048 ids of at most 256 bytes, a 4 MiB body, a 10-second deadline |
| Saved instructions | 64 KiB |
| Chat preferences file | 512 KiB |
| Review token lifetime | 120 seconds, single use |
| Outstanding review tokens | 64 |
| Decided approvals listed | 50 most recent |
| Journals discovered | 50,000, and 500,000 directory entries |
| Journals inspected per refresh call | 1,000 |
| Refresh chunk and deadline in the shell | 200 journals, 2 seconds |
| Refresh reuse window | 1 second |
| Settings store retry | 2 seconds at 20 ms |
| Index busy timeout | 2 seconds |
| Shutdown grace | 5 seconds |
| Model request cancel check interval | 25 ms |
| Approve button delay | 1 second |
| Processes listed | 100 calls; 32 arguments and 256 characters per item, 2,048 characters per call; 100 run journal events read; polled every 2 seconds only while a call runs (ADR-0086) |
| Web audit listed | 100 calls from the 1,000 newest action records; 4,096 characters per target; command text under the Processes bounds; never polled (ADR-0098) |
| Poll and check intervals | chat reconcile 5 seconds, chat fallback poll 750 ms, approvals 3 seconds |

## Development

Run frontend commands from `crates/omgcode-desktop/ui`:

| Script | Purpose |
| --- | --- |
| `npm ci` | Install the locked dependencies. `.npmrc` sets `ignore-scripts=true`, so no install script runs |
| `npm run dev` | Vite dev server on port 1420 |
| `npm run build` | `tsc --noEmit`, then `vite build` into `dist/` |
| `npm run check` | Strict type check |
| `npm run lint` | `biome check .` |
| `npm run format` | `biome format --write .` |
| `npm test` | Vitest with jsdom |
| `npm run coverage` | The same tests with coverage; writes `coverage/lcov.info` for the Sonar analysis and prints a summary |
| `npm run test:dist` | Build, then check the built bundles |

Outside Tauri, the Vite dev server serves an in-memory mock backend from
`src/mock/` so the interface can be reviewed without a service. Production
builds drop it, and a test fails if a mock fixture reaches `dist/`.

To run the real shell in development, use the Tauri CLI through the frontend
package, and always pass a scratch data directory so the run does not touch the
real per-user data:

```text
cd crates/omgcode-desktop/ui
npm run tauri -- dev -- -- --data-dir <scratch-directory>
```

The `tauri` script changes to `crates/omgcode-desktop` before it runs the Tauri
CLI. The first `--` passes the arguments through npm. After `dev`, the next two
`--` separators pass `--data-dir` through the Tauri CLI and Cargo to
`omgcode-desktop`. Use an empty scratch directory. See the
[frontend README](../crates/omgcode-desktop/ui/README.md) for the current
script notes. `tauri dev` drops the `custom-protocol` feature and loads the dev
server.

On a Windows host whose temporary directory is not on NTFS or ReFS or whose
ACLs the private store refuses, see the notes in
[testing standards](testing-standards.md#desktop-shell-and-frontend).

## Tests

- **Frontend (Vitest, jsdom).** Model, transcript, composer, approvals, polling,
  mock, and security scans. The scans assert the rules in the content policy
  list above, that `@tauri-apps/api` is imported only in `src/ipc/client.ts`,
  that the main bundle has no decision command, that no approvals asset
  contains `chat_subscribe`, and that no mock fixture is in `dist`. The suites
  `ui/tests/chat_runner.test.ts` (subscription, reconcile, and fallback poll),
  `ui/tests/live_preview.test.ts` (the live entry and its bounds), and
  `ui/tests/client.test.ts` (the channel wrapper) cover the streaming frontend.
  The main window tests also cover the cancelled notice, which is decided by
  the error code only, saving preferences, the unreadable note, the send
  keys with **Enter to send** on and off, and the skill choice and its
  limit. `ui/tests/settings_window.test.ts` covers the
  read-only view, hostile recorded text, errors, and refresh. The scans
  also confine the settings command to the settings client and keep the
  settings window free of main, approvals, and chat code. Since 0.45.0
  (ADR-0065): `ui/tests/tokens.test.ts` checks the theme's contrast;
  `ui/tests/ui_components.test.ts` the segmented control, tabs, and popover;
  `ui/tests/approvals_diff.test.ts` the line difference and its bounds;
  `ui/tests/approvals_flow.test.ts` the queue, automatic selection, the fresh
  delay and checkbox for every shown review, focus, the absence of a decision
  key, and that a late detail answer is dropped;
  `ui/tests/settings_sections.test.ts` the sections, the workspace field, the
  workspace scope (one workspace, no sidebar, **All settings**, a focus read),
  permission rows, the Advanced groups, and the workspace page's toolchain
  profile switches (ADR-0107); `ui/tests/settings_skills.test.ts`
  the Skills controls and the mock skill commands (ADR-0082);
  `ui/tests/settings_execution.test.ts` the profile and grant controls and
  their mock commands, including assignment (ADR-0088, ADR-0107);
  `ui/tests/settings_discovery.test.ts` the generated proposals, selection,
  add requests with and without a workspace, and their mock commands
  (ADR-0103, ADR-0107); and
  `ui/tests/main_redesign.test.ts` the View menu, the status-bar approvals
  segment, the composer menus and counter, and the compact transcript rows.
  Since ADR-0071: `ui/tests/model_picker.test.ts` covers the model
  list, including the regression that the list is kept when its endpoint
  differs from the composer's; `ui/tests/session_connection.test.ts` covers
  reading a session's provider and model when it opens, switching sessions
  without a dialog, choosing a provider for the open session (the model is
  cleared and disabled until the list arrives), the status bar showing
  no provider dropdown, a status or list that arrives for a session that was left,
  the hosted warning, launch-fixed connections, and a stale choice.
  `ui/tests/settings_window.test.ts` and `ui/tests/settings_sections.test.ts`
  also cover the Ollama Cloud form (the endpoint hidden and the fixed one
  shown, a password field, the key sent once and the field cleared after a
  success and after a refusal, and absent from the page and from every other
  call), a named hosted provider's fixed endpoint and an `openai_compatible`
  endpoint and key (ADR-0092), **Set key…**, **Remove** for a profile with a
  stored key, and the
  hosted label.
  Since ADR-0072: `ui/tests/layout.test.ts` covers the column order, the
  separators' splitter attributes, collapsing each panel to its rail and
  moving focus, each rail button, the waiting count on the rail, the terminal
  panel's note and its absence of any command, keyboard and pointer resizing
  with limits, collapse by drag and by Enter, the conversation's minimum,
  narrowing the window, expanding a panel beside a wide one, collapsing the
  other panel when both minimums do not fit, the size observer, focus on a
  visible row when the open session is in a collapsed group, and disposal.
  `ui/tests/live_preview.test.ts` covers following the end: a send from
  higher up, scrolling up and back down, growth without a scroll, a turn's
  end while pinned and while scrolled up, and **Load more**, including a
  send made while its page loads. `ui/tests/transcript_layout.test.ts`
  checks the cascade that centres operator messages and live and recorded
  replies at 96% width with left-aligned text. jsdom reads the properties
  that decide the layout. The security scan keeps every style write in
  `src/ui/size_property.ts`, checks its two writes and its checks, checks
  that every listed name is a width or height property, and that
  `src/main/layout.ts` still passes a clamped width for one of two sides.
  Since ADR-0101: `ui/tests/splitter.test.ts` covers the splitter's
  attributes, keys in both orientations and placements, Shift steps, the
  limits, a drag that leaves the line, the primary button only, the
  stylesheet's size until a move and its restoration, the neighbour's least
  size, narrowing when the neighbour shrinks, a size per orientation,
  disposal, and the size property's refusals.
  `ui/tests/approvals_flow.test.ts`, `ui/tests/settings_window.test.ts`, and
  `ui/tests/layout.test.ts` cover each window's divider: its place, name,
  control, and property, the stacked `approvals` orientation through a stubbed
  `matchMedia`, the media query matching `approvals.css`, and the `settings`
  divider hiding in a scoped window.
  Vitest processes CSS, so the stylesheet scans read the real source and
  assert it is non-empty.
- **Shell (no webview).** The policy table; the capability files against the
  policy; the build script's command list against the policy; the frontend's
  command names against the policy; the configuration and the content security
  policy; the navigation guard; handler registration; that every handler
  authorizes first with the matching command; handler parameter names against
  the frontend's argument keys; the launch argument parser; and the shutdown
  gate.
- **Service.** Tests with a mock provider cover chat, approvals, review
  tokens, shutdown, and the index. `crates/omgcode-app/tests/desktop_events.rs`
  covers the event hub. `crates/omgcode-app/tests/desktop_settings.rs` covers
  the configuration view, session skills, skill selection, and saved
  preferences. `crates/omgcode-app/tests/desktop_session_connection.rs` covers
  the per-session choice: the confirmation, the recorded choice and model, the
  model list and turn for that session, the launch argument winning, a removed
  or re-added profile making the choice stale, refusals before any dialog, the
  Ollama Cloud dialog text, a hosted provider's dialog naming its host and
  flagging a typed address, model validation, and a damaged file.
  `crates/omgcode-app/tests/desktop_cloud_connection.rs` covers adding Ollama
  Cloud with a key against the real settings and native credential stores
  (the profile, the fixed endpoint, the bound item, selectability), refusals
  that record nothing and never repeat the key, that a request never prints
  its key, replacing a key, removing a bound profile with a declined and an
  accepted dialog (the native item is deleted), the active-profile dialog
  text, and a session that chose a removed profile becoming stale, and since
  ADR-0092 adding a named hosted provider with its fixed endpoint and an
  `openai_compatible` connection with its typed endpoint, with their
  refusals. `crates/omgcode-desktop/tests/layers.rs` checks that the frontend
  names every provider kind and shows core's fixed endpoints. Unit tests in
  `session_connections.rs` cover the file's round trip, bounds, and damage. The desktop chat and event tests also cover cancelling
  a streamed turn and shutting down while one runs, and cancelling a turn
  whose request is not streamed.
- **End to end.** The real Windows shell was driven over the WebView2 remote
  debugging port with real mouse and text input against a mock local model
  server, on 2026-11-16. A second run on the same day drove streaming and the
  pushed events against a paced event-stream mock, and a third drove
  cancellation and exit against a mock that holds a stream open. See
  [testing standards](testing-standards.md#desktop-shell-and-frontend) for the
  procedure and what each run proves.

## Known limits

- A local profile (`lm_studio` or `ollama_local`) records only a literal
  loopback address over plain HTTP. A hosted profile uses its kind's one fixed
  HTTPS endpoint, or, for `openai_compatible`, a typed HTTPS address, and
  needs a stored API key; an `openai` profile with `subscription` uses a
  ChatGPT sign-in instead (ADR-0093). No hosted provider's chat, stream, or
  tool-call shapes were verified with a real key.
  A profile recorded before 0.31.0 has no endpoint and cannot be selected until
  it is removed and added again with `--endpoint`. Any process running as the
  same user can read the stored credential. The Phase D2 and D3
  stored-credential tests ran against DPAPI on Windows and the Secret Service
  on Linux under WSL. A chat turn with a stored credential was not driven
  through the real shell's windows on either platform; the service-level
  tests cover it.
- Linux: the shell builds and its tests pass, but no window has been opened
  there. Nothing that needs a webview has run on Linux. The Unix-only
  inspection and index branches (`O_NOFOLLOW` opens, inspection beside a live
  writer) and the file-symlink refusals ran in the Linux test run.
- Two tests that need file symbolic links print `SKIPPED link assertions` and
  return on a host without the privilege to create them. They did not run on
  the Windows development host; they ran on Linux.
- Not exercised: a live call that reaches the Rust `authorize` check. The
  real shell's windows were not driven again for 0.34.0; the service tests
  cover cancelling a non-streamed turn.
- The CI workflows build and test the shell on Ubuntu and Windows Server
  2025 but open no window there.
- Phase S1 (0.40.0): the `settings` window, saved preferences, and skill
  selection were checked by unit, service, and bundle tests and in the Vite
  development server against the mock backend. The real shell's windows were
  not driven for 0.40.0.
- ADR-0082: the skill commands were checked by service tests with a scripted
  prompt (`crates/omgcode-app/tests/desktop_settings_skills.rs`), by unit,
  layer, and bundle tests, and against the mock backend. The native dialogs
  and the real shell's settings window were not driven by automation, and
  nothing ran on Linux.
- ADR-0088: the profile and grant commands were checked by service tests with
  a scripted prompt (`crates/omgcode-app/tests/desktop_settings_execution.rs`),
  by unit, layer, and bundle tests, and against the mock backend. Until
  ADR-0107 no test applied a container grant, because that changes a
  folder's access-control list and creates an AppContainer profile on the
  host; the confirmation text
  and the paths before the change are tested instead, and the platform refusal
  is tested only for non-Windows builds, which did not run here. A package
  profile with a bound credential was not exercised. The native dialogs and
  the real shell's settings window were not driven by automation.
- ADR-0103: discovery was checked by core tests with Windows and Linux fixture
  trees for every family, and the commands by service tests with a scripted
  prompt and a fixture host
  (`crates/omgcode-app/tests/desktop_settings_discovery.rs`), by layer and
  bundle tests, and by Vitest against a fake transport and the mock backend.
  On Windows the root checks read real access-control lists of fixture
  folders; until ADR-0107 no test applied a grant. The Linux-only core tests (a linked root
  and a FIFO at an installer's path) and the service tests ran in WSL Ubuntu
  24.04 and in CI. Discovery ran against real toolchains only through an
  ignored test run by hand on one Windows computer, the native dialogs and
  the real shell's settings window were not driven by automation, and the
  desktop shell never ran on Linux.
- ADR-0107: assignment was checked by core tests (recording, replay,
  refusals, removal, plans, a changed plan, the grants a removal releases,
  and the workspaces a revocation names), by run and turn tests, by CLI
  tests, and by service tests with a scripted prompt
  (`crates/omgcode-app/tests/desktop_settings_execution.rs`,
  `desktop_settings_changes.rs`, and `desktop_settings_discovery.rs`). On
  Windows the CLI and service tests that register, assign, add with a
  workspace, unassign, remove, or unregister apply and revoke real grants on
  temporary folders, and each test revokes every grant it recorded before it
  ends; service tests that are not about registration record workspaces
  without granting. Layer and bundle tests check the command lists, and
  Vitest checks the switches and the discovery panel against a fake
  transport and the mock backend. The core, CLI, and app suites also ran in
  WSL Ubuntu 24.04. The native dialogs and the real shell's settings window
  were not driven by automation.
- Phase S2 (0.41.0): the change commands were checked by service tests with a
  scripted prompt, by unit, layer, and bundle tests, and in the Vite
  development server against the mock backend. The native dialogs and the
  real shell's settings window were not driven by automation. The Linux
  dialog path has never run.
- 0.42.0 (ADR-0062): the send keys were checked by unit, main window, and
  service tests and in the Vite development server against the mock backend.
  The real shell's window was not driven, and no input method was used, so
  the composition guard is checked against synthetic events only.
- 0.45.0 (ADR-0065): the redesigned windows were checked by unit, window, and
  bundle tests and in the Vite development server against the mock backend.
  The real shell's windows, the window background colour, and the native
  dialogs over the dark theme were not driven by automation, and nothing ran
  on Linux. The contrast test covers token pairs, not every rendered
  combination.
- The approvals polls (3 seconds) add a small constant IPC load while a window
  is visible, and the session tree does not update by push.
- Streaming: the stream rules were checked against local fixtures only, and no
  live LM Studio server was used. No window has been opened on Linux, so the
  channel is unverified there. A streamed text reply has no usage counters
  unless the server sends them. Cancellation interrupts every model and
  compaction request but not a read, list, or edit tool call in progress, and
  the cancel path was checked against local fixtures only, with no live
  server. The thirty-second request deadline (120 seconds on the ChatGPT plan
  route) covers the whole response, so a long generation fails as before. While a turn runs, the transcript is not
  reloaded, so the message just sent is not shown above the live entry until
  the turn settles.
- Opening a very large first index can take seconds, because each journal
  commits once.
- No installer, signing, auto-update, or crash reporting exists.
