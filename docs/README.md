# Project documentation

OMGCode has a Rust CLI prototype for local chat, JSONL sessions, and a
bounded native workspace file broker, and a prototype Tauri desktop shell with a
session tree, search, chat, and an isolated approvals window. The shell is
without an installer and has run only on Windows. The broker is available only through
trusted operator commands and, when a workspace enables `model_tools`, to the
local model's read, list, approval-gated edit, multi-edit, create, rename,
delete, and command tools. The edit and multi-edit tools change one existing
file only through checkpointed apply after an approved, writers-stopped
decision; the create tool makes one new file through the exclusive create,
without a checkpoint; the rename tool moves one file to a name that does not
exist, without a checkpoint; and the delete tool removes one file after a
checkpoint of its bytes. Model network, Git, and MCP tools are not
implemented. The
trusted CLI `run` command runs one program under Linux Landlock or Windows
AppContainer confinement with an operator-declared toolchain profile. A model
tool starts a process only through the approval-gated command tool and the
trusted runner. The guides distinguish implemented behaviour from the
remaining product contracts.

## Start here

1. Read the root [contributor guidance](../AGENTS.md).
2. Read the [coding rules](coding-rules.md) before code or test work.
3. Follow the [security rules](security-coding-rules.md) and
   [testing standards](testing-standards.md).
4. Check the [platform targets](platform-support.md) for platform-specific work.

## Document owners

| Document | Purpose |
| --- | --- |
| [Project overview](../README.md) | Product scope, proposed architecture, and roadmap |
| [Implementation plan](../IMPLEMENTATION_PLAN.md) | Resumable baseline, phase order, acceptance criteria, and resume commands |
| [Build and run](development.md) | Current CLI, build commands, crate boundaries, limits, and dependencies |
| [Desktop interface](desktop-interface.md) | The Tauri shell: windows, commands, session tree, search, chat, approvals, security limits, and development |
| [Coding rules](coding-rules.md) | Rust design, ownership, errors, concurrency, and workflow |
| [Security coding rules](security-coding-rules.md) | Permission, sandbox, credential, Git, and UI boundaries, including the webview command rules |
| [Testing standards](testing-standards.md) | Test design, execution, and evidence requirements |
| [Platform support](platform-support.md) | Windows/Linux targets, prerequisites, and the evidence for each, including the desktop shell |
| [Provider authentication](provider-authentication.md) | Required subscription/API profiles, provider evidence, and readiness gates |
| [Workspace settings](workspace-permissions.md) | Implemented workspace IDs, configuration-only permissions, revisions, audit, and remaining gates |
| [Workspace file access](file-access.md) | Native broker, previews, trusted apply on supported filesystems, checkpoint storage, and existing-only recovery |
| [Filesystem qualification plan](filesystem-qualification.md) | Release filesystem matrix and power-loss campaign for checkpoints and apply; plan only, not run |
| [Development tools](development-tools.md) | Installed/managed toolchains, runtime grants, outputs, caches, and qualification |
| [Command execution](execution.md) | Process-tree runner contract, confinement, trusted runs, toolchain profiles, the run journal, limits, and next increments |
| [Agent instructions](agent-instructions.md) | Product priorities, visible SYSTEM prompt, user additions, and request manifests |
| [System prompt draft](../prompts/system.md) | Versioned initial base instructions for the coding assistant |
| [Context management](context-management.md) | Token-based compaction, cache classes, invalidation, and efficiency measures |
| [Skills](skills.md) | Settings, package validation, enablement, loading, and controlled scripts |
| [Bundled skill sources](../skills/README.md) | Source packages and provenance; an operator imports one with `skills add` |
| [MCP support](mcp.md) | Server connections, transports, credentials, capabilities, and tool authorization |
| [Session storage](session-storage.md) | JSONL layout/schema, tool events, durable journals, lock-free inspection, the derived session index, archive/restore, and safe resume |
| [Memories](memories.md) | User-selected project/global scope, management, retrieval, and lifecycle |
| [Repository checks](repository-checks.md) | CodeRabbit, CodeQL, Sonar, the Rust, Frontend, and Testing build workflows, credentials, and main protection |
| [ADR register](adr/index.md) | Lasting decisions, alternatives, and consequences |
| [Changelog](../CHANGELOG.md) | Changes awaiting release |

