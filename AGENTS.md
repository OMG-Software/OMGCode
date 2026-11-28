# OMGCode contributor guidance

## Project

OMGCode is an early coding-assistant prototype for Windows and Linux. The
repository contains a pinned Rust workspace with configuration-only workspace
preferences, a bounded native file broker exposed through trusted CLI commands,
and native identity checks. Follow [workspace settings](docs/workspace-permissions.md)
and [workspace file access](docs/file-access.md) for their contracts. Enabled
preferences never authorize execution. A prototype Tauri desktop shell adds a
session tree, full-text search, chat, and an isolated approvals window. It is
without an installer, has run only on Windows, and gives the webview no authority: the
Rust side authorizes every command and the trusted CLI remains. Follow the
[desktop interface](docs/desktop-interface.md) contract. The `main` window
renders a recorded assistant reply as a bounded Markdown subset built from
elements and text nodes; links and images stay inert text, and a code block's
**Copy** button writes its shown text to the clipboard
([ADR-0067](docs/adr/0067-render-recorded-replies-as-inert-markdown.md)).
Its sessions panel and a right-hand panel resize and collapse to rails of
icons, and the transcript follows its end after a send until the operator
scrolls up
([ADR-0072](docs/adr/0072-resize-and-collapse-the-main-window-panels-and-follow-the-transcript-end.md)).
Every other section divider in the three windows moves without collapsing,
and one module writes the frontend's only style, a listed size property
([ADR-0101](docs/adr/0101-make-every-section-divider-movable.md)).
Session rows are one line with the time since the last update. The right-hand
panel's tabs read only while visible: a Terminal note that starts no shell and
runs no command; a Files tree of names and kinds only
([ADR-0083](docs/adr/0083-show-a-names-only-workspace-file-tree-in-the-right-panel.md));
a Git branch and changed-path list read in process with `gix`, which applies
no repository configuration and runs no program or hook
([ADR-0085](docs/adr/0085-read-git-status-with-gix-without-running-programs.md));
the session's model command-tool calls, read-only
([ADR-0086](docs/adr/0086-show-the-sessions-subprocesses-in-the-main-window.md));
the session's web fetches and every model action after its first fetched web
content, read-only (ADR-0098);
and a per-part estimate of the next turn's context
([ADR-0087](docs/adr/0087-show-a-per-part-context-estimate-for-the-open-session.md)).
A reply is labelled with the model from its turn's request manifest and can be
copied as Markdown, as plain text, or as a PNG repainted on a canvas, through
the clipboard only, with icon buttons named in their tooltips
([ADR-0068](docs/adr/0068-name-replies-by-model-and-copy-them-as-markdown-text-or-image.md)).
Chat offers the session's model read and list tools; when the workspace
also enables `read` and `write`, six file-write tools (exact edit, multi-edit,
create, rename, delete, and directory creation); and, when it also enables `execute`, `read`, and
`write` and at least one toolchain profile of the running platform is
assigned to the workspace, one command tool whose `profile` parameter lists
those profiles. A command that names another profile is answered with
`profile_not_recorded` before any approval is requested
([ADR-0100](docs/adr/0100-list-the-recorded-toolchain-profiles-in-the-command-tool.md)).
A workspace runs only the profiles assigned to it, through the trusted CLI
`workspace profile-assign` or a switch on its page in the `settings` window.
On Windows assigning is the operator's consent to grant the workspace's
AppContainer read and execute on the profile's folders that this account can
change, after a confirmation that names them; registering a workspace grants
its folder read and write; and unassigning, removing a profile, or
unregistering revokes the grants no longer needed. Existing profiles start
assigned to no workspace
([ADR-0107](docs/adr/0107-assign-toolchain-profiles-to-workspaces-and-grant-on-assignment.md)).
Each workspace preference except `model_tools` is
`deny`, `prompt`, or `allow`, and each file-write tool has a limit that can
only narrow `write` ([ADR-0063](docs/adr/0063-deny-prompt-or-allow-each-preference-wait-for-approvals-in-the-turn-and-journal-model-actions.md)).
A model call whose state is `prompt` requires a trusted single-use approval;
one whose state is `allow` runs without one. Stored settings keep their
meaning, so model writes and commands stay at `prompt` until the operator
chooses `allow`. The edit tool replaces one exact text occurrence
in an existing file, and the multi-edit tool several exact occurrences in one
existing file, each through checkpointed apply. The create tool makes one new
file through the exclusive create, without a checkpoint. The rename tool moves
one regular file to a name that does not exist and never replaces an entry,
without a checkpoint. The delete tool removes one regular file of at most 1 MiB
after a durable checkpoint of its bytes, which the trusted CLI can restore. The
directory tool creates one directory whose parent exists, without a checkpoint
([ADR-0079](docs/adr/0079-add-a-model-tool-that-creates-one-directory.md)). All
six need the operator's writers-stopped acknowledgement when approved. The
create, edit, and multi-edit tools accept 32 KiB of argument JSON and every
other tool 4 KiB. A call with the supported shape and a usable, unique
identifier whose tool is unknown or whose arguments are too large, malformed,
or name an invalid path is answered with a recoverable result and recorded as
refused, and the turn continues
([ADR-0099](docs/adr/0099-recover-from-rejected-tool-calls-and-raise-write-tool-argument-limits.md)).
A separate `web_fetch` preference, `deny` by default, offers one web fetch
tool: the OMGCode process sends one HTTPS `GET` to a public DNS host and
returns a slice of the page text, labelled as untrusted web content. URLs
with another scheme or port, IP literals, and local names are refused, every
resolved address must be global unicast, the connection is pinned to the
checked addresses, and a redirect to another host is not followed. A
refused URL is answered with a recoverable `url_refused` result and recorded
as refused with the URL withheld by its digest, and the turn continues. Under
`prompt` every fetch needs an approval; under `allow` only a host on the
workspace's allowlist of at most 32 patterns runs without one. The settings
lock is released before the request
([ADR-0098](docs/adr/0098-add-a-model-web-fetch-tool-behind-a-host-allowlist.md)).
No other model network tool, and no Git or MCP tool, exists. Chat offers tools only for a
workspace whose `model_tools` preference is enabled. A model write or command
runs without a per-call approval only where the operator set it to `allow`.
There are no other provider adapters than the OpenAI-compatible one and the
Claude Code connection below. Every model tool call, executed or
refused, is recorded in the model action journal, which the trusted
`actions list` command shows; each call a session makes after its first
fetched web content is marked `after_web_content`, and
`actions list --after-web-content` lists only those. The
trusted CLI `run` command starts one program for a workspace whose `execute`
preference is enabled. It uses an operator-declared toolchain profile, records
intent and outcome in a run journal, and applies Landlock and a seccomp socket
filter on Linux or a per-workspace AppContainer on Windows. It never falls back
to the unconfined runner. Every run has tree-wide memory, process-count,
CPU-time, and sampled net disk-growth limits; on Linux the CLI must run in a
delegated cgroup v2. The disk limit measures writable roots and is not a
filesystem quota. Package-network profiles are recorded as configuration only;
no run uses them. The trusted `packages fetch` command downloads from declared
origins into a per-workspace cache. A selected root is exposed read-only on
Linux and Windows; Windows needs a read-execute container grant, and Linux
falls back to application storage. Provider profiles record connection shapes
without credentials. Trusted CLI commands can store API keys in protected OS
storage (DPAPI on Windows, the Secret Service on Linux) and bind them to
`api_key` profiles. An `lm_studio` or `ollama_local` profile can record a
loopback endpoint, and one local adapter serves both through the OpenAI-compatible
`/v1` API. The same client reads a credential for an operator-selected
`lm_studio` or hosted profile that supplies both the endpoint and the
token (ADR-0047, ADR-0048); an `ollama_local` profile has none. An
`ollama_cloud` profile accepts only the fixed HTTPS endpoint
`https://ollama.com/v1`
([ADR-0071](docs/adr/0071-choose-the-provider-and-model-per-session-and-add-ollama-cloud.md)).
The `anthropic`, `openai`, `google`, `meta`, `moonshot`, `tencent`,
`thinking_machines`, `minimax`, `open_router`, `z_ai`, `deepseek`, and `xai`
kinds each accept only their provider's one documented OpenAI-compatible
endpoint, and an `openai_compatible` profile records an operator-typed
`https://` base URL that core refuses rather than rewrites
([ADR-0092](docs/adr/0092-add-hosted-providers-and-an-openai-compatible-connection.md)).
A hosted API-key connection runs only with a stored API key; `anthropic`
still records `subscription` so older journals replay, and no adapter reads
one. No hosted provider's chat, stream, or tool-call shapes were verified with
a real key. An `openai`
profile with authentication `subscription` signs in with ChatGPT through
OpenAI's documented browser flow (PKCE, state, nonce, ID-token and plan-scope
checks) from the trusted CLI or the `settings` window, keeps rotating tokens
in protected OS storage under a per-session lock, and chats through the
Responses API
([ADR-0093](docs/adr/0093-sign-in-with-chatgpt-and-use-the-plan-through-the-responses-api.md)).
One real sign-in, model list, and text turn were observed; the plan route
streams with no `Content-Type`, which its head check accepts
([ADR-0096](docs/adr/0096-accept-a-plan-route-stream-without-a-content-type.md)).
In a request with tools, the plan route sends each tool result of the live
turn as the model's own call and a `function_call_output` item; Chat
Completions, replay, and compaction still send results as user text. Two
read and list tool turns were observed to finish, and other plan usage is
unverified
([ADR-0097](docs/adr/0097-send-a-tool-result-as-its-call-and-output-on-the-plan-route.md)).
A `claude_code` profile drives the operator's own signed-in, unmodified
`claude` CLI, from the trusted CLI's `chat` and `session compact` only:
OMGCode offers no Claude sign-in and never reads, stores, or passes the
Claude credential. On Windows the process runs in the connection's own
AppContainer with grants recorded by `workspace claude-code-grant`; its
environment is built from nothing; its built-in tools, settings, plugins,
and skills are switched off and its init report is checked; and OMGCode's
tools reach it only through a relayed MCP stdio server over a per-turn pipe,
through the same runtime checks. Revoking its grants or removing it ends
its running turns before the command reports success. It is personal use,
not evidence of Anthropic approval, and was tested with a fake `claude` only
([ADR-0109](docs/adr/0109-drive-the-signed-in-claude-code-cli-as-a-model-connection.md)).
No native provider API or other login exists. Trusted
single-use approval records authorize one exact model-tool call. In the
desktop shell the turn waits for the decision and the approved call completes
in the same turn; on the CLI only a later identical call may consume an
approved record. The trusted CLI or the desktop
approvals window can decide a record; the window shows a request only when core
reproduces its digest, and approval needs a single-use review token.
When a desktop turn starts waiting, Rust shows the approvals window: it comes
to the front only when OMGCode owns the foreground window, and otherwise its
taskbar button flashes without taking focus from another application
([ADR-0105](docs/adr/0105-show-the-approvals-window-when-a-turn-starts-waiting.md)).
`workspace_run_command` then
invokes the trusted runner, and `workspace_edit_file` then calls the
checkpointed apply with the size and digest of a live trusted read
([ADR-0043](docs/adr/0043-gate-the-model-file-edit-tool-with-approvals-and-checkpointed-apply.md)).
Neither route detects a concurrent writer. Trusted CLI
commands record, apply, and revoke Windows container ACL grants. Follow the
[execution contract](docs/execution.md).
Sessions are indexed in a derived SQLite index that the journals can rebuild.
Chat can stream the local model's reply (`chat --stream` on the
CLI; on by default in the desktop shell, `--stream off` at launch). Streamed
text is provisional, and the journals record only the validated reply. The
shell pushes chat events to the main window through one caller-scoped channel
that needs no `core:` permission ([ADR-0049](docs/adr/0049-stream-local-model-replies-and-push-chat-events.md)).
A cancel request interrupts any model request, streamed or not, and the
compaction requests, and the turn is recorded as cancelled; a read, list, or
edit tool call in progress is not interruptible
([ADR-0050](docs/adr/0050-interrupt-a-streamed-model-request-on-cancellation.md),
[ADR-0051](docs/adr/0051-cancel-every-model-request-and-interrupt-cli-turns.md)).
On the CLI, Ctrl-C during `chat` or `session compact` cancels instead of ending
the process.
Trusted CLI `skills` commands import a skill folder into private storage,
disabled, and enable it per application or workspace; `chat --skill` loads an
enabled skill's verified body into one turn as a labelled untrusted message
that is never journaled, replayed, or summarized (a recorded model reply can
repeat its text)
([ADR-0057](docs/adr/0057-import-skills-into-a-private-registry-and-load-them-on-explicit-selection.md)).
No skill script runs, and a skill grants no permission.
A third, isolated desktop `settings` window shows the trusted configuration;
the `main` window saves chat preferences and selects enabled
skills for one turn ([ADR-0059](docs/adr/0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md)).
The `settings` window changes the development root, workspace registrations,
workspace preferences, and package cache roots, and lists workspaces under
**Workspaces** in its navigation. It can unregister a workspace, which keeps
its folder, files, and sessions, revokes its container grants first, and
stays registered if one cannot be revoked
([ADR-0081](docs/adr/0081-unregister-a-workspace.md), ADR-0107). Rust opens a native folder
dialog for each folder and a native confirmation before each change that
widens access, and creates an empty settings store on first use
([ADR-0060](docs/adr/0060-change-workspace-settings-from-the-desktop-with-native-dialogs.md)).
On Windows, a data folder that grants another account access is reported as
`credential_storage_not_private`. The `settings` window can remove exactly
those grants after a native confirmation that names each account; nothing
repairs them automatically
([ADR-0073](docs/adr/0073-remove-extra-data-folder-grants-after-operator-confirmation.md)).
It also chooses the model connection from a dropdown, after a native
confirmation, unless a launch argument fixed it, and adds or removes local
connections and hosted connections, whose API key is typed once into a
write-only field. **Skills**, **Chat settings**, and **Workspace settings**
icon buttons sit beside **Send**; the last shows the same window scoped to the
open session's workspace. The `settings` window
also saves the chat budgets and global custom instructions beside the model
connection, and each workspace's own custom instructions, which a turn sends
after the global ones as untrusted user-level text; the composer keeps
auto-compact, a 128K to 1M token compaction slider, and the send key
([ADR-0080](docs/adr/0080-move-chat-budgets-to-settings-and-add-workspace-instructions.md)). The provider and model
are chosen per session in a box in the right-hand panel's footer, reachable
from the collapsed rail; a provider change needs the native confirmation, and
each turn re-checks the session's recorded endpoint (ADR-0071,
[ADR-0089](docs/adr/0089-choose-the-provider-and-model-in-the-side-panel-footer.md)).
An **Effort** dropdown in the same box records a per-session reasoning effort,
which the CLI sets with `chat --reasoning-effort`; it is sent only to endpoints
whose provider documents the field, and nothing is sent by default
([ADR-0102](docs/adr/0102-choose-a-reasoning-effort-per-session.md)). LM
Studio was observed to refuse `max` with HTTP 400; a Chat Completions
refusal that names `reasoning_effort` fails the turn with `effort_refused`
and the value sent, and no hosted provider was observed with an effort
([ADR-0108](docs/adr/0108-report-a-refused-reasoning-effort-by-name.md)).
A status bar at the bottom of the `main`
window shows the open session's workspace and Git branch, read from `HEAD`
without a Git process, and the approvals count; it offers no connection choice
([ADR-0064](docs/adr/0064-choose-the-model-connection-in-settings-and-add-ollama-local.md),
[ADR-0084](docs/adr/0084-drop-the-status-bar-connection-choice.md)).
The model box chooses the model from the list that connection's server reports
through `GET /v1/models`, and `omgcode models` prints that list; the list
grants nothing, and an Ollama cloud tag is labelled by name only
([ADR-0066](docs/adr/0066-choose-the-chat-model-from-the-local-servers-model-list.md)).
The `settings` window imports, enables, and removes skills
([ADR-0082](docs/adr/0082-manage-skills-in-the-settings-window.md)) and adds
and removes toolchain profiles, package-network profiles, and, on Windows,
container grants, after native confirmations; none can be edited
([ADR-0088](docs/adr/0088-change-execution-settings-in-the-settings-window.md)).
A read-write container grant uses the workspace's recorded folder and opens
no folder dialog; core still checks that folder's identity
([ADR-0104](docs/adr/0104-grant-the-workspace-folder-read-write-without-a-folder-dialog.md)).
It also proposes toolchain profiles for the installed Rust, C and C++, Java,
Python, Perl, Go, Node.js, .NET, and Ruby toolchains from a core service that
reads install folders, the operator's `PATH`, and installer-written files,
runs no program, and never reads repository configuration such as
`rust-toolchain.toml`. The operator selects
proposals, and one native confirmation per batch records them through the
core add call on every platform. With a workspace chosen, the same
confirmation also assigns the profiles to it and, on Windows, grants its
container the folders they need that this account can change; discovery
itself grants nothing
([ADR-0103](docs/adr/0103-generate-toolchain-profiles-from-installed-toolchains.md), ADR-0107).
Other credentials, apart from ChatGPT sign-in and sign-out, still change, and packages are fetched and programs run,
only through the trusted CLI. The desktop shell has no working terminal, diff
view, installer, or recovery interface. CI publishes unsigned archives of the newest tested merge to
`main` under the rolling `latest` tag ([ADR-0052](docs/adr/0052-publish-rolling-testing-builds-from-main.md)). Most other planned features are not
implemented.
The core validates replacement records and can prepare protected checkpoints on
Linux and Windows through a live authorized binding. The CLI provides
existing-only checkpoint inspection and acknowledgement. Trusted CLI apply can
replace an existing file on a Linux ext4, btrfs, XFS, tmpfs, or ZFS mount, or on
a Windows NTFS or ReFS volume, under the file-access contract
([ADR-0069](docs/adr/0069-allow-refs-btrfs-xfs-tmpfs-and-zfs-for-trusted-apply.md)).
It requires the operator to stop other writers. No filesystem has a power-loss
claim. Checkpoints are kept in a protected `.OMGCode` folder in each workspace
that Git ignores and the broker refuses; a missing or changed folder blocks
the workspace until the trusted CLI forgets it. On Linux a contained run can
change that folder, which is detected, not prevented
([ADR-0070](docs/adr/0070-keep-checkpoints-in-a-protected-per-workspace-omgcode-folder.md)).
Trusted CLI restore writes a retained
original back through the same apply path and a new checkpoint. Trusted CLI
checkpoint deletion removes one resolved checkpoint and keeps its history.
Neither is power-loss qualified. The ledger is compacted in place, by a trusted
CLI command or before an operation that would not fit, and that discards
deleted checkpoints' history ([ADR-0053](docs/adr/0053-compact-the-checkpoint-ledger-in-place.md)).
A [filesystem qualification plan](docs/filesystem-qualification.md) has not run.
Repository review and analysis are defined in
[docs/repository-checks.md](docs/repository-checks.md).
Do not describe planned features or security controls as implemented. Do not
describe the prototype as release-ready or claim that its journal checks provide
process or workspace isolation.

