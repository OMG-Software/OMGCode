# Filesystem and power-loss qualification plan

Status: plan only. None of this campaign has run. No result in this document
is evidence, and nothing here changes what the
[workspace file access contract](file-access.md) or
[platform support](platform-support.md) claims today.

## Purpose

Trusted apply, restore, checkpoint deletion, and ledger compaction run on Linux
ext4 and Windows NTFS. Their native tests show one rename and the ordered syncs
on one tested mount. They do not show which filesystems, kernels, and Windows
builds a release can support, or what the store and workspace hold after a
power loss. This plan defines the campaign that would answer both questions
before any release claim.

The campaign qualifies the private checkpoint store and trusted publication. It
does not qualify process confinement, the run journal, session journals, or the
derived session index.

## Operations under test

Each cell of the matrix exercises the same operations through the trusted CLI:

| Operation | Native steps that matter |
| --- | --- |
| Store initialization | Private directory creation, initialization journal, ledger creation, directory syncs |
| Apply | Payload and manifest writes, stage creation, commit-intent record, one rename onto the target, file and parent syncs, outcome record |
| Restore | The apply steps with the retained original as the proposed content |
| Checkpoint deletion | Deletion-intent record, removal of each fixed artifact name, entry directory removal, outcome record |
| Ledger compaction | Candidate create and sync, one rename onto `ledger.jsonl`, directory sync, reopen and compare |
| Acknowledgement | One appended record and its sync |

## Release filesystem matrix

A cell passes only when every functional case below passes on it. A cell that
the store or apply must refuse passes only when every refusal is the documented
error and nothing is written to the workspace or the store.

| Platform | Filesystem and mount | Expected result |
| --- | --- | --- |
| Ubuntu LTS (current and previous), Fedora (current) | ext4, default options, local block device | Supported candidate |
| Same distributions | ext4 with `data=journal`, and with `nodelalloc` | Supported candidate; record any difference |
| Same distributions | ext4 on LVM and on dm-crypt | Supported candidate |
| Same distributions | XFS, Btrfs, tmpfs, overlayfs | Apply refuses; store behaviour recorded |
| Same distributions | NFS, SMB/CIFS, FUSE | Apply and store refuse |
| WSL 2 | ext4 virtual disk | Supported candidate; WSL results do not qualify a native Linux host |
| WSL 2 | drvfs (`/mnt/<drive>`) | Apply and store refuse |
| Windows 11 (current and previous feature update), Windows Server 2025 | NTFS on a fixed local disk | Supported candidate |
| Same builds | NTFS on BitLocker | Supported candidate |
| Same builds | ReFS (including a Dev Drive), FAT32, exFAT, removable media | Apply and store refuse |
| Same builds | SMB share, mapped drive, OneDrive placeholder | Apply and store refuse |

Each run records the exact OS build, kernel, filesystem tools version, mount
options or volume flags, storage device model and write-cache setting, and the
`omgcode --version` output with the commit.

### Functional cases per cell

- The existing native suites for the platform crate, `checkpoint_storage`,
  `checkpoint_lifecycle`, and the replacement tests pass unchanged on that
  mount, with the data root and workspace both on it.
- Data root and workspace on different volumes: the store works, and apply
  refuses cross-volume staging as documented.
- Security metadata: owner, group, mode, ACLs, and the refused attribute set
  match the contract on both sides of an apply and a restore.
- Ledger compaction at the record cap, at the byte cap, and on request leaves
  the retained entries' replayed state unchanged.
- A store reaching the 128 MiB aggregate quota refuses further work without
  losing inspection.

## Power-loss campaign

### Method

Two methods are needed because neither alone is enough.

1. **Block-level replay (Linux).** Record each operation's writes with
   `dm-log-writes`, then replay every prefix that ends at a flush or FUA
   boundary onto a fresh device, and also a sample of prefixes between them.
   Mount each replayed image and run the checks below. This shows behaviour
   at every ordering point the kernel can expose. It does not show device
   cache behaviour.
2. **Physical power cut (Linux and Windows).** Use real machines with the
   operation running in a loop and a controllable power switch that removes
   power from the whole machine, not only the guest. Cut power at random times
   weighted towards the rename and sync calls. The store's fault points exist
   only in test builds, so targeting needs a separate instrumented build that
   logs a timestamp before each boundary; the shipped binary must also be cut
   untargeted, because instrumentation changes timing. Use at least
   one consumer SSD with a volatile write cache and one device with power-loss
   protection. Virtual machine resets alone do not count, because a hypervisor
   may keep host-side caches.

Windows has no equivalent of `dm-log-writes` in the inbox tooling. Its evidence
comes only from physical cuts.

### Checks after each restart

- The store opens, or refuses with a documented error. It never accepts a
  ledger that a strict replay rejects, and it never repairs one.
- Every ledger replays to a state that the operation could have produced:
  before it, after it, or an unresolved or uncertain operation that inspection
  reports. A torn final record is refused, not ignored.
- A compaction leaves either the old ledger or the new ledger at
  `ledger.jsonl`, each replaying to the same retained entries. A leftover
  `ledger.compact` is counted and ignored.
- A workspace target holds exactly the expected bytes or exactly the proposed
  bytes. Any other content is a failure, even if the ledger reports the
  operation as uncertain.
- When the ledger records success, the target holds the proposed bytes and the
  checkpoint's original and proposed payloads match their manifest.
- When the ledger records a finished deletion, the entry directory is absent.

### Volume and stopping rule

For each supported candidate cell, run every operation until at least 1,000
physical cuts have landed inside it, and replay every flush-boundary prefix.
Any single failure of the checks stops qualification of that cell until the
cause is understood and fixed, and the full count starts again.

### Known questions the campaign must settle

- Whether the Windows directory flush used after a rename persists the new
  entry. Microsoft documents no contract for it (ADR-0023), so the result can
  only be a measured observation, not a guarantee.
- Whether ext4 with default options needs a parent sync after the compaction
  rename, beyond the one the store already performs, on every tested kernel.
- Whether a device that ignores flushes can produce a state the checks above
  reject. Such a device would be recorded as unsupported, not worked around.

## Evidence and reporting

Store each run's raw logs, replay images or their hashes, and the summary table
in the release evidence location that Phase G defines. Report counts of cuts,
outcomes per check, and every failure. Do not round partial runs up to a pass.
A qualified cell changes [platform support](platform-support.md) and the
file-access contract in the same change, with an ADR when a claim is added.

## Not covered

- Concurrent writers to the workspace target. The contract requires the
  operator to stop them; the campaign keeps them stopped.
- Media corruption, bit rot, and firmware faults outside power loss.
- Recovery of history that compaction discarded.
- macOS, which is outside the first-release target.
