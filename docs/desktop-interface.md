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

Not implemented:

- Installers, signing, and Tauri bundles. `bundle.active` is `false`. The
  `latest` archives hold the bare binaries only.
- Interrupting a read, list, or edit tool call in progress. Cancelling such a
  turn waits for the call to end.
- Changing language-model connections, credentials, toolchain profiles,
  container grants, package-network profiles, and skills in the desktop
  application. They change only through the trusted CLI. Phases S3 to S5 of
  [ADR-0059](adr/0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md)
  are not implemented: the write-only API key field, choosing the active
  connection, execution setup, and skill management.
- Development panels: terminal tabs, task logs, diffs, and a file browser.
- Git, MCP, and memory interfaces, and skill management. The `main` window
  can only select an already enabled skill for one turn.
- A recovery interface for damaged journals or an incomplete tail.
- A provider other than the local LM Studio server. Stored API keys are used
  only for that server's token, through `--provider-profile`. The frontend
  shows nothing about the credential source beyond the endpoint text it
  already displayed.
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
| `--endpoint <url>` | `http://127.0.0.1:1234/v1` | The loopback model endpoint, fixed for the process. Not allowed with `--provider-profile` |
| `--provider-profile <name>` | None: the endpoint is `--endpoint` or its default, and the token comes from `OMGCODE_LM_TOKEN` | A recorded `lm_studio` provider profile that supplies both the endpoint and the stored credential used as the server token, fixed for the process. Not allowed with `--endpoint` |
| `--stream <on\|off>` | `on` | Whether chat turns send streamed model requests and push their text to the window. `off` sends non-streaming requests. The `finished` event is still pushed |

An unknown argument, a missing value, a repeated flag, an empty value, a value
that starts with `--`, a `--stream` value other than `on` or `off`, or both
`--endpoint` and `--provider-profile` prints the usage text to stderr and exits with status 2. The default value of
`--endpoint` does not count as giving it. An invalid endpoint, an unusable data
root, or a service that cannot open prints `omgcode-desktop: <message>` to
stderr and exits with status 1.
With `--provider-profile`, the shell resolves the profile once at launch. The
profile's recorded endpoint becomes the endpoint of the process, and the shell
also resolves the credential once. An unknown profile, a profile of another
provider kind, a profile with no recorded endpoint, an unbound profile, or an
unreadable credential prints `omgcode-desktop: credential: <cause>: <text>` and
exits with status 1. Both kinds of failure happen before any window opens. A
normal close exits with status 0.

The command line is the only configuration. The shell reads no environment
variable and no configuration file for these values. The one exception is the
local server token, which has two sources. Without `--provider-profile`, if
`OMGCODE_LM_TOKEN` is set, the service reads it for each chat turn. With it, the
service reads the credential bound to that profile from protected OS storage
for each chat turn and never reads the variable. The flag carries a profile
name, not a secret. The endpoint has the same two sources: `--endpoint` or its
default without a profile, and the profile's recorded endpoint with one. The webview never receives the token, and cannot choose or
see the profile. The service caches nothing, so a rotated credential is used by
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

The shell has three webview windows and 28 commands. No other command exists.
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
| `chat_defaults` | main | Return the fixed endpoint and the saved or built-in chat preferences |
| `chat_preferences_save` | main | Save the chat preferences the composer starts from |
| `session_skills` | main | List the names of the skills enabled for a session's workspace |
| `chat_send` | main | Start one chat turn on a supervised thread |
| `chat_status` | main | Report the state and outcome of a session's turn |
| `chat_cancel` | main | Request cooperative cancellation of a running turn |
| `chat_subscribe` | main | Subscribe a caller-created channel to chat events |
| `approvals_summary` | main | Return the advisory count of pending approvals |
| `approvals_show_window` | main | Show the existing approvals window |
| `settings_show_window` | main | Show the existing settings window |
| `approvals_list` | approvals | List pending approvals and recent decisions |
| `approval_detail` | approvals | Review one approval and obtain a review token |
| `approval_decide` | approvals | Approve or deny one approval |
| `settings_overview` | settings | Return the trusted configuration, creating an empty settings store when none exists |
| `settings_choose_root` | settings | Choose the development root in a native folder dialog, after a native confirmation |
| `settings_register_workspace` | settings | Register a folder below the root, chosen in a native dialog, after a native confirmation |
| `settings_set_permissions` | settings | Set one workspace's preferences and per-tool write limits; widening any needs a native confirmation |
| `settings_choose_package_cache` | settings | Choose one workspace's package cache root in a native dialog, after a native confirmation |
| `settings_clear_package_cache` | settings | Clear one workspace's package cache root; files stay in place |

