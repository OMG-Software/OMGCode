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
  dropdown until ADR-0084 removed it; the composer still chooses a
  session's provider. A launch argument still fixes the connection for the
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

Not implemented:

- Installers, signing, and Tauri bundles. `bundle.active` is `false`. The
  `latest` archives hold the bare binaries only.
- Interrupting a read, list, or edit tool call in progress. Cancelling such a
  turn waits for the call to end.
- Changing toolchain profiles, container grants, package-network profiles,
  and skills in the desktop application, binding a credential to a local
  LM Studio profile, and adding any cloud provider other than Ollama Cloud.
  They change only through the trusted CLI. The `settings` window adds an
  Ollama Cloud connection with its API key, replaces the key, and removes the
  connection, through the write-only key field (ADR-0071). The rest of phase
  S3 (a local profile's credential) and phases S4 and S5 of
  [ADR-0059](adr/0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md)
  are not implemented: execution setup and skill management.
- Development panels: a working terminal, task logs, diffs, and a file
  browser. The terminal panel (ADR-0072) only says that no terminal is
  available; it starts no shell and runs no command.
- Git, MCP, and memory interfaces, and skill management. The `main` window
  can only select an already enabled skill for one turn.
- A recovery interface for damaged journals or an incomplete tail.
- A provider other than a local LM Studio or Ollama server and Ollama Cloud.
  Stored API keys are used only for an LM Studio server's token and an Ollama
  Cloud key. The `main` window shows nothing about the credential source
  beyond the endpoint text and the hosted label.
- A native operating-system confirmation dialog for approvals.

Platform status. The shell was built, tested, and driven end to end on Windows
11 (build 26300) with WebView2 on 2026-11-16. On Linux (WSL 6.6, Ubuntu 24.04)
the whole workspace, including this crate, compiled against WebKitGTK and
passed formatting, Clippy, and every test on the same day. The Linux binary was
started only far enough to check its launch arguments: no window has been
opened on Linux, so the webview, IPC, capability enforcement, and navigation
guard are unverified there. The CI workflow changes have not run yet. See
[platform support](platform-support.md).

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

The shell has three webview windows and 39 commands. No other command exists.
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
| `session_context` | main | Report whether a session's workspace is registered and the Git branch it has checked out, for display |
| `connection_status` | main | Return a session's connection, where it comes from, its model, and the connections that may be chosen; takes the session id or `null` |
| `connection_select` | main | Request the default or a recorded profile as one session's provider; Rust shows a native confirmation |
| `session_model_select` | main | Record the model chosen for one session; no dialog |
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
| `settings_register_workspace` | settings | Register a folder below the root, chosen in a native dialog, after a native confirmation |
| `settings_set_permissions` | settings | Set one workspace's preferences and per-tool write limits; widening any needs a native confirmation |
| `settings_choose_package_cache` | settings | Choose one workspace's package cache root in a native dialog, after a native confirmation |
| `settings_clear_package_cache` | settings | Clear one workspace's package cache root; files stay in place |
| `settings_select_connection` | settings | Choose the default or a recorded local profile as the model connection, after a native confirmation |
| `settings_add_connection` | settings | Record an `lm_studio` or `ollama_local` profile with a loopback endpoint and no credential, or an `ollama_cloud` profile with its fixed endpoint and a write-only API key |
| `settings_set_api_key` | settings | Replace the stored API key of an `ollama_cloud` profile; the key is write-only and no dialog is shown |
| `settings_remove_connection` | settings | Remove a provider profile; one with a stored API key, or the active one, needs a native confirmation, and the key is deleted with the profile |
| `settings_clear_scope` | settings | Clear the window's workspace scope and return the unscoped overview |
| `settings_repair_data_folder` | settings | Remove the data folder's extra grants, listed by Rust, after a native confirmation that names each account (ADR-0073) |

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
near-black greys with an orange accent, and `color-scheme: dark`. There is no
light theme. Rust opens each window with the same background colour, so a
window does not flash white before its page paints. Native folder and
confirmation dialogs follow the operating system's theme.

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
  strip (tablist semantics: arrow keys select), and a `<details>` popover that
  closes on Escape or a click outside it. They import no IPC code, so every
  bundle can use them. The segmented control never changes itself; its window
  draws it again from the state the core returns.
- **Window sizes.** `main` opens at 1200 by 800 pixels with a minimum of 800
  by 560. `approvals` and `settings` open at 960 by 720 with a minimum of 600
  by 480, wide enough for their two columns.

The `main` window is laid out as follows.

- **Columns.** The sessions panel, the conversation, and the terminal panel,
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
  the frontend's only inline style.
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
  or the open session. The terminal rail holds one button that expands it.
- **Terminal panel.** A title row with a collapse button and a note that no
  terminal is available and that the panel starts no shell and runs no
  command. It calls no command and has no input.
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
  zero. **Review** and the sidebar row only call `approvals_show_window`. The
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
- **Composer.** The message box, then one toolbar: a **Skills (n)** menu,
  where n counts the skills chosen for the turn, hidden when no skill is
  enabled; a **Provider** dropdown and a **Model** dropdown with its
  **Refresh** button for the open session (see "Provider and model per
  session"); and a **Chat settings** menu, opened by a gear icon button named and
  tooltipped "Chat settings" that reports `aria-expanded` and `aria-controls`
  (distinct from the three-slider **Settings** icon), with the read-only endpoint,
  **Auto-compact**, the compaction slider, **Enter to send**, and **Save as
  defaults**. On the right are the byte counter, the running label, the
  **Workspace settings** icon button (see "Settings window"), **Cancel**, and
  **Send**. The counter appears only above 80% of the
  message limit or when the message is over it.
- **No waiting marker.** The `main` window cannot tell which session an
  approval belongs to: `chat_status`, the chat events, and `approvals_summary`
  carry no such signal, and the approval detail is not available to its
  bundle. It therefore shows only the advisory count.

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
Each model request still has a 30-second timeout of its own.
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
and shows its value, such as "200K tokens"; `chat_defaults` reports the
range. Compaction still triggers earlier when the model's safe input is
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
  **Refresh** button, in the composer toolbar since ADR-0071. `chat_models`
  takes the session id, resolves that session's connection exactly as
  `chat_send` does, stored credential included, and sends one
  `GET /v1/models` to its endpoint, which is a loopback endpoint or the fixed
  Ollama Cloud endpoint. Core refuses the whole list when it is not a `data`
  array of at most 512 entries with string ids, or when an id is empty, over
  256 bytes, repeated, or holds a control or bidirectional formatting
  character. The deadline is ten seconds. The answer carries the session id,
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
  workspace text sent to it leave this computer. On an Ollama Cloud
  connection every model is marked "(hosted)" and a warning says that
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
  provider and the model belong to the open session. The composer's
  **Provider** dropdown renders a state read from `connection_status` for
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
  session: both provider dropdowns are disabled and show "Set at launch", and
  the model is still chosen from that connection's list. `chat_send` keeps
  sending the composer's endpoint and model; Send is disabled without a model
  and while the session's provider is being read or has gone stale.
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

## Settings window

**Settings** in the session sidebar calls `settings_show_window`, which shows
the hidden `settings` window. The window calls `settings_overview` when it
starts, when it becomes visible again or gains focus, and when **Refresh** is
pressed. When
no settings store exists, that call creates an empty one, as the CLI's
`workspace status` does (ADR-0060). The window loads hidden at launch, so the
store exists from the first launch.

Since 0.45.0 (ADR-0065) a sidebar chooses one of three sections: **Model**,
**Workspaces** (shown first), and **Advanced**. **Refresh** and the settings
revision sit at the bottom of the sidebar, and errors and notices at the top
of the section. The chosen section, the chosen workspace, and which
disclosures are open are kept in memory only and survive a refresh and every
change. The sidebar is disabled with every other control while a change
waits. The window shows:

- **Model.** The connection the next turn uses, with a **Model provider**
  dropdown, and its provider, endpoint, and source. The dropdown lists the
  default LM Studio endpoint and every selectable profile; when a launch
  argument fixed the connection, it is disabled and says so. Then one row per
  provider profile, with its name, an **Active** chip, its provider,
  endpoint, and authentication, whether a credential is bound, and
  **Remove**. An `ollama_cloud` profile also has a **Hosted** chip, a line
  saying that prompts, workspace text read by tools, and command output leave
  this computer for ollama.com, and a **Set key…** disclosure with a
  password field and **Save key**. Then **Add connection**, a disclosure with
  labelled provider (`Ollama`, `LM Studio`, or `Ollama Cloud (hosted)`),
  name, and loopback endpoint fields, the endpoint prefilled with the
  provider's usual address (`http://127.0.0.1:11434/v1` or
  `http://127.0.0.1:1234/v1`). For Ollama Cloud the endpoint field is hidden
  and the fixed endpoint `https://ollama.com/v1` is shown as read-only text,
  and a labelled **API key** password field (`autocomplete="off"`) appears.
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
  **Register workspace…**, which is disabled until a root is set. With two or
  more workspaces, a **Workspace** field chooses which one is shown; choosing
  sends nothing, and the window shows the first workspace when the chosen one
  is gone. The shown workspace has its folder, a **Model tools** checkbox for
  `model_tools`, and one row each for `read`, `write`, `execute`,
  `task_network`, and `host_services`, each with a short description and a
  Deny, Prompt, or Allow segmented control. Under Write, the disclosure
  **File-write tool limits** holds a row for each model file-write tool's limit
  (edit, multi-edit, create, rename, and delete; since 0.43.0, ADR-0063). A
  tool whose limit is wider than `write` says "Limited by Write:" and the
  `write` state. Then the workspace identifier and its package cache root with
  **Choose…** and, when one is set, **Clear**. Then **Custom instructions**
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
- **Advanced.** Read-only, with no control that changes anything: toolchain
  profiles with their platform, read-only roots, path entries, and
  environment variable names, without values; package-network profiles with
  their origins and how many credentials are bound; container grants with
  their folder, access, and state; and each imported skill's name, its
  application setting, and its workspace overrides, with the workspace shown
  by its folder. A line says these change only through the trusted CLI.
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
  rows, **File-write tool limits**, its identifier, its package cache, and
  its **Custom instructions**.
  It shows no development root, **Register workspace…**, or workspace
  chooser. **All settings** calls `settings_clear_scope`, a `settings`
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
  a connection message; both are removed because the composer chooses the
  provider and model per session (ADR-0071) and shows the failure or
  "The model provider was not changed." note beside its own dropdowns, so no
  connection message is lost. The footer calls no connection command, but
  its focus and tree-change refresh still asks the shared connection state
  to read `connection_status` again, so the composer follows a connection
  changed in the `settings` window. `connection_select` stays in the `main`
  capability only for the composer's **Provider** dropdown, with the native
  Yes/No dialog described there. The branch comes from
  `session_context`, which reads only `HEAD` through the anchored broker (see
  "Git branch display"). The footer reads the branch again when the window
  regains focus, when a session opens, and after a turn settles. Like the
  rest of `main`, it uses no `title` tooltips; the reason a branch is
  unavailable is shown as text.
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
`model_tools`, the five three-state preferences, and `write_tools`. None
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
`<name>`?" with the endpoint, and, for Ollama Cloud, a paragraph saying that
prompts, workspace text read by tools, and command output leave this computer
for ollama.com. The command takes the one settings change at a time, so a
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
  selectable rule: a local kind with a recorded endpoint, or an `ollama_cloud`
  profile with a bound credential), and an unchanged choice
  before any dialog. It then asks "Send chat turns of sessions that have no
  connection of their own to Ollama through the connection `<name>`?" (or
  LM Studio, Ollama Cloud, or the default), with the endpoint,
  and records the choice only on Yes. It applies to turns of sessions
  without their own choice that start afterwards. When a launch argument fixed the connection, the command is
  refused with `connection_fixed`.
