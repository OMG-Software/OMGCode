# 0069: Allow ReFS, btrfs, XFS, tmpfs, and ZFS for trusted apply

- Status: Accepted
- Date: 2026-11-20

## Context

Trusted apply, checkpoint storage, and AppContainer grants accepted only
NTFS on Windows (ADR-0023, ADR-0024, ADR-0029). Linux apply accepted only
ext4 (ADR-0022). Each check compared one filesystem name and refused the rest.
Those decisions recorded the other filesystems as untested, not as unsafe.

Operators keep workspaces on other volumes. A Windows Dev Drive is ReFS, and
Linux hosts commonly use btrfs, XFS, or ZFS. On those volumes the model edit,
multi-edit, and delete tools failed with `native_error`, and the Windows
message (`private storage requires NTFS on Windows`) named the private store
even when the workspace volume was the cause.

Observed facts:

- Windows 11 build 26300 reports these capabilities for a local ReFS volume
  through `GetVolumeInformationW`: persistent ACLs, POSIX unlink and rename,
  case-preserved Unicode names, hard links, named streams, extended
  attributes, and open by file ID. NTFS reports the same required set.
- File identity is the 128-bit `FILE_ID_INFO` everywhere on Windows, so ReFS
  128-bit file IDs need no change. Hard-link support does not widen it: a
  source, stage, or private file must report one link, so a second name for
  the same file ID is refused on ReFS as on NTFS.
- The unchanged Windows platform suite (169 tests: replacement, private store,
  hard links, alternate streams, extended attributes, POSIX rename and delete,
  directory flush, container grants) passes with `TEMP` on that ReFS volume.
  The fixtures need a per-user temporary directory; a new volume root grants
  Authenticated Users Modify only, which lacks `WRITE_OWNER`.
- The unchanged Linux platform suite (142 tests) passes on ext4 and on tmpfs
  under WSL kernel 6.6.

## Decision

Accept a filesystem only when it is on a fixed list and, on Windows, reports
every required capability. Every other filesystem still fails closed before
any artifact is written, with an `Unsupported` error that names the role and
the filesystem.

Windows (`private_store/windows/volume.rs`):

- Accept `NTFS` and `ReFS` by exact name, and require
  `FILE_PERSISTENT_ACLS`, `FILE_SUPPORTS_POSIX_UNLINK_RENAME`,
  `FILE_CASE_PRESERVED_NAMES`, `FILE_UNICODE_ON_DISK`, and
  `FILE_SUPPORTS_EXTENDED_ATTRIBUTES`. Requiring EA support keeps the
  extended-attribute probe meaningful on every accepted volume.
- The same check serves the private store, the replacement target, and the
  container grant directory. The error names which one, for example
  `workspace volume uses exFAT; file writes require NTFS or ReFS`.
- Refuse files that carry the ReFS integrity-stream or no-scrub attribute.
  A stage would have to reproduce them and no record carries them.

Linux:

- Accept the mountinfo types `ext4`, `btrfs`, `xfs`, `tmpfs`, and `zfs` for
  the retained target mount. The refusal reads
  `workspace mount uses nfs4; file writes require ext4, btrfs, XFS, tmpfs, or ZFS`.
- Choose the inode-attribute policy from the open file's own `statfs` magic,
  not from a caller or path. ext4 may carry only `FS_EXTENT_FL`; the others
  may carry no `FS_IOC_GETFLAGS` flag. `FS_IOC_FSGETXATTR` must report no
  settable flag, project ID, or reserved byte. btrfs `NOCOW` and
  compression, XFS allocation hints, and project inheritance are refused.
  XFS reports `FS_XFLAG_HASATTR`, a read-only attribute-fork status bit, on
  new files with no extended attribute (observed on a fresh XFS volume),
  so that bit is ignored; extended attributes are refused separately. An
  extent or CoW extent size reported without its hint flag is the
  filesystem default, which a new stage file also gets, so it is ignored.
- Only tmpfs may lack the attribute ioctls (before Linux 6.0); that is read
  as no attributes. Elsewhere a missing ioctl is refused.
- The generic requirements are unchanged and still fail closed: `openat2`,
  procfs, birth time, `RENAME_NOREPLACE` for rename, and the same mount for the
  private store and the target. tmpfs reports birth time from Linux 5.18; a
  ZFS release without `statx` birth time or `RENAME_NOREPLACE` is refused by
  those existing checks.

