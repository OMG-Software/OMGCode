# OMGCode

**A Rust coding-assistant prototype for Windows and Linux.**

OMGCode aims to connect language models to controlled development tools.
The current code is an early Rust core and command-line prototype. A bounded
native workspace file broker is available through trusted CLI commands. A core
turn/tool runtime and schema-v3 session format let chat offer the selected
model read, list, edit, multi-edit, create, rename, delete, directory-creation,
command, and `web_fetch` tools, only for a workspace that enables `model_tools`
and each tool's own preferences. Writes and commands need an approval unless the
operator sets them to `allow`. Git workflows remain planned. A prototype desktop shell (Tauri, with a strict TypeScript
frontend) browses and searches sessions, runs chat, changes trusted settings in
a separate window, and decides model-tool approvals in another; it has no
installer and has run only on Windows.
The trusted CLI `run` command starts one program for a workspace whose
`execute` preference is enabled. It uses an operator-declared toolchain
profile, records each run in a run journal, and confines the process tree with
Landlock and a seccomp socket filter on Linux or a per-workspace AppContainer
on Windows. Trusted CLI commands record and revoke the Windows container's
directory grants.

> **Status: early prototype, not release-ready.** The Rust CLI can inspect the
> compiled prompt, report implemented capabilities, manage JSONL sessions and
> workspace permission preferences, perform bounded workspace reads/listings,
> create new files/directories through the trusted CLI, preview bounded diffs
> against trusted proposed content, and send a chat request to
> an explicitly selected model connection: a local LM Studio or Ollama server, a
> hosted provider with a stored API key, a ChatGPT sign-in, or, from the CLI or
> a desktop session's own choice, the operator's own signed-in Claude Code CLI
> (ADR-0109, ADR-0111). `omgcode chat --stream` streams
> that model's reply to standard error as provisional text.
> When a workspace enables the `model_tools` preference, chat lets the
> model read and list files inside that workspace through a bounded runtime.
> When `execute`, `read`, and `write` are also enabled and a toolchain profile
> is recorded for the platform, it offers a confined command tool. When `read`
> and `write` are enabled, it offers an
> edit tool that replaces one exact text occurrence in an existing file, a
> multi-edit tool that replaces up to 16 exact occurrences in one existing
> file, a create tool that makes one new file, a rename tool that moves one file
> to a name that does not exist, a delete tool that removes one file after
> a checkpoint of its bytes, and a directory tool that creates one directory
> whose parent exists. A write or command whose preference is `prompt` records
> a pending approval and acts only after the operator approves that exact call
> (a write also needs the acknowledgement that other writers are
> stopped); one whose preference is `allow` runs without one. Edits and
> deletions are applied through a checkpoint, and creation, directory creation,
> and renaming are not checkpointed. A `web_fetch` tool, off by default, reads
> one public HTTPS page per call behind its own preference and a per-workspace
> host allowlist. It has no other model network tool and no Git or MCP tools.
> The trusted
> CLI can replace or restore one existing file through a checkpoint on Linux
> ext4, btrfs, XFS, tmpfs, or ZFS, or on Windows NTFS or ReFS, and can permanently delete one resolved checkpoint.
> The `file` commands cannot delete workspace files, though `file restore
> --removed` recreates one that a model deletion removed. The trusted CLI `run`
> command executes programs under native confinement, and those programs can
> create, change, and delete workspace files without a checkpoint. That
> confinement was tested on one Windows build and one Linux kernel only and is
> not qualified for release. A run has no IP networking unless both the
> `task_network` preference and its `--network` flag allow it; a networked
> Linux run can then reach any address, including loopback and the local
> network. On Linux, a run that both the `host_services` preference and its
> `--host-services` flag allow can reach the host's Unix-domain sockets, such
> as service and credential-agent sockets. Package-network profiles can be
> recorded as configuration only (ADR-0034); no run uses them and they do not
> narrow network access. The trusted `packages fetch` command downloads from
> declared origins into a private per-workspace cache. A selected cache root
> is exposed read-only on Linux and Windows; Windows needs a read-execute
> container grant, and Linux falls back to application storage when no root is
> selected. Provider profiles record connection shapes; hosted
> connections are prototypes that were not tested with a real key. Trusted
> single-use approval records authorize one exact
> model-tool call; the trusted CLI or the desktop approvals window can decide
> them. API keys can be stored in protected OS storage (DPAPI on Windows, the
> Secret Service on Linux) and bound to a profile. The chat client reads one
> through an operator-selected profile (`--provider-profile`) that also
> supplies the endpoint; an `ollama_local` profile records an endpoint without
> a credential. The desktop `settings` window chooses the connection from a
> dropdown. Runs have memory,
> process-count, CPU-time, and sampled disk-growth limits; the disk limit is
> not a filesystem quota. See [command execution](docs/execution.md#tree-limits).

## Goals

1. **Secure operation:** Enforce the user's permissions and contain development
   tools on Windows and Linux.
2. **Prompt injection defence:** Keep untrusted content from granting authority.
   Use runtime controls, source tracking, and adversarial tests alongside the prompt.
3. **Efficiency:** Reduce unnecessary model calls, repeated context processing,
   and tool work while preserving correctness and security.

Security takes priority when an optimization would weaken these boundaries.

## Current prototype

The workspace contains five Rust crates: `omgcode-core`, `omgcode-cli`,
`omgcode-platform`, `omgcode-app`, and `omgcode-desktop`. The CLI command is
`omgcode`. The desktop shell is the `omgcode-desktop` binary, with its
frontend in `crates/omgcode-desktop/ui`. The CLI provides:

- `prompt`: print the compiled, read-only base prompt, version, and SHA-256 digest.
- `capabilities`: report what this prototype implements and rejects.
- `workspace`: configure the development root, register stable workspace IDs,
  change read/write/execute/task-network/host-services preferences, declare
  toolchain profiles, manage Windows container grants, and inspect audited
  policy checks. These checks describe configuration only.
- `run` and `runs`: run one program in a workspace under native confinement
  with a recorded toolchain profile, and inspect, acknowledge, or archive the
  run journal. Output is printed, never stored. See
  [command execution](docs/execution.md#trusted-runs).
- `packages fetch`: download one URL from a declared package-network origin
  into a private per-workspace cache. It is a trusted operator command, never
  a model tool, and the confined run still has no network access. See
  [trusted package fetch](docs/execution.md#trusted-package-fetch).
- `file`: use bounded native reads, listings, exclusive new-file creation,
  single-directory creation, and read-only diff previews through explicit
  workspace/session CLI selections. These trusted operator commands do not
  provide model tool definitions, and `file diff` never writes to the workspace.
- `session`: create, inspect, archive, restore, compact older active context,
  and explicitly interrupt an abandoned turn in a JSONL journal.
- `chat`: send one user turn to an already-running LM Studio or Ollama server
  at a literal loopback HTTP endpoint, or through a selected hosted or ChatGPT
  provider profile. It does not start or download models, and it
  rejects model tool requests unless the session's workspace enables
  `model_tools`. Then it offers read/list tools; when `read` and `write` are
  also enabled, the edit, multi-edit, create, rename, delete, and
  directory-creation tools; and when `execute`, `read`, and
  `write` are all enabled, the command tool. Writes and commands need an
  approval unless set to `allow`. Each is rechecked against live settings
  before every call. `--reasoning-effort` sets a reasoning effort for the session.
- `models`: list the models the selected server reports.
- `approvals` and `actions`: decide trusted per-call approvals, and list the
  model action journal.
- `skills`: import, enable, inspect, and remove skills; `chat --skill` loads
  one into a turn.

Prompt assembly sends the compiled base in the system role. Optional user
preferences and conversation history remain user or assistant messages. The
CLI can record a chat turn and its request manifest in a JSONL journal. It
refuses to chat with an archived session or one with an incomplete prior turn;
it does not replay recorded actions. An abandoned turn must be inspected and
marked interrupted before chat can continue.

The core also implements a session-owned turn/tool runtime and JSONL schema
version 3. Schema version 3 adds readable, bounded context compaction; the
original journal messages are retained, and the summary is labelled untrusted
user-level evidence. That runtime accepts a caller-supplied model interface, validates
strict tool calls, creates a fresh workspace binding
before every file operation, and records tool intents, results, and
source-labelled messages. A write or command tool whose preference is `prompt`
additionally requires a consumed approval; a write also needs the operator's
writers-stopped acknowledgement, and an edit writes only through checkpointed
apply. `chat` runs it with the selected provider adapter for a workspace that
enables `model_tools`.
It has no Git or MCP tool; its only network tool is the gated `web_fetch`.
New sessions use schema version 3; version 1 and 2 journals remain readable
and writable with their original subsets.

The desktop shell is a prototype. It has a session tree grouped by workspace
folder, full-text search over a derived SQLite index, a paged transcript view,
chat that runs the same turn as `omgcode chat`, and an approvals window that
shows an exact reviewed request before it lets the operator decide. The webview
gets no authority: the Rust side authorizes every command, and no command
accepts a host path. The webview is still not a trusted computing base. It has
no installer, working terminal, diff view, or recovery interface, and it has not
been run on Linux. A separate `settings` window shows and changes the trusted
configuration through native dialogs and confirmations; packages are fetched and
trusted runs start only through the CLI. Chat streams the local model's
reply into the window by default (`--stream off` at launch disables it). The
text is provisional, and the journal records only the validated reply. The
shell pushes chat events to the main window through one caller-scoped channel
that needs no Tauri `core:` permission (ADR-0049). See the
[desktop interface guide](docs/desktop-interface.md).

The platform crate checks opened journal handles, captures native directory
identity for workspace registration, and implements handle-relative file
operations. The broker applies the current registered workspace checks, but it
does not isolate hostile same-user processes or contain a process. A read-only
`file diff` preview is available and creates no checkpoint.
The core also validates replacement journal bytes and can prepare protected
Linux and Windows checkpoints through its authorized library API. The CLI can
inspect and acknowledge existing checkpoint state. Trusted `file apply` can
publish one checkpointed replacement on a Linux ext4, btrfs, XFS, tmpfs, or ZFS
mount, or on a Windows NTFS or ReFS volume ([ADR-0069](docs/adr/0069-allow-refs-btrfs-xfs-tmpfs-and-zfs-for-trusted-apply.md)).
Checkpoints live in a protected `.OMGCode` folder in each workspace that Git
ignores and the broker refuses to open; a missing or changed folder blocks
the workspace until trusted `file forget-checkpoint-store` abandons it
([ADR-0070](docs/adr/0070-keep-checkpoints-in-a-protected-per-workspace-omgcode-folder.md)). Stop independent
writers first: a content check and rename cannot guarantee no lost updates. No
filesystem has a power-loss claim. Trusted
`file restore` writes a retained original back through a new checkpoint after
an explicit current-content check. Trusted `file delete-checkpoint --permanent`
removes one resolved checkpoint's files and keeps its ledger history until
trusted `file compact-checkpoints --discard-history`, or an apply, restore, or
deletion that would not fit, compacts the ledger. Neither
command is power-loss qualified. The model edit and multi-edit tools use the same
checkpointed apply, under `prompt` after an approved, writers-stopped decision
and under `allow` without one; the create tool
uses the exclusive create without a checkpoint; the delete tool uses a
checkpointed removal; and the rename tool uses a no-replace rename without a
checkpoint. None detects a concurrent writer, and none is power-loss
qualified. A deleted file is restored with trusted `file restore --removed`,
which does not restore its mode or access list.
Git, MCP, and memories remain planned. See the
[workspace settings contract](docs/workspace-permissions.md), the
[workspace file-access guide](docs/file-access.md), and the [development
guide](docs/development.md) for limits and command details.

The CLI stores provider API keys in operating-system protected storage through
trusted `workspace provider-credential-set` and `provider-credential-remove`
commands (ADR-0042). The chat client reads one for a selected `lm_studio` or
hosted profile (ADR-0092), and a ChatGPT sign-in keeps its own tokens
(ADR-0093). With
`--provider-profile <name>`, `chat` and `session compact` (and the desktop
shell, at launch) take the server token from the stored credential of that
profile and do not read
`OMGCODE_LM_TOKEN` (ADR-0047). A
credential that cannot be read stops the turn. An `lm_studio` profile can
record one loopback endpoint with `provider-profile-add --endpoint`, and a
selected profile supplies both its endpoint and its token, so the stored token
goes only to that endpoint (ADR-0048). `--endpoint` and `--provider-profile`
cannot be combined. A profile recorded before 0.31.0 has no endpoint and must be
removed and added again before it can be selected. Without the flag, `--endpoint`
and its default apply, and the optional local server token comes from
`OMGCODE_LM_TOKEN`, which remains prototype wiring; it is not
a release credential boundary. Do not treat the prototype as release-ready or use it to handle
sensitive credentials.

To build and run the prototype with the pinned Rust toolchain:

```sh
cargo build --workspace --locked
cargo run --locked -p omgcode-cli -- capabilities
cargo run --locked -p omgcode-cli -- prompt
```

To send a test request, start LM Studio and load a model yourself. Replace
YOUR_MODEL_ID with that model's ID and CONTEXT_TOKENS with its validated numeric
context capacity before running this command:

```sh
cargo run --locked -p omgcode-cli -- chat --workspace . --model "YOUR_MODEL_ID" --context-tokens CONTEXT_TOKENS "Say hello in one sentence."
```

The endpoint defaults to `http://127.0.0.1:1234/v1`. The required context value
must come from the selected model's validated profile. The adapter uses a
conservative byte-based estimate; it is not the model tokenizer. Add
`--auto-compact --compact-threshold TOKENS` to compact older active context
before a turn, or run `session compact` explicitly. The original journal is
retained; the summary is untrusted evidence, never instructions or authority. While the
workspace's `model_tools` preference is off, which is the default, the example
sends only the supplied message and saved chat history, not workspace files;
earlier tool results are replaced by a withheld notice. If `model_tools` is
enabled for `.`, the model can call the read and list tools. With `read` and
`write` also enabled, which is the default, it can request an exact file edit;
with `execute` enabled as well it can request the command tool. Each write or
command needs an operator approval unless its preference is `allow`. The file
contents it reads are sent to the local model.
For Ollama, record a profile once and name it on each turn. Set the server's
context length in Ollama (for example `OLLAMA_CONTEXT_LENGTH`), because this
API cannot change it:

```sh
cargo run --locked -p omgcode-cli -- workspace provider-profile-add ollama --kind ollama_local --auth none --endpoint http://127.0.0.1:11434/v1 --revision REVISION
cargo run --locked -p omgcode-cli -- chat --workspace . --provider-profile ollama --model "YOUR_MODEL_ID" --context-tokens CONTEXT_TOKENS "Say hello in one sentence."
```

The loopback connection does not establish how LM Studio or Ollama handles
data upstream; check the server's configuration before sending sensitive text.

## Planned features

- **Release-qualified model providers.** One bounded Chat Completions path
  serves LM Studio, Ollama Local, Ollama Cloud, twelve other hosted providers,
  and an OpenAI-compatible service in the prototype (ADR-0092). None of the
  hosted connections was tested with a real key, and the path's compatibility
  and security are not release-qualified.
- **Subscription authentication:** Required ChatGPT and Claude subscription
  connections, alongside separate API-key connections. A ChatGPT sign-in
  prototype exists (ADR-0093) but plan usage has not been verified with a real
  account; Claude subscription sign-in still needs provider approval. A
  `claude_code` connection drives the operator's own signed-in `claude` CLI
  from the trusted CLI or a desktop session in its own sandbox, without
  handling the Claude credential (ADR-0109, ADR-0111); it is personal use, not
  evidence of approval.
- **Development tools:** Compilers, linkers, SDKs, formatters, linters, script
  interpreters, build systems, package managers, and test tools.
- **Permission settings:** Toggle switches for file access, command execution,
  background tasks, network access, and Git operations, with scoped approvals
  where required.
- **Development root:** A user-selected root that sets the maximum boundary for
  host development data. Sessions will normally have read and write access to
  their active workspace within that root. Other host folders will be denied
  by default.
- **Sandboxed tools:** Extend the current bounded trusted-CLI file broker and
  approval-gated model file and command tools with terminals and background
  tasks governed by the same permission policy and enforced with
  operating-system controls.
- **Git workflows:** Status, diffs, staging, commits, branches, worktrees, fetch,
  pull, and push, with controlled remote access and credential handling.
- **Development panels:** Terminal tabs, background task status and logs, and
  file and hunk diffs.
- **Session interface:** The prototype desktop shell has a searchable session
  tree grouped by working folder, with sorting, archived and active views, and
  status; it supports archive, restore, and interrupt. Recovery workflows,
  storage-use display, and packaging remain planned. The CLI stores the
  versioned JSONL journals and supports inspect, archive, and restore. See the
  [desktop interface guide](docs/desktop-interface.md).
- **MCP support:** Connect local stdio and remote Streamable HTTP servers for
  tools, resources, and prompt templates through core-owned permissions. See
  [MCP support](docs/mcp.md).
- **Prompt settings UI:** Inspect the versioned base prompt compiled into the
  Rust application and shown read-only. Add separate user preferences at
  application, workspace, and session scope. The CLI can print the compiled
  prompt; the desktop settings window does not show or change the
  prompt. See the
  [initial prompt draft](prompts/system.md) and [instruction design](docs/agent-instructions.md).
- **Automatic compaction:** Configure a trigger in tokens; 200k means 200,000
  tokens. Show any earlier trigger required by the selected model's safe budget.
- **Caching:** Reuse validated artifacts and supported provider prompt caches,
  with scoped storage, invalidation, usage reporting, and clear controls.
- **Skills settings:** Add, remove, enable, and disable versioned skill packages.
  Skills use the same permission policy and controlled tools as other work.
  Trusted CLI commands and the desktop `settings` window import, enable,
  disable, and remove skills today ([ADR-0082](docs/adr/0082-manage-skills-in-the-settings-window.md)).
- **Project and global memories:** Save reusable facts or preferences with an
  explicit This project or Global choice. Review, edit, delete, or disable
  entries and control their use in each session. See [memories](docs/memories.md).
- **Change review and recovery:** Session change tracking, checkpoints, and
  worktrees to help review and recover changes.

## Model providers

| Connection profile | Current state |
| --- | --- |
| OpenAI API | Prototype: Chat Completions at `https://api.openai.com/v1` with a stored API key (ADR-0092); not tested with a real key |
| ChatGPT subscription | Prototype: Sign in with ChatGPT through OpenAI's documented browser flow and plan usage through the Responses API (ADR-0093); tested against mock servers, and one real sign-in, model list, and text turn were observed (ADR-0096), and two read and list tool turns finished after tool results were sent as call and output items (ADR-0097) |
| Anthropic API | Prototype: Anthropic's OpenAI SDK compatibility layer with a stored API key (ADR-0092), documented by Anthropic for testing; the native Messages API is planned; not tested with a real key |
| Claude subscription | Required; provider approval and route remain unresolved. A `claude_code` connection drives the operator's own signed-in `claude` CLI from the trusted CLI or a desktop session as personal use (ADR-0109, ADR-0111); it was tested with a fake `claude` only |
| Ollama Local | Prototype: Ollama's OpenAI-compatible `/v1` API over literal loopback HTTP (ADR-0064) |
| Ollama Cloud | Prototype: the fixed endpoint `https://ollama.com/v1` with a stored API key (ADR-0071); chat not tested with a real key |
| OpenRouter | Prototype: `https://openrouter.ai/api/v1` with a stored API key (ADR-0092); upstream routing controls are planned; not tested with a real key |
| Google, Meta, Moonshot, Tencent, Thinking Machines Lab, MiniMax, Z.ai, DeepSeek, xAI | Prototype: each provider's one documented OpenAI-compatible endpoint with a stored API key (ADR-0092); not tested with a real key |
| OpenAI compatible | Prototype: an operator-typed HTTPS base URL with a stored API key (ADR-0092) |
| LM Studio | Prototype: bounded Chat Completions over literal loopback HTTP; a selected profile supplies the endpoint and the optional server token from its stored credential, otherwise `--endpoint` and `OMGCODE_LM_TOKEN` apply; model discovery and release authentication remain unqualified, and streamed replies have met local fixtures only |

API-key and subscription connections are distinct authentication and billing
profiles. Subscription-backed usage for both ChatGPT and Claude is required for
the first release. OpenAI documents a local/open-source integration; Anthropic
currently requires approval for third-party subscription integration. See the
[authentication requirements](docs/provider-authentication.md) for the evidence,
open dependency, and acceptance checks.

Provider support will depend on the capabilities of the selected model and
endpoint. Tool calling, streaming, context limits, and usage reporting will remain
explicit rather than being assumed to work identically across providers.

## Proposed architecture

- **Rust core:** Session management, agent execution, provider adapters,
  authentication, permissions, file access, toolchain profiles, Git operations,
  task supervision, prompt assembly, context management, caching, skills, MCP,
  and scoped memories.
- **Desktop interface:** Tauri with a bundled TypeScript web interface. A
  prototype exists (`omgcode-desktop`); see the
  [desktop interface guide](docs/desktop-interface.md).
- **Execution backends:** Separate Windows and Linux sandbox implementations.
- **Local storage:** Versioned JSONL session journals, archive/restore events,
  a rebuildable SQLite folder and search index (implemented), settings, scoped memories, and referenced
  task artifacts.
- **Credential handling:** Operating-system credential storage, accessed through
  narrow services in the Rust core.

The desktop prototype uses a limited command interface to the Rust core: 66
commands, no host paths, and per-window permissions checked in Rust. Provider
keys will stay out of frontend state, repository files, and task environments.

## Development tools

OMGCode must work with projects in multiple languages. It will support
user-selected installed tools and managed toolchain environments through a
generic sandboxed runner. Tool profiles will include the required executable,
runtime, library, and SDK resources, plus scoped writable build outputs, caches,
and temporary space. Profiles are declared by the operator, who can also select
proposals that the desktop `settings` window generates from installed
toolchains ([ADR-0103](docs/adr/0103-generate-toolchain-profiles-from-installed-toolchains.md)). In
acceptance tests, Rust, Python, Node, C/C++, and Go workflows ran confined on
Windows and Linux; Java has not run yet.

Build scripts and all child programs remain subject to the effective sandbox.
Toolchain access does not grant access to unrelated host data. See
[development tools and toolchains](docs/development-tools.md).

## Permission and sandbox design

OMGCode will own its permission policy and sandbox lifecycle. The design will
use operating-system isolation to enforce restrictions on files, networking,
processes, and host services.

Linux confinement uses Landlock and seccomp
([ADR-0028](docs/adr/0028-confine-linux-runs-with-landlock-and-a-socket-filter.md));
every trusted run is limited in memory, processes, and CPU time, through a
job object on Windows and a delegated cgroup v2 on Linux
([ADR-0031](docs/adr/0031-limit-whole-run-trees-with-job-objects-and-delegated-cgroups.md)).
Windows confinement uses a per-workspace AppContainer with recorded ACL grants
inside a Job Object
([ADR-0029](docs/adr/0029-confine-windows-runs-in-a-workspace-appcontainer-with-recorded-grants.md)).
The trusted CLI `run` is their only caller
([ADR-0030](docs/adr/0030-run-trusted-commands-through-recorded-toolchain-profiles.md)).
Both backends must pass containment and development-tool compatibility tests
before release.

Development tools also need runtime files and temporary storage. These will be
declared sandbox resources, with private scratch space and controlled toolchain
access. The sandbox will not automatically expose the user's home directory,
credentials, or unrelated projects. If required isolation is unavailable,
affected operations will be refused.

Model connections, Git connections, MCP connections, and general task networking
will have separate controls. Sending source context to a cloud model is a data
transfer even when terminal networking is disabled. A local Ollama endpoint can also route
requests to cloud models, so local and cloud execution must remain distinct.

Prompt injection can still cause harmful actions within permissions the user
grants. Write access permits damage inside that writable workspace. The design
aims to contain access and provide review and recovery; it does not promise to
prevent every malicious instruction.

Prompts, custom instructions, memories, skills, summaries, and cached artifacts
cannot grant permissions. See [context management](docs/context-management.md) and
[skills](docs/skills.md) for their trust boundaries and acceptance requirements.

## Roadmap

1. Define the threat model, workspace boundary, and permission contract.
2. Resolve subscription integration eligibility and validate both login/usage paths.
3. Prove Windows/Linux isolation, representative toolchains, and process cleanup.
4. Extend the Rust core and build the desktop interface, session tree, and
   remaining persistence and recovery workflows. The first desktop prototype
   (session tree, search, chat, approvals, and a settings window) is implemented;
   recovery, development panels, and packaging remain. Streaming of the local
   model's reply is implemented for that adapter only (ADR-0049).
5. Expand provider adapters and add core-authorized tool workflows.
6. Add visible instructions, token accounting, compaction, safe caching, skills,
   and project/global memories with boundary tests and efficiency measurements.
7. Add model-scoped file tools, sandboxed terminals, background tasks, and MCP clients.
8. Add mediated Git workflows, diff review, checkpoints, and crash recovery.
9. Verify the full workflow on both platforms and prepare installers and
   documentation.

## Documentation

Start with the [documentation index](docs/README.md). Contributors and coding
agents must follow the [coding rules](docs/coding-rules.md) before code or test
work. The [bundled skill source catalog](skills/README.md) contains source
packages. A skill folder can be imported, enabled per application or
workspace, and loaded into one chat turn with `chat --skill` or the desktop
`main` window; the `settings` window imports, enables, and removes skills.
There is no script execution or automatic selection. See [skills](docs/skills.md).

## Contributing

The project is in its early prototype stage. Use
[GitHub Issues](https://github.com/OMG-Software/OMGCode/issues) to discuss
requirements, design proposals, and gaps in the plan.

See the [development guide](docs/development.md) for the current prototype
commands. Also read the [contributor guidance](AGENTS.md) before making changes.

## Repository checks

Main updates require pull requests. CodeRabbit reviews pull requests, CodeQL
analyzes workflow files and available application languages, and Sonar uses
CI-based analysis. See [repository checks](docs/repository-checks.md) for the
configured scope, credential setup, and current limitations.

## License

OMGCode is licensed under the [Apache License 2.0](LICENSE).
