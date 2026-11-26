# 0104: Grant the workspace folder read-write without a folder dialog

- Status: Accepted
- Date: 2026-11-26

## Context

On Windows every run in a workspace, including the model's command tool,
runs in that workspace's AppContainer. The container starts with no access
to the operator's files, and registering a folder as a workspace does not
give it any. A run is refused until the workspace folder holds an active
read-write container grant (`workspace_access` in
`crates/omgcode-core/src/run.rs`).

[ADR-0088](0088-change-execution-settings-in-the-settings-window.md)
(decision 2) made **Grant folder…** in the `settings` window open a native
folder dialog for every grant. Core accepts read-write only on the
workspace's own directory (`check_grant_scope`), so for read-write the dialog
could only repeat the workspace folder or pick a folder core then refused.
The confirmation then read "Give the workspace `<folder>` read and write
access to the folder `<folder>`?". An operator whose model run was refused
with "grant the workspace directory read-write to its container first"
asked which folder to choose, and then read the grant as giving a folder
access to itself. The text did not say who receives the access: the
workspace's sandbox.

## Decision

1. **A read-write grant opens no folder dialog.**
   `settings_grant_container_folder` with `read_write` uses the workspace's
   recorded directory. The request is unchanged: a revision, a registered
   workspace, and an access, never a path. A read-execute grant still opens
   the dialog. A recorded path that this system cannot form is refused as
   `invalid_request` before any confirmation.
2. **Core still decides.** The window adds no rule. Core opens the path,
   checks that the directory there is the recorded one by volume and file
   identity, and refuses a folder that was moved or replaced, exactly as
   before. The revision is checked before the confirmation and again by core
   with the confirmed revision, so a change made meanwhile is refused as
   `stale_revision` (ADR-0088 decision 4).
3. **The text names who gets the access.** Every grant and revocation
   confirmation, the discovery grant confirmation (ADR-0103), the folder
   dialog title, and the group's help say that the access goes to the
   workspace's AppContainer, the Windows sandbox its commands run in. The
   read-write confirmation names the folder once, as the workspace's, and
   says that registering the folder gave the sandbox no access and that
   every run needs this grant. The form's access options read **Read and
   execute a folder you choose** and **Read and write the workspace's own
   folder**, the button reads **Grant the workspace folder…** for read-write,
   and a hint under the selects says what each choice does.
4. **The run refusal says where to grant.** The fixed text becomes "the
   workspace's sandbox has no read-write grant on the workspace folder; grant
   it first under Settings, Advanced, Container grants, or with `omgcode
   workspace container-grant`". It names no path. The refusal during a run
   is unchanged.
5. **ADR-0088 decision 2 is narrowed** for read-write grants only. The CLI is
   unchanged: `workspace container-grant` still takes a path, and core
   refuses any other folder for read-write.

## Alternatives considered

- **Keep the dialog and only reword the text.** The dialog would still ask
  for a choice that has one valid answer, and a wrong pick would still fail
  only after the dialog, with core's refusal.
- **Start the dialog at the workspace folder.** Less wrong, but it still
  offers a choice with one valid answer.
- **Grant read-write automatically when a workspace is registered.** It would
  change a folder's access-control list on disk without a separate,
  revocable decision. Registration is configuration only, and
  ADR-0060 requires a confirmation before each change that widens access.
- **Name the container by its SID or `OMGCode.Workspace.<id>`.** Accurate,
  but the identifier explains nothing to the operator. The confirmations name
  the AppContainer and call it the workspace's sandbox. The execution guide
  keeps the name.

## Consequences

- The read-write grant takes one confirmation and no folder dialog. A
  read-execute grant is unchanged apart from its text.
- The window and the CLI still record the same events through the same core
  call, and core's checks are unchanged.
- A model that relays the run refusal now tells the operator where to grant.
- No test applies a container grant, because that changes the host's
  access-control lists. Windows integration tests check that a read-write
  grant opens no folder dialog, that its confirmation names the folder once,
  and that a confirmed grant reaches core with the confirmed revision (core
  refuses a stale one before it records anything). The native dialogs and the
  real shell were not driven by automation.

## References

- [ADR-0088](0088-change-execution-settings-in-the-settings-window.md),
  [ADR-0060](0060-change-workspace-settings-from-the-desktop-with-native-dialogs.md),
  [ADR-0103](0103-generate-toolchain-profiles-from-installed-toolchains.md),
  [ADR-0029](0029-confine-windows-runs-in-a-workspace-appcontainer-with-recorded-grants.md)
- [docs/execution.md](../execution.md#container-grants)
- [docs/desktop-interface.md](../desktop-interface.md)