The private store and the workspace must still share one volume or mount.
This decision does not move the store; a workspace on another volume than the
data directory is still refused by the existing same-filesystem checks.

CI gains a filesystem job per new filesystem. It mounts a ReFS VHDX on
Windows Server 2025 or a btrfs, XFS, ZFS, or tmpfs volume on Ubuntu 24.04,
points the temporary directory at it, checks that the temporary directory is
on the expected filesystem through an opt-in test
(`OMGCODE_TEST_EXPECT_FS`), and reruns the platform, core, and CLI
file-access suites there.

This is functional qualification only. It makes no power-loss or crash-safety
claim for any filesystem; the [qualification plan](../filesystem-qualification.md)
has still not run for any cell.

## Alternatives considered

- **Keep NTFS and ext4 only.** Rejected: the earlier ADRs refused the other
  filesystems because nothing had tested them. The unchanged suites pass on
  ReFS and tmpfs locally, and the new CI cells run them on btrfs, XFS, and
  ZFS. Operators on a Dev Drive or btrfs had no working write path.
- **Accept any Windows volume that reports the capability flags.** Rejected:
  flags describe features, not tested behaviour. A filter-driver or network
  filesystem can report them; an untested name stays refused.
- **Accept any Linux filesystem that supports `renameat2`.** Rejected, as in
  ADR-0022: inode attributes and directory sync differ per filesystem and need
  their own policy and tests.
- **Fall back to a copy or a non-atomic write on FAT, exFAT, NFS, or SMB.**
  Rejected: it would weaken the single verified publication and the ACL and
  identity checks that the apply path depends on.
- **Preserve ReFS integrity-stream attributes.** Deferred: it needs a recorded
  field in the checkpoint manifest and a stage that matches before publication.
  Refusal keeps the existing contract.
- **Treat overlayfs as supported.** Rejected: its device and inode numbers can
  change on copy-up, which breaks the identity checks.

## Consequences

- The model edit, multi-edit, and delete tools, trusted CLI apply and restore,
  checkpoint storage, and Windows container grants work on ReFS, btrfs, XFS,
  tmpfs, and ZFS when the private store is on the same volume or mount.
- tmpfs is not persistent. A checkpoint on tmpfs is lost at unmount or reboot.
- SELinux hosts still refuse checkpoint capture of files that carry a
  `security.selinux` extended attribute; that is a separate limit.
- Windows refusals that used to name private storage now name the actual role.
- The CI ZFS cell depends on the runner kernel's ZFS module, and the ZFS
  cell has not run on a developer host. The btrfs and XFS suites passed on
  loop mounts under WSL kernel 6.6, and tmpfs ran there too. No Dev Drive
  was confirmed in the local runs, so Dev Drive coverage rests on Dev Drive
  being ReFS.
- ADR-0022, ADR-0023, ADR-0024, and ADR-0029 are amended by this decision.

## References

- [ADR-0022: Publish one verified stage on Linux ext4](0022-publish-one-verified-stage-on-linux-ext4.md)
- [ADR-0023: Store Windows checkpoints with verified ACLs](0023-store-windows-checkpoints-with-verified-acls.md)
- [ADR-0024: Publish one verified stage on Windows NTFS](0024-publish-one-verified-stage-on-windows-ntfs.md)
- [ADR-0029: Confine Windows runs in a workspace AppContainer with recorded grants](0029-confine-windows-runs-in-a-workspace-appcontainer-with-recorded-grants.md)
- [File access](../file-access.md), [platform support](../platform-support.md),
  [filesystem qualification plan](../filesystem-qualification.md)
- Microsoft: [`GetVolumeInformationByHandleW`](https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-getvolumeinformationbyhandlew),
  [ReFS overview](https://learn.microsoft.com/windows-server/storage/refs/refs-overview),
  [Dev Drive](https://learn.microsoft.com/windows/dev-drive/)
- Linux: [`statx(2)`](https://man7.org/linux/man-pages/man2/statx.2.html),
  [`ioctl_iflags(2)`](https://man7.org/linux/man-pages/man2/ioctl_iflags.2.html),
  [`statfs(2)`](https://man7.org/linux/man-pages/man2/statfs.2.html)
