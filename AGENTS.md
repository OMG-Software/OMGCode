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
Chat offers the local LM Studio model read and list tools; when the workspace
also enables `read` and `write`, three file-write tools (exact edit, multi-edit,
and create); and, when it also enables `execute`, `read`, and `write`, one
command tool. The file-write and command tools each require a trusted
single-use approval. The edit tool replaces one exact text occurrence in an
existing file, and the multi-edit tool several exact occurrences in one
existing file, each through checkpointed apply. The create tool makes one new
file through the exclusive create, without a checkpoint. All three need the
operator's writers-stopped acknowledgement. No model delete, rename, network,
Git, or MCP tool exists. Chat offers tools only for a
workspace whose `model_tools` preference is enabled. There are no unapproved
model writes, other provider adapters, or unreviewed model-callable process
execution. The
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
`api_key` profiles. An `lm_studio` profile can record a loopback endpoint.
Only the local LM Studio adapter reads a credential, through an
operator-selected profile that supplies both the endpoint and the token
(ADR-0047, ADR-0048); no cloud adapter or login exists. Trusted
single-use approval records authorize one exact model-tool call: only a later
identical call may consume an approved record. The trusted CLI or the desktop
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
The desktop shell has no settings interface, development panels, installer, or
recovery interface. CI publishes unsigned archives of each merge to `main` under
the rolling `latest` tag ([ADR-0052](docs/adr/0052-publish-rolling-testing-builds-from-main.md)). Most other planned features are not
implemented.
The core validates replacement records and can prepare protected checkpoints on
Linux and Windows through a live authorized binding. The CLI provides
existing-only checkpoint inspection and acknowledgement. Trusted CLI apply can
replace an existing file on a qualified Linux ext4 mount or on Windows NTFS
under the file-access contract. It requires the operator to stop other writers.
Windows makes no power-loss claim. Trusted CLI restore writes a retained
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