`main` shows the session tree, search, transcripts, and chat. It renders
transcript text. `approvals` lists approval records, shows the reviewed
request, and holds the only decision controls. It starts hidden and never
renders transcript content. `settings` shows the read-only configuration
view. It starts hidden, never renders transcript text, tool output, or a
skill's description or body, and has no decision control. Closing
`approvals` or `settings` hides it. Closing `main` hides every window, runs a
bounded shutdown, and exits.

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
each can be changed before a turn. The model must be loaded on the
server, and the context size must match what the server loaded. A context of
8,192 tokens was too small for a tool-enabled turn in testing, because the
estimate reached the compaction trigger. A context of 32,768 tokens worked.

- **Send key.** With **Enter to send** on, the default, Enter sends the
  message and Shift+Enter or Alt+Enter adds a newline. With it off, Enter
  adds a newline. Ctrl+Enter (Cmd+Enter) sends in both modes. An Enter that
  confirms an input-method composition never sends. A send key never adds a
  newline, so Enter on an empty message shows why it cannot be sent. The
  check box is in **Chat settings** and applies at once; **Save as defaults**
  keeps it for the next run. A change made before the saved defaults load is
  kept; the loaded value does not replace it. `chat_defaults` returns it as `enter_to_send`,
  outside the turn settings, because it never reaches a turn (ADR-0062).
- **Saved preferences.** **Save as defaults** sends the model name, context
  size, output reserve, custom instructions, compaction threshold, the
  automatic compaction switch, and the **Enter to send** choice to
  `chat_preferences_save`. A file or request without `enter_to_send`, such
  as a file saved before version 0.42.0, reads it as on. An older build
  refuses a file that has the field as unknown, reports it as `unreadable`,
  and uses its built-in values until the next save. They carry no
  endpoint and grant nothing. The request refuses an unknown field, so an
  endpoint cannot be saved. A model name over 256 bytes or with a control
  character, and instructions over 64 KiB, are refused with
  `invalid_request`. The budgets are not checked when saved; `chat_send`
  checks them as before. The file is
  `<data_root>/desktop-v1/chat-preferences.json`, at most 512 KiB, read only
  as a regular file and parsed strictly. A save writes a new temporary file,
  syncs it, and renames it over the old one; on Unix it is created with mode
  0600 and the directory is synced. A missing file gives the built-in
  values. A file that cannot be read or parsed also gives the built-in values,
  `chat_defaults` reports it as `unreadable`, and the composer shows a note.
  The next save replaces a damaged regular file. It cannot replace a
  directory or another entry at that name, or a file it is not allowed to
  write: the save fails with `preferences`, and the operator must remove the
  entry or correct its access before saving again.
- **Skills.** When a session opens, `main` calls `session_skills`. It returns
  the names of the skills effectively enabled for the session's registered
  workspace, and nothing for a workspace that is not registered here or when
  no settings store exists. The composer lists them as check boxes and allows
  at most four. `chat_send` carries the selected names in `skills`, and the
  composer clears them after a send. The service refuses more than four, or
  a name that is not a valid skill name, with `invalid_request`, and a
  repeated name with `skill_selection_invalid`, before a turn starts. Core
  then loads each skill as the CLI's `chat --skill` does (ADR-0057): a skill
  that is no longer enabled fails the turn before any model request. A
  selected skill reaches only that turn's first request, as a labelled
  untrusted message, and is never journaled. The window shows names only.
- **Endpoint.** Fixed at launch. With a profile, the launch value is the
  profile's recorded endpoint. A request whose endpoint is not byte for byte
  the launch endpoint is refused with `invalid_request`. Each turn also
  resolves the profile again. If its recorded endpoint no longer equals the
  launch value, because the profile was removed and added again, `chat_send`
  fails at once with a `credential` error that starts `endpoint_changed:` and
  tells the operator to restart the application. No turn starts. A profile
  replaced with the same endpoint and a new credential is accepted, and the
  turn uses the new credential.