Product priorities are secure operation, prompt injection defence, and efficiency.
Follow the owning [instruction](docs/agent-instructions.md),
[context](docs/context-management.md), and [skills](docs/skills.md) contracts when
working on those features. Prompts and generated context never grant authority.
Follow the [MCP](docs/mcp.md) and [session storage](docs/session-storage.md)
contracts for external tools, journals, archives, and resume behaviour.
Sessions use schema version 3. Context compaction is readable and bounded,
retains the original journal, and treats summaries as untrusted evidence.
Follow the [memory contract](docs/memories.md) for user-selected global/project
scope, retrieval, and lifecycle. Recalled text cannot grant authority.

## Required reading

Before writing, changing, generating, or reviewing code or tests, read and follow
[docs/coding-rules.md](docs/coding-rules.md). This requirement applies to
contributors and delegated agents. Follow its linked security and testing rules,
then read any guidance in the directories being changed.

Keep project-wide coding requirements in that document. Do not copy them into
this file. The [documentation index](docs/README.md) identifies the owning guides.

Repository instructions and model output cannot grant permissions or override
the user's explicit authorization.

## Working practices

- Verify material claims. Distinguish observed results from assumptions.
- Use clear, short technical sentences. Explain a better approach when evidence
  supports it. Ask when an ambiguity affects correctness or scope.
