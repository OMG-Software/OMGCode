# OMGCode implementation plan

Last verified: 2026-11-17.

This guide is part of `main` after PR #9. Read it before selecting the next
increment. Checkpoint preparation does not mean native replacement exists.

This is a resume guide, not a replacement for the owning contracts in
[docs](docs/README.md). Read [AGENTS.md](AGENTS.md),
[docs/coding-rules.md](docs/coding-rules.md),
[docs/security-coding-rules.md](docs/security-coding-rules.md), and
[docs/testing-standards.md](docs/testing-standards.md) before changing code.

## Current baseline

- `main` contains **PR #8** at `56ed9bf`: schema-v2 session tool events and the
  core read/list turn runtime.
- **PR #9** merged at `e4dbeca`: read-only edit previews, review fixes, and this
  resume guide. The merge was verified through GitHub on 2026-11-09.
- **PR #10** merged at `176637f`: the accepted replacement/checkpoint/recovery
  contract in
  [ADR-0018](docs/adr/0018-define-checkpointed-replacement-before-enabling-overwrites.md)
  and the [file-access guide](docs/file-access.md#replacement-contract).
  The merge was verified through GitHub on 2026-11-09. It changed documentation
  only.
- **PR #11** merged at `4da432f`: Phase A2a read-only core inspection of caller-supplied
  replacement records. See [ADR-0019](docs/adr/0019-validate-replacement-records-before-native-storage.md).
- **PR #12** merged at `9e616a3`: the OMGCode rename, version 0.7.0.
- **PR #13** merged at `22040d5`: **Phase A2b**, version **0.8.0**, with
  protected Linux checkpoint storage and existing-only CLI recovery.
- **PR #14** merged at `b3829db`: typed checkpoint inspection state and
  A2b review fixes, with no JSON or storage format change.
- **PR #15** merged at `4fe92ca`: **Phase A2c**, version **0.9.0**, with trusted
  Linux ext4 apply. Its pull-request checks passed, but the Rust workflow failed
  on the merge commit because of the test race described in the known cautions.
- **PR #16** merged at `b7fbc0b`: fixes that test race, with tests and
  documentation only.
- **PR #17** merged at `ddfc90a`: **Phase A3a**, the Windows NTFS private store.
- **PR #18** merged at `243ba8d`: **Phase A3b**, version **0.10.0**, with
  Windows NTFS checkpoint storage and trusted apply.
- **PR #19** merged at `270b889`: **Phase A4**, version **0.11.0**, with
  explicit restore and checkpoint deletion.
- **PR #20** merged on 2026-11-11: **Phase B**, version **0.12.0**, with
  read/list model tools in chat behind the `model_tools` workspace preference.
- **PR #21** merged at `6b1ebe4`: **Phase C1**, version **0.13.0**, with the
  execution contract and an unexposed process-tree runner core.
- **PR #23** merged at `2d44d41`: **Phase C2**, version **0.14.0**, with
  `run_confined`: Landlock and a seccomp socket filter on Linux, and a per-run
  Unix-socket mode.
- **PR #24** merged at `aa31976`: **Phase C3**, version **0.15.0**, with
  Windows AppContainer confinement and recorded, revocable container grants.
- **PR #25 and PR #26** merged at `4eeb12f`: **Phase C4**, version
  **0.16.0**, with the trusted CLI `run`, toolchain profiles, the run journal,
  and the `host_services` preference.
- **PR #27** merged at `ac01a74`: **Phase C5**, version **0.17.0**, with
  tree-wide memory, process, and CPU-time limits.
- **PR #29** merged at `4c2a561`: **Phase C6**, version **0.18.0**, with
  outbound network access behind `task_network` and `--network`.
- **PR #30** merged at `dd916fa` on 2026-11-14: **Phase C7** (version
  **0.19.0**, sampled net disk-growth limit) through **Phase C15**. It carried
  the package-network profile contract and records (C8, C9; ADR-0034), the
  trusted allowlisted package fetch (C10; ADR-0035), read-only cache access
  and per-workspace cache roots (C11, C12; ADR-0036, ADR-0037), provider
  profiles and trusted per-call approvals (C13; ADR-0038, ADR-0039), the
  approval-gated model command tool (C14, version **0.25.0**; ADR-0040), and
  its native end-to-end acceptance test (C15).
- **PR #31** merged at `28da2cf` on 2026-11-15: PR #30 review fixes, including
  asynchronous package-fetch file I/O.
- **PR #32** merged at `60430ee` on 2026-11-16, from
  `codex/context-compaction`. The merge was verified through GitHub. It
  carried three phases:
  - **Phase C16**, version **0.26.0**, with manual and pre-turn automatic
    context compaction (ADR-0041). No run accepts a profile or has
    destination-scoped networking, and no provider adapter is enabled.
  - **Phase D1**, version **0.27.0**, the provider credential service
    (ADR-0042). No adapter reads a stored credential.
  - **Phase C17**, version **0.28.0**, the approval-gated model edit tool
    (ADR-0043). It writes only through the checkpointed apply after an
    approved, writers-stopped decision.
