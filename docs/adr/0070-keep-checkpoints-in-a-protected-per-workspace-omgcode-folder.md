# 0070: Keep checkpoints in a protected per-workspace `.OMGCode` folder

- Status: Accepted
- Date: 2026-11-20

## Context

Checkpoint storage (the anchor, the ledger, and `replacement-v1/` with each
operation's payloads) lived in the per-user data directory (ADR-0023). Apply
renames a stage from that store into the workspace, so the store and the
workspace had to share one volume or mount. ADR-0069 accepted ReFS, btrfs,
XFS, tmpfs, and ZFS, but a workspace on another volume than the data
directory was still refused. A Windows Dev Drive is a separate volume by
design, and a btrfs subvolume or a second Linux mount is a separate mount.

The operator asked for a per-workspace folder that Git ignores and that
nothing outside OMGCode can quietly change.

Observed facts:

- The control journal is capped at 4096 events and is never compacted. A
  record per ledger change would exhaust it after a few hundred checkpoints.
- Landlock grants a hierarchy and cannot exclude a child, so a contained
  Linux `run` with workspace write access can write inside any workspace
  folder. A Windows AppContainer receives access only through explicit
  grants, and a protected DACL does not inherit the workspace grant.
- The broker already refuses links, devices, and short-name aliases by
  checking each opened object. A name check alone does not cover an NTFS 8.3
  alias such as `OMGCOD~1`.

## Decision

New checkpoints go to `<workspace>/.OMGCode`. Sessions, journals, the index,
credentials, skills, and the control journal stay in the data directory.

Creation and binding:

- Core creates the folder on the first checkpoint in the workspace, under the
  control lock and after the write permission check, through
  `PrivateDirectory::create_in`. On Windows it carries the protected
  current-user DACL of ADR-0023; on Linux it is mode 0700 and owned by the
  effective user. An existing folder is adopted only when it is empty and
  has exactly that owner-only shape. A folder whose inherited DACL names
  only the user, SYSTEM, and Administrators passes the private-root check
  but is refused, because a later workspace container grant would reach it
  by inheritance; a ReFS run found the gap. Anything else at that name is
  refused with `checkpoint_store_changed`.
- The control journal records `workspace_store_bound` with the folder's
  native identity and creation time. These are operational events; they do
  not change the settings revision.
- The folder holds a `.gitignore` containing `*`, so Git ignores the folder
  and its contents. The operator's own `.gitignore` is never edited.

Tamper evidence:

- The data directory keeps one head record per workspace store in
  `checkpoint-heads-v1/<workspace-id>.json`: the store ID, the committed
  ledger's size and SHA-256, and the next one when a change is in progress.
- Every ledger change (initialization, append, compaction) announces the next
  head before it writes and commits it after the ledger syncs. Opening the
  store replays the ledger and requires its digest to equal the committed or
  the announced head. A writable open commits an announced head that matches.
- A missing folder is `checkpoint_store_missing`. A replaced folder, changed
  access, a different store, a rewritten, extended, or rolled-back ledger,
  removed storage, or a missing head record is `checkpoint_store_changed`.
  Changed access means anything but the exact owner-only shape that
  creation applies, checked on every open: a Windows root open alone would
  accept an inherited DACL naming only trusted principals.
  Nothing is repaired automatically.
- While either error stands, the workspace admits no broker operation, reads
  included, exactly as for an unresolved operation: a rolled-back folder can
  hide one.

Reaching the folder:

- The broker refuses `.OMGCode` as any path component, at any depth, in any
  ASCII letter case, and lists it as blocked. On Windows each opened
  directory's normalized long name is also compared, which refuses a short
  name alias. Model tools, the trusted CLI file commands, and checkpoint
  restore targets all go through this check.
- On Windows a contained run cannot read, list, or write the folder, because
  its protected DACL grants only the current user; a CLI test runs that
  check. Whether a run with the workspace grant can rename the folder was
  not tested; the identity binding detects a rename before the next use.
  On Linux a contained run with workspace
  write access can reach it; the identity binding and the head record detect
  a change before the next use. This is detect-not-prevent.

Recovery and the legacy store:

- `omgcode file forget-checkpoint-store --workspace <id> --abandon` records
  `workspace_store_forgotten` and leaves the folder where it is. The next
  checkpoint needs the old folder moved, deleted, or emptied, then binds a
  new one. The desktop has no equivalent, as for other recovery (ADR-0060).
- The data-directory store becomes the legacy store. Nothing new is written
  there. Admission checks both stores, so a pending legacy operation still
  blocks the workspace. The trusted CLI checkpoint commands use the legacy
  store without `--workspace` and the workspace store with it, and restore
  reads a legacy checkpoint when the workspace store does not hold the ID.

## Alternatives considered

- **Operator-registered stores per volume.** Rejected: one more trusted
  setting per volume, and a mount or subvolume inside a workspace still needs
  its own store.
- **Move the whole data directory to the workspace volume.** Rejected:
  sessions, credentials, and the control journal would follow a workspace
  and be exposed to contained runs.
- **Append `.OMGCode/` to the workspace's root `.gitignore`.** Rejected:
  editing an operator file is surprising and does not cover nested
  repositories. The folder ignores itself instead.
- **Keep the ledger in the data directory and only the payloads in the
  workspace.** Rejected: the ledger and payloads would sync on different
  filesystems with no common ordering, and recovery depends on that order.
- **Record each ledger head in the control journal**, as first planned.
  Rejected: the journal's 4096-event cap and lack of compaction would end
  checkpointing after a few hundred writes. The head file is outside every
  workspace and private like the journal.
- **Refuse the name only by string.** Rejected: an 8.3 alias reaches the same
  folder on NTFS.

## Consequences

- The model edit, multi-edit, and delete tools and trusted apply and restore
  work on any accepted filesystem (ADR-0069) whether or not the data
  directory is on the same volume. A file on another mount or subvolume
  inside the workspace is still refused by the same-filesystem checks.
- `git clean -x`, a backup restore, or a manual delete of `.OMGCode` blocks the
  workspace until the operator forgets the store. Its checkpoints remain
  only as files; no command reads a forgotten folder.
- On Linux a contained run can delete or change checkpoints. The next use
  reports it; the checkpoints are not protected from that run.
- The head record narrows but does not close crash windows. A head that was
  announced and never committed accepts either ledger, which is the intent.
  Neither the folder nor the head is power-loss qualified.
- The ledger head check rereads the whole ledger on each open, as the replay
  already did.
- ADR-0023 and ADR-0053 are amended by this decision.

## References

- [ADR-0023: Store Windows checkpoints with verified ACLs](0023-store-windows-checkpoints-with-verified-acls.md)
- [ADR-0053: Compact the checkpoint ledger in place](0053-compact-the-checkpoint-ledger-in-place.md)
- [ADR-0060: Change workspace settings from the desktop with native dialogs](0060-change-workspace-settings-from-the-desktop-with-native-dialogs.md)
- [ADR-0069: Allow ReFS, btrfs, XFS, tmpfs, and ZFS for trusted apply](0069-allow-refs-btrfs-xfs-tmpfs-and-zfs-for-trusted-apply.md)
- [File access](../file-access.md), [platform support](../platform-support.md),
  [session storage](../session-storage.md)
- Microsoft: [Naming files, paths, and namespaces (short names)](https://learn.microsoft.com/windows/win32/fileio/naming-a-file)
- Linux: [Landlock](https://docs.kernel.org/userspace-api/landlock.html)
