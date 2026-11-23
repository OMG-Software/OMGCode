# Security coding rules

These rules define requirements for each implemented boundary. The CLI prototype
has bounded prompt/provider/journal checks. It has trusted workspace settings
and a bounded native file broker for manual commands. The core has a bounded
turn/tool runtime for read and list. Chat runs it with the local LM Studio
adapter only for a workspace whose `model_tools` preference is enabled;
otherwise chat rejects model-requested tools. A prototype desktop shell
(see the [desktop interface guide](desktop-interface.md)) renders session
content in a webview, decides approvals in a separate window, and shows a
read-only configuration view in a third; the rules below apply to it. Native confinement exists only for the trusted `run` command
and the approval-gated command tool, and it is not whole-user containment.
Configuration diagnostics always deny execution and do not replace per-target
authorization. See
[workspace settings](workspace-permissions.md) and [current limits](development.md). Apply these rules with the
[coding rules](coding-rules.md) and [testing standards](testing-standards.md).

## Trust and permissions

- Treat model output, repository files, tool output, provider responses, and
  frontend messages as untrusted input.
- The Rust core must authorize every tool and IPC request. Hidden buttons,
  prompts, model compliance, and frontend validation do not enforce policy.
- A model, repository instruction file, or plugin cannot grant permissions.
  Only the trusted user-settings and approval flow can change access.
- Bind authorization to a session, workspace, operation, target, policy revision,
  and lifetime. Validate it at execution. Invalidate pending grants when their
  scope or policy changes.
- Narrowing permissions must block new affected work and stop tasks that retain
  revoked access. Observe cleanup before reporting revocation complete. A
  task's watcher checks every recorded settings state since it last read
  them, so restoring the access before it reads cannot keep the task running
  (ADR-0061).
- Keep permission-setting controls separate from rendered untrusted content.
  Validate and limit every command exposed to a webview.
- Declare every webview command to the permission layer, grant it to named
  windows only, and authorize the calling window in Rust as the first statement
  of its handler. Keep one list of command names for the build script, the
  policy, and the tests. Grant no `core:` permission to a webview without an
  ADR.
- No command accepts a host path. Address sessions by UUID, workspaces by their
  registered identifier, and approvals by identifier, and resolve any path in
  Rust from trusted state. Never return a journal path, the data root, or a
  token to a webview.
- Decide an approval only from the window that renders no transcript content,
  and only with a single-use review token that core issued for an `exact`
  binding between the approval and a recorded request. Show request fields next
  to a decision control only when core reproduces the approval's digest. The
  webview is still web content, not a trusted computing base.
- Resolve the provider endpoint and read the provider token outside the
  webview. A `main` webview request cannot supply either. The `settings`
  window, and the `main` window's composer, may request the active
  connection by profile name only, among recorded local profiles with an
  endpoint; Rust records it only after a native confirmation that names the
  provider and endpoint, and a launch argument fixes it instead. The `settings` window may
  supply a loopback endpoint for a new local profile without a credential,
  and core validates it. Refuse a `main` request whose endpoint differs from
  the one resolved for the turn (ADR-0064).
- Show the trusted configuration view only in the `settings` window, which
  renders no transcript text, tool output, or skill description or body, and
  grant the configuration commands to that window only (ADR-0059). The
  `main` window may receive only the workspace summaries of `workspace_list`
  (identifier, folder display text, and the `model_tools`, `read`, `write`,
  and `execute` preferences), the fixed endpoint text, and the names of the
  skills enabled for a session's workspace. Return recorded
  paths and names as display-safe text, never a secret, and never accept them
  back as input.
- A desktop settings change runs only from the `settings` window and through
  the same core function, revision check, and audit event as the CLI. Get
  every folder from a native dialog that Rust opens; never accept a path from
  a webview. Apply a change that widens access only after the operator
  confirms a native dialog whose text Rust composes from fixed words and
  display-safe values. Hold no settings lock while a dialog is open, apply
  with the revision read before it, and observe the stop of runs that lost
  access. Open native dialogs from Rust without a Tauri plugin, so no webview
  can reach a dialog command (ADR-0060).