- **Token.** Resolved for each turn from the launch-time source: the stored
  credential of `--provider-profile`, or else `OMGCODE_LM_TOKEN`. It is held in
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
  labelled "Assistant (generating, not saved yet)", updated at most once per
  animation frame. The frontend removes it when the turn settles and shows the
  recorded transcript, and shows a note when the preview was cut off. It keeps
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
starts, when it becomes visible again, and when **Refresh** is pressed. When
no settings store exists, that call creates an empty one, as the CLI's
`workspace status` does (ADR-0060). The window loads hidden at launch, so the
store exists from the first launch. It shows:

- the settings revision;
- the development root, with **Choose folder…** or **Change…**;
- each registered workspace with its folder, a checkbox for `model_tools`, a
  Deny, Prompt, or Allow selector for each of `read`, `write`, `execute`,
  `task_network`, and `host_services`, a selector for each model file-write
  tool's limit (edit, multi-edit, create, rename, and delete; since 0.43.0,
  ADR-0063), and its package cache root with **Choose…** and, when one is set,
  **Clear**; and **Register workspace…**, which is disabled until a root is
  set. A preference at Allow is shown filled and one at Prompt outlined;
- the connection this process uses: the endpoint, and whether the token comes
  from `OMGCODE_LM_TOKEN` or from a named provider profile set at launch;
- provider profiles with their kind, authentication, endpoint, and whether a
  credential is bound;
- toolchain profiles with their platform, read-only roots, path entries, and
  environment variable names, without values;
- package-network profiles with their origins and how many credentials are
  bound;
- container grants with their folder, access, and state;
- each imported skill's name, its application setting, and its workspace
  overrides, with the workspace shown by its folder.

Only this window receives the configuration view. The `main` window receives
only the workspace summaries of `workspace_list` (each registered
workspace's identifier, folder display text, and `model_tools`, `read`,
`write`, and `execute` states), the fixed endpoint text of
`chat_defaults`, and the enabled skill names of `session_skills`.

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
   seconds. The list shows pending records first, then the 50 most recent
   decided records. Polling stops while a window is hidden.
4. The operator selects a record. The window calls `approval_detail`. Core
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
   It is never the default focus.
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
  It happens only when the operator opens that approval, never while listing.
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
  not grant the three approval commands or any of the six settings-window
  commands. The `settings` file grants only those six. Every handler also calls the Rust
  window policy as its first statement. A capability file edited by mistake
  does not expose a decision to `main`.
- **No `core:` permissions.** Neither window is granted any Tauri core
  permission. Adding one needs an ADR.
- **No host paths.** No command accepts a path. The service returns no journal
  path, no data root, no token, and no tool output digest. Workspace folder
  display text is the only path-like text the `main` window receives. The
  `settings` window also receives the recorded development root, profile
  roots and path entries, package cache roots, and grant folders, all as
  display-safe text. No command takes any of them back as input. A settings
  change gets its folder from a native dialog that Rust opens.
- **Fixed endpoint and token.** The webview cannot choose the provider, choose
  the credential source, or read the token. The profile and its endpoint are
  fixed at launch. A stored credential goes only to the endpoint recorded with
  its profile (ADR-0048). The wire contract is unchanged.
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
  script or style, remote resources, and browser storage.
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
  security policy stand between it and the page. Preview text is returned as
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
| Commands | 23 |
| Sessions per listing | 5,000 |
| Search terms and length | 16 terms, 256 characters |
| Search hits per request | 200 |
| Transcript entries per page | 200 |
| Transcript text per page | 1 MiB (at least one entry) |
| Running chat turns | 2 |
| Chat preview per request | 16 KiB |
| Chat delta text | 1,024 bytes |
| Previews kept for untracked sessions | 4 |
| Client preview bound | 64 KiB of UTF-16 code units |
| Finished chat results kept | 64 |
| Message and instructions prefilter | 64 KiB each |
| Skills selected per turn | 4 |
| Saved model name | 256 bytes, no control characters |
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

On a Windows host whose temporary directory is not on NTFS or whose ACLs the
private store refuses, see the notes in
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
  settings window free of main, approvals, and chat code.
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
  preferences. The desktop chat and event tests also cover cancelling
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