## Updates required with a change

| Change | Update |
| --- | --- |
| Any repository change | Changelog and the relevant owning guide |
| A coding convention | Coding rules; an ADR when the choice needs lasting rationale |
| A security property, permission, or trust boundary | Security coding rules and relevant tests |
| A test convention or verification command | Testing standards |
| Repository analysis, CI workflows, or branch rules | Repository checks and relevant testing/security guidance |
| Workspace identity, settings, revisions, or policy audit | Workspace settings, session storage, security rules, and relevant tests |
| Workspace file broker, native file boundary, or uncertain creation | Workspace file access, platform support, security rules, session storage, and relevant tests |
| A supported platform or prerequisite | Platform support |
| Provider login, subscription entitlement, or billing | Provider authentication and security rules |
| Tool discovery, runtime access, build environments, or build/package caches | Development tools and platform support |
| Prompt text, user additions, or instruction assembly | Agent instructions, prompt draft, and security rules |
| Model context, compaction, or LLM artifact caching | Context management, security rules, and testing standards |
| Skills, imports, enablement, or skill resources | Skills, agent instructions, and relevant security/testing guidance |
| MCP connections, tools, resources, prompts, or client capabilities | MCP, security rules, and testing standards |
| Session format, persistence, archives, restore, or resume | Session storage, security rules, and testing standards |
| The desktop shell, its commands, windows, frontend, or approval review | Desktop interface, security rules, testing standards, and platform support |
| Memory saving, scope, retrieval, deletion, or reuse | Memories, agent instructions, context management, and security/testing guidance |
| Product scope or the proposed architecture | Root README |
| Project implementation status or module guidance | Root AGENTS.md and relevant directory guidance |
| A lasting decision with a plausible alternative | New ADR and register entry |
| A new document | This index, or its owning sub-index |

Keep each rule in one owning guide and link to it elsewhere. Use the ADRs for
rationale rather than duplicating full policy text. Do not create a new document
for each implementation task or temporary investigation.

Use Markdown headings, fenced code blocks with language tags, and relative
links for repository files. Keep whitespace clean and end files with a newline.

## Current enforcement status

Repository review and analysis are configured in the repository-checks guide.
The Rust workflow adds formatting, strict Clippy, unit/integration tests, and
separate documentation tests on Windows and Linux. The development guide owns
the current commands and limits. The desktop frontend (`crates/omgcode-desktop/ui`)
has its own Frontend workflow with strict type checks, Biome, and Vitest; that
workflow and the Rust workflow's frontend build step have not run on GitHub
yet. The trusted `run`
command uses native confinement that was tested on one Windows build and one
Linux kernel only. Every trusted run has memory, process-count, CPU-time, and
sampled net disk-growth limits; the disk limit is not a filesystem quota.
Package-network profiles are recorded as configuration only (ADR-0034); no run
uses them, and network access remains all-or-nothing. The trusted
`packages fetch` command downloads from declared origins into a per-workspace
cache. A selected root is exposed read-only on Linux and Windows; Windows
needs a read-execute grant, and Linux falls back to application storage.
Provider profiles record connection shapes without credentials, and an
`lm_studio` profile can record a loopback endpoint. Only the local LM Studio
adapter reads a stored credential, through an operator-selected profile that
supplies both the endpoint and the token (ADR-0047, ADR-0048); no cloud adapter
or login exists. That adapter can stream a reply (ADR-0049): the text is
provisional, and the journals record only the validated reply. Approval records authorize one exact
model-tool call, and the file-write and command tools are their only consumers. Chat
dispatches the read/list tools and, with the required live permissions, the
approval-gated edit, multi-edit, create, rename, delete, and command tools, only
for a workspace that enables `model_tools`. The edit and multi-edit tools write
only through checkpointed apply after an approved, writers-stopped decision, the
create tool only through the exclusive create, the rename tool only through the
no-replace rename, and the delete tool only through a checkpointed removal, each
after the same decision, and none detects concurrent writers.
A read-only CLI preview compares a workspace file with trusted proposed
content without writing to the workspace or creating a checkpoint.
Dependency security audits, native containment qualification, and release checks
still need their own implementation and evidence.
