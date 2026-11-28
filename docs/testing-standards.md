# Testing standards

Use these standards with the [coding rules](coding-rules.md) and
[security rules](security-coding-rules.md).

## Current state

The pinned Cargo workspace has five crates: `omgcode-core`,
`omgcode-cli`, `omgcode-platform`, `omgcode-app`, and `omgcode-desktop`. The
desktop crate has a TypeScript frontend with its own tests (see
[Desktop shell and frontend](#desktop-shell-and-frontend)). It includes deterministic core,
CLI, and platform tests. The core includes configuration-only workspace records,
permission preferences, and a local settings/audit journal. Directory identity
checks require a native file ID and directory creation time. On Linux, capture
fails when the filesystem does not provide birth-time metadata. Windows reads a
128-bit file ID from an open handle. Identity records alone are configuration
metadata. The v0.3.0 core also authorizes a bounded native file broker, exposed
only through trusted CLI commands. Broker operations add live handle checks and
use a separate file-operation journal. The v0.4.0 core adds a session-owned
turn/tool runtime and schema-version-2 journals. That runtime is exercised with
a scripted model and the real control store, broker, and temporary workspace.
Since 0.12.0, CLI chat tests also drive it through the LM Studio adapter against
a local mock server. The v0.5.0 CLI adds a
read-only edit preview that compares a broker-read file with trusted proposed
content and never writes or checkpoints. These features do not provide an
execution sandbox. The 0.13.0 platform crate adds an unexposed process-tree
runner core; its tests re-execute the unit-test binary as the child program.
The 0.14.0 Linux confinement tests run that child under Landlock and seccomp
and record each probe's errno: Landlock denials report `EACCES`, and socket
denials report `EPERM`. A classic-BPF interpreter in the unit tests evaluates
every filter branch. These tests ran on WSL 6.6 with Landlock ABI 3; they do not
qualify other kernels. The 0.15.0 Windows tests copy the test binary into a
granted directory, run it in a fresh AppContainer, and record each probe as
`ok`, `denied`, or the OS error. The same probes run unconfined as a positive
control. Grant tests change real ACLs on temporary NTFS or ReFS directories, and each
test container deletes its profile when dropped, so runs leave no per-user
AppContainer profiles. These tests ran on Windows 11 build 26300. Since
0.70.0 (ADR-0107) registering a workspace on Windows grants its folder, and
assigning a profile grants its roots, so the CLI and desktop service tests that
register, assign, or add with a workspace also change real ACLs on temporary
folders; each test revokes every grant it recorded before it ends, which also
deletes the workspace's AppContainer profile. Tests that are not about these
changes record workspaces and assignments through the core primitives, which
grant nothing.

The 0.16.0 `run_workflow` tests drive `omgcode run` through the real CLI with
system tools. Since 0.17.0, on Linux they start the CLI through
`systemd-run --user --scope --expand-environment=no -p Delegate=yes`, and the
platform tree-limit tests rerun themselves the same way. The switch keeps `$`
in test commands literal on systemd 258 and later, and
`dollar_signs_reach_the_program_verbatim` checks it. These tests need a user
service manager and fail
without one; they must not be skipped. Each limit test must show that the
limit ends the whole run as `LimitExceeded`. Since 0.19.0 the disk test writes
past a 16 MiB limit and expects the `disk` result; the platform tests also
exercise the sampler directly, including, on Unix, a link that must not be
followed and a ceiling reached during a measured scan. Windows scenes grant
the workspace and revoke every grant when dropped.

The `claude_code_process` tests run in a supervised copy of their binary,
on Linux in its own delegated scope, with every temporary folder inside one
folder of the supervising process. A test whose turn does not end even
after its cancellation ends that copy, and no drop of the copy runs. The
supervisor then stops what the copy left running: the copy's scope on
Linux, where the copy's exit ends the fake and, through its closed pipes,
the relay but no descendant that ignores them, while on Windows each tree's
job ends with the copy. It waits until every fake's, relay's, and linger's
alive lock is free and names the process that held it, on Windows revokes
each connection left behind, which deletes its AppContainer profile (Linux
has no grant or container profile to remove), and removes the folder. A
folder whose scope could not be stopped, whose processes were not seen to
end, or whose connection could not be revoked is kept and named, so nothing
a retry or the operator needs is removed before the cleanup was confirmed.
A copy still running after 20 minutes, as one whose test never returns
would be, is ended and reaped before that cleanup, and fails the run; one
that cannot be ended leaves its folder kept and named. Each `systemctl`
command of the scope's stop is bounded to two minutes, and one that runs
longer fails the stop. On Windows a scene whose connection cannot be
revoked when it is dropped keeps its folders for the supervisor's retry.
`a_supervisor_stops_and_cleans_up_after_an_abandoned_turn` makes a copy end
while its turn's fake, relay, and a linger run and checks that path;
`a_supervisor_keeps_the_folder_when_the_scope_stop_fails`,
`a_supervisor_keeps_the_folder_when_a_revocation_fails`, and
`a_supervisor_keeps_the_folder_when_an_alive_file_names_no_holder` inject
each failure and check that the folder is kept and named;
`a_supervisor_ends_a_copy_that_runs_past_its_deadline` checks the copy's
deadline; and, on Windows, `a_scene_whose_revocation_fails_keeps_its_folders`
checks the kept scene. The
linger is a descendant of the fake that ignores its closed pipes and ends
itself only after five minutes, so on Linux only the scope's stop ends it.

The `toolchain_acceptance` tests run real toolchains. They are
`#[ignore]` because they need host toolchains named by `OMGCODE_ACCEPT_*`
variables. Run them explicitly with `--ignored` on both platforms and report
each language that did not run. A missing variable fails the case rather than
passing it. Since 0.27.0 the credential tests use the real native store:
DPAPI files in a temporary private NTFS or ReFS root on Windows, and the Secret Service
default collection on the session bus on Linux. On Linux they need a running,
unlocked Secret Service and fail without one; they must not be skipped or
mocked. Only the Linux prompt-refusal tests use a mock Secret Service over a
socket pair, because a real service cannot be made to answer with a prompt on
demand. CI installs gnome-keyring and unlocks a login collection with one empty
password line in the existing user session bus. The native store belongs to
the user's account, so a test against it must delete only the items it
created, including after a failed assertion. Tests of stored-credential use
(0.30.0, [ADR-0047](adr/0047-read-the-local-provider-token-from-the-credential-store.md))
assert the `Authorization` header that a mock provider received: the stored
token is the bearer, and a different `OMGCODE_LM_TOKEN` value is ignored. The
endpoint tests (0.31.0, [ADR-0048](adr/0048-bind-a-stored-credential-to-its-profile-endpoint.md))
use two mock providers: the token must reach the profile's endpoint, and the
test asserts that the second provider received nothing. They also assert that a
control journal with a profile that has no endpoint replays and serializes
with its exact bytes. See [workspace permissions](workspace-permissions.md)
and the [workspace file-access contract](file-access.md) for their scope and
current limits. [Repository review and analysis](repository-checks.md) have
separate workflows.

The directory-record unit tests cover native path encoding, record JSON shape,
stability after child writes, same-path replacement, and ancestor/sibling
observations. They also cover missing creation-time metadata and reject opening
a FIFO as a directory. Control-store tests cover strict journal replay, stale
revision rejection without a write, workspace ID reuse and replacement, moved
or stale directory rejection, bindings, audit records, and lock contention.
CLI workflow tests bound subprocess waits and captured stream reads, and assert
the exact number of durable `policy_checked` events in the tested workflow.
They exercise configuration commands and confirm that policy checks remain
configuration-only. File broker tests cover bounded reads, listings, exclusive
creation, scope denials, path/link checks, collisions, audit privacy, and
uncertain-create recovery. Edit-preview unit and CLI tests cover exact hunk
rendering, unchanged content, CRLF and final-newline preservation, line-count
and diff bounds, binary and oversized refusal, proposed-source link rejection,
permission denial, and absence of workspace mutation. Turn-runtime tests cover allowed read/list through a
fresh live binding, policy denial at the broker, revocation during model I/O,
unknown and malformed calls, duplicate identifiers, per-round, aggregate,
prompt-capacity, and selected per-message limits, cancellation, deadline expiry,
archived and active-turn refusal, workspace metadata mismatch, model-addressable
listing names, interim assistant text retention, source-labelled hostile
content, non-UTF-8 withholding, and schema-v1/v2/v3 replay including
compaction, tool output-identity checks, and failed-turn call-ID reuse. Public integration tests exercise these operations
through the broker API; core unit tests inject the uncertain outcome. These
tests do not qualify release filesystem support or runtime containment.

Use the locked Cargo commands below. They are also configured in
`.github/workflows/rust.yml` for Ubuntu 24.04 and Windows Server 2025. A CI
matrix or passing test suite does not establish release support or sandbox
containment.

The `filesystems` CI job
([ADR-0069](adr/0069-allow-refs-btrfs-xfs-tmpfs-and-zfs-for-trusted-apply.md))
reruns the platform, core, and CLI file-access suites with the temporary
directory on another filesystem. It mounts ReFS on Windows Server 2025 and
btrfs, XFS, tmpfs, and ZFS on Ubuntu 24.04, and sets `TEMP` and `TMP` or
`TMPDIR` to that mount. Before the suites it runs the ignored guard test
`the_temporary_directory_is_on_the_expected_filesystem` with `--ignored`. The
guard reads `OMGCODE_TEST_EXPECT_FS`, fails when the variable is missing, and
fails unless the temporary directory is on the named filesystem. The job also
fails unless exactly one guard test passed. These runs show functional
behaviour only and make no power-loss claim. The btrfs, XFS, and ZFS cells have
not run on a developer host, because creating those mounts needs root.

Documentation-only changes need documentation checks. Do not report application
tests for documentation-only work.

## Behaviour and regression tests

- Use native Rust `#[test]` functions and, when an async runtime is selected,
  its supported test attribute. A separate BDD framework is not required.
- Name each test for an observable behaviour and its relevant condition.
  Arrange setup, action, and assertions in Given/When/Then order. Short comments
  can label phases; Rust does not require Catch2-style macros.
- Check exact outputs, errors, side effects, and persistent state. Include
  boundary and failure cases. Do not test only a private helper's chosen shape.
- For a regression, add the failing behaviour test before the production fix.
  Observe its failure on the unfixed baseline when safe and practical. Record
  a limitation if the failure cannot be reproduced; do not weaken the assertion.
- Put isolated unit tests beside the module, normally in `#[cfg(test)] mod tests`.
  Put cross-component or executable tests in the owning crate's `tests/`
  directory. Keep reusable support separate from test entry points.
- Use a real library target when integration tests need a reusable core.
  Do not make internal functions public merely to test implementation details.
- Executable documentation examples must compile and run as doc tests when
  applicable. Mark genuinely illustrative pseudocode clearly.

## Isolation and concurrency

- Give tests private temporary directories, repositories, databases, ports, and
  bounded resources. Do not use real home folders or persistent credentials.
- Avoid process-global environment or working-directory mutation in parallel
  tests. Pass explicit configuration or isolate the case in a subprocess.
- Use barriers, channels, deterministic clocks, and injected failures for
  concurrency. Arbitrary sleeps are not evidence of ordering.
- A fixed amount of work is not a reliable delay. Its duration depends on the
  host, so a loop that takes two seconds on a quiet machine can outlast a
  wall-clock limit on a loaded one. When a test needs two processes to
  overlap, make each wait for a signal from the other, such as a file the
  other writes, with a bounded wait and a failing exit when the signal never
  comes. A pass then proves the overlap. A fixture that must reach a state
  before the test acts signals that state; the test does not sleep and hope.
- A Unix child can inherit a test-held `flock` until `exec`, so the lock can
  outlive the test's close. CLI integration tests must open lock-holding stores
  inside `without_child_spawns` and start children through the spawn gate in
  `crates/omgcode-cli/tests/support/spawn_gate.rs`. Do not retry on `Busy`, which
  would hide real contention. Serial runs cannot detect this race, so run the
  default parallel suite as well.
- A loopback port is private only while a socket holds it. A port that a test
  binds and then releases can be bound by a concurrently started test's mock
  server, which then receives a request meant to fail and loses its own
  scripted reply. Keep a scripted server's listener bound until its test
  finishes, closing connections past the script at once, and give an endpoint
  that must fail a listener that closes every connection rather than a
  released port. The helpers are `refuse_until_stopped` and `unused_endpoint`
  in the CLI and desktop test support. The core provider tests' one-shot
  fixtures keep each port bound until the test binary exits, through
  `bind_fixture_listener`, so no later fixture in that binary receives a port
  whose earlier connection is still in TIME_WAIT.
- On Windows, a mock server that closes first after a large request can delay
  the next connect to it until the connect timeout (ADR-0078). A mock that
  serves the model provider client must read until the client closes, with
  `await_client_close`, so the client holds the TIME_WAIT and its reused-port
  skip applies. The helper reports whether the client closed, orderly or
  abortively, and the shared scripted mocks fail when it did not, so a
  regression of the close order is a test failure rather than a returning
  flake. Do not use that order for a client without the skip.
- CLI compaction tests seed large, private journal fixtures in one batch and
  let the child replay them. This keeps hundreds of fixture syncs from holding
  the spawn gate while unrelated tests are trying to start.
- Test cancellation before start, during work, during partial writes, and during
  shutdown. Observe task completion, resource release, and descendant cleanup.
- Bound every waiting integration test. A timeout is a failure, not a skip.
- Cross-compilation and mocks cannot establish operating-system containment.
  Run actual filesystem, process-tree, IPC, and network tests on both Windows
  and Linux before claiming those properties. Workspace identity tests are
  configuration checks only. The native broker adds bounded filesystem
  operations but does not qualify release filesystem support or runtime
  containment. The [workspace permission contract](workspace-permissions.md)
  and [file-access contract](file-access.md) define their scopes and remaining
  platform checks.

## Required boundary coverage

As the relevant components are implemented, include:

| Area | Required cases |
| --- | --- |
| Permissions | Allowed/denied requests, stale grants, scope mismatch, revocation; workspace registration and directory replacement; a run withdrawn by any recorded settings state still stops when access is restored before its watcher reads the settings (ADR-0061) |
| Workspace identity | Native paths, stable directory IDs after child writes, ancestor/sibling checks, missing creation-time metadata failure |
| Files | Bounded read/list/create/mkdir, policy scope, traversal, native paths, symlinks/junctions, hard links, special files, collisions, uncertain creation, audit privacy, path-swap races, and read-only edit-preview bounds/no-mutation |
| Processes | Descendant escape attempts, limits, cleanup, startup failure |
| Disk limits | Sampled growth ends the whole run as `disk`, baseline excludes prior content, links and reparse points are not followed, the ceiling stops a scan early, unreadable or oversized roots fail closed |
| Networking | Disabled egress, host-local access, redirects, provider routing |
| Model web fetch | URL refusal before any approval (scheme, port, user info, IP literal, local name, length); every resolved address checked and the connection pinned to it; same-host redirects re-checked and capped, cross-host redirects reported and not followed; status, content-type, encoding, body-size, deadline, and cancellation handling; HTML hiding and character stripping; the `prompt` and `allow` gates with and without an allowlisted host, approval staleness after a host or preference change, and digest reproduction in review; no settings lock held during the request; result labels and byte bounds; the `network` journal target and `after_web_content` before and after a session's first fetched content, across turns and never in another session. Client tests use a loopback server under a test-only policy and never fetch a real site ([ADR-0098](adr/0098-add-a-model-web-fetch-tool-behind-a-host-allowlist.md)) |
| Package network | Profile storage and the trusted fetch: recording, replay, revision, origin normalization, duplicate and invalid-origin refusal, removal, task-network gating, undeclared-origin and path refusal before a request, redirect refusal, size limits, cache-name validation, existing-cache refusal, remaining-time send and body reads under a trickled response, atomic no-replace publication under a concurrent collision, private cache content, and start/finish audit without the URL text. Cache roots cover per-workspace set/clear/replay, overlap refusals, grant revocation when a root changes or clears, stale foreign-grant refusal, refusal of a root another workspace can already read, the configured fetch location, Linux read/write/parent denial, Windows access with a scoped grant, and Windows refusal without one or without a selected root. No run accepts a profile. Registry credentials cover: bind, rotate, unbind, profile-removal unbinding, and replay with no secret in the journal; refusal of an unknown profile or origin, a stale revision, a credential identifier bound twice (including across a provider binding), and tampered replay records; earlier journals without the new fields; secret validation (control characters, whitespace, `basic` without a user and colon, oversize) before any native write; the shared store, uncertain-write, and refusal-before-write ordering; and, against a loopback fixture with the real native store, the exact bearer and basic `Authorization` header in the raw request head, no header for an unbound origin or a narrower nested origin, no request at all when the bound secret cannot be read, a redirect from a credentialed origin leaving its target untouched, the start record carrying `credential_id` only when used, and no secret in errors, audit, reports, CLI output, or any data-root file. No run test is added: a run never receives the credential, so the run suites' curated-environment test covers it ([ADR-0056](adr/0056-send-registry-credentials-to-one-declared-origin.md)). A future run-side mechanism must add allowed-origin success, undeclared-origin refusal, redirect and credential refusal, offline fallback, and platform refusal ([ADR-0034](adr/0034-gate-package-networking-behind-enforceable-destination-control.md), [ADR-0035](adr/0035-fetch-packages-through-a-trusted-allowlisted-cache.md), [ADR-0037](adr/0037-use-a-per-workspace-cache-root-and-a-scoped-windows-grant.md)) |
| Providers | Partial/malformed streams, duplicate calls, interruption, retries |
| Provider profiles | Kind/authentication pairing, duplicate and invalid names, case-insensitive lookup, revisioned add/remove, replay, no credential or endpoint storage, and capability reporting. Hosted adapters and the ChatGPT sign-in run against local mocks; no hosted API key was used, and one real ChatGPT sign-in is recorded in [ADR-0096](adr/0096-accept-a-plan-route-stream-without-a-content-type.md) |
| Provider credentials | Identifier format refused before I/O; empty, oversized, and line-ending input; `Debug` redaction; native round trip, create-only refusal, not-found, and delete on both hosts; on Windows, edited, truncated, and identifier-swapped blobs as corrupt and broadened or unprotected DACLs refused; on Linux, exact item attributes and label and duplicate items as corrupt; revisioned bind/unbind and replay; refusal for subscription and `none` profiles before any native store; rotation deleting the old item; a binding refused before any journal write deleting the new item; a binding whose journal sync failed keeping and naming the new item, with the binding on disk naming a present item; store-failure cleanup classification, cleanup of a left item, and an identifier kept when cleanup fails; on Windows, an injected directory-sync failure after a complete write removing the file, and with a failed removal as well, both failures reported and the file kept loadable and deletable; the secret read before the control store opens; on Linux, a pending placeholder loading as corrupt, a mock service's create and delete prompts refused without `Prompt` being called, and a create reply carrying both an item and a prompt refused with its placeholder deleted; profile removal naming its credential; and no secret in stdout, stderr, the control journal, or any file under the data root. A locked Linux collection is not exercised, because unlocking it again needs a prompt ([ADR-0042](adr/0042-store-provider-credentials-in-protected-os-storage.md)) |
| Approvals | Request validation, pending/list/show, approve/deny, single-use consume, expiry, binding mismatch, settings-revision invalidation, replay, unknown IDs, and limits; a `file_write` approval refused without `--writers-stopped`, the flag refused for other operations, the acknowledgement reported and replayed, an earlier journal's unacknowledged `file_write` record replayed but refused at consumption, and a misplaced acknowledgement refused at replay. No operation executes |
| Authentication | Callback binding, consent, subscription entitlement, refresh, revocation, account/billing isolation |
| Toolchains | Native compiler/SDK dependencies, interpreters, formatter/build/test flows, caches, helper processes, denied host access |
| Git | Hostile config/hooks/helpers, metadata paths, changed push approvals |
| Persistence | Partial operations, migration failure, recovery without replay |
| Frontend | Untrusted rendering, IPC authorization, no secret disclosure |
| Sessions | Concurrent edits/worktrees, JSONL naming/schema/durability, index rebuild, archive/restore, safe resume, v1/v2 boundaries |
| Turn/tool lifecycle | Fresh bindings, strict schemas, unknown/malformed/duplicate calls, round/call/byte limits, cancellation, deadline, source labels, denial at the broker, no replay |
| Context compaction | Conservative accounting, effective trigger below/at/above, manual and pre-turn auto-compaction, schema-v3 event, summary trust label, latest-turn and tool-pair retention, chunk and depth limits, chunks that fit the full summarizer request, input-only estimates, the history-message limit, no-progress refusal for non-reducing summaries, token redaction, and original-journal retention. Saved tool results are withheld from the summarizer when either `model_tools` or `read` is disabled, a derived summary is withheld after either is revoked, and a withdrawal between summarizer requests stops before the next one |
| Process-tree runner | Exit codes, exact environment, argument quoting, bounded output, deadline, cancellation, descendant escape attempts, inherited handles, observed cleanup, cancellable disk sampling, bounded traversal queues, real scan errors preserved during shutdown, invalid specifications |
| Linux confinement | Read-only and read-write rules, path truncation, denied host files and directories, inheritance by descendants, IP and `io_uring` denial in both Unix-socket modes, Unix-socket and datagram-pair denial in `PairsOnly`, host Unix-socket connection, workspace socket binding, and datagram pairs in `HostServices`, unexecutable programs, missing/link/special confinement paths, ABI selection below and above the minimum |
| Windows confinement | Granted read-only and read-write paths, denied host files, directories, named pipe, and loopback TCP, a writable container temporary directory, inheritance by descendants, the same probes unconfined, missing, weaker, and foreign grants, a missing profile, `LOCALAPPDATA`, or container name, refused `HostServices`, an ungranted program, the refused entry, path, and levels reported without the path in the error text, and, through the CLI, an ungranted profile root and package cache root named by role with the path only in `refused_path` (ADR-0106) |
| Container grants | SID derivation, propagation to existing and new children, exact read-back, protection state kept, junctions not followed, hard-link reach, refused replaced/junction/file/relative paths, profile creation and deletion, journal intent and outcome order, replay validation, revision rules, scope refusals, pending and revoking recovery, the revocation reserve, CLI grant/list/revoke, refusal off Windows |
| Model tool chat | Preference off by default and rechecked per call, settings compatibility, confinement against parent, absolute, rooted, UNC, link, junction, and hard-link paths, strict provider tool-call parsing, token removal before journal and model, refusal of token-bearing calls, withheld replay after revocation, finish reason, one call per turn in escape cases |
| Model command tool | Strict profile/program/argument/cwd parsing, tool advertised only with live model-tools/execute/read/write and a profile of the platform assigned to the workspace, the assigned profiles listed in order, a profile that is not assigned refused with `profile_not_recorded` before any approval and recorded as refused, a profile declared on the other platform neither offered nor accepted, and a refusal with no assigned profile not asking for a name (ADR-0100, ADR-0107), first call records a pending approval and starts nothing, denial, approval consumption, exact digest and binding, settings-revision invalidation, run hand-off failure, output bounds, no replay, and a durable `model_tools` revocation count that stops a model-origin run even after re-enable. An end-to-end CLI case drives the real chat process, a mock provider, an operator approval, and the trusted runner to a successful confined command on both hosts |
| Model edit tool | Strict path/`old_text`/`new_text` parsing with unknown-field, empty-`old_text`, duplicate-key, absolute, and parent-path refusal; the tool advertised and dispatched only with live model-tools/read/write and denied before any file access otherwise; first call records a pending `file_write` approval and writes nothing; a summary with sizes and short digests but no file text or control characters; denial; approval consumption and one checkpointed apply; a further identical call after success needing a new approval; a same-size changed file and a same-size different replacement each needing a new approval; recoverable no-match, ambiguous (including overlaps), unchanged, non-UTF-8, oversized, too-large, and pending-checkpoint results; `session inspect` listing the exact request; schema-0.2.0 replay. A CLI case drives the real chat process, a mock provider, and operator approval to an applied edit and a recorded checkpoint ([ADR-0043](adr/0043-gate-the-model-file-edit-tool-with-approvals-and-checkpointed-apply.md)) |
| Model multi-edit and create tools | Strict parsing of `edits` (zero and 17 entries, empty `old_text`, unknown or duplicate keys at both levels, wrong types) and of `content`; both tools advertised and dispatched only with live model-tools/read/write, in the documented order, and denied before any file access otherwise; first call records a pending `file_write` approval and writes nothing; summaries with sizes and short digests but no text; denial; approval consumption and one checkpointed apply for a multi-edit (edits listed out of offset order, touching matches) and one exclusive create with no checkpoint; a changed file or different request needing a new approval; an `workspace_edit_file` approval never consumed by an equivalent multi-edit and the reverse, and a create approval never authorizing an edit; recoverable no-match and ambiguous (naming the edit index), overlap, unchanged, non-UTF-8, too-large, `file_exists`, and missing-parent results that record no approval; an output-too-large refusal that keeps the approval; schema-0.2.0 and 0.3.0 replay; desktop review fields, the shared eight-candidate bound across the three tools, stale review after a change or once a created path exists, and hostile text escaped. CLI cases drive the real chat process, a mock provider, and operator approval for each tool ([ADR-0054](adr/0054-add-multi-edit-and-create-model-file-tools.md)) |
| Model rename and delete tools | Strict parsing of `from`/`to` and `path` (missing, extra, wrong-type, absolute, parent, and empty values); both tools advertised and dispatched only with live model-tools/read/write, in the documented order within the tool-definition cap (ten since ADR-0098), and denied before any file access otherwise; first call records a pending `file_write` approval and writes nothing; summaries with sizes and short digests but no text; denial; approval consumption and one no-replace rename with no checkpoint or one checkpointed removal; a changed source, a different destination, or another tool's approval needing a new approval; recoverable `not_found`, `file_exists`, missing-parent, `edit_too_large`, and same-name results that record no approval; non-UTF-8 files accepted; an output-too-large refusal that keeps the approval; schema-0.4.0 replay. Platform cases refuse existing destinations, directories, links and junctions, and hard links, refuse a swapped or changed source, delete only the verified file, report a swapped name as uncertain on Linux, and inject faults before and after each native call. Ledger cases cover the `remove` kind: replay, every refusal (proposed bytes, a stage record, a carried stage identity), inventory without a stage, compaction, deletion of the entry, restore of the exact bytes including non-UTF-8, and refusal of an existing target. Review cases cover exact fields, stale destination or source, and hostile path text. CLI cases drive the real chat process, a mock provider, operator approval, and `file restore --removed` ([ADR-0055](adr/0055-add-checkpointed-delete-and-no-replace-rename-model-tools.md)) |
| MCP | Version/transport compatibility, authorization, untrusted content, local confinement, OAuth/redirects, cancellation |
| LM Studio | Model discovery, token/no-token profiles, streamed tools, context limits, unavailable models, no hosted-tool bypass |
| Instructions | Compiled base/UI/request match, runtime replacement denial, role-injection attempts, preference/task precedence, revisions, safe rendering |
| Compaction | Token thresholds and smaller limits, summary injection, concurrent steering, tool-pair integrity, bounded failure |
| LLM caches | Content/policy/account isolation, revocation, stale/poisoned entries, quota/clear, no side-effect replay |
| Skills | Package/link validation, scope overrides, disabled loading, script containment, live disable and context invalidation |
| Memories | Explicit project/global choice, identity isolation, lifecycle, injection, stale facts, bounded retrieval, invalidation, no replay resurrection |

Use deterministic local provider mocks in normal tests. Live-provider tests must
be explicit opt-in, use disposable data and a budget, and report which endpoint
and model were tested. A mock pass is not a live-provider compatibility result.

The current LM Studio adapter tests use a local HTTP fixture. They do not call a
live model. The adapter's text method sends one Chat Completions request,
non-streaming by default, and rejects any tool call in the reply. Its streamed
methods (0.32.0) are tested against the same kind of fixture. The fixtures
cover the strict stream rules (content type, line forms, `[DONE]`, an
incomplete stream, duplicate keys, an `error` member, finish reasons, and the
size and deadline bounds), tool-call assembly by index, redaction of the token
across piece boundaries, the display-safe stream, the desktop event hub, and
the frontend runner and live preview. A mock pass is not a live-provider
result, and no live LM Studio server was used for the stream rules. The
cancellation fixtures (0.33.0) cover a cancel before the request is sent, before
the response head, and during the body; the stream parser under arbitrary
chunking; the turn runtime's cancelled arm; the cancellation of streamed
text-only and tool-enabled turns; the desktop cancel and shutdown cases; and the
frontend notice, which is decided by the error code. They used the same kind of
local fixture. The 0.34.0 fixtures (ADR-0051) add a cancel of a non-streamed
text and tool request before the request and before the response head; a
unit test, ordered after the head arrived, that cancels while the rest of a
held body is awaited; a slow non-streamed reply that a live token does not disturb;
the runtime's use of a caller's token; the cancellation of non-streamed text
and tool turns, of the compaction before a turn, and of `compact_journal`; a
token set before a turn; the desktop cancel of a non-streamed turn and the
shutdown grace with a turn held in an event sink; and, on Linux only, `SIGINT`
delivered to the CLI during `chat` and `session compact`. The platform
interrupt test also covers `watch_interrupt`. In `omgcode-app`, these turn and
desktop tests start the clock at the cancel or shutdown request and require
the turn or compaction to end within five seconds: six tests in
`chat_turn.rs`, the non-streamed cancel in `desktop_chat.rs`, and the streamed
cancel and shutdown in `desktop_events.rs`. The other `omgcode-app`
cancellation tests are not timed, and the `omgcode-core` adapter tests keep
their own bounds. In the listed tests, work before the cancel, such as synced
journal appends, is not timed: a slow Windows runner once spent 5.9 seconds
there. The adapter requires the
compiled base prompt and accepts a literal loopback HTTP endpoint or a hosted
HTTPS endpoint that core validates (ADR-0092). The
adapter is not a permission boundary: the CLI selects tool mode from the live
`model_tools` preference, and the runtime rechecks that preference before each
call. An end-to-end acceptance case starts the real chat CLI under a delegated
cgroup on Linux, has the mock provider request the platform shell's `echo`,
approves the recorded call between turns, and asserts that the second identical
turn consumes the approval and runs the command through the trusted runner to
an `exited` result with code 0 and the expected output. On Windows the scene
grants the workspace. The case needs those host facilities and fails without
them; it does not qualify confinement correctness, which the platform suites
own. It passed on Windows 11 build 26300 and WSL 6.6 (Ubuntu 24.04) on
2026-11-14. The turn-runtime tests use a scripted model interface. The tool-enabled chat tests
use a mock that returns the documented OpenAI-compatible tool-call shape; they
do not establish live LM Studio tool-call compatibility. A single opt-in spot-check
with LM Studio 0.4.28 and `google/gemma-4-e4b` on Windows is recorded in the implementation plan. Model discovery, streaming
against a live server, cancellation against a live server, an interrupt
delivered to the CLI on Windows, and live model compatibility remain future
qualification work. The CLI can inspect
and archive sessions, but it does not execute recorded tool actions or resume
an incomplete turn.

The [authentication contract](provider-authentication.md) adds subscription
acceptance gates; the [toolchain contract](development-tools.md) defines real
development workflows to qualify on each supported OS. Documentation checks
cannot establish login entitlement or compiler compatibility.

The [instruction](agent-instructions.md), [context](context-management.md), and
[skills](skills.md) guides own detailed acceptance cases. Include malicious
instructions in direct and indirect content, including compaction summaries and
cached artifacts. A model refusing an attack is not proof that the core denied
an unauthorized action. Inspect the actual execution and disclosure boundaries.
Measure efficiency on useful tasks, including compaction/cache overhead and task
correctness; do not infer savings from cache configuration alone.

The [MCP](mcp.md) and [session storage](session-storage.md) guides add connection,
archive, persistence, corruption, and recovery cases. Run real process/file tests
on both platforms before claiming containment or durability. Parser/example
checks are not those runtime tests. Qualify LM Studio's endpoint/model/auth mode
separately; OpenAI compatibility is not proof of complete OpenAI API behaviour.
The [memory contract](memories.md) adds scope/lifecycle cases. Check both allowed
global reuse and denied project-memory disclosure; a successful retrieval alone
does not establish correct scope or current factual accuracy.

## Cargo verification baseline

The workspace pins Rust 1.99.1 and commits `Cargo.lock`. Run from its root:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
```

For workspace library targets, run documentation tests separately:

```text
cargo test --workspace --doc --locked
```

Cargo's `--all-targets` covers libraries, binaries, tests, benches, and examples;
it does not include doc tests. Review the manifest's target settings and required
features to confirm that the intended tests actually execute.

Run declared supported feature configurations separately, including defaults.
Do not blindly use `--all-features`: optional backends or platform features can
form unsupported combinations. Explicitly document any excluded combination and
its reason. Test a claimed minimum Rust version separately when it differs from
the pinned development toolchain.

Build the desktop frontend before any of these Cargo commands. The desktop
crate embeds `crates/omgcode-desktop/ui/dist`, so the commands fail without it.
The frontend gates are listed in
[Desktop shell and frontend](#desktop-shell-and-frontend). Do not report a gate
that did not run as active.

## Desktop shell and frontend

Frontend commands, run from `crates/omgcode-desktop/ui` with Node 24:

```text
npm ci
npm run check
npm run lint
npm test
npm run test:dist
```

`npm run build` must run before the Cargo commands above. `npm test` runs the
`dist` checks only when `dist/` exists; otherwise they show as skipped, so use
`npm run test:dist` to build and run them.

- **Frontend tests (Vitest, jsdom).** They cover the interface models, the
  main window and transcript rendering, the approvals review flow, polling, the
  chat runner (subscription, reconcile check, and fallback poll), the live
  preview and its bounds, the channel wrapper, and the mock backend. Security scans assert that the source has no
  `innerHTML`, `outerHTML`, `insertAdjacentHTML`, `document.write`, `eval`, or
  `new Function`; that the HTML entries have no inline script or style and no
  `http` or `https` URL; that no browser storage is used; that
  `@tauri-apps/api` is imported only in `src/ipc/client.ts`; that the main
  bundle contains neither `approval_decide` nor `approval_detail`; that no
  approvals asset contains `chat_subscribe`; and that no mock fixture is in
  `dist`. jsdom is not a browser. These tests do not
  establish how WebView2 or WebKitGTK behave.
- **Desktop crate tests (no webview).** They check that the policy table, the
  capability files, the build script's command list, and the frontend's command
  names agree; the configuration and content security policy; the navigation
  guard; handler registration; that every handler authorizes first with its own
  command; that handler parameter names match the frontend's argument keys; the
  launch argument parser; and the shutdown gate. They do not exercise the
  capability layer, which Tauri enforces and which needs a real webview.
- **Service tests.** `DesktopService` is tested with a mock provider. They cover
  chat turns, cancellation, shutdown, review tokens, approvals, the chat event
  hub (preview bounds, replay, event order, and the `finished` guard), and the
  session index with its refusals and rebuilds. Core tests cover lock-free
  inspection, discovery, the index, and approval review.
- **Real-application end to end.** On 2026-11-16 the built Windows shell was
  driven over the WebView2 remote-debugging port with real mouse and text
  input, against a mock local model server and a scratch data root. The run
  created a session in a registered workspace, ran a chat turn that requested
  `workspace_edit_file`, reviewed the pending approval in the approvals window,
  approved it with writers-stopped, and ran a second turn that applied the edit
  (the file on disk changed). It also searched with a marked snippet, archived
  and restored a session, and closed with exit status 0 in about one second
  with no leftover processes. From page script, `main` was refused
  `approval_decide`, `approval_detail`, and `approvals_list`; `approvals` was
  refused `session_tree` and `chat_send`; a different endpoint returned
  `invalid_request`; an unregistered workspace returned `unknown_workspace`; and
  remote fetch, navigation, `window.open`, and string evaluation were blocked.
  This proves the Windows shell's wiring against a mock server. It does not
  prove a live model, Linux, packaging, or resistance to a hostile page. It did
  not exercise shutdown with a provider request in flight (the 0.33.0 service
  tests cover shutdown during a streamed request), or a live call that
  reaches the Rust `authorize` check past the capability refusal.
- **Streaming end to end.** On 2026-11-16 the built Windows shell was driven
  the same way against a mock server that paces an event stream. A streamed
  tool-enabled turn showed the live entry, reset it for the second model
  request, kept markup as literal text, and marked a bidirectional override.
  The recorded reply appeared within 100 ms of the end of the stream, which
  shows the pushed `finished` event. A stream without `[DONE]` showed its
  partial text and then failed, and the partial text was not recorded. A
  subscription made from page script replaced the page's own and received the
  events in order; the page then settled through its five-second check.
  `approvals` was refused `chat_subscribe`. With `--stream off` the request was
  not streamed and no live entry appeared. This proves the Windows wiring
  against a mock. It does not prove a live LM Studio server, Linux, or the
  fallback poll, which only the frontend tests exercise.
- **Cancellation end to end.** On 2026-11-16 the built Windows shell was driven
  the same way against a mock server that sends two pieces and then holds the
  stream open. In a workspace without model tools, so the turn was text-only,
  the Cancel button ended the turn: the window settled 74 ms after the click
  with the plain notice `Turn cancelled.`, the server saw the connection close
  28 ms after the click, `chat_status` reported `failed` with the code
  `cancelled`, and the next turn in the session was accepted. Then the window
  was closed while a second stream was held: the process exited with status 0
  148 ms after the close message was posted, and after a restart the session
  was idle, showed both turns as `Turn failed: cancelled`, held none of the
  partial text, and accepted a new turn. The host was at about 100% CPU from
  an unrelated build during this run. The shutdown time is measured from the
  posting of the close message, because the helper that posts it takes
  seconds to start on a loaded host. This proves the Windows wiring against a
  mock. It does not prove a live LM Studio server or Linux.
- **ReFS and `TEMP`.** ReFS is supported
  ([ADR-0069](adr/0069-allow-refs-btrfs-xfs-tmpfs-and-zfs-for-trusted-apply.md)),
  so `TEMP` and `TMP` may be on NTFS or ReFS. They must name a per-user
  directory. A fresh volume root grants Authenticated Users only Modify, which
  lacks the `WRITE_OWNER` right that the fixtures use. Give the directory the
  Full Control shape of `%TEMP%`: run `icacls <dir> /inheritance:r` and grant
  the current user, SYSTEM, and Administrators full control. Report the
  filesystem that `TEMP` used. On 2026-11-20 the Windows platform suite (169
  tests) passed with `TEMP` on a local ReFS volume on Windows 11 build 26300.
- **Temp-directory ACL.** A data root under the user's Temp directory inherits
  an ACL that the private store refuses. The fixtures reset it; for a manual
  run do the same with
  `icacls <dir> /inheritance:r /grant:r *<SID>:(OI)(CI)F`.
- **Skipped link tests.** `a_symlinked_journal_is_rejected` and
  `a_database_file_that_is_a_link_is_refused` print `SKIPPED link assertions` and
  return when the host cannot create file symbolic links (Windows error 1314).
  Report them as skipped, not passed, and run them on a host that can. The
  directory-junction tests ran on the Windows development host, and both link
  tests ran on Linux.
- **Linux.** On 2026-11-16 the workspace, including the desktop crate, passed
  formatting, Clippy, and every test on WSL 6.6 (Ubuntu 24.04, ext4), from a
  clean LF clone with the frontend bundle built on Windows. That run executed
  the Unix-only branches of inspection and the index. The Linux binary was
  started only to check its launch arguments. No window has been opened on
  Linux, so do not describe the webview, IPC, capability enforcement, or
  navigation guard as tested there.
- **Private data root on Unix.** The checkpoint store refuses a data root that
  is not mode 0700. A fixture that creates the directory itself must set that
  mode; the default create mode is refused with "private directory ownership
  or mode is invalid".
- **Secret Service state.** A login collection can exist and still be locked.
  Check its `Locked` property, not only that the default alias resolves,
  before running the credential suites. A daemon started by bus activation is
  locked and owns the bus name, so an unlock sent afterwards reaches a second
  instance and changes nothing. If `Locked` stays true, stop every
  `gnome-keyring-daemon`, unlock, and poll `Locked` until it is false. Under
  WSL the virtual machine restarts when idle and the collection comes back
  locked, so unlock and run the tests in one session.

## Evidence and completion

Record exact commands, toolchain, platform, feature selection, completed exit
statuses, test counts, ignored cases, filtered cases, and timeouts. Investigate a
targeted filter that executes zero tests. A legitimately empty suite is not
coverage of the requested behaviour.

A build still running, compile-only run, startup abort, crash, or incomplete log
is not a passing test run. Check the actual final summary and process result.
Tests ignored for a specific job need a stated reason and a job that exercises
the required behaviour where applicable.

Re-run the changed or new test directly and confirm it executed. Inspect
delegated results yourself. Broaden checks when changed dependencies or unresolved
risks justify it, and disclose checks that could not run.

Rustfmt, Clippy, property tests, fuzzing, sanitizers, and Miri each cover limited
properties. Use suitable tools for the risk, with a functioning harness and real
execution. Do not treat a clean report from an aborted tool as a pass.

## References

- [Cargo test targets and doc tests](https://doc.rust-lang.org/cargo/commands/cargo-test.html)
- [Cargo features](https://doc.rust-lang.org/cargo/reference/features.html)
- [Clippy in CI](https://doc.rust-lang.org/clippy/continuous_integration/)

## File-broker review regressions

Run the Linux pinned-read path-swap regression with its channel checkpoint;
static pre-operation replacement tests do not exercise that interval.
The Linux removed-entry and real permission-refusal tests must also execute.
The permission fixture expects an unprivileged user whose directory mode checks
are enforced; privilege bypass is a failure, not a silent skip.

The review regression target checks that successful and capacity-refused file
requests leave settings bytes unchanged and that permission revocation still
succeeds. Archive tests cover raw-byte preservation, full logs, unresolved-state
refusal, destination collisions, directory-sync faults, read-only inspection,
and continuation with a fresh active log. The CLI workflow checks recovery
commands after a development-root change.

## Replacement acceptance gates (planned)

Phase A2a has read-only initialization/ledger record tests through the public
core API. They cover complete/incomplete histories, ordering, acknowledgements,
scope/UUID/UTC validation, strict JSON, descriptors, raw native paths, and exact
byte/count bounds. A separate executable doc example checks the initialization
API. They do not test native checkpoint persistence or workspace replacement.

Phase A2b adds native Linux private-store and pinned source-capture tests,
core `checkpoint_storage` integration tests, CLI `checkpoint_recovery` tests,
and instance-local fault injection in the private writer. They cover exact
bytes and modes, source metadata restrictions, strict inventory, malformed or
missing artifacts, live authority, unchanged targets, reserved tail capacity,
64 retained intents, actual-file aggregate quota, initialization interruptions,
partial writes, sync uncertainty, acknowledgement, and existing-only reopen.
Recovery output excludes payloads. Inspection and acknowledgement offer no
apply or restore path; Phase A4 adds restore only as a separate command.
The tests use actual temporary files. They do not simulate physical power loss,
foreign-user attacks, privileged bind mounts, or qualify a release filesystem.
No A2b test demonstrates native replacement or metadata publication.

Phase A2c adds native Linux ext4 publication and CLI acceptance tests. Exercise
exact content and metadata, a new inode, retained checkpoints, final target and
stage checks, non-CAS interleavings, authority refusal, and one native attempt.
Test failures at the commit, verification, file sync, parent sync, and terminal
append boundaries. Reopen actual files to distinguish unchanged targets,
published targets, complete visible outcomes, and malformed ledger tails.
Use any accepted filesystem (ext4, btrfs, XFS, tmpfs, or ZFS) for Linux
temporary targets and private storage, and report which. The native cross-mount
case also uses a disposable directory under `/dev/shm` on tmpfs; it skips with a
message when `TMPDIR` is itself on `/dev/shm`. The Linux platform suite (142
tests) passed on ext4 and on tmpfs under WSL 6.6. Tests do not establish
physical power-loss behavior.

Phase A3b runs the capture, storage, and apply tests natively on Windows NTFS.
Exercise exact security descriptors, each refused metadata kind, sharing
conflicts, the step order, and post-rename failures. A recheck test must defeat
the earlier checks before it can show a later one works: restore the file times
after changing security, streams, or the name, and assert that the retained
snapshot still matches. Check that the stage keeps its owner-only DACL through
preparation, rechecks, conflicts, and failures before the rename, even when the
source grants other principals. Inject a failed privacy restore, and recreate the
crash state by copying the exact source descriptor onto a retained stage: recovery
must accept it after commit intent and refuse it before. Write the source DACL
and the copy through the same API and assert the copy matches exactly: .NET
`FileSecurity` reorders access entries, and an elevated runner adds its own
default entries. CI temporary files inherit no entries, so a test that needs
inherited entries must create them. To reproduce that locally, point `TMP` and
`TEMP` at a directory without inheritable entries. Tests do not cover audit
SACLs, a foreign owner,
a second local user, a real second volume, filter drivers, or physical power
loss. They did not cover ReFS when written. The Windows platform suite has since
passed with `TEMP` on a local ReFS volume, and the CI `filesystems` job runs it on
ReFS ([ADR-0069](adr/0069-allow-refs-btrfs-xfs-tmpfs-and-zfs-for-trusted-apply.md)).

Phase A4 adds restore and checkpoint-deletion tests in the core replacement
module, the `replacement_records` ledger tests, the platform removal tests, and
CLI `checkpoint_lifecycle` tests. Run them on native Windows NTFS or ReFS and
on Linux ext4, btrfs, XFS, tmpfs, or ZFS.
Restore tests must show stale-content conflicts, foreign or missing checkpoints,
denied authority, preserved security metadata, and a new checkpoint holding the
replaced bytes. Deletion tests must show that unresolved operations are refused,
that history survives deletion, that one retained slot is freed without
eviction, and that unknown names and hard links are never removed. Inject a
failure after each intent, artifact, directory, and outcome boundary, and show
that only the same request finishes an interrupted deletion. On Windows, cover a
stage with a foreign DACL, a read-only file, and a refused read-only file whose
DACL denies attribute writes. Tests do not establish power-loss behavior.

Before enabling another native replacement backend, implement the
[accepted replacement contract](file-access.md#replacement-contract)
and run the remaining acceptance cases on native Windows and Linux. Use private,
disposable files. Use barriers or injected failures at the actual stage/check/
commit/sync boundaries; sleeps and pre-operation path swaps are insufficient.

| Boundary | Required evidence |
| --- | --- |
| Admission | Missing/expired/stale bindings, wrong session/workspace, read/write denial, changed policy, full logs, and quotas refuse replacement. Settings and permission revocation remain usable. |
| Preconditions | Stale digest, stale size, and a changed entry observed before commit intent cause a controlled conflict, including swaps with equal bytes. A mismatch after intent stops the attempt and stays unresolved. Reject invalid UTF-8 and unsafe proposed-source objects. Equal proposed bytes cause no replacement or checkpoint. Check exact line endings and byte counts. |
| Concurrent change | In-place writes and entry swaps with mismatches observable at the final check produce conflicts and leave workspace entries unchanged by the broker. Preserve the independent edit. An A-to-B-to-A write can go undetected when all checked metadata matches again. Exercise that case and changes after the check; report the documented non-CAS limit rather than asserting isolation. |
| Native objects | Links, junctions/reparse points, hard links, devices, mount crossings, invalid native paths, unsupported metadata, and cross-filesystem staging refuse replacement. Verify stage and parent substitution checks. |
| Checkpoints | Exact prior/proposed bytes and digests, exclusive-name collisions, private modes/ACLs, 1 MiB payload bounds, 64 KiB manifests, 64 entries, aggregate 128 MiB quota, and bounded unknown-layout refusal. Count incomplete artifacts; do not evict evidence. Ledger compaction keeps every retained entry's replayed state, drops only deleted entries' history, refuses unresolved work, runs before apply or deletion on a full ledger, and leaves a valid ledger at every injected fault. |
| First use | Upgraded installations with no replacement state keep ordinary reads usable without creating replacement files. Interrupt each initialization/sync boundary. Validate the bounded initializing/ready anchor, reject orphaned or partial stores, and refuse to recreate a missing ledger after initialization. Settings changes remain usable. |
| Publication | Observe a prepared durable checkpoint before the commit attempt. Preserve required security metadata. Assert one native commit attempt and verify both checkpoint payloads remain intact. No truncate, copy/delete, or weaker fallback. |
| Failure and reopen | Inject failures at every intent, payload, manifest, stage, native commit, file/directory sync, outcome, and acknowledgement boundary. Inspect the final ledger and disk state. Post-commit uncertainty never becomes clean failure or success without evidence. |
| Recovery | Reopen never replays work. Missing/corrupt/foreign artifacts fail closed. Inspection is existing-only, acknowledgement does not repair, and caps preserve reserved recovery capacity. Verify no automatic stage deletion or rollback. |
| Restore boundary | Verify inspection and acknowledgement expose no restore path. Test restore's fresh read/write authority, explicit expected current bytes, detection of later user changes, a new checkpoint preserving them, refusal of foreign, deleted, or pending checkpoints, and missing-target restore-as-create refusal. |
| Checkpoint deletion | Require an explicit permanent acknowledgement. Refuse pending, unresolved, deleted, and unknown operations. Record intent before removal and keep history afterward. Remove only fixed names, one entry at a time, without recursion, links, or ACL repair. Interrupt every boundary; only the same request may finish it, and other work stays blocked meanwhile. |
| Compatibility and privacy | Existing settings, file-operation-v1, and session journals remain unchanged. New admission checks both logs. Downgrade remains unsupported while replacement state exists. Keep checkpoint contents out of audit output, model context, and conversation events. |

A passing rename test does not establish checkpoint power-loss durability,
Windows directory-option support, ACL preservation, or a conflict-free edit.
Record the host OS/filesystem, verified API requirements, and remaining gates
before marking a backend available. Apply the normal completed-result and
targeted-test requirements above. Existing create/read tests are not evidence
that replacement or restore works.
