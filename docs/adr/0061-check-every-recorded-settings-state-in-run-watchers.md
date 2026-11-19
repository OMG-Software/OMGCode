# 0061: Check every recorded settings state in run watchers

- Status: Accepted
- Date: 2026-11-19

## Context

A trusted run has a watcher. Each time the control journal changes, the
watcher read the settings and stopped the run if they no longer allowed it
([execution contract](../execution.md#settings-changes-during-a-run)). A
settings command records its change, releases the control lock, and waits for
the runs that the settings withdraw.

The watcher read only the latest settings. It missed a withdrawal that was
undone before it read them. For example, `execute` is turned off and then on
again by another command between two polls (every 50 ms). The watcher then saw
`execute` on and kept the run going. The command that turned it off also found
no withdrawn run once its lock was released, so it reported success. Two
withdrawals already avoided this with counts that replay derives from the
journal: `model_tools` revocations and container-grant revocation requests.
Others did not, including `execute`, `read`, `write`, `host_services`,
`task_network`, the package cache root, and the development root.

The security rules require that narrowing permissions stops tasks that retain
revoked access and that cleanup is observed before revocation is reported
complete ([security rules](../security-coding-rules.md)). The gap was found in
review of the desktop settings window (ADR-0060), which uses the same core
functions as the CLI.

## Decision

1. **The watcher checks every recorded state.** A run takes the number of
   settings events under the control lock when it starts. When the journal
   changes, its watcher replays it once and checks the run against the state
   after each event recorded since it last read them, oldest first, then
   against the latest state. A state that withdrew the run stops it, even if a
   later event restored the access. The reason is the one that state gives,
   for example "the workspace execute preference is disabled".
2. **The change's own event is the evidence.** Nothing else is written. The
   control journal is append-only, so a recorded change cannot be missed: not
   when access is restored before the watcher reads, not when the journal is
   full, and not when the command crashes after recording its change.
3. **Commands still wait for what they withdrew.** While it still holds the
   control lock, right after it records its change, a settings command lists
   the pending runs that the new settings no longer allow
   (`run::withdrawn_runs`). It writes nothing. After it releases the lock it
   waits for those runs and any others the live settings withdraw, so it
   reports success only after their cleanup is observed. If the run journal
   cannot be read, the change stays recorded and the watchers still stop the
   runs. The command waits for what it can find, then fails: the CLI with code
   `run`, the desktop with `runs_not_stopped`.
4. **Every caller lists.** The desktop settings changes and the CLI commands
   `workspace permissions`, `set-root`, `set-package-cache`,
   `clear-package-cache`, `profile-remove`, `package-profile-remove`, and
   `provider-profile-remove` list before they release the lock.
   `container-revoke` keeps its existing per-workspace count and named runs.

## Alternatives considered

- **Record the withdrawn run identifiers in a new journal event** under the
  same lock as the change. This was the first fix in review. It needs a second
  write after the change, which can fail (a full journal or an unreadable run
  journal) or be lost to a crash, and it would make older binaries refuse the
  journal. Each failure left the change recorded without the protection.
  Rejected.
- **More per-workspace counts derived on replay**, one for each kind of
  withdrawal, as for `model_tools`. This cannot tell whether a new development
  root still contains a workspace, because that is a filesystem fact replay
  must not read. A count of all root changes would stop runs the change left
  allowed. Rejected as imprecise.
- **Only listing withdrawn runs under the lock.** That made a command report
  `runs_not_stopped` instead of success, but the run kept going. Rejected as
  incomplete.
- **Stopping every run of the workspace on any narrowing change.** Simple, but
  it stops runs the change does not affect. Rejected.

## Consequences

- A run that any recorded settings state withdrew stops, even if access is
  restored before its watcher reads the settings. Restoring access does not
  restore a run.
- Runs that no recorded state withdrew are not stopped.
- No new event type and no extra write. The journal stays readable by earlier
  binaries.
- A watcher keeps one copy of the settings state for each event since its last
  read, usually one or two. The journal's limit of 4096 events bounds it. It
  already replayed the whole journal on every change.
- The filesystem checks in the rule, such as whether a workspace is still
  inside the development root, run against the filesystem at the time of the
  check for each state.
- Verified on Windows and Linux:
  - a unit test that replay returns each state since an event;
  - a Linux core test that a withdrawal undone before the read still
    withdraws the run;
  - a desktop test that an unreadable run journal still records the narrowing
    and reports it;
  - a CLI test that revokes `execute`, lists, and restores it under one lock
    while a real confined run is live. The watcher alone stops the run as
    `revoked`, with no command waiting.

## References

- [Execution contract](../execution.md#settings-changes-during-a-run)
- [Security coding rules](../security-coding-rules.md)
- ADR-0030 (trusted runs), ADR-0029 (Windows grants and their revocation
  count), ADR-0060 (desktop settings changes)