- Saved desktop preferences carry no endpoint, provider, credential, or
  permission and grant nothing. Bound and strictly parse the file, and fall
  back to built-in values when it cannot be read.
- A per-turn skill selection from a webview names skills only. Bound it, and
  let core load and check each skill for that turn as for the CLI's
  `chat --skill`.

## Instructions, summaries, caches, and skills

- Follow the [instruction](agent-instructions.md), [context](context-management.md),
  and [skills](skills.md) contracts. Keep policy and operation state outside
  generated text. Never deserialize a model summary into an approval or grant.
- Compile the visible base prompt into the application. Do not accept runtime
  replacements or promote editable preferences into the base's instruction
  channel. Prompt priority guides the model; core authorization must still deny
  unauthorized actions even when model instruction following fails.
- Preserve source trust when transforming content. Repository/skill/tool text
  must not become a trusted user addition through summarization or cache reuse.
- Validate current access before using cached data. Content hashes and TTLs do
  not authorize disclosure. Invalidate derived context after relevant permission,
  prompt, account, source, or skill changes.
- Keep application context caches and skill registries outside task-writable
  storage. Protect sensitive cached code and journal data with per-user access
  and retention rules. General caches must not store credentials.
- Treat imported skills and their resources as untrusted packages. Bound parsing
  and loading, reject escaping paths and unsafe links, and separate import from
  enablement and execution. Skill metadata cannot pre-approve tools.
- Skill scripts use the ordinary confined runner. Disabling a skill must block
  new dependent work and observe cleanup of attributed tasks before completion.
- Do not replay cached model tool calls or infer a fresh verification from an
  old result. Reconcile ambiguous side effects against core-owned records.
- Follow the [memory contract](memories.md). Only the trusted user flow can save
  or broaden a memory's scope. Project IDs, source trust, current revisions, and
  per-user protections govern retrieval. Memories cannot alter permissions.
  Withdrawal invalidates dependent context; old journals cannot recreate entries.

## Filesystem boundary

- The development root is the maximum host-development-data boundary.
  Restrict a session to its active workspace by default. Declare read-only
  runtimes, toolchains, and private writable scratch resources separately.
- Deny undeclared host data. Do not expose the host home directory, application
  settings, secret stores, or general caches to tasks.
- Base authorization on native paths and the object actually opened. Address
  traversal, links, junctions, reparse points, hard links, mount boundaries,
  alternate Windows path forms, and path-swap races.
- A string-prefix comparison or check-then-open canonicalization is insufficient.
  Use appropriate operating-system containment and object-relative operations.
- Do not automatically follow Git metadata or links outside the permitted root.
  A display of the checked-out branch reads only `HEAD`, bounded, through the
  anchored broker, with no Git process, and follows a linked worktree's
  `gitdir` only inside the development root (ADR-0064).
  Refuse layouts whose required access cannot be enforced.
- Bound reads, writes, searches, extraction, and temporary storage. Check the
  original content and version before replacing a file or applying a patch.
- Make recovery preserve unrelated changes. A checkpoint must not restore over
  later user work without detecting it.
- A read-only edit preview compares bytes read through the broker with trusted
  operator-selected proposed content. It is untrusted evidence, not a write
  grant. Its digest is a snapshot, not a conflict-free replacement token. The
  preview must not write, checkpoint, or promise that apply can proceed.
  Open a trusted proposed source without following a final link and reject
  reparse, device, or non-regular handles before reading it.
