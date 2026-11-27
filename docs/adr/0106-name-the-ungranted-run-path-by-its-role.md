# 0106: Name the ungranted run path by its role

- Status: Accepted
- Date: 2026-11-26

## Context

On Windows a run starts only when the workspace's AppContainer can already
use every path the run lists: read-execute on each toolchain-profile root and
the package cache root, read-write on the workspace folder, and read-execute
on the resolved program
([ADR-0029](0029-confine-windows-runs-in-a-workspace-appcontainer-with-recorded-grants.md)).
The runner checks each path's live access-control list before anything
starts, and it accepts a grant to the container or an entry for every
application package.

When a path failed, the refusal was one fixed sentence, "a confinement path
is not granted to the container at its listed level", or, for the program,
"the program is not readable and executable by the container". Neither said
which path failed or what access it needed. The trusted CLI `run` command and
the model's command tool both reported that text, and a model relays the
tool's text to the operator.

An operator hit this from the desktop chat with a `rust` profile of four
roots. Three were under `Program Files` and `C:\Windows`, which every
application package can read. The fourth, the rustup toolchain under the
user's own folder, had no grant. The model could only ask which path had
failed, and the operator could not tell from the error. It took reading each
root's access-control list by hand to find the missing grant.

The tool result is sent to the model provider as soon as it is recorded.
A host path outside the workspace can name the user's account, for example
`C:\Users\<name>\.rustup\…`. No other run refusal includes a host path: they
are fixed text plus identifiers, and ADR-0104's workspace-grant refusal says
where to grant without naming a folder.

## Decision

1. **The runner reports which path failed.** On Windows, a path or program
   the container cannot use at the level it needs returns
   `ProcessError::Ungranted` with an `UngrantedPath`: the entry
   (`ReadOnly(index)`, `ReadWrite(index)`, or `Program`), the path as listed,
   the access needed, and the access the container has. A DACL that cannot
   be read is still `ProcessError::Confinement`. The display text names the
   entry and both levels, never the path. The paths checked, their order, and
   the levels are unchanged.
2. **Core names the path by its role.** Core keeps the paths it listed for
   the run: the profile name and roots in recorded order, the package cache
   root, and the workspace folder. It matches the refused path against them
   by path, because the lists hold other entries in an order that differs by
   platform. The one index it uses is a read-only entry's: every platform
   lists the profile roots first among the read-only entries, so a refused
   read-only entry whose position holds a root with that path is that root.
   A package cache root that is also a profile root is then named by the
   entry Windows refused, not by whichever list core checks first.
   `RunError::NotGranted` carries an
   `UngrantedRunPath` with the role (`workspace`, `profile_root` with the
   profile and its 1-based position, `package_cache`, `program`, or `other`),
   the access needed and held, and the path. The run journal still records
   `not_started` with no confinement, as for any other refused start.
3. **The text that reaches the model names the role and the access, not the
   path.** For example: "the workspace's sandbox has no access to root 4 of
   toolchain profile `rust`, which needs read-execute. The settings window
   and `omgcode workspace profile-show rust` list the profile's roots in
   order; grant that folder read-execute under Settings, Advanced, Container
   grants, or with `omgcode workspace container-grant`". Profile names are
   operator-chosen identifiers that the command tool already lists to the
   model ([ADR-0100](0100-list-the-recorded-toolchain-profiles-in-the-command-tool.md)).
   A workspace-folder refusal says the recorded grant may have been changed
   outside OMGCode and should be revoked and granted again. The command
   tool's result code stays `command_failed`.
4. **The trusted CLI also shows the path.** `run` keeps the same `message`
   and adds `refused_path` beside it, with the role, `needed`, `granted`, and
   the path as `path` (when it is valid Unicode), `path_display`, and
   `path_native`, as the CLI already reports journal paths.

## Alternatives considered

- **Put the path in the message.** This is the most direct fix for the
  operator, but the same text is the model's tool result and goes to the
  provider automatically. A program that runs can print host paths in its
  output; that is the program's output, which the operator chose to run. A
  refusal is core's own text, written before anything runs.
- **Keep the path out of the model text but not record the reason.** Without
  a role the model still cannot tell the operator what to grant, and the
  operator has to repeat the access-control-list check by hand.
- **Record the reason in the run journal.** It would let the desktop's
  processes tab show it, but it changes a validated journal record, which
  older builds would then refuse to read. The role and position already lead
  to the folder in the settings window.
- **Report the list index only.** Indexes depend on how each platform builds
  the lists, and the operator never sees those lists. Core uses a read-only
  entry's index only to tell apart two roles that share a path.
- **Say whether this account can apply the grant**, as toolchain discovery
  does ([ADR-0103](0103-generate-toolchain-profiles-from-installed-toolchains.md)).
  It would add another access check after a refusal for a case the settings
  window's grant already reports. Not done now.

## Consequences

- A model that relays a refused run can tell the operator which root of
  which profile to grant, at which level, and where.
- An operator on the CLI gets the path itself. An operator in the desktop
  finds it from the profile and position in the settings window; the
  transcript does not show it.
- The model and the provider do not receive the host path from a refusal.
  This is not a general rule that model-visible text never contains host
  paths: command output still can.
- Windows platform tests check the reported entry, path, and levels for an
  ungranted read-only path, a read-execute grant listed as read-write, and
  an ungranted program, and that the error text leaves the path out. Core
  tests check every role's text for the access and where to grant, and that
  no path component appears. Windows CLI tests check a real ungranted
  profile root and package cache root end to end, with real grants on
  temporary folders.

## References

- [ADR-0029](0029-confine-windows-runs-in-a-workspace-appcontainer-with-recorded-grants.md),
  [ADR-0100](0100-list-the-recorded-toolchain-profiles-in-the-command-tool.md),
  [ADR-0103](0103-generate-toolchain-profiles-from-installed-toolchains.md),
  [ADR-0104](0104-grant-the-workspace-folder-read-write-without-a-folder-dialog.md)
- [docs/execution.md](../execution.md#environment-and-confinement)
