# 0053: Compact the checkpoint ledger in place

- Status: Accepted; amended by
  [ADR-0070](0070-keep-checkpoints-in-a-protected-per-workspace-omgcode-folder.md),
  which records the ledger head before and after each compaction
- Date: 2026-11-17

## Context

The replacement ledger (`replacement-v1/ledger.jsonl`) records every checkpoint
operation, deletion, and acknowledgement. Through 0.34.0 it was bounded at
4,096 records and 8 MiB, and no record was ever removed. A successful apply
writes five records and its deletion two, so an installation could perform
about 580 applies with deletion in its lifetime. After that every apply,
restore, and deletion was refused, and the only recovery was to remove the
store by hand. [ADR-0025](0025-restore-and-delete-checkpoints-explicitly.md)
recorded compaction or rotation as future work.

The ledger is the store's only evidence. Replay must rebuild every retained
entry's state from it. The store accepts only a ledger that a strict replay
validates. An operator can stop other writers to the store's private directory,
but no claim about power-loss behaviour is qualified for either platform, and
Windows directory flushes keep the ADR-0023 limitation.

## Decision

The [workspace file access contract](../file-access.md#implemented-ledger-compaction-phase-a5)
owns the details.

- **Bounds.** The ledger may hold 65,536 records and 64 MiB, with 64 KiB per
  record. The store's aggregate quota grows from 64 MiB to 128 MiB so that a
  full ledger does not consume the payload budget. The 64-entry bound is
  unchanged.
- **Format.** A compacted ledger keeps the original `created` record byte for
  byte. A `compacted` record follows as the second record. It holds the
  replaced ledger's size and SHA-256, its record count, and the revision of the
  last started operation. One `checkpoint_carried` record follows for each
  retained entry in recorded order. Each carries that entry's whole recorded
  state. Replay rebuilds a carried entry through the ordinary transitions, so a
  state that a real ledger cannot reach is refused. A carried entry must be
  resolved. Later starts may not go below the recorded revision floor.
- **Triggers.** The trusted `file compact-checkpoints --discard-history` command
  compacts on request. Apply, restore, and deletion compact first when the
  ledger lacks room for their records. Neither path runs while an operation is
  unresolved or a deletion is unfinished. Neither writes anything when nothing
  would be dropped or when the rewrite would be larger.
- **Publication.** The store replays the candidate in memory and requires the
  retained entries to match exactly. It writes `ledger.compact` with an
  exclusive private create, syncs it, and reads it back. It checks that the
  live ledger is unchanged and closes its handle. One handle-relative native
  rename then replaces `ledger.jsonl`: `renameat` on Linux and a POSIX-semantics
  replace-if-exists rename on Windows. The store flushes the directory, reopens
  the ledger, and compares its bytes. A failure after the candidate exists
  poisons the writer and is reported as uncertain. Nothing is retried. A stale
  candidate counts against the quota until the next compaction removes it.
- **Replay cost.** An append validates only its own record against the replayed
  state. Operation IDs are indexed, so a full replay is linear in its records.

## Alternatives considered

- **Raise the caps only.** This postpones the failure. A higher fixed cap
  still ends an installation's use of checkpoints, and replay cost grows with
  lifetime use.
- **Rotate to numbered ledgers.** Keeping every replaced ledger preserves the
  history but makes the store unbounded, or needs a second eviction rule. The
  store would also have to validate a chain of files on every open.
- **Compact only on operator request.** An installation that reaches the cap
  would fail an apply without warning. The automatic trigger runs only where
  the operation would otherwise be refused, and it uses the same path.
- **Compact into a second store and switch an anchor.** This needs a new
  pointer record and its own recovery rules. A single atomic rename within the
  existing directory keeps one ledger name and one anchor.
- **Drop retained history too and keep only a summary.** A carried record
  already holds everything replay needs. A free-form summary could not be
  checked against the ordinary transitions.

## Consequences

- An installation can apply, restore, and delete without a lifetime limit,
  provided at most 64 entries are retained.
- Deleted entries' history is discarded. Only the replaced ledger's digest,
  size, and record count remain. The digest cannot reproduce or verify the
  history. Each compaction replaces the previous claim, so the digests form a
  chain only if an operator keeps each replaced ledger.
- Operation-ID reuse is detected only against the history the ledger still
  holds. The store creates random version-4 IDs.
- Readers older than 0.35.0 refuse a ledger with either new record type.
  Downgrading after compaction needs the store to be removed by hand.
- A full open still reads and parses up to 64 MiB, and every admission still
  rereads and digests managed files within the 128 MiB quota.
- Neither the rename nor the directory flush is power-loss qualified. The
  [filesystem qualification plan](../filesystem-qualification.md) describes
  the campaign that has not run.

## References

- [Workspace file access](../file-access.md)
- [ADR-0023](0023-store-windows-checkpoints-with-verified-acls.md)
- [ADR-0025](0025-restore-and-delete-checkpoints-explicitly.md)
