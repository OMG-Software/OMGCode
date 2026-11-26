# 0088: Change execution settings in the settings window

- Status: Accepted, amended by [ADR-0104](0104-grant-the-workspace-folder-read-write-without-a-folder-dialog.md)
- Date: 2026-11-24

## Context

[ADR-0059](0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md)
and [ADR-0060](0060-change-workspace-settings-from-the-desktop-with-native-dialogs.md)
left toolchain profiles, package-network profiles, and container grants to the
trusted CLI, and [ADR-0082](0082-manage-skills-in-the-settings-window.md)
(decision 6) kept them read-only when it opened skills. An operator reported
that nothing in **Advanced** can be added, edited, or removed, and chose to
make all three remaining groups changeable from the window.

Each group widens or records what a run may reach. A toolchain profile names
directories a run can read and execute and sets its environment. A
package-network profile records registry origins; no run uses it. A container
grant changes a folder's access-control list on disk for a workspace's
AppContainer. Core already validates all three and the CLI already changes
them (`workspace profile-add`, `profile-remove`, `package-profile-add`,
`package-profile-remove`, `container-grant`, `container-revoke`).

## Decision

1. **Six `settings`-only commands, each calling the CLI's core operation.**
   `settings_add_toolchain_profile`, `settings_remove_toolchain_profile`,
   `settings_add_package_profile`, `settings_remove_package_profile`,
   `settings_grant_container_folder`, and `settings_revoke_container_grant`
   call `add_profile`, `remove_profile`, `add_package_profile`,
   `remove_package_profile`, `grant_container_access`, and
   `revoke_container_grant`. Validation stays in core, and no new control
   journal event exists: the events are the CLI's.
2. **No path crosses the webview.** A toolchain add carries the name, the
   variables, and how many read-only roots (1 to 16) and `PATH` entries (0 to
   16) to choose; Rust opens one native folder dialog per folder, in order, and
   starts a `PATH` dialog at the first root. A grant carries a registered
   workspace and the access; Rust opens the folder dialog. New
   `FolderPurpose` values give each dialog a fixed title. Variable names and
   values and package origins are typed in the webview: they are stored in
   plain text by design, or parsed by core, and are not paths.
3. **Every change is confirmed natively, with text Rust composes.**
   - Adding a toolchain profile lists the name, the platform, every root and
     `PATH` entry, and the variable **names**, never the values, and says a
     profile grants nothing by itself.
   - Adding a package-network profile lists the origins as core parsed them
     (`parse_origin`), so a bad origin is refused before any dialog and the
     operator confirms what will be recorded, and says the profile is
     configuration only.
   - A grant names the workspace, the folder, and the access, and says the
     folder's access-control list changes on disk for that workspace's
     AppContainer until revoked.
   - Removals and revocations are confirmed. A package-profile removal says how
     many bound credentials are unbound and their stored keys deleted, as the
     CLI does.
   Names and paths are display-safe lines, bounded.
4. **Order and races follow ADR-0060 and ADR-0082.** Rust reads the store,
   checks the revision, releases the store, opens every dialog, then applies
   with the revision the operator confirmed, so a change made meanwhile is
   refused as `stale_revision`. Core's revocation needs no revision; Rust
   checks the confirmed revision under the lock it holds before revoking.
5. **Removals and revocations stop the runs they withdraw, as the CLI does.**
   The commands use the settings-change path that lists the withdrawn runs
   under the lock and waits for them (`runs_not_stopped`, `runs_unchecked`).
   A revocation also names the workspace's pending runs, as `container-revoke`
   does, and an unreadable run journal is reported after the revocation, not
   before. A run already started keeps its recorded toolchain profile.
6. **Container grants are Windows only.** Both grant commands return
   `unsupported_platform` before any dialog elsewhere, and the overview carries
   `container_grants_supported` so the window says so and offers no control. A
   grant whose access-list change fails stays `pending`, and a failed
   revocation stays `revoking`; the commands fail with
   `container_access_failed` with fixed text that names no path or account, the
   window reads the settings again, and **Revoke** is the retry, as for the CLI.
7. **No edit.** Core has no edit operation for any of the three, and an edit
   would be a non-atomic remove then add that could leave the operator without
   the profile. The window offers add and remove only, and its help says so.
8. **What stays CLI-only.** Binding or removing a registry credential, the
   trusted package fetch, `run`, the data folder's other setup, and the
   checkpoint commands. A package-profile removal unbinds credentials because
   the core removal event does; the window cannot bind one.
9. **ADR-0059, ADR-0060, and ADR-0082 are narrowed.** Their statements that
   toolchain profiles, package-network profiles, and container grants change
   only through the trusted CLI no longer hold; the rest of those decisions
   stands.

## Alternatives considered

- **Type paths in the webview.** A compromised window could name any
  folder. Rejected for the reason in ADR-0059 decision 4.
- **Keep a server-side draft and one command per folder.** It would show each
  chosen root in the form before the change, but it adds state that must be
  expired and bounded. Counts plus one confirmation that lists every folder
  give the operator the same decision with no draft.
- **An edit through remove then add.** Not atomic: a failure between the two
  loses the profile, and a grant revoked and re-granted leaves a window with
  no access. Rejected; core would need a real edit operation first.
- **Confirm only the widening changes.** A removal changes what runs can do and
  a package-profile removal deletes keys, so each is confirmed too.
- **Show variable values in the confirmation.** The values are plain text by
  design and may hold something the operator did not mean to share on a shared
  screen; the names are the decision.
- **Add per-group commands for credentials.** Out of scope: binding a
  credential has its own write-only design (ADR-0056, ADR-0071).

## Consequences

- The three groups are changeable from the window with the CLI's validation and
  events, and the CLI still works on the same store.
- The service has six more methods, the shell six more `settings` commands
  (56 in all and 24 in `settings` once ADR-0081 to ADR-0089 merged in version
  0.56.0), and the overview one more field.
- A refusal by core that depends on the folders (overlap, a `PATH` entry outside
  a root) arrives after the dialogs, because the window does not duplicate
  core's checks. The error is controlled text.
- A profile or grant that is wrong is removed and added again; there is no edit.
- No test applies a container grant, because it changes the host's access-control
  lists; the paths before the change, the confirmation text, and the platform
  refusal are tested. The native dialogs and the real shell were not driven by
  automation, and nothing ran on Linux.

## References

- [docs/desktop-interface.md](../desktop-interface.md)
- [docs/execution.md](../execution.md)
- [docs/workspace-permissions.md](../workspace-permissions.md)
- [ADR-0059](0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md),
  [ADR-0060](0060-change-workspace-settings-from-the-desktop-with-native-dialogs.md),
  [ADR-0082](0082-manage-skills-in-the-settings-window.md),
  [ADR-0056](0056-send-registry-credentials-to-one-declared-origin.md)
