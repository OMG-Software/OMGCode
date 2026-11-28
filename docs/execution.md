# Command execution

This guide owns the execution contract: how OMGCode starts a program, bounds
it, and stops its whole process tree, and which native confinement each platform
must apply before a command can be exposed. The
[development-tool contract](development-tools.md) owns toolchain profiles; the
[security rules](security-coding-rules.md#process-and-sandbox-boundary) own the
general process boundary.

## Status

Version 0.13.0 added the **process-tree runner core** in the
`omgcode-platform` crate (`process::run`). It applies no filesystem or network
confinement.

Version 0.14.0 added **`process::run_confined`**, which applies the
[Linux confinement](#linux-confinement) below: a Landlock filesystem allowlist
and a seccomp socket filter.

Version 0.15.0 added [Windows confinement](#windows-confinement): the tree runs
in a per-workspace AppContainer with no capabilities, and trusted CLI commands
record and apply the [container grants](#container-grants) that decide what it
can reach.

Version 0.16.0 adds [trusted runs](#trusted-runs). `omgcode run` starts one
program through `run_confined` under a recorded
[toolchain profile](#toolchain-profiles), behind the workspace `execute`
preference, and records it in the [run journal](#run-journal). It is a
trusted operator command. A model command tool can request one, but only a
trusted approval record, or, since 0.43.0, the operator's `allow` state for
`execute`, `read`, and `write` (ADR-0063), lets it call the same runner; no
other model tool starts a process, and nothing calls the unconfined `run`.
Settings
diagnostics keep `execution_allowed: false`, because a diagnostic never
authorizes a run. Do not describe either confinement as qualified beyond the
tested kernel and Windows build.

Version 0.17.0 adds [tree limits](#tree-limits): every trusted run is bounded
in memory, live processes, and CPU time for its whole tree. Windows applies
them to the run's Job Object. Linux applies them to a child of the runner's
delegated cgroup v2, and refuses a run when the runner is not in one.

Version 0.18.0 adds [network access](#network-access). A trusted run passes
`--network` to get outbound IP networking, and the workspace `task_network`
preference must allow it. Runs without the flag keep no IP networking.

Version 0.19.0 adds a sampled [disk-growth limit](#tree-limits). Every trusted
run bounds the allocated bytes it may add to its writable roots. No portable
per-run disk quota exists, so the supervisor measures growth on a separate
thread rather than the kernel enforcing a quota.

Version 0.23.0 adds a per-workspace package cache root. A configured root is
exposed read-only on Linux and Windows; Windows requires a read-execute
container grant on that root. Without a configured root, Linux keeps the
application-storage fallback and Windows exposes nothing. See
[ADR-0037](adr/0037-use-a-per-workspace-cache-root-and-a-scoped-windows-grant.md).

## Accepted decisions

The user selected these on 2026-11-11; see
[ADR-0027](adr/0027-confine-execution-with-native-primitives-starting-with-a-process-tree-core.md).

| Area | Decision |
| --- | --- |
| Linux confinement | Landlock for filesystem access and seccomp for the rest. Both work unprivileged and do not need user namespaces. |
| Windows confinement | AppContainer for filesystem and network isolation, inside a Job Object for the process tree. |
| First caller | Trusted operator CLI commands only. A model-callable command tool needs its own later decision. |
| First increment | This process-tree core, unexposed, with tests on both platforms. |

Every later increment must fail closed. If a required primitive or rule cannot
be applied, nothing starts. There is never a fallback to unconfined execution.

## Runner contract

`process::run(spec, limits, cancel)` starts one program and returns only after
the whole tree is observed empty, or with an error.

Inputs:

- `program` must be an absolute path. The runner never searches `PATH`.
  On Windows it must be a plain `.exe` file. Batch scripts are refused because
  `cmd.exe` re-parses their arguments. Names ending in `.` or a space, stream
  names, quotes, and device paths are also refused.
- `arguments` are passed exactly. On Windows they are quoted for the Microsoft C
  runtime rules; programs that parse their command line differently are not
  covered.
- `working_directory` must be absolute.
- `environment` is the complete environment. Nothing is inherited. Names must be
  non-empty, unique (case-insensitively on Windows), and contain no `=` or NUL.
- Arguments plus environment are limited to 128 KiB, 4096 arguments, and 512
  environment entries. The Windows command line is limited to 32,767 UTF-16
  units.

Limits:

| Limit | Default | Maximum |
| --- | --- | --- |
| Deadline from start | 60 seconds | 1 hour |
| Retained stdout, retained stderr | 1 MiB each | 16 MiB each |
| Cleanup observation | 5 seconds | 30 seconds |

Behaviour:

- Standard input is the null device. Standard output and error are pipes.
  Output beyond the retained limit is drained and counted, so a writer never
  blocks; the result reports the total and whether it was truncated.
- The tree is terminated when the program exits, when the deadline elapses, or
  when cancellation is requested. Descendants never outlive a successful run.
- A run that returns `Ok` reports `Exited`, `Signalled` (Linux), `DeadlineExceeded`,
  or `Cancelled`. Each means the tree was observed empty and both output streams
  closed.
- Failures are typed. Invalid specifications and limits start nothing. `Spawn`
  leaves no process. `Supervision` is returned only after the tree was observed
  empty. `CleanupUnconfirmed` means a process or an open output handle remained
  after the cleanup timeout; it is never reported as success.

## Confined runs

`process::run_confined(spec, confinement, limits, cancel)` takes the same
specification and limits as `run`, plus a `Confinement`:

| Field | Meaning |
| --- | --- |
| `read_only` | Paths that may be read, listed, and executed. Toolchain roots, system libraries, and the program belong here. |
| `read_write` | Paths that may also be written, created, renamed, truncated, and removed beneath. The workspace and a private scratch directory belong here. |
| `unix_sockets` | `PairsOnly` (default) or `HostServices`; see below. Windows refuses `HostServices`. |
| `network` | `Denied` (default) or `Internet`; see [network access](#network-access). |
| `container` | The workspace container name. Windows requires it; Linux ignores it. |

On Windows the paths are not applied at start: access comes from the
persistent [container grants](#container-grants), and the paths are checked
against them. See [Windows confinement](#windows-confinement).

Linux rules:

- Filesystem operations that Landlock handles are denied outside the listed
  paths, including in the user's home directory and other host data. Metadata
  changes and some other operations are not restricted anywhere; see
  [known limits](#known-limits). A directory rule covers everything beneath it.
  Rules are additive: a read-only path inside a read-write path is still
  writable.
- Each path must be absolute and name an existing directory, regular file, or
  character device. No component may be a symbolic link. At most 64 paths are
  accepted.
- With `network: Denied`, IP sockets are refused in every Unix-socket mode,
  including loopback. With `Internet`, see [network access](#network-access).
- Setup fails closed. A missing path, a link, an unsupported type, a missing
  primitive, or an old kernel starts nothing. A program that the rules do not
  let the kernel execute fails as `Spawn` with `EACCES`.
- The outcome's `confinement` field records what applied:
  `Unconfined` for `run`, `Landlock { abi, unix_sockets, network }` on Linux,
  or `AppContainer { network }` on Windows.

### Unix-domain sockets

The user chose a per-run mode with the stricter one as the default; see
[ADR-0028](adr/0028-confine-linux-runs-with-landlock-and-a-socket-filter.md).

| Mode | Allowed | Reaches host services |
| --- | --- | --- |
| `PairsOnly` (default) | `socketpair(AF_UNIX)` of stream or sequenced-packet type only | No |
| `HostServices` | `socket(AF_UNIX)` of any type, every Unix `socketpair`, and socket files in read-write paths | **Yes: every pathname socket the user can reach** |

`HostServices` exists for developers whose builds need the Docker daemon or a
similar service. Landlock cannot limit which pathname sockets are reached, so
it also exposes the D-Bus session bus, SSH agents, and any other socket the
user can open. Access to the Docker daemon is usually equivalent to root on the
host. A [trusted run](#trusted-runs) selects it only when the workspace
`host_services` preference and the run's `--host-services` flag are both set,
and records the mode. No model can select it.

On Windows, `HostServices` is refused with `Unsupported`. In `PairsOnly` mode a
host named pipe or Unix socket is reachable only when its own ACL admits the
container or every application package; a pipe with the default security
descriptor is not. The tree can still create pipes and sockets of its own.

### Network access

The user chose the C6 options on 2026-11-13; see
[ADR-0032](adr/0032-allow-outbound-network-for-trusted-runs-through-task-network.md).

| Platform | `Denied` (default) | `Internet` |
| --- | --- | --- |
| Linux | No `AF_INET` or `AF_INET6` socket. | `socket(AF_INET)` and `socket(AF_INET6)` of any type. **Every destination is reachable, including loopback services, the local network, and cloud metadata endpoints.** On Landlock ABI 4 and later, an explicit TCP bind is denied, but `listen` on an unbound socket still accepts connections on a port the kernel chooses. |
| Windows | The container has no capabilities: no network, including loopback. | The container gets the `internetClient` capability: outbound connections to the internet. Loopback, private networks, and inbound connections stay denied. |

On both platforms a networked tree can send any data it can read, workspace
files included, to any host it can reach. Every other socket family keeps its
rule, `socketpair` stays Unix-only, and `io_uring` stays denied.

On Linux, name resolution and certificate checks read host files, which the
rules would otherwise hide. `process::network_support_paths()` returns the
existing entries of this fixed list, each resolved to its real path, and a
trusted run adds them read-only:

- `/etc/resolv.conf`, `/etc/hosts`, `/etc/nsswitch.conf`, `/etc/gai.conf`,
  `/etc/host.conf`, `/etc/services`, and `/etc/protocols`;
- `/etc/ssl/certs`, `/etc/ssl/cert.pem`, `/etc/ssl/openssl.cnf`,
  `/etc/pki/tls/certs`, `/etc/pki/tls/cert.pem`, `/etc/pki/tls/openssl.cnf`,
  `/etc/pki/ca-trust/extracted`, `/etc/ca-certificates/extracted`,
  `/usr/share/ca-certificates`, and `/usr/local/share/ca-certificates`.

The list names public trust stores, not `/etc/ssl` or `/etc/pki`, which hold
private keys beneath them. A path whose real path is, lies beneath, or
contains `/etc/ssl/private` or `/etc/pki/tls/private` is left out. A trust
store in another place, or a private key kept inside a trust store, is not
covered: list such a store as a profile root only after checking it.

A link such as `/etc/resolv.conf` is resolved when the run starts; a later
change of its target is not followed. Resolvers that need other services, such
as `nscd` or `systemd-resolved` through a Unix socket, need host services too.
Windows resolves names and checks certificates through system services, so it
adds no paths.

### Package-network profile (contract only)

The user selected a package-network profile as the next increment.
[ADR-0034](adr/0034-gate-package-networking-behind-enforceable-destination-control.md)
defines its contract, and version 0.20.0 records operator-declared profiles as
configuration. No run accepts a profile, the runtime has no destination
allowlist, and `--network` remains all-or-nothing.

A profile is an operator-declared, revisioned list of registry origins:
scheme, host, optional port, and an optional path prefix. Normal use is
`https`; `http` is allowed only for a loopback test fixture. A profile has no
credentials, wildcards, user information, or redirects to another origin. It
is configuration only and never authorizes a run. At most 32 profiles are
recorded, each with 1 to 8 origins.

`workspace package-profile-add <name> --registry <origin>... --revision <n>`,
`package-profile-show <name>`, and
`package-profile-remove <name> --revision <n>` manage the records. Each
mutation needs the expected settings revision and increments it. The desktop
`settings` window adds and removes profiles the same way, after a native
confirmation (ADR-0088); binding a credential stays CLI-only.

Any implementation must pass these gates before it can be described as
implemented:

- the confined tree cannot reach any destination other than the declared
  origins or a trusted proxy, including direct IP, IPv6, UDP, DNS, loopback,
  alternate socket families, and paths that bypass the proxy;
- the mechanism fails closed, and a platform that cannot enforce it refuses
  the profile rather than falling back to general network access;
- its exact limits and bypass analysis are documented and qualified on both
  supported hosts;
- a registry credential never enters a profile, run environment, journal, or
  model context. The trusted fetch's bound credential ([ADR-0056](adr/0056-send-registry-credentials-to-one-declared-origin.md)) is a
  separate path; a run never receives one;
- acceptance tests prove allowed-origin success, undeclared-origin refusal,
  redirect refusal, credential refusal, offline fallback, and platform refusal.

The current primitives cannot meet that gate. Landlock ABI 4 and later filters
TCP bind and connect by port, not address. Classic seccomp cannot read the
`sockaddr` contents. Windows AppContainer capabilities cannot name
destinations, and granting none blocks loopback too. Proxy environment
variables and an advisory loopback proxy can be bypassed by a hostile tree.
Candidate mechanisms for a later decision are a trusted host-side fetch into a
private cache with an offline run, an OS destination filter, or a Linux-only
network-namespace relay.

### Trusted package fetch

Version 0.21.0 adds a trusted operator fetch for declared origins; see
[ADR-0035](adr/0035-fetch-packages-through-a-trusted-allowlisted-cache.md).
It is never a model tool, and the confined run still has no network access.

```text
omgcode packages fetch <workspace-id> --profile <name> --url <url>
    --name <cache-file> [--max-bytes <1-1073741824>]
    [--timeout-secs <1-600>]
omgcode workspace set-package-cache <workspace-id> <dir> --revision <n>
omgcode workspace clear-package-cache <workspace-id> --revision <n>
```

`set-package-cache` selects one workspace's cache directory. It must exist,
must not overlap application storage or the development root, and must not
overlap another workspace's root. Clearing or replacing a root revokes
read-execute container grants that covered the old root, and selecting a
directory another workspace's container can already read is refused. A stale
foreign grant record refuses the selection because a renamed directory keeps
its ACL and separation cannot be proved. Runs that held the old exposure are
stopped before the command completes.
`clear-package-cache` removes the selection
and leaves cached files in place. Without a selected root, Linux uses the
application-storage fallback and Windows has none. A 0.23.0 control journal
cannot be read by 0.22.0.

Before the request it checks:

1. The workspace is registered and its live `task_network` preference is
   enabled.
2. The named package-network profile exists.
3. The URL is absolute and matches a declared origin's scheme, host, port, and
   path prefix. It has no credentials, query, or fragment. Normal use is
   `https`; `http` is accepted only for a loopback test fixture.
4. The cache name is one safe file name and the file does not already exist.
5. The byte limit is 1 byte to 1 GiB and the deadline is 1 to 600 seconds.

The request is one GET. Redirects are not followed, proxy environment
variables are ignored, and nothing is retried. No credential is sent unless
the operator bound one to the matched origin (see
[registry credentials](#registry-credentials)). TLS
uses the operating-system trust store through `reqwest`'s `native-tls`
feature; certificate validation is never disabled. The response must be 2xx
and its body is streamed, hashed with SHA-256, and stopped if it passes the
byte limit. The request send and every body read are bounded by the remaining
deadline, and a body that completes after it is rejected.

The file is written as a private `.partial-<fetch-id>` entry, synced, and
published with an atomic no-replace hard link into the selected workspace
cache root, or into `<data-dir>/package-cache/<workspace-id>/` on Linux when no
root is selected. A fetch with no selected root is refused on Windows before
anything is recorded. Existing names are never overwritten; two concurrent
fetches cannot replace each other, and the loser reports already-cached. The
cache makes no power-loss claim. The control
journal records `package_fetch_started` before the request and
`package_fetch_finished` after it; both preserve the settings revision. They
record the profile identity and revision, a SHA-256 of the URL (never its
text), the cache name, limits, HTTP status, byte count, and content digest. A
failure records a controlled reason. An abandoned fetch leaves its start
record without a finish; nothing retries automatically.

The run does not contact the registry itself. When the workspace cache exists
as a plain directory, the run adds only that directory to its read-only rules
and sets `OMGCODE_PACKAGE_CACHE` to it. The parent, application storage,
journals, settings, and other workspaces' roots stay outside the run's view,
and the run cannot write the cache. On Windows the selected root requires a
read-execute container grant on that directory; without it the run fails
before the program starts. Without a selected root, Linux uses the
application-storage fallback and Windows exposes nothing. The root's ancestors
must be traversable by the container, so use a dedicated top-level directory
rather than a user's private temporary tree. Tool-specific use of the
directory is operator configuration. A run never receives a registry
credential. `workspace status` lists selected roots under `package_cache_roots`.

### Registry credentials

[ADR-0056](adr/0056-send-registry-credentials-to-one-declared-origin.md) lets the operator bind one stored secret to one declared origin of
a package-network profile. Only `packages fetch` reads it, and it sends it to
that origin alone. Binding is a trusted CLI action, never a model tool:

```text
omgcode workspace package-credential-set <profile> --origin <origin-url> --auth bearer|basic --revision <n>
omgcode workspace package-credential-remove <profile> --origin <origin-url> --revision <n>
```

`package-credential-set` reads the secret from piped standard input only (as
`provider-credential-set` does), refuses a terminal, and reads it before the
control store opens. `--origin` must equal one of the profile's stored origins
after normalization. The secret is at most 16 KiB of visible ASCII with no
control character, line break, or leading or trailing whitespace. A `bearer`
token has no whitespace; a `basic` secret is `user:password` with a nonempty
user. Invalid input is refused before anything is stored. Storage, rotation,
and recovery follow the provider credential rules (ADR-0042): the native item
is stored, then `package_credential_bound` is recorded, then a replaced item is
deleted. `package_credential_removed` and a profile removal that names its
bound identifiers unbind the same way. `package-profile-show` and `status`
list bound origins with the scheme, identifier, and backend, never the secret.

At fetch time the matched origin decides; when several origins match, the
longest path prefix wins. A bound origin sends `Authorization: Bearer <secret>`
or `Authorization: Basic <base64(secret)>`, marked sensitive. An unbound origin
sends no header. If the credential store cannot be opened or the bound item
cannot be read, the fetch fails before the start record and sends nothing; it
never falls back to an unauthenticated request or another credential.
Redirects stay refused, so the header cannot follow one. `package_fetch_started`
records the `credential_id` only when one was attached, and the receipt shows
it. The credential still crosses the network to the declared origin, and the
registry can log it; use `https`. The runner's environment is not touched.

## Container grants

A grant gives one workspace's container access to one directory and everything
beneath it. Grants are lasting ACL entries, recorded in the control journal.
Since 0.70.0 ([ADR-0107](adr/0107-assign-toolchain-profiles-to-workspaces-and-grant-on-assignment.md)) the operator changes that need a grant
make it through the same calls: `workspace register` grants the workspace
folder read-write, `workspace profile-assign` grants read-execute on the
profile's folders the container cannot read yet, and `profile-unassign`,
`profile-remove`, and `unregister` revoke the grants no longer needed. See
[assignments](#assignments). The commands below remain for any other folder.

| Command | Effect |
| --- | --- |
| `omgcode workspace container-grant <id> <path> --access read-write --revision <n>` | Grant the workspace directory itself. No other directory may be granted read-write. |
| `omgcode workspace container-grant <id> <path> --access read-execute --revision <n>` | Grant the workspace, a directory inside it, or a directory outside the development root, such as `%USERPROFILE%\.cargo`. |
| `omgcode workspace container-revoke <grant-id>` | Remove the entry and record it. Needs no revision. |
| `omgcode workspace status` | Lists outstanding grants as `pending`, `active`, or `revoking`. |

Rules:

- The container is `OMGCode.Workspace.<workspace id as 32 hex digits>`. Its SID
  is derived from that name, so it is stable for the workspace.
- No grant may overlap application storage. A read grant may not cover the
  development root or another workspace. One grant per workspace and directory;
  revoke it before changing its level. At most 32 grants per workspace and 256
  in all.
- The journal syncs `container_grant_requested` before the ACL changes; it
  needs the expected settings revision and advances it. After the change is
  read back, `container_grant_applied` marks the grant `active`. If the change
  fails, the grant stays `pending`, because the entry may exist; revoke it.
- Revocation records `container_revoke_requested`, removes the entry, confirms
  none remains, and records `container_grant_revoked`. If removal fails, the
  grant stays `revoking`; running the revocation again retries it. Revocation
  outcomes keep the revision.
- Every other journal append must leave room, in events and bytes, for each
  outstanding grant to reach a confirmed revocation, so a full journal never
  strands container access.
- The first grant creates the container's AppContainer profile. Revoking the
  container's last grant deletes the profile and its folder. If that deletion
  fails, the command reports an error after the revocation was recorded.
- The granted directory must still be the recorded one, by volume and file
  identity. A moved or replaced directory cannot be revoked through OMGCode
  until it is back at its recorded path.
- A per-workspace package cache root outside the development root and
  application storage is a valid read-execute grant target. Grant that root,
  not a shared parent. The grant is container-wide, so use separate roots for
  workspaces that must stay isolated. The container must be able to traverse
  the root's ancestors; a dedicated top-level directory is recommended.
- Grants work only on Windows. Elsewhere `container-grant` fails and records
  nothing. The desktop `settings` window grants and revokes through the same
  core calls after a native confirmation, and returns `unsupported_platform`
  elsewhere (ADR-0088). A read-execute grant there starts with a native
  folder dialog. A read-write grant opens none and uses the workspace's
  recorded directory, the only one core accepts for that access
  ([ADR-0104](adr/0104-grant-the-workspace-folder-read-write-without-a-folder-dialog.md)).
  A revocation there stops the workspace's earlier runs, as
  `container-revoke` does.
- A run refused for a missing read-write grant says the workspace's sandbox
  has no read-write grant on the workspace folder, and names the `settings`
  window's **Container grants** group and `workspace container-grant`.
- Registering a workspace on Windows grants its container read-write on its
  folder when no grant of the workspace is recorded on that folder and this
  account can change the folder's access-control list. Registering a folder
  that is registered already grants it if that grant is missing, which is how
  a workspace registered before 0.70.0 gets it. An active read-write grant on
  the folder needs nothing. Any other grant of the workspace on the folder,
  a read-execute grant or one left `pending` or `revoking`, refuses the
  grant, because core records one grant per folder: `register` fails, names
  that grant with its `container-revoke` command, and says to register again.
  A failed grant leaves the
  workspace registered and is reported; `register` prints `grant_id` when it
  granted. When only an administrator can change the folder's
  access-control list, or whether this account can could not be read, no
  grant is attempted, and `register` prints `not_granted` with the folder and
  the reason, `needs_administrator` or `access_unknown`, as `profile-assign`
  does. Runs in that workspace are refused until it has the grant.

## Trusted runs

The user chose the C4 options on 2026-11-11; see
[ADR-0030](adr/0030-run-trusted-commands-through-recorded-toolchain-profiles.md).

```text
omgcode run <workspace-id> --profile <name> [--cwd <relative dir>]
    [--timeout-secs <1-3600, default 600>]
    [--max-output-bytes <at most 16 MiB, default 1 MiB>]
    [--max-memory-mib <16-1048576, default 8192>]
    [--max-processes <1-65536, default 1024>]
    [--max-cpu-secs <1-604800, default 7200>]
    [--max-disk-mib <16-1048576, default 8192>]
    [--host-services] [--network] -- <program> [arguments...]
omgcode runs audit [--limit <n>] [--archive <file>]
omgcode runs acknowledge <run-id>
omgcode runs archive --archive-dir <existing dir>
```

`run` prints `{"run": report}`. The report holds:

- the recorded `start` and `finish`;
- `stdout` and `stderr`, each as text with `utf8`, `total_bytes`, and
  `truncated`;
- on Linux, `scratch_removed`;
- for a `revoked` run, `revoked_because`.

`finish.usage` gives the tree's CPU time and, where the platform reports it,
peak memory. Since 0.19.0 it also gives `peak_disk_bytes`, the highest net
growth of the writable roots above the pre-run baseline.

The command succeeds whenever the run was supervised and its outcome recorded,
whatever the program's exit code. Read `run.finish.result`. A failure prints a
JSON error with code `run`.

### Checks

While holding the control journal lock, `run` checks:

1. The workspace is registered, and its live `execute`, `read`, and `write`
   preferences are enabled. A run writes inside the workspace, so it needs all
   three.
2. For `--host-services`: the host is Linux, and the live `host_services`
   preference is enabled. For `--network`: the live `task_network`
   preference is enabled.
3. The profile exists and was declared on this platform. Each root and `PATH`
   entry is still the recorded directory, and no root now overlaps the
   development root or application storage. Otherwise the profile is stale:
   remove it and add it again. Since 0.70.0 the profile must also be assigned
   to the workspace; otherwise the run is refused with text that names no
   path and says to assign it ([ADR-0107](adr/0107-assign-toolchain-profiles-to-workspaces-and-grant-on-assignment.md)).
4. The working directory is a relative path that stays inside the workspace.
   The default is the workspace itself.
5. The program resolves:
   - A bare name is searched in the profile's `PATH` entries, in order. On
     Windows `.exe` is tried first. The caller's `PATH` is never searched.
   - A relative path containing a separator names a file in the workspace.
   - An absolute path is used as given.

   The file must exist, and its directory must lie inside a profile root or
   the workspace. The path that runs is the one found, before links resolve.
6. On Windows, the workspace has an active read-write container grant, and no
   grant of the workspace is `pending` or `revoking`.
7. The [tree limits](#tree-limits) are in range and can be applied. On Linux
   the CLI must run in a delegated cgroup.

A refused run records nothing. Then the run takes its owner lock, the intent
is synced to the run journal, the control and journal locks are released, and
the program runs.

### Settings changes during a run

A run's watcher rereads the settings each time the control journal changes,
and checks the run against them. Since 0.41.0 it checks the state after every
event recorded since it last read them, not only the latest, so a change that
was undone before it read them still stops the run (ADR-0061). The run takes
the event count under the control lock when it starts. The checks are:

- the workspace is still registered and inside the development root;
- `execute`, `read`, and `write` are still enabled, `host_services` for a
  run that selected host services, and `task_network` for a run that
  selected network access;
- the run's profile still exists with the same identity and, since 0.70.0,
  is still assigned to the workspace;
- for a model run authorized by a standing `allow` (since 0.43.0,
  ADR-0063), the model command state is still `allow`. A change to `prompt`
  stops it, although `prompt` still permits an approved run;
- on Windows, the workspace still has its active read-write grant, no grant of
  the workspace is `revoking`, and no revocation of any grant of the workspace
  was requested since the run started. Every grant applies to the container,
  so revoking any of them withdraws the run, including a grant added after it
  started. The watcher compares a per-workspace count of revocation requests,
  which it cannot miss between two polls. A `pending` grant only adds access,
  so it does not stop a run.

When a check fails, or the settings cannot be read, the watcher stops the whole
tree. The outcome is recorded as `revoked`, and the report gives the reason in
`revoked_because`.

`workspace permissions`, `profile-remove`, `container-revoke`, and `set-root`
record their change first. `set-package-cache`, `clear-package-cache`,
`package-profile-remove`, and `provider-profile-remove` do the same. While
they still hold the control lock, all but `container-revoke` list the pending
runs that the new settings withdraw (ADR-0061). Then they release the lock and
wait up to 60 seconds for those runs and every other pending run that the
settings no longer allow, so a change that restores access in between cannot
hide them. If the run journal cannot be read, the change stays recorded and
the watchers still stop the runs; the command waits for what it can find and
then fails with code `run`.
`container-revoke` also waits for every pending run of the grant's workspace
that started before the revocation; it lists them while it holds the control
lock, under which every run starts. `profile-unassign` records its change
first too. `profile-unassign`, `profile-remove`, and `unregister` list every
pending run under the lock before the change, and also wait for those of each
workspace whose grant they tried to revoke, because any revocation withdraws
the workspace's runs (ADR-0107). If the run journal cannot be read, for
example after an interrupted append, they still revoke the grants, then fail
with code `run`, say that the change was recorded and every grant it
released was revoked, and name each grant, since their JSON is not printed.

A run counts as stopped only when its owner lock is free and an outcome other
than `cleanup_unconfirmed` is recorded. These runs are reported as not stopped,
and the command fails with code `run` and lists them:

- a run still live after the wait;
- a run whose cleanup was unconfirmed, because processes may remain, until it
  is acknowledged;
- a run whose supervisor ended without an outcome, even if it was acknowledged
  while the command waited.

The change itself stays recorded. Inspect the runs with `runs audit`. A change
that withdraws nothing returns at once.

Settings commands wait up to 10 seconds for another command's control lock,
because run watchers hold it briefly while they reread the settings. On
Windows, a check of a run's owner lock also waits up to 10 seconds while the
lock file is being removed, which the supervisor does as the run ends.

### Environment and confinement

Nothing is inherited. The run receives `PATH` joined from the profile's
entries, the runner-owned variables below, and the profile's variables.

| Platform | Runner-owned variables | Read-only | Read-write |
| --- | --- | --- | --- |
| Linux | `HOME`, `TMPDIR`, and `XDG_CACHE_HOME`, all set to `<data-dir>/run-scratch/<run id>` (mode 0700, removed after the run), and, when the workspace cache exists, `OMGCODE_PACKAGE_CACHE` set to the selected root or the application-storage fallback | Profile roots, `/dev/zero`, `/dev/random`, `/dev/urandom`, the workspace's [package cache](#trusted-package-fetch) when it exists, and, with `--network`, the [network support paths](#network-access) | The workspace, the scratch directory, `/dev/null` |
| Windows | `LOCALAPPDATA`, `SystemRoot`, `windir`, and `HOME` and `USERPROFILE` set to the container's `AC` folder. Windows then sets `TEMP` and `TMP` inside it. When a granted package cache root exists, `OMGCODE_PACKAGE_CACHE` is set to it | Profile roots, checked against their live ACLs; the package cache root, checked against its grant | The workspace, through its grant |

Linux runs use `PairsOnly` sockets unless host services were selected. Runs
have no IP networking unless `--network` was passed. Windows
roots that every application package can read, such as `Program Files`, need
no grant; others need a read-execute [container grant](#container-grants).
A folder under the user's own profile, such as a rustup toolchain or
`%USERPROFILE%\.cargo`, is never readable by every package, so it always
needs a grant.

On Windows the runner checks each listed path, then the resolved program,
against its live access-control list before anything starts. When the
container lacks the access a path needs, the run is recorded as
`not_started` and fails with a refusal that names the path's role and the
access: root N of the named toolchain profile (numbered from 1 in the order
`profile-show` and the `settings` window list them), the workspace's package
cache root, the workspace folder, or the program file. The refusal says what
access the container has and where to grant it. It never includes the path,
because the model's command tool returns the same text and a path under the
user's folder can name the account. The trusted CLI's `run` error adds
`refused_path` with the role, `needed`, `granted`, and the path as `path`,
`path_display`, and `path_native` (ADR-0106). A DACL that cannot be read is
still reported as a confinement rule that could not be prepared.

`SIGINT` and `SIGTERM` on Linux, and Ctrl+C and Ctrl+Break on Windows, cancel
the run. The tree is terminated and the outcome is recorded as `cancelled`.
The interrupt watcher belongs to the run and is stopped and joined when it
ends.

### Tree limits

The user chose the C5 options on 2026-11-12 and the C7 option on 2026-11-13;
see [ADR-0031](adr/0031-limit-whole-run-trees-with-job-objects-and-delegated-cgroups.md)
and [ADR-0033](adr/0033-bound-run-disk-growth-by-sampling-writable-roots.md).
Every trusted run has all four limits. There is no unlimited run.

| Limit | Flag and default | Windows | Linux |
| --- | --- | --- | --- |
| Memory | `--max-memory-mib`, 8192 | `JobMemoryLimit`: committed private memory of the whole job | `memory.max`, with `memory.swap.max` 0 where swap is accounted and `memory.oom.group` 1 |
| Processes | `--max-processes`, 1024 | `ActiveProcessLimit`: processes alive at once | `pids.max`: tasks alive at once, so threads count |
| CPU time | `--max-cpu-secs`, 7200 | Job accounting: user plus kernel time of every process the job held | `cpu.stat` `usage_usec` of the run's cgroup |
| Disk growth | `--max-disk-mib`, 8192 | Sampled allocated bytes under the workspace and the container profile folder | Sampled allocated bytes under the workspace and the per-run scratch directory |

The kernel enforces memory and processes. The supervisor enforces CPU time by
polling and disk growth by sampling. When the tree reaches a limit, the runner
terminates the whole tree and records `limit_exceeded` with the limit, even if
the program then exited by itself:

- **Memory:** on Linux the kernel kills the whole cgroup when it runs out of
  memory, and the runner sees `oom_kill` in `memory.events`. On Windows an
  allocation that would pass the limit fails, and the job posts a memory-limit
  message.
- **Processes:** a start that would pass the limit fails. Linux counts it in
  `pids.events`; Windows posts an active-process-limit message.
- **CPU time:** the supervisor compares the tree's accounting with the limit
  at each 10 ms poll. A tree with many busy threads can pass it by up to one
  poll's worth of CPU time per thread before it is stopped.
- **Disk growth:** the supervisor measures the writable roots before the
  program starts and once a second while it runs, on a separate thread. Either
  the allocated-byte total or the file-length total passing its baseline plus
  the limit ends the tree. A final measurement runs after the tree is empty,
  so a short run that wrote past the limit and exited is still recorded as
  `limit_exceeded`. See [disk growth](#disk-growth).

The report and the `run_finished` record give the tree's CPU time and, where
the platform reports it, its peak memory: the job's peak commit on Windows, or
`memory.peak` on Linux 5.19 or later. They also give the highest net disk
growth observed above the pre-run baseline.

Windows does not guarantee delivery of job messages. If one is lost, the job
still enforces the limit, but the run ends as the program decides and is not
recorded as `limit_exceeded`. CPU time and disk growth do not depend on
messages.

Windows also sends the limit messages of a job nested in the run's job, such
as a tool's own stricter job, to the run's port. So the runner counts a
message only when the run's own job confirms it:

- **Processes:** the job holds as many processes as its limit.
- **Memory:** the job's peak commit came within a sixteenth of its limit. An
  allocation refused further below the limit, because it was large, is still
  refused but is not recorded as `limit_exceeded`.

These checks narrow attribution but do not identify the sending job. A job
message carries the receiving port's own completion key and, for memory, only
the process ID. If the run is already within a sixteenth of its memory limit
when a nested job reaches its own memory limit, the runner still ends the run
as `limit_exceeded` for memory.

Each 10 ms poll takes at most 256 job messages, so a tree that keeps starting
processes cannot hold the supervisor away from its other checks. Once the
program has exited, the runner takes up to 64 such batches before it stops the
tree, until the queue is empty or a limit is confirmed. So a limit reported
behind other messages still decides the result. If messages remain after those
batches and CPU time is within its limit, the runner cannot tell whether a
limit was reached. It still stops the tree, then records `supervision_failed`
rather than a normal exit. On Windows 11 build 26300, a
nested job's limit messages did not reach the run's port in testing; the
check follows the documented behaviour.

#### Disk growth

No portable per-run disk quota exists for an unprivileged runner: Job Objects
have no disk accounting, a cgroup v2 has no disk controller, and filesystem
quotas need administrative setup. The limit is therefore sampled by the
supervisor; it is not a kernel quota. See
[ADR-0033](adr/0033-bound-run-disk-growth-by-sampling-writable-roots.md).

- **Measured roots:** the confinement's read-write directories and regular
  files, except character devices such as `/dev/null`. On Windows the
  container profile folder (`%LOCALAPPDATA%\Packages\<name>\AC`) is also
  measured, because the container writes there without a grant. A root inside
  another root counts once.
- **Baseline and growth:** the allocated-byte and file-length totals are
  measured immediately before the program starts. Each budget is that baseline
  plus the limit. Deletions reduce the totals, so the recorded
  `peak_disk_bytes` is net growth.
- **Allocation and length:** Linux uses `st_blocks * 512` for allocation,
  Windows uses the allocated (compressed) size, and both use the file length.
  Each file contributes to both totals, and the limit applies to each. Length
  catches writes that a filesystem has not written back yet; allocation
  catches a pre-existing sparse file being filled in. A sparse file is
  therefore counted conservatively at both its length and its allocation.
- **Sampling:** a dedicated thread measures every second. The supervisor
  checks its result at the normal 10 ms poll, and a final measurement runs
  after the tree is empty. A tree can pass the limit between samples; the
  overshoot depends on the write rate.
- **Fail closed:** a baseline that cannot be measured records `not_started`.
  An unreadable root or more than 4,000,000 entries while the run lives stops
  it as `supervision_failed` rather than claiming a limit that was not
  measured. Symlinks and other reparse points are not followed. A run that
  makes a writable path unreadable to the supervisor is stopped for the same
  reason.
- **Not counted:** directory entries, inode or MFT use, and filesystem
  metadata. A hard-linked file is counted once per name, so several names
  overcount it. Other writers to the same roots are counted as the run's
  growth. A tree that deletes and rewrites the same bytes can churn more than
  the limit without adding total use.

#### Linux delegated cgroup

Linux needs the runner itself in a cgroup v2 that is delegated to its user.
Start the CLI in a scope of its own:

```text
systemd-run --user --scope --expand-environment=no -p Delegate=yes -- omgcode run <workspace-id> ...
```

In scope mode `systemd-run` replaces itself with `omgcode`, so the standard
streams, exit status, and signals are the CLI's own. Since systemd 258,
`systemd-run --scope` expands `$VARIABLE`, `${VARIABLE}`, and `$$` in the
command's arguments by default, before `omgcode` sees them. Without
`--expand-environment=no`, a run would start, and its journal would record, a
different command line than the one typed: `sh -c 'echo $$'` becomes
`sh -c 'echo $'`. The switch exists since systemd 254; older versions reject
it and never expand scope arguments. Before recording a run,
the CLI checks:

- `/proc/self/cgroup` names one unified v2 cgroup, and `/sys/fs/cgroup` is the
  v2 hierarchy;
- that cgroup, its `cgroup.procs`, and its `cgroup.subtree_control` belong to
  the current user, and it is not the root;
- it offers the `memory` and `pids` controllers and has `cgroup.kill`
  (Linux 5.14 or later).

Then the CLI moves itself into a leaf, `omgcode-supervisor`, and enables
`memory` and `pids` for the delegated cgroup's children. cgroup v2 allows that
only while the delegated cgroup holds no process, so a scope shared with a
shell or with `cargo` is refused. Each run gets `omgcode-run-<pid>-<n>` beside
the supervisor leaf. The program joins it before `exec`, before any other
setup, so every descendant is counted. The runner kills the tree with both the
process group and `cgroup.kill`, and counts it empty only when the group is
gone and `cgroup.events` reports `populated 0`. It removes the run cgroup
afterwards; a cgroup that cannot be removed makes cleanup unconfirmed.

The confined tree's Landlock rules let it write only to the workspace, its
scratch directory, and `/dev/null`, so it cannot change its limits or leave
its cgroup. Do not place a development root under `/sys/fs/cgroup`. Other
programs of the same user, outside the run, can still change the delegated
cgroup.

### Model command tool

Version 0.25.0 adds `workspace_run_command` to the core turn runtime. The
runtime advertises it only when the session workspace's live `model_tools`,
`execute`, `read`, and `write` preferences are enabled and, since 0.65.0, at
least one toolchain profile is recorded for the running platform
([ADR-0100](adr/0100-list-the-recorded-toolchain-profiles-in-the-command-tool.md)).
Since 0.70.0 that profile must be assigned to the workspace ([ADR-0107](adr/0107-assign-toolchain-profiles-to-workspaces-and-grant-on-assignment.md)).
Each call rechecks
them, and the trusted run checks them again. Since 0.43.0 (ADR-0063) its
effective state is the most restrictive of the three. At `prompt` each call
needs an approval, as described below; at `allow` it starts the trusted run
without one, under the same profile, limits, and confinement. The per-tool
write limits do not apply to it. The turn releases its settings lock before
the run starts, so a standing-allow run records `standing_allow` in its start
record. The runner refuses it before recording anything unless the state is
still `allow` under the runner's own settings lock, and its watcher withdraws
it once the state leaves `allow`. An approved run is not affected by a change
to `prompt`. A model-origin run records a
durable per-workspace count of `model_tools` true-to-false changes; disabling
the preference stops the run even if it is re-enabled before the watcher reads
the setting.

Arguments are a recorded toolchain `profile`, a `program`, at most 64
`arguments`, and an optional workspace-relative `cwd`. The tool has no shell
mode: it never composes or re-parses a command line. The model may name any
program the approved profile exposes, including an interpreter such as `sh` or
`cmd`; that program's own parsing is part of the exact request the operator
approves. The tool fixes its limits: a 20-second deadline, 4 KiB retained from each
output stream, 1 GiB memory, 64 processes, 20 seconds of CPU, and 256 MiB of
disk growth. It selects no network or host services.

Since 0.65.0 (tool schema 0.9.0) the `profile` parameter lists, as an `enum`,
the names of the profiles recorded for the running platform, in the order
they were added; the list is read from the same settings snapshot that
selects the tools. Since 0.70.0 (tool schema 0.10.0) it lists only the
profiles of this platform assigned to the workspace, in profile order. A name
grants nothing. A call that names a profile not assigned to the workspace for
this platform, compared case-insensitively under the lock that admits the
call, is answered with `profile_not_recorded` and the assigned names; the
code is unchanged and the text says "assigned to this workspace". With no
profile assigned, the result says that no command can run instead of asking
for a listed name. Nothing runs, no approval is requested or consumed, the model action
journal records the call as refused (`invalid_request`), and the turn
continues. The runner still looks the profile up under its own lock, so a
profile removed after that check still refuses the run.

The first call matches no approval, records a pending approval with a bounded
summary that contains no argument text, and returns `approval_required` with
the approval identifier. Nothing runs. The operator runs
`approvals approve <id>`; the model must call again with the same arguments.
The runtime checks the exact digest, session, workspace, operation, and
settings revision, consumes the approval once, drops the settings lock, and
calls the trusted `run` path. A denial returns `approval_denied` and is not
re-requested. An expired or consumed record causes a new pending request. A
run that fails after consumption uses the approval. In the desktop shell the
turn instead waits for the decision, and an approved call runs in the same
turn (ADR-0063).

Output is untrusted and bounded. The session journal records the tool intent
and result; the run journal records the run. No replay executes a recorded
call. The model can request but cannot approve. The CLI approval commands and
the desktop shell's approvals window are the two operator surfaces; the window
adds no authority and needs a review token (see the
[desktop interface guide](desktop-interface.md#approvals)). A CLI acceptance test runs
one approved command through the real chat process, a mock provider, and the
trusted runner on both Linux and Windows, and asserts a successful exit, the
bounded output, and the consumed approval. On Linux the chat process starts in
a delegated cgroup; on Windows the test scene grants the workspace.

The approval-gated `workspace_edit_file` tool (0.28.0, ADR-0043) uses the same
approval records with the `file_write` operation, but never starts a process:
it writes only through the checkpointed apply in the
[file-access contract](file-access.md#model-edit-route-adr-0043). Approving its
record requires `approvals approve <id> --writers-stopped`, which the command
operation refuses. The `workspace_multi_edit_file` and `workspace_create_file`
tools (0.36.0, ADR-0054) and the `workspace_rename_file` and
`workspace_delete_file` tools (0.38.0, ADR-0055) and the
`workspace_create_directory` tool (0.55.0, ADR-0079) use the same `file_write`
records, each under its own digest domain, and start no process either.

### Model web fetch

Version 0.64.0 adds the `web_fetch` model tool
([ADR-0098](adr/0098-add-a-model-web-fetch-tool-behind-a-host-allowlist.md)).
It starts no process and does not use `task_network` or a trusted run. The
OMGCode process sends one HTTPS `GET` itself, behind the `web_fetch`
preference and host allowlist described in
[workspace permissions](workspace-permissions.md#web-fetch-adr-0098). The
model passes `url`, an optional `offset` into the page text, and an optional
`max_bytes` (default 12 KiB, at most 15 KiB); a longer page is read with
further calls, each of which fetches the page again.

The network policy is fixed:

- The URL uses `https` on port 443, has no user name or password, is at most
  2048 bytes, and names a DNS host. IP literals, `localhost`, single-label
  names, and local-use suffixes such as `.local` and `.internal` are refused,
  and the fragment is dropped. A refused URL is answered with `url_refused`
  before any approval.
- Every address the name resolves to must be global unicast, or the fetch is
  refused with `blocked_address`. The client connects only to the checked
  addresses, so a later DNS answer cannot move the connection to a private
  one.
- The client uses no proxy, cookies, or credentials, sends no `Referer`, and
  sends a fixed `OMGCode/<version> web_fetch` user agent and
  `Accept-Encoding: identity`. It verifies certificates against the system
  store, requires TLS 1.2 or later, and allows 10 seconds to connect and 30
  seconds for the whole fetch. Cancelling the turn stops the fetch.
- A redirect to the same host is followed at most five times, each hop
  checked again. A redirect to another host is reported with its target and
  not followed, so the model must request the target and pass the gate again.
- Only a `2xx` response with an HTML, plain-text, Markdown, CSV, XML, JSON, or
  YAML type returns text; other statuses return their code and no body. At
  most 2 MiB of the body is read. HTML is converted to text in process,
  without scripts, styles, frames, embedded objects, or elements hidden by
  `hidden`, `aria-hidden="true"`, or an inline style. Control, zero-width, and
  bidirectional formatting characters are removed, and the text is capped at
  1 MiB.
- The settings lock is released before the request starts, so a fetch never
  blocks a settings change. A change applies from the next call; it does not
  stop a fetch in flight.

The result is labelled `source web_fetched; untrusted web content from
<host>`. It is evidence, never instructions, and grants no permission. Every
call is recorded in the model action journal as `network` with the URL as
its target, and every later action in the same session carries
`after_web_content` (see the
[model action journal](workspace-permissions.md#model-action-journal)).

### Claude Code connection process

Version 0.72.0 starts the operator's signed-in `claude` program as a model
connection
([ADR-0109](adr/0109-drive-the-signed-in-claude-code-cli-as-a-model-connection.md)).
It is not a run: no model tool starts it, it uses no toolchain profile and no
workspace container, and the run journal does not record it. Each model
request of a `chat` or `session compact` that names a `claude_code` profile
starts one confined process through the same streaming spawn as trusted runs,
and a tool turn keeps that process for all its rounds.

- **Container.** On Windows the process runs in the connection's own
  AppContainer, `OMGCode.ClaudeCode.<profile id as 32 hex digits>`, with the
  `internetClient` capability. `omgcode workspace claude-code-grant <name>
  --revision <n>` grants it read-execute on the program's folder, on the
  folder of the `omgcode` executable that runs the command (the MCP relay),
  and on the connection's runtime folder (`claude-code/<profile id>` in
  application storage, created by the grant), and read-write on the sign-in
  folder. Each grant is recorded before and after its ACL change, with the
  same grant reserve as workspace grants. `omgcode workspace
  claude-code-revoke <name>` revokes them all and deletes the container
  profile; it needs no revision, refuses a profile of another kind, and also
  deletes a container profile that an interrupted revocation left. A grant
  checks its whole plan before it records the first intent. A profile with
  grants cannot be removed, and a turn refuses to start until every role
  holds an active grant for the folder it names. A folder that holds the
  development root, a workspace, or application storage, or is a drive root
  or the home folder or its ancestor, is refused. The sign-in folder must
  also stay apart from every workspace container grant and every other
  connection's sign-in folder, and a new development root or workspace
  container grant may not overlap it.
- **Linux.** Landlock allows reading the program and relay folders, the
  per-turn folder, `/usr`, `/lib`, `/lib64`, `/bin`, the network support
  files, and `/dev/zero`, `/dev/random`, and `/dev/urandom`, and writing the
  sign-in folder, the per-turn temporary folder, and `/dev/null`. The
  seccomp filter allows IP sockets and `AF_UNIX`. No grant is recorded; the
  runtime folder is created with mode `0700`.
- **Limits.** The process tree has 4 GiB of memory, 512 processes, four
  hours of CPU time, a one-hour deadline, 64 KiB of captured standard error,
  and no disk-growth limit. On Linux the CLI must run in a delegated cgroup
  v2, as for `run`. Cancellation, a lockdown failure, and every other
  failure end the whole tree, and the end is observed before the turn
  returns. A failed termination or emptiness check counts as unconfirmed
  cleanup, so the tree's own fallback kill still runs.
- **Withdrawal.** Each process holds a shared lock on
  `claude-code/<profile id>.turns` in application storage, outside every
  grant, from before its per-turn folder is made until its tree was
  observed ended. Before the launch the turn checks every settings state
  recorded since it resolved the connection, and while the process runs a
  watcher checks each later state every 100 ms. A state that removes the
  connection or, on Windows, leaves a role without an active grant ends the
  tree, and the turn fails with `claude_code_withdrawn`; unreadable settings
  do the same. `workspace claude-code-revoke`, `workspace
  provider-profile-remove`, and the `settings` window's removal record the
  change, release the settings, and wait up to 30 seconds for an exclusive
  lock. A turn that does not end in time fails the command with a `control`
  error that says the change is recorded.
- **Standard streams.** Standard input carries the rendered conversation
  and is then closed. Standard output is read as bounded stream-json lines;
  nothing it says is trusted. The tool channel is a named pipe (Windows) or a
  socket in a `0700` per-turn folder (Linux) that the relay reaches after a
  per-turn nonce, not an inherited handle. At most eight output events wait
  for the turn, so a turn that is running a call or waiting for an approval
  holds back the process's output. Each write to the relay ends at the
  request timeout or on cancellation, and the main agent's text has an
  8 MiB budget. The Linux folder has a random
  name and is created exclusively under `XDG_RUNTIME_DIR`, or `/tmp` when
  that is unset, relative, or too long, because a socket path must fit in
  107 bytes and one beneath application storage does not.

### Toolchain profiles

Profiles are declared by the operator. Nothing is recorded automatically: the
desktop `settings` window can propose profiles from the installed toolchains,
and the operator selects and confirms each one (see
[generated profiles](#generated-profiles)). Every run names one, whether the
trusted CLI or the model command tool starts it, and there is no fallback to
an unconfined run. Since 0.70.0 a workspace runs only the profiles assigned
to it, and the model command tool is offered only while at least one profile
of the running platform is assigned to the workspace; it lists their names
(ADR-0100, [ADR-0107](adr/0107-assign-toolchain-profiles-to-workspaces-and-grant-on-assignment.md)).

| Command | Effect |
| --- | --- |
| `omgcode workspace profile-add <name> --root <dir>... [--path <dir>]... [--env NAME=VALUE]... --revision <n>` | Record a profile. Needs the expected revision and increments it. |
| `omgcode workspace profile-show <name>` | Show one profile. |
| `omgcode workspace profile-remove <name> --revision <n>` | Remove it, end its assignments, and revoke the grants they alone needed. Needs the expected revision and increments it. |
| `omgcode workspace profile-assign <workspace> <name> --revision <n>` | Let the workspace run the profile; on Windows, first grant the folders its container needs. |
| `omgcode workspace profile-unassign <workspace> <name> --revision <n>` | End the assignment and revoke the grants no other assigned profile needs. |
| `omgcode workspace status` | Lists profiles under `profiles` with the revision that added each, and each workspace's `assigned_profiles`. |

Rules:

- **Names:** at most 64 ASCII letters, digits, `.`, `-`, or `_`, starting with
  a letter or digit. Names are unique case-insensitively. Each profile records
  the platform it was declared on.
- **Roots:** 1 to 16 existing directories, recorded by identity. They may not
  overlap the development root in either direction, overlap application
  storage, or duplicate each other.
- **`PATH` entries:** up to 16, each an existing directory inside a root.
- **Variables:** up to 64, with names of at most 128 bytes and values of at
  most 4096 bytes, and no NUL.
  - A name starts with a letter or `_`, continues with letters, digits, or
    `_`, and is unique case-insensitively.
  - `PATH`, `HOME`, `TMPDIR`, `TEMP`, `TMP`, `USERPROFILE`, `LOCALAPPDATA`,
    `SYSTEMROOT`, `WINDIR`, `XDG_CACHE_HOME`, and `OMGCODE_PACKAGE_CACHE` are
    reserved for the runner, in any case.
  - Values are stored in the private settings journal and shown by `status`.
    Never put secrets in them.
- **Limit:** at most 32 profiles.
- **Changes:** there is no edit. Remove a profile and add it again. The
  journal events are `profile_added` and `profile_removed`. The desktop
  `settings` window adds and removes profiles through the same core calls: it
  opens a native folder dialog for each root and `PATH` entry, shows a native
  confirmation that lists them and the variable names, and applies the change
  with the confirmed revision ([ADR-0088](adr/0088-change-execution-settings-in-the-settings-window.md)).

#### Assignments

Since 0.70.0 ([ADR-0107](adr/0107-assign-toolchain-profiles-to-workspaces-and-grant-on-assignment.md)) a profile is recorded once and assigned to
each workspace that may run it. Assigning to a workspace is the operator's
consent to give that workspace's sandbox the profile's folders.

- **Events:** `profile_assigned` and `profile_unassigned` name a workspace and
  a profile, and each needs the expected revision and increments it. Replay
  refuses an unknown workspace or profile, an assignment that exists already,
  and an unassignment of one that does not. Assigning also refuses a profile
  declared on another platform. Removing a profile ends every assignment of
  it, and unregistering a workspace drops its assignments. An older build
  refuses the new events.
- **Planning, on Windows:** core lists each live root of the profile that the
  workspace's container cannot read yet, each folder once, skipping a folder
  that already has a recorded grant of the workspace. Folders that only an
  administrator can grant, and folders whose access-control list could not be
  read, are listed apart and not granted. A root that is no longer the
  recorded directory is listed as unknown. Off Windows the plan is empty. A
  workspace with a container grant that is pending or being revoked is
  refused a plan on every platform, because its runs are refused until that
  grant is settled; revoke it first.
- **Assigning:** under the store's lock core plans again and refuses the
  assignment when the folders to grant, or the folders left out and their
  reasons, differ from the plan the operator confirmed. It then applies `check_container_grants` to the batch, grants
  each folder read-execute through `grant_container_access`, refusing before
  anything is recorded a folder that is no longer the directory the operator
  confirmed, and records the assignment last. When a grant fails, or the
  assignment cannot be recorded, the profile stays unassigned and core
  revokes the grants this call made that no assigned profile needs, which
  withdraws the workspace's runs; a grant whose revocation fails stays
  recorded, `pending` or `revoking`, until it is revoked. A grant the call
  made that a profile already assigned to the workspace needs is kept, and
  stays recorded in its state, `pending` if its access-control change failed.
  Core returns those revocations, and the grants it kept, with the error:
  `profile-assign` names each grant not revoked with its `container-revoke`
  command and each grant kept, and the `settings` window counts them and
  points to each one's **Revoke**, and says so when only an AppContainer
  profile could not be deleted. When the rollback revoked every grant the
  call made, the `settings` window says so, and that none is left pending,
  instead of pointing to a pending grant. When the rollback revoked a grant, or tried
  to, both wait for the workspace's withdrawn runs to stop before they report,
  as an unassignment does. `profile-assign` is itself the consent,
  so it plans and assigns in one call, and prints `assigned_profile_id`,
  `granted` (each `grant_id` with its folder), and `not_granted` with the
  reason.
- **Unassigning and removing:** the change is recorded first. Then core
  revokes the workspace's read-execute grants whose folder is one of the
  profile's roots or holds one of them, that neither a profile still assigned
  to the workspace nor the workspace's package cache root needs. A profile
  still assigned, and the cache root, need a grant on their folder, and a
  grant on a folder that holds it when no other active grant of the workspace
  is on that folder or above it: a root inside a granted folder was readable
  when it was planned, so it got no grant of its own. A pending or revoking
  grant may hold no access, so it never stands in. So a grant on an outer
  profile's root, kept while a nested root of another profile relies on it,
  is revoked when that other profile is unassigned too. Whether a folder
  holds another is read from their live identities, or, when either was
  moved, removed, or cannot be read, from their recorded canonical paths, so
  a missing nested root still keeps the outer grant while its profile is
  assigned and releases it afterwards. That decides only what is kept; each
  revocation still checks its folder's live identity. The workspace folder's
  read-write
  grant is never revoked here.
  grant is never revoked here. Every grant is attempted, even after one
  fails. `profile-unassign` and `profile-remove` print what they revoked
  under `revoked`; a failed revocation leaves the change recorded, each grant
  not revoked is named with its `container-revoke` command, and a grant whose
  AppContainer profile could not be deleted afterwards is named as revoked;
  when that is the only failure, the message says every grant was revoked.
- **Unregistering:** `workspace unregister` revokes every grant of the
  workspace first and unregisters it only when every revocation was
  confirmed; otherwise the workspace stays registered with the grants not yet
  revoked, each named. When every grant was revoked but the removal cannot be
  recorded, the workspace also stays registered, and the runs the
  revocations withdrew are still waited for. See [workspace settings](workspace-permissions.md#unregistering-a-workspace).
- **Runs:** each revocation withdraws every run of its workspace, as
  `container-revoke` does, so these commands stop and wait for those runs.
- **Upgrading:** existing profiles start assigned to no workspace. Assign each
  workspace's profiles once. A profile whose folders are already granted
  grants nothing new.

The `settings` window shows one switch per profile of this platform on each
workspace's page. Turning one on or off runs the same core calls after a
native confirmation that names the workspace, the profile, and each folder it
grants or each grant it revokes; see the [desktop interface](desktop-interface.md).

A profile lists what a toolchain needs, not just its executable. Tools that
read configuration outside their installation need that directory as a root.
For example, a distribution Node reads `/etc/ssl`.

#### Generated profiles

Version 0.68.0 adds **Generate toolchain profiles from local packages** to the
**Toolchain profiles** group of the `settings` window
([ADR-0103](adr/0103-generate-toolchain-profiles-from-installed-toolchains.md)).
Core's trusted discovery service proposes profiles; it records and grants
nothing. Adding proposals with a workspace chosen also assigns them to it
(ADR-0107).

- **Families:** Rust (rustup toolchains), C and C++ (MSVC with the Windows
  SDK, Clang, and MinGW GCC on Windows; GCC and Clang on Linux), Java, Python,
  Perl, Go, Node.js, .NET, and Ruby. Each family is one detector in a
  registry.
- **Inputs:** the standard install folders, the operator's `PATH`,
  `JAVA_HOME`, `GOROOT`, and `RUSTUP_HOME`, and small files the installers
  wrote (rustup's `settings.toml`, a JDK's `release`, Go's `VERSION`), each
  checked to be a regular file before it is opened. Discovery runs no
  program, confined or not, reads no registry key, and never reads
  repository configuration such as `rust-toolchain.toml`. A folder that
  `PATH` or one of those variables names is read like any install folder,
  installer files included, even inside the development root; core refuses
  it as a root before any confirmation, and the list shows why.
- **Proposals:** each lists its roots, `PATH` entries, and variables, plus the
  system part every profile needs (`System32`, or `/usr` and the real
  top-level `bin`, `lib`, `lib64`, and `lib32` directories). No root is a filesystem or drive
  root, the home directory or its ancestor, or itself a link: core records a
  root's resolved location, so a linked root is left out. Variables are
  derived setup values only, such as `JAVA_HOME`, `GOROOT`, `DOTNET_ROOT`,
  and MSVC's `INCLUDE` and `LIB`; reserved names are never proposed. A Rust
  proposal is one toolchain directory, without the rustup proxies or
  `~/.cargo`. At most 32 proposals are returned, 8 per base name, its
  preferred installation (such as rustup's default toolchain) first and
  then newest first; past 32, every base name keeps its first installation before any
  keeps a second, and the list says how many installations it left out. A
  folder listing examines at most 16,384 entries and keeps the first 512
  directories by name.
- **Checks:** each proposal shows the reason core would refuse it now, from
  `check_profile`, which applies every `add_profile` rule and records nothing.
- **Adding:** the window sends only the settings revision and the digests of
  1 to 8 selected proposals. Rust discovers again, refuses a selection that no
  longer matches (`discovery_changed`), shows one native confirmation that
  lists every root, `PATH` entry, and variable with its value, and records each
  profile with `add_profile` from the confirmed revision.
- **Assigning to a workspace:** on every platform the operator can choose a
  workspace, and adding then also assigns the added profiles to it in the
  same confirmation (ADR-0107). On Windows each root says whether that
  workspace's container can read it, needs a read-execute grant, needs an
  administrator, or could not have its access-control list read. The
  access-control lists are read while the settings store is closed. The one
  confirmation also names the folders the assignment grants, which are only
  those this account can change. Before the dialog, core's
  `check_container_grants` applies every grant rule to the whole batch (the
  development-root scope, application storage, an existing grant, and the
  32-per-workspace and 256 limits), and a refusal names the folder. After it,
  each profile is recorded with `add_profile`, then the grants are applied and
  the profiles assigned as `profile-assign` does. Administrator-only folders,
  and folders whose access-control list could not be read, are named and
  skipped. A recorded grant whose access list then cannot be changed stays
  `pending` and is reported as `container_access_failed`; revoke it. The
  separate grant command of 0.68.0 is removed.
- **Notes:** a proposal says when its family has not passed the confined
  acceptance runs (only Rust, MSVC, Linux GCC, Python, Go, and Node.js have),
  when it is a runtime without a compiler, and that offline runs cannot
  download dependencies.

Known limits: a model command runs with no network, a 20-second deadline, and
1 GiB of memory, so a `cargo` build that needs crates.io dependencies fails
unless they are already present. Only a trusted CLI `run --network` under the
workspace's `task_network` preference can fetch them. Toolchains installed in
custom folders that are not on `PATH` are not found. No CLI command lists
proposals.

### Run journal

`<data-dir>/runs-v1.jsonl` sits beside the control journal and shares its
store identity. Each line has schema version 1, an event UUID, a contiguous
sequence, a UTC timestamp, and a tagged `action`:

| Action | Meaning |
| --- | --- |
| `created` | Header |
| `run_started` | Synced before the program starts. Records the run ID, workspace, settings revision, profile ID, name, and revision, program path, argument count, `arguments_sha256`, working directory, socket mode, since 0.17.0 `limits` (`max_memory_bytes`, `max_processes`, `max_cpu_ms`, and since 0.19.0 `max_disk_bytes`), since 0.18.0 and only for a networked run `network` with the `support_paths` it could read (at most 32), and, since 0.22.0 and only when true, `package_cache` for read access to the workspace cache. |
| `run_finished` | Records the result (`exited`, `signalled`, `deadline_exceeded`, `cancelled`, `revoked`, `limit_exceeded` with its `limit`, `not_started`, `supervision_failed`, or `cleanup_unconfirmed`), the applied confinement, the elapsed time, the output byte counts, and, for a started run, `usage` (`cpu_ms`, where reported `peak_memory_bytes`, and since 0.19.0 `peak_disk_bytes`) |
| `run_acknowledged` | The operator cleared a pending run |

How it works:

- **What is not recorded:** argument text, environment values, and output.
  The digest is SHA-256 over each argument's native bytes, each prefixed by
  its length as a little-endian `u64`.
- **Pending runs:** a run whose outcome is missing, or whose cleanup was
  unconfirmed, stays pending.
  - `runs audit` lists pending runs.
  - `runs acknowledge` clears one. That claims no outcome and retries
    nothing. On Linux it also removes a leftover scratch directory.
  - A run whose supervising process is still live cannot be acknowledged.
    That process holds `<data-dir>/run-owners/<run id>.lock` from before the
    intent is recorded until the outcome is recorded.
- **Limits:** at most 16 pending runs; 8 MiB, 4,096 events, and 64 KiB per
  event. A journal that records the 0.19.0 fields cannot be read by 0.18.0.
  A 0.22.0 journal that records `package_cache` cannot be read by 0.21.0.
- **Reserve:** a new run must leave room for every pending run's remaining
  events, 1 KiB each: two for a run in progress, one for a run that needs
  acknowledgement.
- **Archiving:** `runs archive` moves a journal with no pending runs into an
  existing directory and reports its SHA-256. A new journal starts on the next
  run. `runs audit --archive` reads an archived file. A bare file name names
  a file in the current directory.
- **Locking:** the journal lock is held only while appending. A busy lock is
  retried for up to 10 seconds.

A run whose outcome cannot be recorded still prints its report, then fails;
it stays pending.

### Acceptance

`crates/omgcode-cli/tests/run_workflow.rs` covers these cases with system
tools on both platforms:

- confinement and recording;
- refusals;
- host-service selection;
- stale profiles;
- deadlines, concurrency, and abandoned runs;
- every tree limit, including disk growth;
- interrupts, on Linux.

`crates/omgcode-cli/tests/toolchain_acceptance.rs` runs real Rust, Python,
Node, C/C++, Go, and Java workflows. Its file header lists the toolchain
variables. Each workflow:

- builds or compiles, runs tests, formats where the toolchain has a formatter,
  and runs scripts;
- runs a subordinate process;
- keeps its outputs in the workspace;
- fails a missing dependency as an ordinary exit;
- runs a hostile program that must be denied host data, writes outside the
  workspace, and a loopback listener.

The C program checks files only. These tests are ignored by default, and a
missing toolchain variable fails the test.

## Platform mechanics

### Windows

- `CreateProcessW` starts the program suspended. `PROC_THREAD_ATTRIBUTE_JOB_LIST`
  places it in a new Job Object at creation, so it cannot start a descendant
  outside the job. The runner checks membership with `IsProcessInJob` before
  resuming it.
- The job sets `KILL_ON_JOB_CLOSE` and `DIE_ON_UNHANDLED_EXCEPTION` and grants no
  breakaway. Closing the job handle, including on an error path, kills every
  member.
- `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` limits inheritance to the null input and
  the two output pipe ends. Those ends are inheritable only while a module lock
  is held around `CreateProcessW`.
- Termination uses `TerminateJobObject`. Cleanup is observed when the job's
  active process count is zero. The pipes report end of file once all members
  are gone. A member's process object can become signaled shortly after the
  count reaches zero, while the kernel finishes its exit, so a check for that
  object must allow a short wait.

### Windows confinement

For `run_confined`, the runner checks before anything starts:

1. `unix_sockets` is not `HostServices`; otherwise `Unsupported`.
2. `container` is set; otherwise `InvalidSpec`.
3. The container's profile exists; otherwise a `Confinement` error. The runner
   never creates a profile.
4. `LOCALAPPDATA` is in the environment; otherwise `InvalidSpec`. Windows needs
   it to start an AppContainer process. It replaces it with the container's
   folder, `%LOCALAPPDATA%\Packages\<name>\AC`, and sets `TEMP` and `TMP` to
   `AC\Temp`, which the container can write.
5. Each `read_only` path allows the container at least read-execute, and each
   `read_write` path read-write. Each path is opened without following a final
   reparse point. Allow entries for the container SID or for every application
   package count, inherited or explicit, less deny entries for them.
6. The program allows the container read-execute. `CreateProcessW` maps the
   image with the caller's token, so without this check a program the container
   cannot read would still start.

A failed check starts nothing. The runner then passes
`PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` with the container SID, beside
the job and handle lists above. Without network access it lists no
capabilities. With `Internet` it lists only the `internetClient` capability
SID, enabled. Every descendant stays in
the same container and job.

Grants are applied by `container::grant`:

- Read-execute is `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE`. Read-write adds
  `FILE_GENERIC_WRITE | DELETE`, without `WRITE_DAC`, `WRITE_OWNER`, or
  `FILE_DELETE_CHILD`. Both are inherited by directories and files.
- The directory is opened with backup semantics and without following a final
  reparse point. It must be a directory on an NTFS or ReFS volume that reports the required
  capabilities ([ADR-0069](adr/0069-allow-refs-btrfs-xfs-tmpfs-and-zfs-for-trusted-apply.md)), with the recorded identity.
- `SetEntriesInAclW` replaces any explicit entry for the SID, and
  `SetSecurityInfo` writes the DACL through the same handle, keeping its
  protected state. Windows propagates the entry to existing children; on a
  large tree this takes time, and the control journal stays locked meanwhile.
  Propagation does not follow junctions.
- The entry is read back and must be exactly one allow entry with the expected
  mask and inheritance. Revocation removes every explicit allow entry for the
  SID and confirms none remains.

### Linux

- The program starts as the leader of a new process group.
- Before `exec`, the child:
  - sets `PR_SET_PDEATHSIG` to `SIGKILL` and confirms its parent is still the
    runner;
  - marks every descriptor above standard error close-on-exec with
    `close_range` (Linux 5.11 or later);
  - sets `no_new_privs`;
  - installs a seccomp filter that makes `setsid` and `setpgid` fail with `EPERM`
    and kills the process for any foreign ABI (32-bit or x32).
- Descendants inherit the filter, so none can leave the process group.
  Termination sends `SIGKILL` to the group. Cleanup is observed when signal 0 to
  the group reports no such process.
- Supported architectures are x86_64 and aarch64. Others return `Unsupported`.

### Linux confinement

For `run_confined`, the parent prepares everything that can fail before
`fork`:

1. It reads the Landlock ABI with `landlock_create_ruleset`. Without Landlock,
   or below ABI 3, it returns `Unsupported`. ABI 3 is the first that handles
   truncation, so earlier ABIs could not keep a read-only file intact.
2. It creates a ruleset that handles every filesystem right the ABI knows,
   explicit TCP bind from ABI 4, TCP connect from ABI 4 unless the run has
   network access, and signal and abstract-socket scoping from ABI 6. Rights added
   after ABI 6 are not handled. No network rule is added, so a handled right
   is denied for every port.
3. It opens each path with `openat2(O_PATH, RESOLVE_NO_SYMLINKS |
   RESOLVE_NO_MAGICLINKS)`, checks its type, and adds a path-beneath rule.
   Read-only rules grant execute, read, and list. Read-write rules grant every
   handled right except creating device nodes, and except creating socket
   files in `PairsOnly` mode. File rules keep only the rights that apply to
   files.

Rules bind to the opened objects, so a later rename does not change the grant.
The ruleset descriptor is close-on-exec. The child then runs C1's steps and, after
`no_new_privs`, calls `landlock_restrict_self` before installing the seccomp
filter. The confined filter adds these refusals, each with `EPERM`:

- `socket` for every family other than `AF_UNIX`, except `AF_INET` and
  `AF_INET6` with network access, and the Unix rule for the selected mode;
- `socketpair` for every family other than `AF_UNIX`;
- `io_uring_setup`, `io_uring_enter`, and `io_uring_register`, because
  `io_uring` can create sockets without `socket`.

Datagram socket pairs are refused in `PairsOnly` mode because `sendto` on a
connected Unix datagram socket can still address a host socket path, such as
`/dev/log`. Stream sockets refuse a destination address, and sequenced-packet
sockets ignore it.

## Known limits

These remain true after C6:

- **`process::run` is unconfined.** A program started by the platform
  function `run` can read and write anything the user can, use the network,
  and talk to host services. Only `run_confined` applies the rules above.
  The trusted CLI `run` uses `run_confined` only.
- **Profile variables are not secret.** They are stored in the settings
  journal and shown by `status`. There is no credential path for runs.
- **Node on Windows.**
  - Node's module loader reads every ancestor directory, starting at the drive
    root, and fails with `EPERM` in a container. Declare
    `NODE_OPTIONS=--preserve-symlinks --preserve-symlinks-main` in the
    profile.
  - Child processes with piped standard streams never return, even with a
    timeout; only the run deadline ends them. Inherited streams work. This
    includes the isolated `node --test`; use `--test-isolation=none` (Node
    22.8 or later).
- **Acceptance coverage.** These results are from 0.16.0, before tree
  limits. The toolchain cases have not been rerun under the 0.17.0 limits.
  They passed with:
  - Windows: Rust 1.99.1 (MSVC 14.50 and SDK 10.0.26100), Python 3.14.4,
    Node 24.15.0, Go 1.27.4, and MSVC C/C++;
  - Linux: Rust 1.99.1, Python 3.12.3, Node 18.19.1, Go 1.26.10, and
    GCC 13.3.

  The Java case has not run, because neither host had a JDK. Not covered:
  disk exhaustion; package downloads through `--network`, which have not
  been run as acceptance cases; and Python and Node formatters, because none
  were installed. On
  Windows, the Rust, Python, and Node toolchains were user-owned copies
  granted read-execute, because the installed directories needed
  administrator rights to grant.
- **Windows grants are lasting and shared.** Grants are ACL entries that stay
  until revoked. A run's `Confinement` is checked against them, but grants it
  does not list still apply to it. Programs running as the same user can add or
  remove entries; the journal records only changes made through OMGCode.
- **Windows reads beyond grants.** Paths that every application package may
  read stay readable inside the container, including the Windows directory,
  `Program Files`, and many machine registry keys.
- **Windows hard links.** A hard link shares one security descriptor, so a
  grant reaches the file under every name, including names outside the granted
  directory. Grant directories that contain no hard links to host data.
- **Windows ancestors.** The container can open granted paths below
  directories it cannot read, but it cannot list or read those ancestors. Tools
  that search parent directories or resolve real paths may fail; Node does, as
  described above.
- **Windows loopback.** Loopback traffic is dropped without a reply, so a
  connect fails only after the system timeout (about 21 seconds), not at once.
- **Windows build coverage.** Windows confinement was tested on Windows 11
  build 26300 only.
- **Linux kernel coverage.** Confinement needs Landlock ABI 3 (Linux 6.2 or
  later) with Landlock enabled. It was tested on WSL 6.6 (ABI 3) only.
- **What Landlock does not cover.** Landlock up to ABI 6 does not restrict
  `stat`, `readlink`, `chdir`, `access`, `flock`, or `fcntl`, or metadata
  changes with `chmod`, `chown`, `setxattr`, and `utime`. These work on any
  path the program can name, including unlisted host files, within the user's
  own rights. On ABI 3 and 4, device `ioctl` is not restricted either: a
  character device reachable through any rule, read-only included, accepts
  state-changing `ioctl` calls. Grant devices only when needed. Before ABI 6, a
  confined program can signal other processes of the same user.
- **`HostServices` exposure.** See [Unix-domain sockets](#unix-domain-sockets).
- **Network access is all or nothing.** Without `--network`, package
  downloads, loopback test servers, and development servers fail. With it,
  a Linux run reaches every destination, loopback and the local network
  included, and a Windows run reaches the internet but not loopback. There is
  no destination allowlist. A networked Linux run can accept TCP
  connections: on ABI 3 through any bind, and on ABI 4 and later through
  `listen` on an unbound socket, which takes a port the kernel chooses. UDP
  bind is never limited. See
  [network access](#network-access).
- **No run destination enforcement.** Package-network profiles are recorded,
  and the trusted [`packages fetch`](#trusted-package-fetch) command contacts
  only declared origins. No run accepts a profile, and `--network` remains
  all-or-nothing. On Linux a run can read only its workspace's cache; Windows
  does not expose the cache. A proxy or proxy environment variables are not a
  boundary: a hostile tree can connect directly.
- **Package fetch limits.** The fetch is trusted-CLI only. It sends no
  credentials unless one is bound to the matched origin
  ([registry credentials](#registry-credentials)), follows no redirects,
  ignores proxy variables, and uses the system TLS store. On Windows a connect
  refused because the chosen local port still holds a TIME_WAIT connection to
  the same origin is retried on another port before any byte is sent
  ([ADR-0078](adr/0078-skip-windows-local-ports-held-in-time-wait.md)); no
  other failure is retried. The cache is the selected per-workspace root or, on Linux,
  the application-storage fallback. It makes no power-loss claim. Tests use
  loopback HTTP; a live HTTPS registry is not qualified.
- **Package cache exposure.** A run receives `OMGCODE_PACKAGE_CACHE` when the
  workspace cache exists as a plain directory, and may read only that
  directory. On Windows the selected root needs a read-execute container grant
  and traversable ancestors, or the run fails before it starts. Linux falls
  back to application storage when no root is selected; Windows exposes
  nothing. There is no invalidation, eviction, or freshness check, and
  configuring a toolchain to use the directory is operator work.
- **Model command limits.** The model command tool accepts no model-selected
  limits, network, or host services. It uses a 20-second deadline and 4 KiB
  per output stream. On the CLI the operator flow spans two turns; in the
  desktop shell the turn waits. The operator decides in the CLI or in the
  desktop approvals window. A denied exact request stays denied for the current settings
  revision; a failed run consumes the approval. A CLI acceptance test runs one
  approved command through the real chat process and the trusted runner on
  both hosts; it needs a delegated cgroup on Linux and a workspace container
  grant on Windows. Confinement correctness remains the platform suites'
  responsibility.
- **Model web fetch limits.** A fetched URL can carry workspace data to an
  allowlisted host under `allow`, or to any host whose URL the operator
  approves. Hiding detection covers attributes and inline styles only: text
  hidden by a stylesheet class or placed off screen reaches the model, and no
  script runs, so a page built by JavaScript returns little text. A network
  that requires a proxy cannot be used. Tests use a loopback server under a
  test-only policy; no real site was fetched in the automated suites, and no
  test makes a TLS handshake fail.
- **Network coverage.** Network access was tested on Windows 11 build 26300
  and WSL 6.6 (Landlock ABI 3) only, including a TLS fetch with the system
  `curl` on both. The ABI 4 TCP-bind denial has not run on a live kernel.
- **Tree limits differ by platform.** Windows limits committed private memory;
  Linux limits cgroup memory, which includes page cache and kernel memory
  charged to the tree. Linux counts threads against the process limit. CPU
  time is checked by polling and can overshoot; see [tree limits](#tree-limits).
- **Disk growth is sampled, not a quota.** The supervisor measures the
  writable roots once a second, so a tree can pass the limit between samples.
  Growth is net and applies to allocated bytes and file lengths separately, so
  a write is seen before writeback and a sparse file is counted
  conservatively. Directory entries, inode or MFT use, and filesystem metadata
  are not counted; a hard-linked file is counted once per name. Other writers
  are counted as the run's growth. A run that
  makes a writable path unreadable to the supervisor, or whose roots hold more
  than 4,000,000 entries, fails as `supervision_failed`. See
  [disk growth](#disk-growth).
- **Disk-limit coverage.** The limit was tested on Windows 11 build 26300 and
  WSL 6.6 only. The tests write 16 MiB or more and observe `limit_exceeded`;
  they do not establish behaviour under a full volume, concurrent writers, or
  inode exhaustion.
- **Linux needs a user service manager.** Every Linux run needs a delegated
  cgroup, normally a `systemd-run --user` scope. Hosts without a user systemd
  instance, or with cgroup v1 or a hybrid hierarchy, cannot run. Tree limits
  were tested on WSL 6.6 only.
- **Linux runner crash.** `PR_SET_PDEATHSIG` reaches only the direct child.
  If the runner process dies, descendants keep running until something else
  stops them. Windows kills the job when the runner's handle closes.
- **Linux reaping.** Killed descendants are reaped by the system's subreaper or
  init. Where nothing reaps orphans, cleanup is reported unconfirmed.
- **Process-group programs.** Programs that call `setsid` or `setpgid` (for
  example `setsid`, job-control shells, or Python with `start_new_session`) get
  `EPERM`.
- **Windows concurrent spawns.** A `std::process` spawn on another thread can
  inherit the runner's pipe ends during `CreateProcessW`. The run then reports
  cleanup unconfirmed instead of hanging.
- **Process-group number reuse.** After the group empties, the final signal-0
  probe could reach a new group with the same number. This needs the kernel to
  reuse the number within one 10 ms poll, and the probe sends no signal.

## Next increments

| Increment | Scope | Must establish |
| --- | --- | --- |
| C2 (0.14.0) | Linux Landlock filesystem rules and seccomp network denial | Done: see [confined runs](#confined-runs) and ADR-0028 |
| C3 (0.15.0) | Windows AppContainer | Done: see [Windows confinement](#windows-confinement), [container grants](#container-grants), and ADR-0029 |
| C4 (0.16.0) | Trusted CLI `run` and the `execute` preference | Done: see [trusted runs](#trusted-runs) and ADR-0030 |
| C5 (0.17.0) | Tree-wide memory, process, and CPU-time limits | Done: see [tree limits](#tree-limits) and ADR-0031 |
| C6 (0.18.0) | Outbound network access behind `task_network` and `--network` | Done: see [network access](#network-access) and ADR-0032 |
| C7 (0.19.0) | Sampled disk-growth limit for every trusted run | Done: see [disk growth](#disk-growth) and ADR-0033 |
| C8 (contract only, 0.19.0) | Package-network profile contract and implementation gate | Done as a contract: see [package-network profile](#package-network-profile-contract-only) and ADR-0034. No destination enforcement. |
| C9 (0.20.0) | Package-network profile records | Done as configuration: `workspace package-profile-add/show/remove` and the control journal events. No run accepts a profile. |
| C10 (0.21.0) | Trusted allowlisted package fetch | Done: see [trusted package fetch](#trusted-package-fetch) and ADR-0035. The run remains offline. |
| C11 (0.22.0) | Read-only package cache for Linux runs | Superseded by C12: Linux exposure moved to the per-workspace root, with an application-storage fallback. |
| C12 (0.23.0) | Per-workspace package cache root and scoped Windows grant | Done: see [trusted package fetch](#trusted-package-fetch) and ADR-0037. Windows needs a read-execute grant on the workspace root. |
| C13 (0.24.0) | Provider profiles and trusted approval records | Done: see the [provider authentication guide](provider-authentication.md#recorded-provider-profiles-0240) and [approval records](agent-instructions.md#approval-records). No adapter or model tool executed. |
| C14 (0.25.0) | Approval-gated model command tool | Done: see [model command tool](#model-command-tool) and ADR-0040. A CLI acceptance test qualifies one successful approved command on both hosts; confinement correctness remains the platform suites' responsibility. |
| C17 (0.28.0) | Approval-gated model edit tool | Done: see the [model edit route](file-access.md#model-edit-route-adr-0043) and ADR-0043. It starts no process; it writes one exact edit through the checkpointed apply after a writers-stopped approval. |
| C18 (0.36.0) | Approval-gated model multi-edit and create tools | Done: see the [model multi-edit and create routes](file-access.md#model-multi-edit-and-create-routes-adr-0054) and ADR-0054. They start no process; multi-edit writes through the checkpointed apply and create through the broker's exclusive create, each after a writers-stopped approval. |
| C19 (0.38.0) | Approval-gated model rename and delete tools | Done: see the [model rename and delete routes](file-access.md#model-rename-and-delete-routes-adr-0055) and ADR-0055. They start no process; delete writes through a checkpointed removal and rename through a no-replace rename, each after a writers-stopped approval. |
| C20 (0.37.0) | Registry credentials for the trusted fetch | Done: see [registry credentials](#registry-credentials) and ADR-0056. One bound secret per declared origin; no run receives it. |
| C21 (0.64.0) | Model web fetch behind `web_fetch` and a host allowlist | Done: see [model web fetch](#model-web-fetch) and ADR-0098. It starts no process; the OMGCode process fetches one public HTTPS page per call. |

Later work needs its own decisions:

- development-server networking;
- model Git and MCP tools, and any model network tool other than `web_fetch`;

Live qualification on each supported OS remains a separate gate.