- The [accepted replacement contract](file-access.md#replacement-contract)
  defines gates for the trusted apply path. The core's read-only
  record inspectors validate claims in supplied bytes only. Its native Linux
  checkpoint store checks actual opened objects, owner/mode, scope, payloads,
  and manifests before admission. A recorded ready,
  prepared, commit, outcome, or acknowledgement cannot prove a native effect,
  authorize access, or trigger replay. Apply requires fresh
  read/write authority, expected exact bytes, native object checks, a durable
  private checkpoint, bounded storage, and a separately versioned ledger.
  Detected conflicts must leave workspace entries unchanged. Stage only in
  private storage on the target filesystem; refuse unavailable native or
  persistence controls. A precheck plus rename cannot guarantee no lost update
  against independent writers. Preserve uncertain outcomes without automatic
  retry, cleanup, or restore. A restore needs fresh current-content checks and
  must checkpoint later user work before replacing it.
- Checkpoint deletion is an explicit trusted-operator action for one resolved,
  retained checkpoint. Record a durable deletion intent before removing anything.
  Remove only the fixed artifact names, one native entry at a time, without
  recursion or following links, and refuse hard-linked or substituted entries.
  Never repair ACLs, clear attributes, evict history, or delete as a side effect
  of inspection, acknowledgement, admission, or quota pressure. An interrupted
  deletion blocks other file work until the same request finishes it.
- Checkpoint preparation requires fresh read/write authority and a source
  capture whose bytes, identity, and supported security metadata came from the
  same pinned file. Keep private initialization separate from an unused-store
  observation. When at least one checkpoint path remains, missing or invalid
  evidence must block new file work without repair, retry, eviction, or old-log
  growth. This layout cannot detect complete erasure of both checkpoint paths.
  Operator inspection/acknowledgement cannot grant authority or expose payload
  bytes.
  The Windows backend must verify exact protected
  per-user DACLs and refuse a failed directory flush. That flush has no Microsoft
  durability contract, so it must not be described as power-loss safe. The older
  journal helper's Windows directory-sync no-op is not a checkpoint durability
  primitive.
- Linux apply must retain the source and both parents, check the same accepted
  mount (ext4, btrfs, XFS, tmpfs, or ZFS; ADR-0069), preserve supported
  owner/group/mode, and refuse unsupported metadata.
  Verify the stage before syncing commit intent. Recheck the native objects and
  live authority before one rename attempt. Sync the published file and both
  changed parents before success. An error after intent preserves uncertainty;
  no replay, retry, stage cleanup, or rollback is authorized. The stop-writers
  flag acknowledges a caller responsibility. It provides no isolation.

The v0.3.0 native file broker currently exposes bounded read, list,
exclusive-create, and single-directory creation through trusted CLI commands.
The v0.4.0 core turn runtime exposes only read and list to its caller-supplied
model interface. It validates strict tool schemas and limits, opens the control
store for each call, obtains a fresh binding, releases the settings lock before
the next model request, and records tool intents and results as untrusted
evidence.
Its checks do not isolate hostile same-user processes or provide process
containment. Since 0.12.0, chat drives this runtime when the session's
workspace enables `model_tools`. Since 0.28.0 the model writes only through
approval-gated tools: the exact edit (ADR-0043), since 0.36.0 the multi-edit
and create tools (ADR-0054), and since 0.38.0 the rename and delete tools
(ADR-0055). General file tools remain disabled; capability output keeps
`workspace_file_tools` false. For model tools:

- Check the live `model_tools` preference before each call, then let the broker
  check the operation preference. A journal only selects a registration; never
  build a workspace ID from journal data.
- Reject absolute, rooted, prefixed, and parent-relative model paths before the
  broker, and keep the broker's native containment checks.
- Parse provider tool calls strictly. Refuse duplicate keys, legacy function
  calls, unexpected call fields, and more than 64 raw calls in one response.
- Remove known secrets from tool text before it is recorded or sent. The CLI
  removes the configured LM Studio token.
- Do not add a tool to the advertised set without an ADR, tests, and a settings
  decision.
 Do not treat journal metadata or enabled
preferences as authority. Follow the [workspace file-access contract](file-access.md)
for current native operations, failure states, and explicit recovery.

## Process and sandbox boundary

The [execution guide](execution.md) owns the runner contract. Version 0.13.0
added an unconfined process-tree runner core. Version 0.14.0 added
`run_confined`, with Landlock and a seccomp socket filter on Linux. Version
0.15.0 added a per-workspace AppContainer on Windows with recorded ACL grants.
Version 0.16.0 adds the trusted CLI `run`, which calls `run_confined` through
`omgcode_core::run`. Never call the unconfined `run` from the CLI, a model
tool, or the core. Do not call `run_confined` on a platform until its
confinement is implemented and tested. Its Windows `CreateProcessW`, job,
AppContainer, and ACL wrappers and its Linux pre-exec `prctl`, seccomp,
`close_range`, and Landlock calls are reviewed unsafe code in the platform
crate.
- Start processes only through `omgcode_core::run::run`. It checks the live
  `execute` preference, records the intent before the start and the outcome
  after it, and builds the environment from the profile only. Never inherit
  the caller's environment, and never search the caller's `PATH`.
- Give every trusted run tree limits. Never start a run without them, and
  never fall back to an unlimited run when they cannot be applied. On Linux a
  run cgroup must be joined by the child before `exec`, and limit files must
  stay outside the confined tree's Landlock rules.
- Give every trusted run a disk-growth limit. The supervisor measures the
  confinement's writable roots before the run and while it runs, as allocated
  bytes and as file lengths; a failed measurement stops the run rather than
  claiming a limit. The disk limit is sampled and is not a kernel or
  filesystem quota. Do not describe it as one, and do not count the read-only
  roots or host data. A run's recorded limit and peak growth are evidence, not
  authority.
- Never record argument text, environment values, or output in a journal.
  Record the argument digest and byte counts.
- Treat profile variables as public. Do not add a way to pass credentials
  through a profile without a reviewed credential design.
- Change container ACLs only through `ControlStore` grant and revoke methods,
  so every change has a synced intent and outcome. Never grant read-write
  beyond the workspace directory, and never let a full journal block
  revocation.
- On Windows, build a run's `Confinement` from the recorded grants. Never
  create a container profile at run time.
- Default to `UnixSocketAccess::PairsOnly`. `HostServices` exposes every
  pathname Unix socket the user can reach, including container daemons and
  credential agents. Select it only when the workspace `host_services`
  preference and the run's own flag both ask for it, and never for a model.

- Apply policy and operating-system restrictions before untrusted code starts.
  Never fall back to unrestricted execution when setup or a capability fails.
- Keep process permissions minimal and apply limits to descendants. Restrict
  filesystem access, networking, inherited handles, environment variables,
  host IPC, privileges, and resource consumption.
- Do not expose administration services, container-daemon sockets, credential
  agents, or host-local services through an unrestricted broker channel.
- Use explicit executable paths and structured arguments. Do not build shell
  commands by concatenating untrusted strings. Shell scripts require explicit
  permission, correct platform-specific argument handling, and the same sandbox.
- Track, terminate, and reap the full task process tree. Wait for cleanup on
  cancellation, timeout, permission revocation, and fatal task failure.
  A nonzero command exit must still drain and clean up its task's descendants.
- Keep broker interfaces narrow, authenticated, session-scoped, and bounded.
  A broker must not become a general host shell or arbitrary network relay.

## Provider traffic and tool calls

- Follow the [authentication contract](provider-authentication.md) for
  subscription and API-key connections. Check identity, usage entitlement,
  credential audience, and billing mode separately.
- Bind login callbacks to the current trusted attempt, validate provider
  responses, and protect token refresh and sign-out state. A login must not
  grant filesystem or command permissions.
- Model traffic, Git traffic, and task networking need separate policy.
  An approved model endpoint does not grant terminal networking.
- Sending workspace context to a cloud model is data egress. Require an explicit
  permitted provider/model profile and disclose routing changes. A localhost
  endpoint alone does not prove that inference stays local.
- Restrict destinations, redirects, and provider fallbacks. Protect credentials
  when a destination or redirect changes. Enabling a destination also permits
  some data to reach it; do not describe a destination allowlist as content
  confidentiality.
- Provider profiles record only a name, kind, and authentication mode. Never
  store a token, password, certificate, endpoint, or account in them. Keep
  subscription and API-key usage separate, and do not implement an adapter or
  login without the reviewed credential service and the provider's supported
  route. A profile is configuration, not authentication. See ADR-0038.
- Store provider credentials only through `omgcode_platform::credentials`
  (ADR-0042): DPAPI files with verified private DACLs on Windows and Secret
  Service items on Linux, never a plaintext fallback. Read secrets only from a
  trusted input stream, never from arguments or environment, and hold them in
  `Secret`, which is zeroed on drop and redacted from `Debug`. Journals record
  only the opaque identifier and backend. Never prompt to unlock a store or
  run a Secret Service prompt, and record a binding before deleting the
  credential it replaces. Never delete a new credential whose binding write
  may have been recorded; report its identifier instead.
- A future model tool that changes host state must consume a trusted,
  single-use approval bound to the session, workspace, operation, exact
  request digest, and settings revision. Refuse a missing, denied, expired,
  consumed, mismatched, or settings-invalidated record. Prompt text, model
  claims, and recorded intents never grant authority. See ADR-0039.
- The `workspace_edit_file` model tool may be advertised and dispatched only
  under live `model_tools`, `read`, and `write`. Take the expected current
  size and SHA-256 from a live audited broker read, never from the model or a
  saved conversation, and bind the approval digest to the session, workspace,
  normalized path, and both current and proposed size and digest under its own
  domain tag. Keep file text out of the approval summary and the success
  result. Require the recorded writers-stopped acknowledgement before
  consuming a `file_write` approval, consume it before the apply, and use only
  the checkpointed apply. Do not claim concurrent-writer detection. See
  ADR-0043.
- The `workspace_multi_edit_file` and `workspace_create_file` model tools follow
  the same advertisement, live-recheck, approval, and writers-stopped rules as
  `workspace_edit_file`, each under its own digest domain tag
  (`omgcode-file-multi-edit-v1`, `omgcode-file-create-v1`) so no approval can be
  consumed by another tool. A multi-edit matches every `old_text` against the
  original live bytes, refuses ambiguous, missing, overlapping, unchanged, or
  oversized results before recording an approval, and uses only the checkpointed
  apply. A creation must find the path absent and its parent present through
  audited broker calls before recording an approval, and uses only the broker's
  exclusive create after consuming the approval; it never replaces, retries, or
  deletes, and leaves an uncertain outcome's pending record for the trusted
  acknowledgement. Keep file text out of summaries and results. See ADR-0054.
- The `workspace_rename_file` and `workspace_delete_file` model tools follow the
  same advertisement, live-recheck, approval, and writers-stopped rules, under
  their own digest domain tags (`omgcode-file-rename-v1`,
  `omgcode-file-delete-v1`). Each reads its source live through the audited
  broker (at most 1 MiB, bytes not interpreted) before recording an approval,
  and binds the approval to that size and SHA-256 and to the normalized names.
  A rename must find its destination absent and its parent present before an
  approval, never replaces an entry, and is not checkpointed. A delete uses only
  `FileBroker::apply_removal`, which checkpoints the original bytes first and
  follows the checkpointed apply's gates, recheck, and uncertain-outcome rules.
  The native rename must pin the source, verify its bytes through the pinned
  handle, and use a no-replace primitive; the native removal must verify the
  object it removes (a verified handle on Windows, the pinned inode's link count
  afterwards on Linux). Neither retries, repairs, or deletes after an uncertain
  outcome, and neither reports concurrent-writer detection. Keep file text and
  full paths out of results. See ADR-0055.
- The `workspace_create_directory` model tool follows the same advertisement,
  live-recheck, approval, and writers-stopped rules under its own digest domain
  tag (`omgcode-directory-create-v1`). It must find the path absent and its
  parent present through audited broker calls before recording an approval,
  and uses only the broker's single-directory create after consuming it: no
  recursive creation, no replacement, and no retry. See ADR-0079.
- The `workspace_run_command` model tool may be advertised only under live
  `model_tools`, `execute`, `read`, and `write`. Its first call records a
  pending approval and starts nothing; only a later exact call may consume an
  approved record and call the trusted runner. The model cannot approve,
  select limits, select network or host services, or reuse an approval. It
  cannot ask the tool to compose or re-parse a shell command line; it may
  name a program the approved profile exposes, including an interpreter, and
  the operator's exact-request approval covers that program and its
  arguments. Treat its output as untrusted. See ADR-0040.
- Do not enforce a package or task destination list with proxy environment
  variables or an advisory loopback proxy: a confined tree can ignore them and
  connect directly. A package-network profile is configuration only until a
  mechanism constrains every egress path, including direct IP, IPv6, UDP, DNS,
  loopback, and alternate socket families. See ADR-0034.
- A trusted `packages fetch` is an operator action, never a model tool. It may
  contact only a declared origin and path prefix, must disable redirects and
  proxy variables, cap and hash the response, and write only the private
  per-workspace cache. It sends no credential unless the operator bound one to
  the matched origin (ADR-0056): then it sends only that origin's credential,
  read through core's load under the open control store, as one sensitive
  `Authorization` header, and never to another origin. A bound credential that
  cannot be read stops the fetch before any request; it never falls back to no
  credential or another credential. Never put it in a URL, run environment,
  journal, report, error text, or model context, and record only its opaque
  identifier. Record the intent and outcome without
  the URL text. A run may read only the selected per-workspace cache root; do
  not expose its parent, journals, settings, or another workspace's root. On
  Windows the root needs a read-execute container grant, and overlapping cache
  roots are refused. Do not expose the cache on a platform that cannot
  enforce the same boundary.
- Assemble and validate a complete structured tool call before dispatch.
  Validate names, arguments, limits, session scope, and provider call identity.
  A provider adapter must parse provider JSON strictly; the core runtime then
  revalidates names, argument shape, paths, and bounds before any broker call.
- Deduplicate calls within the correct request/session scope. After an ambiguous
  result, reconcile the operation instead of automatically executing it again.
- Parse malformed, oversized, interrupted, and unsupported provider responses as
  controlled failures. Model-supplied claims of success are not execution results.

## MCP boundary

- Follow the [MCP contract](mcp.md). Core-owned clients must authorize calls
  independently of model requests, server metadata, annotations, and prompts.
- Launch local servers through the same confined runner as development tools.
  SDK defaults must not create an unrestricted process or inherit host secrets.
- Give remote MCP traffic its own explicit destination/account/tool scopes.
  Treat authorization discovery URLs, redirects, tool headers, resources, and
  results as untrusted. Do not pass unrelated tokens to servers.
- Expose only implemented client capabilities. Server-requested input or work
  cannot silently grant consent, model usage, host access, or UI privileges.
- Revoke affected calls/grants on disable and observe local cleanup. Preserve
  uncertain remote outcomes; disconnection is not evidence of rollback.

## Git and credentials

- Treat Git configuration, hooks, filters, helpers, external diff/merge commands,
  submodules, transports, and worktree metadata as untrusted.
- Run Git workers in confinement with controlled configuration. Do not run
  repository-selected executable hooks or helpers with host privileges.
- Limit remote access to the authorized repository, destination, and operation.
  Use the credential service without exposing general host credential stores.
- Bind push approval to the source commit, destination ref, remote identity,
  and relevant expected remote state. Reassess changed operations.
- Unsupported required filters or repository layouts must produce explicit
  compatibility failures, rather than silently changing stored content.
- Keep model/API keys and Git credentials out of frontend state, model context,
  task environments, process arguments, logs, and error reports. Use narrow
  host-side credential services and redact before recording diagnostics.
- Apply the same protections to subscription access/refresh tokens, login codes,
  and verifier values. Do not import unrelated client login caches or silently
  switch subscription requests to separately billed API usage.
- An adapter obtains a stored credential only through core's load that checks
  the profile's provider kind, authentication mode, binding, and backend. Do not
  read the native store directly, and do not send a credential to a provider
  other than the one it was recorded for.
- When a selected credential cannot be read, stop the request. Do not fall back
  to another credential or to no credential.
- The operator chooses the credential source outside the webview and the model
  (a command-line argument or trusted setting). Do not combine two credential
  sources for one request.
- Send a stored credential only to the endpoint recorded with its profile. Do
  not edit a recorded endpoint in place; replacing it means removing the
  profile, which removes the credential.
- Validate an endpoint with one core rule, both when it is recorded and when
  it is used, so a journal cannot introduce an endpoint the command would
  refuse. A provider kind with no adapter accepts no endpoint.
- An adapter takes its endpoint and its credential decision from one core
  result. Do not take the endpoint from one place and the credential from
  another, and do not accept an endpoint argument beside a selected profile.
- Review secret-bearing types for accidental `Debug`, serialization, cloning,
  and long retention. Use reviewed secret-handling libraries where needed;
  do not claim that dropping an ordinary string erases its memory.
- Use established cryptographic and TLS libraries. Never disable certificate
  validation to make a connection work.

## Development toolchains

- Follow the [development-tool contract](development-tools.md). Explicitly
  declare installed or managed toolchain inputs and narrowly scoped exceptions
  to workspace-only data access.
- Expose required compilers, helper binaries, libraries, SDKs, and non-secret
  setup resources read-only. Keep trusted discovery and broker search paths
  outside project control.
- Redirect outputs, virtual environments, temporary files, and mutable caches
  to designated writable resources. Prevent alternate writable paths to a
  supposedly read-only toolchain.
- Do not inherit a general host PATH, home, credential-bearing configuration,
  or global package cache. Derive the required environment without secrets.
- Apply containment to build scripts, package hooks, test programs, code
  generators, language servers, and all descendants. Tool registration is not
  permission to run commands outside confinement.
- Separate toolchain installation/updates and package networking from ordinary
  execution. Do not make a host installation writable to an untrusted task.

## Frontend, persistence, and dependencies

- Render untrusted content through safe text or sanitized Markdown. Disable
  executable HTML and automatic remote resource loads.
- Build the desktop interface only from elements and text nodes. Do not use
  `innerHTML`, `outerHTML`, `insertAdjacentHTML`, `document.write`, `eval`,
  `new Function`, inline script or style, browser storage, or remote URLs in
  the frontend. Tests must scan for each. Show bidirectional override
  characters as visible markers.
- Escape any untrusted field that appears beside a decision control with core's
  display-safe functions, so control, bidirectional, zero-width, and other
  invisible format characters are visible. They do not detect homoglyphs or mixed
  scripts, and new code must not claim they do.
- Treat the derived session index as a cache. It is never an authority. Resolve
  a journal path from it only through a function that revalidates the stored
  path against the strict layout and the file name's UUID, and then validate the
  journal again when it is opened. Build a search expression from user text by
  quoting terms and binding it as a parameter, never by concatenating query
  syntax. Opening an observer must not take the writer lock.
- Do not load remote pages or previews into a webview with privileged IPC.
  Restrict navigation, resource loading, clipboard effects, and terminal escape
  features. Terminal output must not change application permissions.
- Parameterize database queries and validate serialized state. Preserve
  transaction and migration integrity during failure and restart.
- Store tool lifecycle and authorization records outside the task's writable
  view. An interrupted operation must remain visibly incomplete until reconciled.
  Schema-version-2 session tool intents and results are inspection evidence;
  a recorded intent is not permission to replay, and a failed turn may leave an
  intent without a result.
- Follow [session storage](session-storage.md) for core-owned JSONL journals,
  single-writer durability, bounded parsing, recovery, and archive transitions.
  Keep journals/indexes/artifacts outside task access and redact known secrets
  before persistence. Historical grants or transcript role labels cannot confer
  current authority. Never execute recorded tool calls during replay or restore.
- Review dependency updates, build scripts, procedural macros, frontend packages,
  and update mechanisms as executable supply-chain inputs.
- Explain the threat, failure behaviour, and tests for changes to these
  boundaries. Passing memory-safety checks alone is not security verification.