- **PR #33** merged at `9c9f606` on 2026-11-17, from
  `codex/desktop-interface`. **PR #34**, from `codex/request-cancellation`,
  was stacked on it and merged into that branch first, so both landed in the
  same merge commit. The merges were verified through GitHub. Together they
  carried:
  - **Phase E**, version **0.29.0**, the prototype desktop interface and
    session tree (ADR-0044, ADR-0045, ADR-0046). Its windows have run only on
    Windows; on Linux it builds and its tests pass. See
    [the desktop interface guide](docs/desktop-interface.md).
  - **Phase D2**, version **0.30.0**: the local LM Studio adapter reads its
    token from a stored credential through an operator-selected profile
    (ADR-0047).
  - **Phase D3**, version **0.31.0**: a stored credential is bound to its
    profile's loopback endpoint, and a selected profile supplies both the
    endpoint and the token (ADR-0048).
  - **Phases D4 and E6**, version **0.32.0**: the local model's reply streams,
    and chat events are pushed to the desktop window (ADR-0049).
  - **Phase D5**, version **0.33.0**: a cancelled turn interrupts a streamed
    request and is recorded with the failure reason `cancelled` (ADR-0050).
  - **Phase D6**, version **0.34.0** (PR #34): a cancel request stops every
    model request, streamed or not, and the compaction requests, and Ctrl-C
    cancels a CLI `chat` turn or `session compact` (ADR-0051).
- **PR #36** merged at `b58f3f7` on 2026-11-17, from `codex/testing-builds`:
  the Testing build workflow, which tests each push to `main`, packages
  unsigned Windows, Ubuntu, and Fedora 44 archives with no installer, and
  publishes them as the rolling `latest` prerelease (ADR-0052). No
  application change; the version stays 0.34.0. On 2026-11-17 its first two
  runs, for `b58f3f7` and `4f94774`, were still queued and no `latest`
  release existed, so the workflow has not yet completed on GitHub.
- **PR #37** merged at `4f94774` on 2026-11-17: chat-turn cancellation tests
  in `omgcode-app` measure their timing from the cancel rather than from the
  turn start. Tests and documentation only.

Implemented today:

- Pinned Rust 1.99.1 workspace with `omgcode-core`, `omgcode-cli`,
  `omgcode-platform`, `omgcode-app`, and `omgcode-desktop`.
- Compiled, versioned base prompt `0.1.4` with a visible digest.
- Workspace settings, native directory identity, process-local bindings, and a
  separate settings/audit journal.
- Bounded native read, list, exclusive file creation, and single-directory
  creation through trusted CLI commands, with a separate file-operation journal
  and explicit recovery.
- JSONL sessions with schema version 3: tool requests/results, tool messages,
  archive/restore/interrupt, readable context compaction, and strict replay.
  Version 1 and 2 journals remain readable and writable with their subsets.
- Conservative context accounting, manual `session compact`, and pre-turn
  auto-compaction for the local LM Studio route. Summaries are labelled
  untrusted evidence and follow the live tool-result replay rule (ADR-0041).
- Core turn/tool runtime for read/list, the approval-gated exact edit, and the
  approval-gated command through a caller-supplied model interface. It has strict tool-call parsing, round/call/byte/prompt limits,
  cooperative cancellation, source-labelled messages, and fresh per-operation
  policy bindings. Chat dispatches it through the LM Studio adapter for a
  workspace that enables `model_tools`.
- Read-only `file diff`: trusted CLI preview of a broker-read file against
  complete proposed UTF-8 content. Exact digests, bounded one-hunk diff, no
  workspace mutation, no checkpoint, no replacement.
- LM Studio Chat Completions chat. Requests are non-streaming by default, and
  `chat --stream` and the desktop shell stream the reply as provisional text
  (ADR-0049). It is text-only
  unless the session's workspace enables `model_tools`; then it offers the read
  and list tools, the approval-gated edit tool when `read` and `write` also
  allow it, and the approval-gated command tool when `execute`, `read`, and
  `write` also allow it. See
  [ADR-0026](docs/adr/0026-offer-read-list-tools-through-a-workspace-preference.md),
  [ADR-0040](docs/adr/0040-gate-the-model-command-tool-with-approvals-and-the-trusted-runner.md),
  and [ADR-0043](docs/adr/0043-gate-the-model-file-edit-tool-with-approvals-and-checkpointed-apply.md).
- Read-only core initialization/ledger record inspectors with bounded strict
  JSON, lifecycle validation, manifest references, and unresolved-state retention.
  The pure inspectors open no files and execute no recorded work.
- Linux protected checkpoint preparation through the core broker, strict
  manifests and payload checks, bounded initialization, quota and recovery
  capacity, and fail-closed admission. No workspace entry is replaced.
- Existing-only `file checkpoints` and `file acknowledge-checkpoint` recovery.
  Inspection returns metadata. Acknowledgement clears a blocker without success,
  repair, permission changes, or replay.
- Trusted `file apply` on Linux ext4: exact precondition, stopped-writers
  acknowledgement, private stage, durable commit intent, one native rename,
  and verified sync/outcome ordering. See [ADR-0022](docs/adr/0022-publish-one-verified-stage-on-linux-ext4.md).
- Windows NTFS checkpoint storage and trusted `file apply`, with no power-loss
  claim. See [ADR-0024](docs/adr/0024-publish-one-verified-stage-on-windows-ntfs.md).
- `process::run_confined` on Linux: a Landlock allowlist (ABI 3 or later),
  seccomp refusal of IP sockets and `io_uring`, and `PairsOnly` (default) or
  `HostServices` Unix-socket access. See
  [ADR-0028](docs/adr/0028-confine-linux-runs-with-landlock-and-a-socket-filter.md).
- `process::run_confined` on Windows: a per-workspace AppContainer with no
  capabilities, checked against persistent ACL grants, with `HostServices`
  refused. Trusted `workspace container-grant` and `container-revoke` record
  each intent and outcome in the control journal. See
  [ADR-0029](docs/adr/0029-confine-windows-runs-in-a-workspace-appcontainer-with-recorded-grants.md).
- Trusted `omgcode run` on Linux and Windows:
  - behind the live `execute` preference, through `run_confined` only;
  - operator-declared toolchain profiles (`workspace profile-add`,
    `profile-show`, `profile-remove`) and a curated environment;
  - a bounded `runs-v1.jsonl` journal of intents and outcomes, with no
    argument text or output, managed by `runs audit`, `acknowledge`, and
    `archive`;
  - a `host_services` preference that a run's `--host-services` flag also
    needs (Linux only);
  - `--network` behind the live `task_network` preference;
  - memory, process, CPU-time, and sampled net disk-growth limits, recorded in
    the run journal;
  - interrupt-to-cancel handling.

  See [ADR-0030](docs/adr/0030-run-trusted-commands-through-recorded-toolchain-profiles.md),
  [ADR-0031](docs/adr/0031-limit-whole-run-trees-with-job-objects-and-delegated-cgroups.md),
  [ADR-0032](docs/adr/0032-allow-outbound-network-for-trusted-runs-through-task-network.md),
  and [ADR-0033](docs/adr/0033-bound-run-disk-growth-by-sampling-writable-roots.md).
- A [package-network profile](docs/execution.md#package-network-profile-contract-only)
  contract and implementation gate (ADR-0034). Version 0.20.0 records
  operator-declared profiles as configuration only. No run accepts a profile,
  no destination filter exists, and `--network` remains all-or-nothing.
- A trusted [`packages fetch`](docs/execution.md#trusted-package-fetch)
  command (ADR-0035). It contacts only declared origins, refuses redirects and
  credentials, bounds and hashes the response, writes a private per-workspace
  cache, and records the intent and outcome. The run remains offline and does
  not yet read the cache.
- Read-only run access to a per-workspace fetched-package cache root
  (ADR-0037, superseding ADR-0036). The root is selected with
  `workspace set-package-cache`, exposed read-only on Linux and Windows, and
  named by `OMGCODE_PACKAGE_CACHE`. Windows requires a read-execute container
  grant on the root; Linux falls back to application storage when no root is
  selected. Overlapping roots are refused.
- Provider profiles record connection shapes without credentials or adapters
  (ADR-0038). Trusted approval records bind a future model-tool call to a
  session, workspace, operation, request digest, and settings revision, expire,
  and are consumed once (ADR-0039). Provider profiles enable no connection; the
  command and edit tools are the only model tools that consume an approval
  record.
- Provider credentials in protected OS storage (ADR-0042): DPAPI files with
  verified private DACLs on Windows and Secret Service items in the default
  collection on Linux, with no plaintext fallback. Trusted
  `workspace provider-credential-set` reads an API key from standard input and
  binds a new opaque identifier to an `api_key` profile in the control
  journal; `provider-credential-remove` and `provider-profile-remove` unbind
  and then delete the native item. Only the local LM Studio adapter reads a
  stored credential, through a selected `lm_studio` profile (ADR-0047). An
  `lm_studio` profile can record one endpoint, fixed for its life, and a
  selected profile supplies both the endpoint and the token (ADR-0048). No
  cloud adapter exists.
- The `workspace_run_command` model tool (ADR-0040) is offered only with live
  `model_tools`, `execute`, `read`, and `write`. Its first call records a
  pending approval and starts nothing; an approved exact call consumes the
  approval and uses the trusted runner with fixed limits.
- The `workspace_edit_file` model tool (ADR-0043) is offered only with live
  `model_tools`, `read`, and `write`. It replaces one exact text occurrence in
  an existing UTF-8 file of at most 1 MiB. Each call reads the live file
  through the audited broker; its approval digest binds the session,
  workspace, normalized path, and both current and proposed size and SHA-256.
  The first call records a pending `file_write` approval with no file text;
  approval needs `--writers-stopped`, and an identical later call consumes it
  and calls the checkpointed apply with the live-read expected value.
  `session inspect` lists recorded tool requests for review. No model create,
  delete, rename, network, Git, or MCP tool exists.
- Trusted `file restore` through a new checkpoint and
  `file delete-checkpoint --permanent` for one resolved checkpoint, on the same
  filesystems as apply. Neither is power-loss qualified. See
  [ADR-0025](docs/adr/0025-restore-and-delete-checkpoints-explicitly.md).
- In-place checkpoint ledger compaction (0.35.0, on branch
  `codex/checkpoint-ledger-compaction`, not merged): 65,536 records and
  64 MiB for the ledger, 128 MiB for the store, trusted
  `file compact-checkpoints --discard-history`, and automatic compaction
  before an apply, restore, or deletion that would not fit. Deleted entries'
  history is discarded. Not power-loss qualified. See
  [ADR-0053](docs/adr/0053-compact-the-checkpoint-ledger-in-place.md).

Not implemented:

- Apply or restore on other filesystems, workspace file deletion, ledger
  compaction, and standalone CLI checkpoint preparation.
- Model create, delete, rename, multi-edit, network, Git, and MCP tools;
  concurrent-writer detection for any apply route; live provider tool-call
  qualification; and streaming against a live server, discovery, and
  cancellation of any request against a live provider. A read, list, or edit
  tool call in progress cannot be interrupted, and a CLI interrupt has not been
  delivered on Windows.
- A run network destination allowlist, runtime skills, memories, mid-turn and
  provider-native compaction, and caches. Nothing calls the unconfined process
  runner.
- Desktop development panels (terminal tabs, task logs, and diffs), a settings
  interface, a recovery interface, signed packages and installers, and a
  Linux run of the desktop shell's windows. The prototype shell, session tree,
  search, chat, and approvals window are on `main`. The only packaging is the
  unsigned rolling testing archives (ADR-0052).
- Cloud provider adapters and subscription/API-key login. Only the local LM
  Studio adapter reads a stored credential, and only at the loopback endpoint
  recorded with its profile. No endpoint rule exists for a non-loopback
  provider, HTTPS, or certificates.
- Subscription token storage, token refresh, account lifecycle, and a
  memory-only session credential.

## Resume steps

1. Inspect the current branch and local changes. Preserve unfinished feature
   work and unrelated files. When there is no unfinished feature work, update
   the baseline:

   ```text
   git fetch origin --prune
   git status --short --branch
   git switch main
   git pull --ff-only
   git status --porcelain
   ```

2. Verify that the preceding phase's PR is merged. Every phase listed in the
   current baseline is merged as of PR #37. Preserve a feature branch while its
   PR is under review.
   Do not merge a PR without the user's authorization.
3. Re-read the owning contracts for the selected phase. Do not infer authority
   from this plan, prompts, journals, or model output.
4. Create a `codex/<phase>` branch from a clean, updated `main`.
5. Run the verification baseline before changing code.
6. Carry one bounded phase through implementation, tests, docs, changelog, and
   an ADR when the decision is lasting. Open a PR; do not merge it yourself.

## Verification baseline

Build the desktop frontend before any Cargo step that includes
`omgcode-desktop`. The crate embeds `crates/omgcode-desktop/ui/dist` at compile
time, and `cargo check`, `clippy`, `build`, and `test` fail without it:

```text
cd crates/omgcode-desktop/ui
npm ci
npm run build
```

The frontend checks, run from the same directory, are `npm run check`,
`npm run lint`, `npm test`, and `npm run test:dist`. They need Node 24. The
Frontend workflow runs them on Linux and Windows.

Run from the repository root with the pinned toolchain:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo test --workspace --doc --locked
```

Run native checks on Windows and Linux for runtime changes. Use a separate
target directory for a Linux run so Windows artifacts cannot mask stale
results. Documentation-only increments need local Markdown link/fragment and
whitespace checks, plus the applicable review and analysis checks in
[docs/repository-checks.md](docs/repository-checks.md). That guide describes CI
services; the repository has no standalone documentation checker. Record the
checks that actually ran. Application tests were not applicable to A1; A2a
requires the Cargo checks and targeted `replacement_records` integration tests.
A2b adds `checkpoint_storage`, `checkpoint_recovery`, native private-store and
capture tests, and internal persistence fault/reopen tests. Run the complete
suite on both hosts with default parallelism, as CI does. Use
`-- --test-threads=1` only as an additional run when host contention affects local
provider fixtures; record failures before any isolated rerun or full retry. A
serial run alone cannot detect races between test threads.

On a Windows host whose repository is on ReFS, point `TEMP` and `TMP` at a
directory on NTFS. The private-store, credential, container, and replacement
tests need NTFS. Earlier notes here put `TEMP` and `TMP` on the same volume as
the fixtures; that works only when the repository volume is NTFS. Phase E
development ran with the repository on a ReFS volume. See
[testing standards](docs/testing-standards.md#desktop-shell-and-frontend).

Phase E final verification on 2026-11-16 used Rust 1.99.1 and default test
parallelism, with the Windows and Linux runs one after the other.

- **Windows** (Windows 11 build 26300, repository on ReFS, `TEMP` and `TMP`
  on NTFS): formatting, Clippy with `--locked`, all-target tests, and doc
  tests each exited zero. 782 tests in 36 test binaries and one doc test
  passed. None failed. Eight were ignored, the same eight as before this
  phase: one in `run_workflow`, six in `toolchain_acceptance`, and one in
  `omgcode-platform`. The two file-symlink tests printed `SKIPPED link
  assertions`. The frontend passed `npm run check`, `npm run lint`, and 113
  Vitest tests, including the four bundle checks.
- **Linux** (WSL 6.6, Ubuntu 24.04, ext4, a clean LF clone of `60430ee` with
  the changed files overlaid and the frontend bundle built on Windows):
  formatting, Clippy with `--locked`, all-target tests with `--no-fail-fast`,
  and doc tests each exited zero. 776 tests in 36 test binaries and one doc
  test passed. None failed. Eight were ignored. The platform suite re-runs
  four tests inside `systemd-run` scopes and prints a nested result line for
  each; those four are not counted twice here. The frontend commands did not
  run on Linux, because that host has Node 18.
- **Failures before the final runs.** The first complete Linux run failed 19
  tests. Eighteen credential tests failed because the login keyring was
  locked; they passed after it was unlocked, with no code change. One desktop
  approval test failed because its fixture created the data root with the
  default mode; the fixture now creates it with mode 0700. An earlier Linux
  run stopped at one `omgcode-app` test that assumed the Windows spelling of
  the data directory; that assertion is now case-insensitive.
- No test timed out and no provider fixture was retried on either host.

Phase D2 final verification on 2026-11-16 used Rust 1.99.1, default test
parallelism, and `--no-fail-fast`, with the Windows and Linux runs one after
the other.

- **Windows** (Windows 11 build 26300, `TEMP` and `TMP` on NTFS, real DPAPI
  store): formatting, Clippy with `--locked`, all-target tests, and doc tests
  each exited zero. 815 tests in 39 test binaries and one doc test passed.
  None failed. The same eight were ignored. The run took under four minutes.
- **Linux** (WSL 6.6, Ubuntu 24.04, ext4, a clean LF clone of `cbbec85` with
  the changed files overlaid, real Secret Service with the login collection
  unlocked): the same four commands each exited zero. 807 tests in 39 test
  binaries and one doc test passed, not counting five nested result lines from
  the platform suite's `systemd-run` re-runs. None failed. Eight were ignored.
- **Failures before the final runs.**
  - Linux Clippy failed once on `drop_non_drop`: `CredentialStore` has drop
    glue on Windows and none on Linux, so an explicit `drop` of it was a lint
    error only there. A block scope replaced it.
  - Three Linux desktop credential tests failed once because the WSL virtual
    machine had restarted and the login collection was locked again. They
    passed after it was unlocked, with no code change.
  - The implementing worker's own Windows run had one `run_workflow` test
    exceed its 60-second limit while another project was compiling on the
    host. It passed on a single rerun and in both full runs above. No timeout
    was changed.
- The frontend did not change in D2, so its commands were not rerun.
- A manual run with the real binaries on Windows showed the stored token
  arriving as the bearer header while a different `OMGCODE_LM_TOKEN` value was
  ignored, an unknown profile refused with exit 1 by both `omgcode chat` and
  `omgcode-desktop`, and the shell starting with a valid profile. It was done
  before the profile checks were moved ahead of the native store open and was
  not repeated afterwards; the automated tests cover the final order.
- Not run: a chat turn with a stored credential through the real shell's
  windows, on either platform.

Phase D3 final verification on 2026-11-16 used Rust 1.99.1, default test
parallelism, and `--no-fail-fast`, with the Windows and Linux runs one after
the other.

- **Windows** (Windows 11 build 26300, `TEMP` and `TMP` on NTFS, real DPAPI
  store): formatting, Clippy with `--locked`, all-target tests, and doc tests
  each exited zero. 839 tests in 40 test binaries and one doc test passed.
  None failed. The same eight were ignored. The run took about two minutes.
- **Linux** (WSL 6.6, Ubuntu 24.04, ext4, a clean LF clone of `b9f7d11` with
  the changed files overlaid, real Secret Service with the login collection
  unlocked): the same four commands each exited zero. 831 tests in 40 test
  binaries and one doc test passed, not counting five nested result lines from
  the platform suite's `systemd-run` re-runs. None failed. Eight were ignored.
  The new `launch_flags` suite ran the real desktop binary there: both flags
  together exited 2, and a profile with no settings store exited 1 and
  created nothing, each before any window.
- No command failed or timed out in these two final runs.
- The frontend did not change in D3, so its commands were not rerun.
- A manual run with the real binaries on Windows used two mock local servers.
  The stored token arrived only at the endpoint the profile records while a
  different `OMGCODE_LM_TOKEN` value was set. `--endpoint` beside
  `--provider-profile` was a usage error with exit 2 and no request, for both
  `omgcode chat` and `omgcode-desktop`. The shell started with a valid profile
  and reported the profile's endpoint. The first attempt at the shell check
  attached to another application that already held the WebView2 debugging
  port; it was repeated on a free port.
- Not run: a chat turn with a stored credential through the real shell's
  windows, on either platform.

Phase D4 and E6 final verification on 2026-11-16 used Rust 1.99.1, default
test parallelism, and `--no-fail-fast`.

- **Windows** (Windows 11 build 26300, `TEMP` and `TMP` on NTFS, real DPAPI
  store): formatting, Clippy with `--locked`, all-target tests, and doc tests
  each exited zero. 923 tests in 41 test binaries and one doc test passed.
  None failed. The same eight were ignored. The test run took about two
  minutes.
- **Linux** (WSL 6.6, Ubuntu 24.04, ext4, a clean LF clone of `19b3747` with
  the changed files overlaid, real Secret Service with the login collection
  unlocked): the same four commands each exited zero. 915 tests in 41 test
  binaries and one doc test passed, not counting five nested result lines from
  the platform suite's `systemd-run` re-runs. None failed. Eight were ignored.
  The desktop binary exited 2 for `--stream maybe`.
- **Frontend** (Windows, Node 24): the type check and Biome were clean. 155
  tests in 11 files passed, and the five bundle tests passed after a rebuild.
  The frontend commands were not run on Linux, whose Node is too old.
- **Failures before the final runs.**
  - The first Linux run did not run at all: `cargo` was not on the path of
    the non-login shell, and every step exited 127. The script now loads the
    Cargo environment and stops if `cargo` is missing. The rerun is the result
    above.
  - The first end-to-end run reported one failed step. The driver fetched the
    approvals bundle from page script, and the content security policy
    blocked the fetch. The check was removed, because the bundle test already
    covers it, and the whole run was repeated.
  - Workers' own test failures were in tests they were writing. No existing
    assertion was weakened and no timeout was changed.
- **Real shell, Windows.** The built shell was driven over the WebView2
  debugging port against a paced event-stream mock server and a scratch data
  root. All eleven steps passed on the repeated run:
  - a streamed tool-enabled turn showed the live entry for the first request,
    reset it for the second, and showed seven distinct preview states;
  - markup in the stream stayed literal text, no element was created, no
    script ran, and a bidirectional override was shown as `[U+202E]`;
  - both requests carried `"stream":true` and `Accept: text/event-stream`, and
    none carried `stream_options`;
  - the recorded reply appeared 49 ms after the mock ended the stream, which
    is the pushed `finished` event, not the five-second check;
  - a stream that ended without `[DONE]` showed its partial text, then failed
    with `local model stream ended before completion`, and the partial text
    was not in the transcript;
  - a second subscription made from page script received `request_started`,
    three `delta` events, and `finished` in order, and the page, whose own
    subscription it had replaced, settled about four seconds later through
    the reconcile check;
  - `approvals` was refused `chat_subscribe` by its capability;
  - with `--stream off` the request carried `"stream":false`, no live entry
    appeared, and the reply appeared 79 ms after the response;
  - both launches closed with exit status 0 and left no process.
- Not run: any live LM Studio server; the channel or any window on Linux; the
  750 ms fallback poll in the real shell (the frontend tests cover it);
  `chat --stream` by hand in a terminal (the CLI tests run the real binary).

Phase D5 final verification on 2026-11-16 used Rust 1.99.1, default test
parallelism, and `--no-fail-fast`. An unrelated project was building on the
host for the whole period, and the host stayed between 82% and 100% CPU on all
16 logical processors. A watcher waited about 20 minutes for the load to drop.
It did not, so the runs were made under that load.

- **Windows** (Windows 11 build 26300, `TEMP` and `TMP` on NTFS, real DPAPI
  store): formatting, Clippy with `--locked`, and doc tests each exited zero.
  The all-target test run exited non-zero: 942 tests passed, **one failed**,
  and the same eight were ignored, in 41 test binaries. The run took about four
  and a half minutes, against about two minutes on a quiet host. It was run
  twice with the same result.
- **The Windows failure** is
  `run_workflow::concurrent_runs_complete_and_are_recorded`, which reached its
  60-second limit. It runs a CPU-bound loop in the confined runner and does not
  use the provider or chat code. The same test, run alone against a clean
  checkout of `02221e9` at 90% load, failed the same way. That commit passed it
  on a quiet host earlier the same day. This change touches nothing in
  `omgcode-platform` or the run path. The cause is taken to be host load. It
  is still a failed check in this run, and it was not rerun on a quiet host.
  No timeout was changed.
- **A third Windows run after the commit** (`f3d8be2`, host at 77% to 100%
  CPU) gave the same result: 942 passed, the same one failed, eight ignored.
  The test's own shell loop was then timed outside OMGCode on the same host:
  one `cmd` loop of 30,000 `cd .` iterations took 22 to 24 seconds, and two at
  once took 38 seconds, before any confinement, limit sampling, or journal
  work. The test allows 60 seconds for two confined runs. Its duration
  therefore depends on host speed, and it has not been seen to fail on a quiet
  host.
- **The test was then changed** so that it no longer depends on host speed.
  Each run writes its file and waits until it sees the other run's file, on
  both platforms, so exit code 0 from both also proves the runs overlapped.
  The assertions and the 60-second limit are unchanged, and no production
  code changed. The script's exit behaviour was first checked in a plain
  shell: exit 3 when the other file never appears, exit 0 at once when it is
  present, and both exit 0 when two start together. The changed test then
  passed three runs in 1.2 to 3.0 seconds at 82% to 91% CPU.
- **Full runs with the changed test**, on the same loaded host.
  - Windows, at 100% CPU: formatting, Clippy with `--locked`, all-target
    tests, and doc tests each exited zero. 943 tests in 41 test binaries and
    one doc test passed. None failed. Eight were ignored.
  - Linux (a clean LF clone of `f3d8be2` with the changed files overlaid):
    the same four commands each exited zero. 935 tests in 41 test binaries and
    one doc test passed, not counting the five nested result lines. None
    failed. Eight were ignored. The `run_workflow` binary, which holds the
    changed test, took 2.2 seconds.
  - Still not run: either suite on a quiet host after these changes.
- **Linux** (WSL 6.6, Ubuntu 24.04, ext4, a clean LF clone of `02221e9` with
  the changed files overlaid, real Secret Service with the login collection
  unlocked): formatting, Clippy, all-target tests, and doc tests each exited
  zero on the final run. 935 tests in 41 test binaries and one doc test passed,
  not counting five nested result lines from the platform suite's
  `systemd-run` re-runs. None failed. Eight were ignored.
- **Frontend** (Windows, Node 24): the type check and Biome were clean. 159
  tests in 11 files passed, and the five bundle tests passed after a rebuild.
  The frontend commands were not run on Linux.
- **Failures before the final runs.**
  - The first Linux run had one failed test,
    `cancelling_while_the_response_head_is_awaited_stops_the_request`. The test
    cancelled after a fixed 150 ms sleep. On the loaded host the client had not
    connected by then, so the adapter returned `Cancelled` without connecting,
    which is correct, and the mock server timed out waiting for a connection.
    The test now cancels when the server signals that it has read the request.
    It then passed three runs on Windows and the full Linux rerun above. The
    adapter was not changed.
  - The first end-to-end run reported one failed step. The driver timed the
    exit from the start of the PowerShell helper that posts the close message,
    and on the loaded host an idle application took 4.6 seconds by that
    measure. The helper now prints the moment it posts the message, and the
    whole run was repeated.
  - The implementing worker reported the Windows failure above and one Clippy
    lint in a test it was writing, which it fixed.
- **Real shell, Windows.** The built shell was driven over the WebView2
  debugging port against a mock server that holds a stream open, with a
  scratch data root and a workspace without model tools, so every turn was
  text-only. All nine steps passed on the repeated run:
  - the Cancel button ended a held turn: the window settled 74 ms after the
    click with the notice `Turn cancelled.` and no error styling, the server
    saw the connection close 28 ms after the click, and `chat_status` reported
    `failed` with the code `cancelled`;
  - the next turn in the session was accepted and completed;
  - closing the window while a second stream was held ended the process with
    status 0, 148 ms after the close message was posted (101 ms for an idle
    application in the same run), and the server saw the connection close;
  - after a restart the session was idle, showed both turns as
    `Turn failed: cancelled`, held none of the partial text, and accepted a
    new turn.
- Not run: any live LM Studio server; any window on Linux; the Windows suite
  on a quiet host; cancellation of a tool-enabled turn in the real shell (the
  service tests cover it); `--stream off` in the real shell for this change.

These local results do not establish remote CI status: neither workflow change
has run. The real shell was driven end to end on Windows only
([testing standards](docs/testing-standards.md#desktop-shell-and-frontend)).
No window has been opened on Linux.

A2b local verification on 2026-11-10 passed formatting, Clippy, serial all-target
tests, and doc tests with Rust 1.99.1. Ubuntu 24.04 under WSL, uid 1000, passed
266 tests and one doc test. Native Windows passed 235 tests and one doc test.
Each command exited zero; no tests failed, were ignored, or were filtered.
The final provider fixtures did not time out. Earlier baseline timeout evidence
is retained; no assertion or timeout was weakened. These local results do not
establish remote CI status or native replacement qualification.

A2c started from the verified `b3829db` merge. The unchanged baseline passed
266 Linux and 235 Windows tests, plus one doc test on each host. Windows needed
workspace-local `TEMP` and `TMP` and an execution context that permits native
hard links and archival operations. Keep the original access-denied results;
do not weaken assertions to make a restricted run pass.

A2c final verification passed formatting, Clippy, serial all-target tests,
and doc tests with Rust 1.99.1. Linux ran 293 tests and one doc test. Native
Windows ran 237 tests and one doc test. Each final command exited zero; no tests
failed, were ignored, or were filtered. Linux apply ran on ext4 under WSL
with a separate tmpfs mount for the cross-mount refusal case. See the
[native apply limits](docs/file-access.md#implemented-native-apply-phase-a2c).

The lock-race fix started from `4fe92ca`, where Linux `checkpoint_recovery` failed
7 of 150 parallel runs and `cli_workflow` failed 7 of 150. After the fix, each of
those suites and `replacement_apply` had no failures in 300 parallel runs. Final
verification used default test parallelism with Rust 1.99.1 and passed
formatting, Clippy, all-target tests, and doc tests. Linux under WSL ran 294 tests
and one doc test. Native Windows ran 238 tests and one doc test, with
workspace-local `TEMP` and `TMP`. No tests failed, were ignored, or were filtered.

A3b final verification used default test parallelism with Rust 1.99.1 and passed
formatting, Clippy, all-target tests, and doc tests. Native Windows ran 309 tests
and one doc test, with `TEMP` and `TMP` on the same NTFS volume as the fixtures.
Linux under WSL ran 296 tests and one doc test. No tests failed, were ignored, or
were filtered. Temporary mutations of the Windows refusal checks were each caught
by a test, except for two that do not change behaviour on the tested host: a link
check that `stage_metadata` repeats, and the first of two identity comparisons in
the recheck. These results do not establish remote CI status, power-loss behaviour,
or release filesystem support.

A4 started from the unmerged PR #18 head `2dc3b50`. The unchanged baseline passed
309 Windows and 296 Linux tests, plus one doc test on each host. The first
Windows run failed 10 platform tests with access denied when a fixture opened
with `WRITE_OWNER`. The worktree's `TEMP` inherited only modify access for
Authenticated Users under an unelevated token. Pointing `TEMP` and `TMP` at a
workspace-local directory that grants the current user full control fixed it.
No assertion was weakened. A4 final verification ran after merging PR #18's head
`17c48ec`, with Rust 1.99.1 and default test parallelism. It passed formatting,
Clippy, all-target tests, and doc tests. Native Windows NTFS ran 324 tests and
one doc test. Linux ext4 under WSL, uid 1000, ran 310 tests and one doc test. No
tests failed, were ignored, or were filtered. These results do not establish
remote CI status, power-loss behaviour, or release filesystem support.

Phase B started from `main` at `270b889`. Its final verification ran with Rust
1.99.1 and default test parallelism. It passed formatting, Clippy, all-target
tests, and doc tests. Native Windows NTFS ran 339 tests and one doc test. Linux
ext4 under WSL, uid 1000, ran 325 tests and one doc test. No tests failed, were
ignored, or were filtered. Temporary removal of path confinement, the core
tool-text filter, and the CLI filter call were each caught by a test. These
results use a mock LM Studio server. They do not establish live tool-call
compatibility, remote CI status, or release readiness.

A review follow-up then withheld saved tool results without live read
authority, refused token-bearing tool calls, reported the final finish reason,
and gated tool mode by platform. The same checks passed: Windows NTFS ran 345
tests and one doc test, and Linux ext4 under WSL ran 331 tests and one doc test,
with none failed, ignored, or filtered. Forcing replay in the runtime or text
chat, removing the token refusal or its nested-argument decoding, and dropping
the finish reason in the runtime or provider were each caught by a test.

An opt-in live spot-check then ran the debug CLI on Windows against LM Studio 0.4.28 and `google/gemma-4-e4b`
at `http://127.0.0.1:1234/v1`, with no token and disposable data. Text-only chat
with the preference off, a read, and a directory listing succeeded. Revoking
read produced `tool_denied`. Explicit parent, absolute, rooted, and `..` listing
calls each failed as `invalid_tool_call` before file access. Sentinels outside
the workspace never reached the journal. This is one model on one host, not a
provider qualification.

C1 started from PR #20's head `14cf21f`. Its final verification ran with Rust
1.99.1 and default test parallelism. It passed formatting, Clippy, all-target
tests, and doc tests. Native Windows ran 353 tests and one doc test. Linux ext4
under WSL (kernel 6.6, uid 1000) ran 338 tests and one doc test. No tests failed,
were ignored, or were filtered. Five temporary mutations were each caught:
removing the Linux seccomp filter, close-on-exec marking, or kill after a normal
exit, and allowing Windows job breakaway or dropping the handle list. These
results do not establish confinement, remote CI status, or release readiness.
After PR #20's review follow-up was merged in, the same checks passed with 359
Windows tests and 344 Linux tests, each with one doc test, and none failed,
ignored, or filtered.

C2 started from `main` at `6b1ebe4`. Its final verification ran with Rust
1.99.1 and default test parallelism. It passed formatting, Clippy, all-target
tests, and doc tests. Native Windows ran 361 tests and one doc test. Linux ext4
under WSL (kernel 6.6, Landlock ABI 3, uid 1000) ran 364 tests and one doc
test. No tests failed, were ignored, or were filtered. Eight temporary
mutations were each caught: removing `landlock_restrict_self`, allowing
`socket` in `PairsOnly`, allowing `io_uring_setup`, leaving truncation
unhandled, following symbolic links in rule paths, allowing datagram pairs,
allowing non-Unix families in `HostServices`, and accepting special files as
rule paths. These results do not establish confinement on other kernels,
remote CI status, or release readiness.

PR #23's first CI run failed one Windows test, the C1 escape test, which had
also failed on `main` at `6b1ebe4`. Windows reported the job empty before the
killed grandchild's process object was signaled. It did not fail in 200 local
runs or 40 local suite runs. The test now waits up to five seconds for that
object; a surviving grandchild still fails it. Review fixes then qualified the
allowlist claims: Landlock does not restrict metadata changes, and does not
restrict device `ioctl` before ABI 5. Re-verification passed formatting,
Clippy, all-target tests, and doc tests: Windows 362 tests and Linux 365, each
with one doc test, none failed, ignored, or filtered.

C16 started from `97c733e`, the PR #31 head. Review found that tool-enabled
turns replayed the full journal and ignored compaction, and that summaries
could carry saved tool results past a revoked `read`. Both were fixed through
one `replay_active` path and an `includes_tool_results` record field. Final
verification ran with Rust 1.99.1 and default test parallelism. It passed
formatting, Clippy, all-target tests (`--no-fail-fast`), and doc tests. Native
Windows ran 505 tests and one doc test, with `TEMP` and `TMP` in a
workspace-local directory granting the current user full control; without
that grant, 11 platform replacement tests fail with access denied, as recorded
for A4. Linux ext4 under WSL (kernel 6.6, uid 1000) ran 501 tests and one doc
test; five more result lines are the tree-limit tests rerunning themselves in
`systemd-run` scopes. Eight tests were ignored on each host: the opt-in
toolchain acceptance and internet cases. None failed. Three temporary
mutations were each caught: full-journal replay in tool mode, never
withholding a tool-derived summary, and always sending tool results to the
summarizer.

The PR #32 CodeRabbit round fixed six more findings: schema-2 journals again
accept tool manifests; the pre-turn estimate counts input only; the 128-message
and 256 KiB assembly limits trigger compaction; summary chunks are sized from
the summary request's reserve and full fixed prompt; non-reducing summaries
are refused; and live authority is rechecked before each summarizer request
carrying tool-derived content. Re-verification passed formatting, Clippy,
all-target tests, and doc tests: Windows 512 tests and Linux ext4 508, each
with one doc test and eight opt-in tests ignored, none failed. Six temporary
mutations were each caught; the shared summary reserve has no dedicated
test. These results use a mock LM Studio
server. They do not establish live summarizer quality, remote CI status, or
release readiness.

A mock or compile-only result does not establish native containment, live
provider compatibility, or release filesystem support.

## Phase plan

### Phase A: replacement contract, checkpoints, and recovery

The read-only preview is evidence, not an apply operation. Replacement needs a
separate reviewed contract before any overwrite is enabled. Split this phase
at that review boundary:

- **A1 (merged as PR #10):** accepted contract and ADR, platform gates, and
  acceptance cases. It completed contract review, not Phase A.
- **A2:** implement and verify the accepted contract in bounded increments.
  Review platform wrappers and storage compatibility before enabling a trusted
  CLI apply command. An unqualified backend must refuse replacement. Do not
  treat passing existing read/create tests as replacement evidence. The first
  recovery surface is inspection and acknowledgement. Explicit restore was
  left to A4, with its own current-content and checkpoint tests.

  - **A2a (merged as PR #11):** read-only byte-input initialization/ledger validation.
    Preserve recorded uncertainty and test malformed, incomplete, and bounded
    histories. No native storage, writer, CLI, or admission change.
  - **A2b (merged as PR #13):** protected Linux native store and initialization,
    manifest/payload validation, aggregate quota, reserved-capacity durable
    append, existing-only inspection, and fault/reopen tests. Integrate live
    admission with native storage and existing-only missing-state rules.
    Library-only preparation stops unresolved; CLI inspection/acknowledgement
    does not apply an edit. See [ADR-0021](docs/adr/0021-store-checkpoints-with-native-private-handles.md).
  - **A2c (merged as PR #15):** trusted Linux ext4 apply with exact supported
    metadata, durable intent, native publication, and uncertain-outcome recovery.
    Other filesystems and Windows refuse apply. Release filesystem and power-loss
    qualification, Windows ACLs, and restore remain open.
  - **A3a (merged as PR #17):** Windows NTFS private store with exact per-user
    DACLs, as a separate public type the core did not yet use. See
    [ADR-0023](docs/adr/0023-store-windows-checkpoints-with-verified-acls.md).
  - **A3b (merged as PR #18):** Windows capture,
    security-metadata preservation, and retained-handle publication, with the
    store wired to `PrivateDirectory` and the core's refusal lifted. See
    [ADR-0024](docs/adr/0024-publish-one-verified-stage-on-windows-ntfs.md).
    Release qualification and power-loss evidence remain open.
  - **A4 (merged as PR #19):**
    explicit trusted restore through the apply path and a new checkpoint, and
    permanent deletion of one resolved checkpoint. Deletion uses intent and
    outcome ledger records and non-recursive single-entry native removal. It
    keeps history and frees a retained slot. Inspection and acknowledgement
    stay read-only. See
    [ADR-0025](docs/adr/0025-restore-and-delete-checkpoints-explicitly.md).
    Ledger compaction was left to A5; release qualification and power-loss
    evidence remain open.
  - **A5 (branch `codex/checkpoint-ledger-compaction`, not merged):** raise
    the ledger bounds to 65,536 records and 64 MiB and the store quota to
    128 MiB, and compact the ledger in place by trusted command or before an
    operation that would not fit. See
    [ADR-0053](docs/adr/0053-compact-the-checkpoint-ledger-in-place.md).
    The [filesystem qualification plan](docs/filesystem-qualification.md)
    defines the release matrix and power-loss campaign; it has not run.

Contract decisions accepted in A1:

- The expected-content precondition (digest and size) and what happens when the
  target changes before or during the operation.
- Explicitly do not claim atomic compare-and-swap. A hash check followed by a
  rename is not CAS against an independent editor. Keep the limitation visible
  in an ADR and the file-access guide.
- The checkpoint location, digest, retention, and recovery rules. Checkpoints
  must remain outside the workspace and must not overwrite unrelated data.
- Platform replacement primitives and failure modes on Windows and Linux. No
  weaker fallback is allowed.
- Whether the first apply command is trusted CLI only. It must not become a
  model tool before approval, containment, and policy review exist.

The [accepted replacement contract](docs/file-access.md#replacement-contract)
owns the selected limits, state transitions, and recovery rules. The native
primitive and durability gates there remain implementation prerequisites.

Runtime acceptance criteria for A2:

- Replacement requires a live binding, write permission, and the expected
  current digest/size. A mismatch produces a controlled conflict with no
  mutation.
- A checkpoint of the prior bytes is durable before replacement, with exact
  size/digest and bounded storage.
- Intent/outcome audit and uncertain-outcome handling are explicit. No
  automatic retry, repair, or deletion after an ambiguous result.
- Tests cover stale digest, concurrent edit, links/reparse points, hard links,
  oversize input, uncertain sync, checkpoint recovery, and denied policy.
- No model tool route, process execution, deletion, or recursive operation is
  added in this phase.
- Update [docs/file-access.md](docs/file-access.md),
  [docs/security-coding-rules.md](docs/security-coding-rules.md),
  [docs/testing-standards.md](docs/testing-standards.md), the changelog, and a
  new ADR.

### Phase B: provider tool-call integration for read/list

**PR #20, `codex/model-read-tools`.** The user chose a persisted
workspace preference with no per-call prompt, and required that a model call
never read outside the registered workspace. Chat offers the read and list tools
only when the session's workspace enables `model_tools`. The dispatcher rechecks
that preference live before each call. Model paths are refused before the
broker when absolute, rooted, prefixed, or parent-relative; the broker still
refuses links and names outside the workspace. The LM Studio adapter parses tool
calls strictly, and the configured token is removed from tool results before
they are recorded or sent. Earlier tool results are replayed only under live
`model_tools` and `read` preferences, and token-bearing calls are refused. See
[ADR-0026](docs/adr/0026-offer-read-list-tools-through-a-workspace-preference.md).
Live LM Studio tool-call compatibility remains open.

Acceptance criteria:

- Tools are off by default. A real adapter maps provider responses into the
  runtime without trusting provider metadata or role labels.
- Strict JSON parsing rejects malformed, unknown, duplicate, and oversized
  calls before dispatch.
- Every operation rechecks the live binding and policy at the broker boundary.
- Source-labelled tool content stays user-level; the base prompt remains the
  only system message.
- No write, process, network, Git, or MCP tool is enabled by this phase.
- Local-provider fixtures cover text-only, malformed, unknown, duplicate, and
  policy-denied calls. Live compatibility remains a separate qualification.

### Phase C: real execution backends and toolchain profiles

Follow [docs/execution.md](docs/execution.md),
[docs/development-tools.md](docs/development-tools.md), and
[docs/platform-support.md](docs/platform-support.md). Add confined execution
only with explicit toolchain profiles, bounded output, controlled networking,
and observed process-tree cleanup. Never fall back to unrestricted execution.

The user chose Landlock and seccomp on Linux, AppContainer inside a Job Object
on Windows, and trusted CLI commands as the first caller. See
[ADR-0027](docs/adr/0027-confine-execution-with-native-primitives-starting-with-a-process-tree-core.md).

- **C1 (PR #21):** the
  execution contract and a process-tree runner core in `omgcode-platform`.
  It covers bounded output, deadline, cancellation, and observed cleanup. It
  denies Linux process-group escape with seccomp and Windows job breakaway.
  There is no confinement and no caller.
- **C2 (PR #23):** Linux Landlock filesystem
  rules and seccomp network denial. The user chose a per-run Unix-socket mode:
  `PairsOnly` by default, and `HostServices` for developers who need the
  Docker daemon or similar services. See
  [ADR-0028](docs/adr/0028-confine-linux-runs-with-landlock-and-a-socket-filter.md).
- **C3 (PR #24):** Windows AppContainer with
  recorded, reversible workspace access entries and no network capability. The
  user chose persistent grants with explicit revocation, read-execute grants
  for toolchain roots, refusing `HostServices` on Windows for now, and journal
  records with trusted CLI commands. See
  [ADR-0029](docs/adr/0029-confine-windows-runs-in-a-workspace-appcontainer-with-recorded-grants.md).
- **C4 (PR #25 and PR #26):** trusted CLI `run` behind the `execute`
  preference. The user chose:
  - operator-declared toolchain profiles recorded with a revision;
  - a separate bounded run journal that stores no output;
  - a `host_services` preference plus a per-run flag;
  - acceptance workflows for Rust, Python, Node, C/C++, Go, and Java on both
    platforms.

  Java has not run, because no JDK was installed. See
  [ADR-0030](docs/adr/0030-run-trusted-commands-through-recorded-toolchain-profiles.md).
- **C5 (merged as PR #27):** tree-wide limits for every trusted run. The user
  chose:
  - a delegated cgroup v2 on Linux, with OMGCode started in its own
    `systemd-run --user --scope -p Delegate=yes` scope;
  - limits always applied, failing closed where they cannot be;
  - memory, process-count, and CPU-time limits;
  - `run` flags with defaults, recorded in the run journal.

  See
  [ADR-0031](docs/adr/0031-limit-whole-run-trees-with-job-objects-and-delegated-cgroups.md).
- **C6 (merged as PR #29):** outbound network access behind `task_network` and
  `--network`, with resolver and public certificate paths on Linux. See
  [ADR-0032](docs/adr/0032-allow-outbound-network-for-trusted-runs-through-task-network.md).
- **C7 (merged in PR #30):** a sampled net
  disk-growth limit for every trusted run. The user chose a disk limit as the
  next increment. The supervisor measures the writable roots before the run
  and once a second on a separate thread, fails closed when it cannot measure,
  and records the limit and peak growth. See
  [ADR-0033](docs/adr/0033-bound-run-disk-growth-by-sampling-writable-roots.md).
- **C8 (merged in PR #30):** the package-network
  profile contract and implementation gate. The user chose a package-network
  profile, then chose contract-only after the primitive review. No run accepts
  a profile; an advisory proxy is rejected because the tree can bypass it.
  See [ADR-0034](docs/adr/0034-gate-package-networking-behind-enforceable-destination-control.md).
- **C9 (merged in PR #30, version 0.20.0):** operator-declared package-network
  profile records. Profiles are stored through the control journal, with
  revisions and strict origin validation. They are configuration only; no run
  accepts one and no destination filter exists.
- **C10 (merged in PR #30, version 0.21.0):** the trusted allowlisted package
  fetch. The user selected a trusted host-side fetch into a private cache. The
  command contacts only declared origins, refuses redirects and credentials,
  bounds and hashes the body, and audits the intent and outcome. The run
  remains offline and does not read the cache. See
  [ADR-0035](docs/adr/0035-fetch-packages-through-a-trusted-allowlisted-cache.md).
- **C11 (merged in PR #30, version 0.22.0):** read-only Linux run access to the
  workspace package cache. Superseded by C12, which moves exposure to a
  per-workspace root and adds Windows access.
- **C12 (merged in PR #30, version 0.23.0):** per-workspace cache roots and a
  scoped Windows grant. The operator selects a root outside application
  storage and the development root; fetch and run use it; Windows needs a
  read-execute grant on that root. See
  [ADR-0037](docs/adr/0037-use-a-per-workspace-cache-root-and-a-scoped-windows-grant.md).
- **C13 (merged in PR #30, version 0.24.0):** provider-profile records and trusted
  per-call approval records. Profiles store only a name, kind, and
  authentication mode; no credential, endpoint, adapter, or login exists at
  this version (0.31.0 later lets an `lm_studio` profile record an endpoint).
  Approvals are bound, expiring, single-use records; the first consumer is the
  command tool in C14. See
  [ADR-0038](docs/adr/0038-record-provider-profiles-without-credentials.md) and
  [ADR-0039](docs/adr/0039-gate-future-model-tools-with-bound-single-use-approval-records.md).
- **C14 (merged in PR #30, version 0.25.0):** the `workspace_run_command` model
  tool. It is offered only with live model-tools/execute/read/write, records a
  pending approval on the first call, and consumes an approved exact record
  before calling the trusted runner. It uses fixed limits and no network or
  host services. See
  [ADR-0040](docs/adr/0040-gate-the-model-command-tool-with-approvals-and-the-trusted-runner.md).
- **C15 (version 0.25.0 qualification):** native end-to-end
  acceptance for the model command tool. The real chat CLI, a mock provider,
  two turns, an operator approval, and the trusted runner run one successful
  confined command on Linux and Windows. Linux needs a delegated cgroup;
  Windows needs a workspace container grant. The case fails, rather than
  skips, when the host cannot provide them. Confinement correctness remains
  the platform suites' responsibility.

- **C16 (version 0.26.0):** context accounting and compaction. `session
  compact` and `chat --auto-compact --compact-threshold` summarize older
  active messages with the selected local model. A schema-v3
  `context.compacted` event stores a labelled untrusted summary, the boundary,
  digest, model, and optional usage; the original journal is unchanged. Chat
  reports a conservative context meter. Saved tool results reach the
  summarizer, and a summary built from them is replayed, only under live
  `model_tools` and `read`. See
  [ADR-0041](docs/adr/0041-compact-active-context-with-readable-summaries.md).
  Mid-turn compaction, persisted settings, provider-native compaction, and
  caches remain planned.
- **C17 (version 0.28.0):** the `workspace_edit_file` model tool. It is
  offered and dispatched only under live `model_tools`, `read`, and `write`,
  and replaces exactly one occurrence of `old_text` within the 4 KiB argument
  limit and the 1 MiB file bound. Missing, repeated, or unchanged text is a
  recoverable result. The handler reads the live file through the audited
  broker and binds a `file_write` approval to a domain-tagged digest of the
  session, workspace, normalized path, and both current and proposed size and
  SHA-256. The approval decision records the operator's `--writers-stopped`
  acknowledgement, and core refuses to consume a `file_write` record without
  it. An approved identical call consumes the record and calls
  `apply_replacement`; checkpoint conflicts, pending or busy stores, and
  unsupported filesystems are recoverable results. Tool schema 0.3.0;
  0.2.0 sessions still replay. See
  [ADR-0043](docs/adr/0043-gate-the-model-file-edit-tool-with-approvals-and-checkpointed-apply.md).
  Concurrent-writer detection, multi-edit, create, delete, and rename remain
  unimplemented.
- **Run network destination allowlist: deferred (2026-11-16).** No
  unprivileged mechanism passes the ADR-0034 gate on both hosts. The only
  candidate is a Linux user and network namespace with a trusted relay, which
  Windows cannot match and stock Ubuntu 24.04 restricts. The user chose to
  defer it and continue with Phase D. Package downloads keep using the
  trusted `packages fetch` cache with offline runs, or the all-or-nothing
  `task_network` path.

Next decisions, not yet scheduled:

- cloud provider adapters, subscription/API-key login, token refresh, and
  endpoint rules for non-loopback providers, HTTPS, and certificates (one
  adapter, the local LM Studio route, now reads a stored credential, only at
  its profile's loopback endpoint);
- model create, delete, rename, and multi-edit tools, and network, Git, and
  MCP tools;
- registry credentials and development-server networking.

### Phase D: additional providers, streaming, and account lifecycle

Follow [docs/provider-authentication.md](docs/provider-authentication.md).
Keep subscription and API-key profiles separate. Do not silently fall back to
paid API usage. Verify current primary documentation before implementing a
login or entitlement route.

- **D1 (version 0.27.0):** the provider credential service. See
  [ADR-0042](docs/adr/0042-store-provider-credentials-in-protected-os-storage.md).
  `omgcode-platform` stores immutable credentials under random identifiers:
  DPAPI blobs (current user, UI forbidden, entropy bound to the store label
  and identifier) written create-only into `credentials-v1` of the verified-ACL
  private store on Windows, and one Secret Service item per credential in the
  default collection on Linux. A locked collection is reported, never
  unlocked. Secrets are held in a zeroed, redacted, 16 KiB-bounded buffer.
  The control journal records `provider_credential_bound` and
  `provider_credential_removed` with an opaque identifier and backend, and a
  bound profile's removal names its credential. Trusted CLI
  `provider-credential-set` reads standard input only and orders native store,
  journal binding, then old-item deletion, reporting an undeletable item as a
  `credential_orphaned` warning. Only `api_key` profiles accept a credential.
  Native tests run on both hosts; Linux needs an unlocked Secret Service, which
  CI provides with gnome-keyring. No adapter reads a stored credential.
- **D2 (version 0.30.0):** the first stored-credential read. See
  [ADR-0047](docs/adr/0047-read-the-local-provider-token-from-the-credential-store.md).
  `omgcode chat --provider-profile <name>`, `session compact
  --provider-profile <name>`, and `omgcode-desktop --provider-profile <name>`
  (fixed at launch) make the LM Studio adapter read its token from the
  profile's stored credential and ignore `OMGCODE_LM_TOKEN`. Core's
  `provider_credential_binding` takes no credential store and refuses an
  unknown profile, another provider kind, a `subscription` profile, and an
  unbound `api_key` profile; a `none` profile gives no token there. The native
  store is opened only after it returns a binding, and
  `load_provider_credential` refuses a binding from another backend before it
  reads a secret. Every failure ends the turn with a `credential` error and a
  cause code, with no fallback to no token or to another token. The secret is loaded
  while the settings store is held and the store is released before any
  request. The desktop resolves it again each turn and checks it once at
  launch. A read records nothing. The token is held in a zeroing buffer; the
  provider clients' header copies are not zeroed. `capabilities` reports
  `stored_credential_adapters: ["lm_studio"]`; `provider_adapters` stays
  `false`.
- **D3 (version 0.31.0):** the stored credential is bound to its profile's
  endpoint. See
  [ADR-0048](docs/adr/0048-bind-a-stored-credential-to-its-profile-endpoint.md).
  `provider-profile-add --endpoint <url>` records one endpoint on an
  `lm_studio` profile. It must pass the rule the adapter enforces on every
  request (plain `http`, a literal loopback IP, a non-zero port, `/v1`, no
  user information, query, or fragment, at most 256 bytes), through one core
  implementation that the record and replay also use. Every other kind refuses
  an endpoint. The endpoint is fixed for the profile's life: to change it,
  remove the profile (which deletes its credential) and add a new one.
  `provider_credential_binding` returns the recorded endpoint with the
  credential decision, and a selected profile with no endpoint fails with
  `endpoint_not_recorded` before any native store access. `--endpoint` and
  `--provider-profile` are mutually exclusive in `chat`, `session compact`, and
  `omgcode-desktop`. The desktop fixes the profile's endpoint at launch and
  refuses a turn with `endpoint_changed` if a re-added profile records another.
  The profile record's `endpoint` field is written only when present, so a
  journal without endpoints keeps its bytes; a journal with one is not
  readable by 0.30.0 or earlier. A profile recorded before 0.31.0 must be
  removed and added again. `capabilities` reports
  `provider_profile_endpoints: true`.
- **D4 (version 0.32.0):** the local adapter can stream. See
  [ADR-0049](docs/adr/0049-stream-local-model-replies-and-push-chat-events.md).
  `LocalModelClient::complete_streaming` and `complete_with_tools_streaming`
  send the same body as the non-streaming methods with `stream: true` and read
  server-sent events under strict rules (success status and content type, the
  existing 1 MiB and thirty-second bounds over the whole stream, no repair,
  `[DONE]` required or `IncompleteStream`, duplicate-key refusal, a non-null
  `error` refused, tool calls assembled by `index`). `stream_options` is never
  sent. The non-streaming methods stay the default. `ChatHooks::on_stream`
  makes a turn stream and receives `RequestStarted` and `Text` events;
  compaction requests never stream. `StreamRedactor` removes the known token
  across piece boundaries, and the final reply is still redacted as a whole.
  The journals record only the validated reply. `omgcode chat --stream`
  writes the provisional text to standard error through `DisplaySafeStream`.
  `capabilities` reports `streaming_adapters: ["lm_studio"]`. The stream rules
  were checked against local fixtures only.
- **D5 (version 0.33.0):** a cancelled streamed request is interrupted. See
  [ADR-0050](docs/adr/0050-interrupt-a-streamed-model-request-on-cancellation.md).
  `complete_streaming` and `complete_with_tools_streaming` take a
  `CancelToken`. The streamed request runs on an async client inside a
  single-thread runtime created for that request, with the blocking client's
  settings and the same thirty-second deadline. The token is checked before the
  request, while waiting for the response head, and while waiting for each
  piece of the body, every 25 ms. A set token drops the request and returns
  `ProviderError::Cancelled`. `ToolModelRequest` carries the token and
  `ModelError::Cancelled` ends the turn with the failure reason `cancelled`
  when the token is set. A streamed text-only turn now has a cancel token. The
  application error kind `cancelled` replaces `turn` for a cancelled
  tool-enabled turn. `chat_cancel` stops a streamed request, the window shows
  "Turn cancelled.", and exit leaves no open turn for a streamed turn. No
  journal record changes shape. A non-streamed request, the compaction request,
  and a tool call in progress are not interrupted, and the CLI has no cancel
  source. The cancel path was checked against local fixtures only.
- **D6 (version 0.34.0):** every model request can be cancelled, and an
  interrupt cancels a CLI turn. See
  [ADR-0051](docs/adr/0051-cancel-every-model-request-and-interrupt-cli-turns.md).
  `complete` and `complete_with_tools` take a `CancelToken`; all four adapter
  methods share the async transport of D5, and reqwest's `blocking` feature is
  removed. A non-streamed body read in part is discarded. Every chat turn
  creates one token before the compaction check and hands it to
  `ChatHooks::on_cancel_token` on every path; `ChatHooks::cancel` lets a front
  end supply it and `TurnRuntime::with_cancel_token` passes it to the runtime.
  A token set before the turn starts records nothing. `compact_journal` takes
  a token, and a cancelled compaction records nothing. `omgcode chat` and
  `session compact` install the platform interrupt handler through the new
  `watch_interrupt`; the command fails with the code `cancelled`, and a
  started chat turn is recorded as cancelled. A handler that cannot be
  installed fails the command with the code `interrupt`. `capabilities`
  reports `cancellable_adapters: ["lm_studio"]`. The desktop stops a
  non-streamed turn and a pre-turn compaction with no command change. Checked
  against local fixtures; the CLI interrupt was delivered on Linux only.

Still open in Phase D: cloud provider adapters, subscription and API-key
login, token refresh, account lifecycle, a memory-only session credential,
endpoint rules for non-loopback providers, HTTPS, and certificates,
qualification of the stream rules and of cancellation against a live LM Studio
server, an interrupt delivered to the CLI on Windows, interrupting a read,
list, or edit tool call in progress, and a longer deadline for long
generations (the thirty-second deadline covers the whole response). The frontend shows nothing about the credential source beyond the
endpoint text it already displayed.

### Phase E: desktop interface and session tree

The Rust core remains the authority. The UI must not receive raw journal paths,
credentials, or unrestricted host capabilities. Follow the session, permission,
context, and security contracts. Follow the
[desktop interface guide](docs/desktop-interface.md).

Version 0.29.0 built the prototype on branch `codex/desktop-interface`. It
merged to `main` in PR #33 with Phases D2 to D6.

- **E1 (ADR-0044):** lock-free journal inspection. `SessionInspection::inspect`
  reads one journal with no lock call, no link following, and the writer's
  validator. Windows lock and sharing violations become `Busy`. Bytes after the
  last line feed are reported as `incomplete_tail`. It adds times and a bounded
  transcript entry list. `session::discover` walks only the dated layout and
  never opens a journal or follows a link. Limits: 50,000 journals and 500,000
  directory entries.
- **E2 (ADR-0044):** a derived SQLite index in
  `<data_root>/session-index-v1/index.sqlite3` through `rusqlite` 0.43.0 with
  the bundled SQLite and FTS5. Incremental, bounded refresh; a busy journal
  keeps its last metadata; an invalid journal is listed with a code. A
  missing, corrupt, or foreign index is deleted and rebuilt.
- **E3 (ADR-0045):** the `omgcode-app` library. The chat turn, compaction,
  session creation, the data-root resolver, and token redaction moved out of
  the CLI, which is now a thin adapter. `DesktopService` is the
  front-end-neutral command service, including review tokens. Core adds
  `review_approval` and display-safe text. No CLI behaviour changed.
- **E4 (ADR-0045, ADR-0046):** the Tauri shell `omgcode-desktop`: two windows,
  18 commands (19 since E6), a capability file per window, the Rust window
  policy, the content
  policy, the navigation guard, and a bounded shutdown.
- **E5 (ADR-0045, ADR-0046):** the strict TypeScript frontend with Vite,
  Vitest, and Biome: the session tree, search, transcript, composer, and the
  separate approvals bundle.
- **E6 (ADR-0049):** the command `chat_subscribe` (`main` only; 19 commands)
  takes a caller-created Tauri IPC channel and needs no `core:` permission.
  The service keeps one subscriber, replays the preview of every running turn
  to a new one, and pushes `request_started`, `delta` (at most 1,024 bytes),
  `preview_truncated` (16 KiB preview per request), and `finished`. A guard
  sends `finished` after the result is recorded and records a panicked turn as
  failed first. `omgcode-desktop --stream <on|off>` defaults to `on`. The
  frontend subscribes once, reads `chat_status` on `finished`, keeps a
  five-second reconcile check, falls back to the 750 ms poll if subscribing
  fails, and shows one live entry "Assistant (generating, not saved yet)" with
  the preview. The approvals polls are unchanged.
- **D5 (ADR-0050):** `chat_cancel` interrupts a streamed turn, the main window
  shows the notice "Turn cancelled." decided by the error code `cancelled`, and
  exit cancels every running turn. No command or wire type changes.
- **D6 (ADR-0051):** `chat_cancel` and exit also stop a non-streamed turn
  (`--stream off`) and a pre-turn compaction. No command or wire type changes.

Still open for the desktop:

- opening the shell's windows on Linux. It builds and its tests pass there,
  including the Unix-only inspection and index branches, but its webview, IPC,
  capability enforcement, and navigation guard have not run on Linux;
- development panels (terminal tabs, task logs, and diffs), a settings
  interface, and persisted chat settings;
- push for approvals and the session tree, which still poll or refresh on
  request, and a run of the event channel on Linux, where no window has been
  opened;
- a recovery interface for an incomplete tail or a damaged journal, plus
  `session.archive_requested` stop-and-archive and storage-use display;
- Git, MCP, skills, and memory interfaces;
- a native confirmation dialog for approvals, which would remove the webview
  from the final decision;
- signing and installers (Phase G); the unsigned testing archives of
  ADR-0052 are the only packaging;
- automatic repair of a corrupt index behind a valid header, and a protection
  against two versions sharing one data root and rebuilding each other's index.

### Phase F: runtime skills, MCP, memories, compaction, and caches

Follow [docs/skills.md](docs/skills.md), [docs/mcp.md](docs/mcp.md),
[docs/memories.md](docs/memories.md), and
[docs/context-management.md](docs/context-management.md). Imported content is
untrusted; enablement is a trusted user action; stale derived context must be
invalidated.

Version 0.26.0 implements the first compaction increment: conservative
accounting, a visible meter, manual `session compact`, and pre-turn
auto-compaction for the local LM Studio route. Mid-turn compaction after tool
results, persisted settings, provider-native compaction, caches, skills, MCP,
and memories remain open.

### Phase G: Git, platform qualification, packaging, and release

Mediated Git workflows need controlled credentials, hooks/helpers, network
policy, and push approval. Platform qualification, security testing, and
release readiness remain separate gates.

PR #36 added the first packaging step: a Testing build workflow publishes
unsigned Windows, Ubuntu, and Fedora 44 archives of each push to `main` as the
rolling `latest` prerelease, with no installer
([ADR-0052](docs/adr/0052-publish-rolling-testing-builds-from-main.md)). The
workspace tests do not run on Fedora. The workflow had not completed a run
when this plan was last verified. Signing, installers, Git workflows,
platform qualification, and release readiness remain open.

## Known cautions

- Build `crates/omgcode-desktop/ui/dist` before any Cargo command that includes
  `omgcode-desktop`. Without it the build fails with `The frontendDist
  configuration is set to "ui/dist" but this path doesn't exist`.
- Never run `tauri dev` or the shell without `--data-dir <scratch>`. Without it
  the shell uses the real per-user data directory.
- Two tests print `SKIPPED link assertions` and return on a host that cannot
  create file symbolic links (error 1314): `a_symlinked_journal_is_rejected`
  and `a_database_file_that_is_a_link_is_refused`. They did not run on the
  Phase E Windows host; they ran in the Linux run. A privileged Windows run
  has not executed them.
- A Linux login keyring can exist and still be locked, which fails every
  credential suite with "an unlocked Secret Service default collection is
  required". Check the collection's `Locked` property before a Linux run.
- A test fixture that creates its own data root must make it mode 0700 on
  Unix. The checkpoint store refuses the default mode.
- A data root under the user's Temp directory inherits an ACL that the private
  store refuses. Reset it before a manual run:
  `icacls <dir> /inheritance:r /grant:r *<SID>:(OI)(CI)F`.
- The session index file carries a version. A binary with a different index
  version deletes and rebuilds the index, so two versions sharing a data root
  rebuild each other's index. Rebuild also fails on Windows while another
  process has the database open.
- `.claude/` is untracked local tooling state. Do not commit it.
- Linux CLI integration tests raced on inherited `flock` descriptors, producing
  `Busy` and mock-provider timeouts. The spawn gate in
  [testing standards](docs/testing-standards.md#isolation-and-concurrency) covers
  the CLI suites. `omgcode-core`'s `file_broker` FIFO test still spawns one child
  beside in-process stores; it did not fail in 60 parallel runs and is not gated.
- Windows runs under host load can outlast the default 10-second CLI test
  deadline. The disk-limit case now gives the run an explicit 45-second
  deadline and the CLI 60 seconds, so a missed sampler ends as a recorded
  deadline instead of a killed CLI with a pending run. The pre-existing
  multi-fixture provider tests also timed out locally under host load; they
  pass individually and passed CI on `987a5aa`. Recheck timeouts on a clean
  host rather than weakening an assertion. A test that fails only under load
  may be measuring the host: `concurrent_runs_complete_and_are_recorded` spun
  a fixed loop and was changed to a file rendezvous. To tell load from a
  regression, run the failing test alone on a clean checkout of the previous
  commit, and time its workload outside OMGCode.
- `.serena/` is untracked and unrelated. Do not commit or delete it without
  deciding what it is.
- The turn runtime is library/test infrastructure until a provider adapter and
  an explicit tool-dispatch decision exist.
- The edit preview is a snapshot, not a lock or an apply token. A later
  replacement must recheck content identity.
- Native containment, Windows ACL hardening, directory power-loss durability,
  and release filesystem support remain unqualified. The Windows directory flush
  has no Microsoft durability contract; never describe it as power-loss safe.
