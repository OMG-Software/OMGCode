# 0061: Record run withdrawals in the settings journal

- Status: Accepted
- Date: 2026-11-19

## Context

A trusted run has a watcher. Each time the control journal changes, the
watcher reads the live settings and stops the run if they no longer allow it
([execution contract](../execution.md#settings-changes-during-a-run)). A
settings command records its change, releases the control lock, and waits for
the runs that the settings withdraw.

Reading only the live settings misses a withdrawal that is undone before the
watcher reads it. For example, `execute` is turned off and then on again by
another command between two polls (every 50 ms). The watcher then sees
`execute` on and keeps the run going. The command that turned it off found no
withdrawn run once its lock was released, so it reported success. Two
withdrawals already avoid this with counts that replay derives from the
journal: `model_tools` revocations and container-grant revocation requests.
Others did not, including `execute`, `read`, `write`, `host_services`,
`task_network`, the package cache root, and the development root.

The security rules require that narrowing permissions stops tasks that retain
revoked access and that cleanup is observed before revocation is reported
complete ([security rules](../security-coding-rules.md)). The gap was found in
review of the desktop settings window (ADR-0060), which uses the same core
functions as the CLI.

## Decision

1. **A settings change records the runs it withdrew.** While it still holds
   the control lock, right after it records its change, the command calls
   `run::record_withdrawn_runs`. That lists the pending runs that the new
   settings no longer allow, using the same rule as the watcher, and appends
   them in a `runs_withdrawn` event. Runs already recorded are skipped. Runs
   the settings still allow are not recorded, so a change stops only the runs
   it affects.
2. **The event keeps the settings revision.** Like a container revocation
   request, it confirms that access a revisioned change removed stays
   removed. A window that read revision N and made a change still sees N+1.
3. **Replay derives the set.** The snapshot holds the set of withdrawn run
   identifiers, derived on replay and never serialized. An event holds 1 to
   256 identifiers; more are split. Replay refuses an empty or longer event,
   a nil identifier, and an identifier already recorded.
4. **The watcher and the stop check honour it.** A run's watcher stops a
   recorded run with the reason "a settings change withdrew this run", even
   when the live settings allow it again. `stop_withdrawn_runs` waits for
   every pending recorded run as well as the runs the live settings withdraw.
5. **Every caller records.** The desktop settings changes and the CLI
   commands `workspace permissions`, `set-root`, `set-package-cache`,
   `clear-package-cache`, `profile-remove`, `package-profile-remove`, and
   `provider-profile-remove` record before they release the lock.
   `container-revoke` keeps its existing per-workspace count and named runs.

The watcher cannot read the settings while a command holds the lock, and
runs start only under it. So a watcher never sees a change without its
record, and no run can start between the change and the record.

## Alternatives considered

- **More per-workspace counts derived on replay**, one for each kind of
  withdrawal, as for `model_tools`. This needs no new event and is atomic
  with the change. It cannot tell whether a new development root still
  contains a workspace, because that is a filesystem fact replay must not
  read. A count of all root changes would stop runs that the change left
  allowed. Separate counts per preference would also have to be matched to
  what each run selected. Rejected as imprecise.
- **A withdrawal record in the run journal.** The run journal knows each
  run's outcome, so it could drop records of finished runs. The watcher
  would also have to watch the run journal, and the record would be written
  under two locks. Rejected for the added I/O and lock ordering.
- **Stopping every run of the workspace on any narrowing change.** Simple,
  but it stops runs the change does not affect. Rejected.
- **Only reporting the gap.** Listing withdrawn runs under the lock and
  passing them to the stop check made the command report `runs_not_stopped`
  instead of success, but the run kept going. Rejected as incomplete; this
  was the first fix in review and is replaced by this decision.

## Consequences

- A run that a change withdrew stops even if access is restored before its
  watcher reads the settings. Restoring access does not restore a run.
- The control journal gains an event type. A binary from before 0.41.0
  refuses a journal that contains one, as for any new event type.
- Each recorded run adds about 40 bytes to the control journal, and only
  when a change finds pending runs it withdraws. The journal's existing event
  and byte limits bound this. Recorded identifiers are kept for the life of
  the journal.
- If the record cannot be written, for example because the journal is full
  or the run journal cannot be read, the change stays recorded. The command
  still stops what the live settings withdraw, then fails: the CLI with code
  `run`, the desktop with `runs_not_stopped`. The withdrawal event uses no
  grant or skill reserve, so a full journal can lose this protection but
  never strands a container revocation.
- A process that crashes between the change and the record leaves the change
  without a record. The live-settings check still applies.
- Verified on Windows by unit tests of the event and by a CLI test that
  revokes `execute`, records, and restores it under one lock while a real
  confined run is live. The run stops as `revoked` with the new reason, and
  its outcome is observed. The Linux run of the same tests is by CI.

## References

- [Execution contract](../execution.md#settings-changes-during-a-run)
- [Security coding rules](../security-coding-rules.md)
- ADR-0030 (trusted runs), ADR-0029 (Windows grants and their revocation
  count), ADR-0060 (desktop settings changes)