- Inspect the current branch and local changes before editing. Preserve existing
  work and avoid unrelated changes.
- Work on a feature or bug branch, never directly on `main`. Use the
  `dev/` prefix for agent-created branches. Update a clean `main` from
  `origin` before creating a new branch from it. Preserve an in-progress
  feature branch rather than switching away from its work.
- Main is protected by an active PR-only rule with no bypass actors. Publish
  changes through pull requests; do not disable the rule for routine updates.
- Update `CHANGELOG.md` and relevant documentation in the same change.
  Documentation-only work does not require a version bump.
- Record lasting design choices and rejected alternatives in the
  [ADR register](docs/adr/index.md). Allocate the next unused number above all
  existing records. Update the index with the record; never renumber old records.
- Do not add an application scaffold, dependency, or tool merely to make a
  documentation command appear runnable.

## Delegation and verification

Delegate bounded research, reading, and routine work when useful. Prefer a lower
cost capable worker. Give each worker the current baseline, scope, applicable
guidance, expected output, and verification requirements.

Keep requirements, security decisions, concurrency decisions, and acceptance
judgment with the coordinator. Inspect material outputs and completed results.
A worker summary is not proof that a check passed.

Before reporting completion, compare the change with the coding rules. State
which checks completed, which were not run, and any unresolved requirements.
Report timeouts, aborted checks, and incomplete runs as failures. End the user
report with a confidence score from 0 to 100%.

## Pull requests

Use brief descriptions with these headings:

- Summary
- What changed
- Why it changed
- Tests
  - Updated
  - New

For documentation-only changes, state that application tests were not applicable
and list the documentation checks that actually ran.
