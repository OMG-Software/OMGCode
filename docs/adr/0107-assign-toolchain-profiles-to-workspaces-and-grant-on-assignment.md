# 0107: Assign toolchain profiles to workspaces and grant on assignment

- Status: Accepted
- Date: 2026-11-27

## Context

A toolchain profile was recorded once for the whole computer, and every
workspace whose `execute` preference allowed it could run any recorded
profile ([ADR-0029](0029-confine-windows-runs-in-a-workspace-appcontainer-with-recorded-grants.md),
[ADR-0100](0100-list-the-recorded-toolchain-profiles-in-the-command-tool.md)).
On Windows each workspace runs in its own AppContainer, so it can use a
profile only after its container has a read-execute grant on each of the
profile's folders that every application package cannot already read. Those
grants were a separate step, done per folder under **Container grants** or,
for discovered toolchains, with a separate command after its own
confirmation ([ADR-0103](0103-generate-toolchain-profiles-from-installed-toolchains.md)).
Registering a workspace granted nothing, and the operator then granted its
folder read-write without a folder dialog
([ADR-0104](0104-grant-the-workspace-folder-read-write-without-a-folder-dialog.md)).

An operator's model command in one workspace failed because the container had
no grant on the rustup toolchain under the user's own folder. ADR-0106 now
names the missing grant, but the operator still had to find out that a
recorded profile does nothing in a workspace until its folders are granted
there. Recording a profile looked like the decision to use it, and the grant
looked like a technical detail.

The operator's direction was that adding a toolchain to a workspace is the
consent to give that workspace's sandbox the toolchain's folders. A draft
that granted missing folders when a run started, automatically under `allow`
and after an approval under `prompt`, was written and dropped before it was
committed: it put a filesystem grant behind a model's request and behind a
per-call approval of a command, which is a different decision.

## Decision

1. **Profiles are assigned to workspaces.** A workspace records the
   identifiers of the profiles assigned to it, in assignment order. Two
   control events record the change, `profile_assigned` and
   `profile_unassigned`, and each advances the revision. Replay rebuilds the
   list and refuses an unknown workspace or profile, an assignment that
   already exists, and an unassignment of one that does not. Removing a
   profile ends every assignment of it, and unregistering a workspace drops
   its list. `workspace status` shows each workspace's assigned profiles. An
   older build refuses the new events, as it refuses any event it does not
   know. Assigning refuses a profile declared on another platform.
2. **A run needs an assigned profile.** The runner refuses a recorded profile
   that is not assigned to the run's workspace before anything starts, with
   text that names no path and says to assign it in the `settings` window or
   with `omgcode workspace profile-assign`. The run watcher withdraws a run
   whose profile was unassigned, as it does for any other settings change
   that withdraws a run. The model command tool is offered only while at
   least one profile of this platform is assigned to the workspace, and its
   `profile` parameter lists only those profiles. A command that names
   another profile is still answered with `profile_not_recorded`, whose text
   now says "assigned to this workspace". The tool schema version is 0.10.0.
3. **Assigning is the consent to grant.** On Windows, assigning plans the
   read-execute grants first: each live root the workspace's container
   cannot read yet, each folder once, skipping a folder that already has a
   recorded grant of the workspace. Folders that only an administrator
   can grant, and folders whose access-control list could not be read, are
   listed apart and not granted. The operator confirms that plan. Under the
   store's lock core plans again and refuses the assignment if the folders
   to grant changed, so no folder is granted that the operator did not see.
   Core then checks the batch against the grant limits, applies each grant,
   and records the assignment last. A failed grant leaves the profile
   unassigned; grants applied before it stay recorded, and one whose
   access-control change failed stays `pending` until it is revoked. Off
   Windows the plan is empty and assigning records only the event.
4. **Registering grants the workspace folder.** On Windows, registering a
   folder grants its container read-write on that folder when no grant of the
   workspace is recorded on it and this account can change the folder's
   access-control list. Registering an already registered folder that has no
   such grant grants it, which is how a workspace registered before this
   change gets it. A failed grant leaves the workspace registered and is
   reported.
5. **Removing revokes what is no longer needed.** Unassigning a profile, and
   removing an assigned profile, revoke the workspace's read-execute grants
   on that profile's roots that no profile still assigned to the workspace
   lists and that are not the workspace's package cache root. The workspace
   folder's read-write grant is never revoked by these. Unregistering a
   workspace revokes every grant of the workspace first and unregisters it
   only when every revocation was confirmed; otherwise it stays registered
   with the grants not yet revoked, and the operator retries. The
   unregistration primitive and replay still refuse while a grant exists
   ([ADR-0081](0081-unregister-a-workspace.md)). Any revocation still
   withdraws every run of its workspace, so each caller lists the pending
   runs before the change and waits for those of the workspaces whose grants
   it tried to revoke.
