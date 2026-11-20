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
[desktop interface](docs/desktop-interface.md) contract.
Chat offers the local LM Studio or Ollama model read and list tools; when the workspace
also enables `read` and `write`, five file-write tools (exact edit, multi-edit,
create, rename, and delete); and, when it also enables `execute`, `read`, and
`write`, one command tool. Each workspace preference except `model_tools` is
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
after a durable checkpoint of its bytes, which the trusted CLI can restore. All
five need the operator's writers-stopped acknowledgement when approved. No model network,
Git, or MCP tool exists. Chat offers tools only for a
workspace whose `model_tools` preference is enabled. A model write or command
runs without a per-call approval only where the operator set it to `allow`.
There are no other provider adapters. Every model tool call, executed or
refused, is recorded in the model action journal, which the trusted
`actions list` command shows. The
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
`/v1` API. Only the local adapter reads a credential, for an operator-selected
`lm_studio` profile that supplies both the endpoint and the token
(ADR-0047, ADR-0048); an `ollama_local` profile has none. No cloud adapter or
login exists. Trusted
single-use approval records authorize one exact model-tool call. In the
desktop shell the turn waits for the decision and the approved call completes
in the same turn; on the CLI only a later identical call may consume an
approved record. The trusted CLI or the desktop
approvals window can decide a record; the window shows a request only when core
reproduces its digest, and approval needs a single-use review token.
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
workspace preferences, and package cache roots. Rust opens a native folder
dialog for each folder and a native confirmation before each change that
widens access, and creates an empty settings store on first use
([ADR-0060](docs/adr/0060-change-workspace-settings-from-the-desktop-with-native-dialogs.md)).
It also chooses the model connection from a dropdown, after a native
confirmation, unless a launch argument fixed it, and adds or removes local
connections without a credential. A status bar at the bottom of the `main`
window shows the open session's workspace and Git branch, read from `HEAD`
without a Git process, and offers the same connection choice through the same
native confirmation
([ADR-0064](docs/adr/0064-choose-the-model-connection-in-settings-and-add-ollama-local.md)).
The composer chooses the model from the list that connection's server reports
through `GET /v1/models`, and `omgcode models` prints that list; the list
grants nothing, and an Ollama cloud tag is labelled by name only
([ADR-0066](docs/adr/0066-choose-the-chat-model-from-the-local-servers-model-list.md)).
Credentials, execution setup, and skills still change only through the
trusted CLI. The desktop shell has no development
panels, installer, or recovery interface. CI publishes unsigned archives of the newest tested merge to
`main` under the rolling `latest` tag ([ADR-0052](docs/adr/0052-publish-rolling-testing-builds-from-main.md)). Most other planned features are not
implemented.
The core validates replacement records and can prepare protected checkpoints on
Linux and Windows through a live authorized binding. The CLI provides
existing-only checkpoint inspection and acknowledgement. Trusted CLI apply can
replace an existing file on a Linux ext4, btrfs, XFS, tmpfs, or ZFS mount, or on
a Windows NTFS or ReFS volume, under the file-access contract
([ADR-0068](docs/adr/0068-allow-refs-btrfs-xfs-tmpfs-and-zfs-for-trusted-apply.md)).
It requires the operator to stop other writers. No filesystem has a power-loss
claim. Trusted CLI restore writes a retained
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
  `codex/` prefix for agent-created branches. Update a clean `main` from
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