- **Adding needs no dialog.** `settings_add_connection` takes a name of at
  most 256 bytes and a kind. For `lm_studio` or `ollama_local` it takes an
  endpoint and no key, and records a profile with authentication `none`;
  core refuses an endpoint that is not `http://<loopback IP>:<port>/v1`. For
  `ollama_cloud` it takes an `api_key` and no endpoint: Rust supplies the one
  fixed endpoint `https://ollama.com/v1` and records authentication
  `api_key`. A request that gives a local kind a key, gives the cloud an
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
  of an `ollama_cloud` profile (and refuses any other kind or an unknown
  name) the way the CLI's `provider-credential-set` does: the new item is
  stored, then bound, then the old item is deleted. It shows no dialog,
  because the key goes only to the same fixed host the profile already names
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
- **Ollama context length.** OMGCode cannot set Ollama's context length over
  this API. Set it in Ollama, for example with `OLLAMA_CONTEXT_LENGTH`, and
  give the composer the same context size.

## Approvals

A model tool call needs a trusted single-use approval when its effective
state is Prompt: the file-write tools (edit, multi-edit, create, rename, and
delete) and the command tool under the default `write` and `execute` state,
and, since 0.43.0, the read and list tools when `read` is Prompt
([agent instructions](agent-instructions.md#approval-records),
[deny, prompt, or allow](workspace-permissions.md#deny-prompt-or-allow-adr-0063)).
A call whose state is Allow needs none. The CLI
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
3. The operator opens the `approvals` window. It polls `approvals_list` every 3
   seconds. Polling stops while a window is hidden. Since 0.45.0 (ADR-0065)
   the window shows a queue beside a review pane; each column scrolls on its
   own, and below 720 pixels wide they stack. The queue has two tabs:
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
- **Rename:** From, To, Size, and SHA-256 of the source. The review is stale
  once the source changed or the destination exists.
- **Delete:** File, Size, and SHA-256. The review is stale once the file
  changed or is gone.
- **Read:** Action (`Read a file` or `List a directory`), Path, and `Most bytes` or
  `Most entries`. Reviewing it reads no file.

Review reads only what the approval needs:

- Only `command`, `file_read`, and `file_write` approvals can be reviewed.
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

## Security properties and limits

What the design does:

- **Two enforcement layers.** Every command is declared to the Tauri build, so
  each needs an explicit permission in a capability file. The `main` file does
  not grant the three approval commands or any of the eleven settings-window
  commands. The `settings` file grants only those eleven. Every handler also calls the Rust
  window policy as its first statement. A capability file edited by mistake
  does not expose a decision to `main`.
- **No `core:` permissions.** Neither window is granted any Tauri core
  permission. Adding one needs an ADR.
- **One inline style.** `ui/src/main/layout.ts` sets the shell's
  `--left-width` and `--right-width` through the CSSOM, each a whole number
  of pixels it computes and clamps (ADR-0072). No other frontend code sets a
  style, and no text reaches one; `tests/security.test.ts` checks both.
- **No host paths.** No command accepts a path. The service returns no journal
  path, no data root, no token, and no tool output digest. Workspace folder
  display text is the only path-like text the `main` window receives. The
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
  Ollama Cloud key into a password field of the `settings` window (ADR-0059
  decision 6, ADR-0071 decision 5). The key is in that webview's memory
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
  `settings` webview still reaches only the fixed host `ollama.com`; choosing
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
  window loads its own bundle. The six settings-window command names appear
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
  preferences off, clear package cache roots, and open the native dialogs
  again and again, one at a time. It cannot choose a folder, answer a
  dialog, widen access, read a secret, or call any other window's command.
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
  user-initiated and write-only. Visible markers replace bidirectional
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
| Commands | 39 |
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
| Listed models | 512 ids of at most 256 bytes, a 1 MiB body, a 10-second deadline |
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
  permission rows, and the read-only Advanced section; and
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
  call), **Set key…**, **Remove** for a profile with a stored key, and the
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
  that decide the layout. The security scan keeps
  inline style to the two width properties in `src/main/layout.ts`.
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
  Ollama Cloud dialog text, model validation, and a damaged file.
  `crates/omgcode-app/tests/desktop_cloud_connection.rs` covers adding Ollama
  Cloud with a key against the real settings and native credential stores
  (the profile, the fixed endpoint, the bound item, selectability), refusals
  that record nothing and never repeat the key, that a request never prints
  its key, replacing a key, removing a bound profile with a declined and an
  accepted dialog (the native item is deleted), the active-profile dialog
  text, and a session that chose a removed profile becoming stale. Unit tests in
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

- Only `lm_studio` profiles can record an endpoint, and the only rule is for
  a literal loopback address over plain HTTP. No rule exists for a
  non-loopback provider, HTTPS, or certificates, and no cloud adapter exists.
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
- The CI workflow changes for the shell have not run.
- Phase S1 (0.40.0): the `settings` window, saved preferences, and skill
  selection were checked by unit, service, and bundle tests and in the Vite
  development server against the mock backend. The real shell's windows were
  not driven for 0.40.0.
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
  server. The thirty-second request deadline
  covers the whole response, so a long generation fails as before. While a turn runs, the transcript is not
  reloaded, so the message just sent is not shown above the live entry until
  the turn settles.
- Opening a very large first index can take seconds, because each journal
  commits once.
- No installer, signing, auto-update, or crash reporting exists.