6. **The trusted CLI and the `settings` window ask for the assignment.**
   - `omgcode workspace profile-assign <workspace> <name> --revision <n>`
     plans and assigns, since running it is the consent; it prints the
     assigned profile, the grants made, and the folders not granted with the
     reason. `profile-unassign` prints what it revoked. `register` adds the
     grant it made, and `unregister` and `profile-remove` add what they
     revoked.
   - The workspace page of the `settings` window shows one switch per
     recorded profile of this platform. Turning one on runs
     `settings_assign_toolchain_profile`, whose native confirmation names the
     workspace, the profile, each folder to grant, and the folders that are
     skipped; turning one off runs `settings_unassign_toolchain_profile`,
     whose confirmation names the grants it revokes and says the workspace's
     runs stop. The register, unregister, and remove-profile confirmations
     say what they grant or revoke.
   - Adding discovered profiles with a workspace chosen also assigns them in
     the same confirmation, which lists the folders it grants. The separate
     `settings_grant_discovered_roots` command is removed. The workspace
     choice is shown on every platform.
7. **No migration.** Existing profiles start assigned to no workspace. A
   workspace's model command tool is withheld, and its CLI runs are refused,
   until the operator assigns a profile to it once. Grants already recorded
   stay, so assigning a profile whose folders are granted grants nothing new.

## Alternatives considered

- **Grant at run time.** Grant missing folders when a run starts, without a
  prompt under `allow` and after an approval under `prompt`. The grant would
  follow a model's request rather than an operator's decision about the
  workspace, and an `allow` workspace would widen its sandbox with no
  operator action at all. Dropped before it was committed.
- **Keep profiles global and grant every workspace.** Recording a profile
  would grant its folders to every registered workspace, and to each one
  registered later. That widens every sandbox for a toolchain one project
  needs, and removing it from one workspace would mean removing it from all.
- **Keep the separate grant step and only explain it better.** The
  refusal already names the missing grant (ADR-0106). Separate steps still
  let a recorded profile look usable when it is not.
- **Migrate by assigning every profile to every workspace.** It would keep
  existing runs working but turn a global setting into many per-workspace
  consents the operator never gave, and on Windows it would have to apply
  grants during an upgrade.
- **Grant every planned folder, including those that need an
  administrator.** This account cannot change them; listing them lets the
  operator ask an administrator, and a run that needs one is refused with
  ADR-0106's text.

## Consequences

- An operator decides once per workspace which toolchains it may run, and on
  Windows that decision is also the grant. The grant is not hidden: the CLI
  prints it and the `settings` window confirms it natively.
- A workspace registered before this change has no assigned profile, so its
  model has no command tool and its CLI runs are refused until a profile is
  assigned. On Windows, registering the folder again grants read-write if
  that grant is missing.
- Unassigning or removing a profile, or unregistering a workspace, can stop
  the workspace's runs, because any revocation withdraws them.
- The mapping from roots to grants is by folder: a grant whose folder is also
  a root of another assigned profile, or the workspace's package cache root,
  stays.
- Core tests cover recording, replay, refusals, removal, plans, plan
  changes, the grants a removal releases, and the workspaces a revocation
  names. Run and turn tests cover the unassigned-profile refusal, the
  watcher, and the tool list. Windows CLI and desktop service tests apply and
  revoke real grants on temporary folders; desktop tests that are not about
  registration record workspaces without granting. The desktop shell's
  native dialogs were not driven.

## References

- [ADR-0029](0029-confine-windows-runs-in-a-workspace-appcontainer-with-recorded-grants.md),
  [ADR-0081](0081-unregister-a-workspace.md),
  [ADR-0088](0088-change-execution-settings-in-the-settings-window.md),
  [ADR-0100](0100-list-the-recorded-toolchain-profiles-in-the-command-tool.md),
  [ADR-0103](0103-generate-toolchain-profiles-from-installed-toolchains.md),
  [ADR-0104](0104-grant-the-workspace-folder-read-write-without-a-folder-dialog.md),
  [ADR-0106](0106-name-the-ungranted-run-path-by-its-role.md)
- [docs/execution.md](../execution.md)
