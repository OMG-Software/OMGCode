# 0086: Show the session's subprocesses in the main window

- Status: Accepted
- Date: 2026-11-24

## Context

Operator feedback asked for a "background tasks" tab in the right-hand panel
of the `main` window. The operator clarified that it lists the subprocesses of
the current session.

In OMGCode a session starts exactly one kind of subprocess: a model
`workspace_run_command` call, which runs through the trusted runner
([execution contract](../execution.md)). Three trusted records describe such a
call, and none is complete:

- The model action journal records the session, the call identifier, how the
  call was authorised or why it was refused, and the start and finish times.
  It records no argument text. A finished call stores the run identifier as
  its `operation_id`.
- The run journal records the resolved program, the profile name, the result,
  the elapsed time, and the limits. It records only a count and a digest of the
  arguments, and it has no session identifier.
- The session journal records the exact arguments JSON of each tool request
  with its call identifier, unique within one turn only. The transcript
  already sends these to the webview.

The webview has no authority (ADR-0045). Command text is chosen by the model
and is untrusted.

## Decision

Add the `main`-window command `session_processes { session_id }` and a
self-contained view module `createProcessesTab()`, shown as the right-hand
panel's **Processes** tab, which reads only while it is visible.

The command is read-only and resolves everything from trusted state by the
session identifier. On the blocking pool it reads:

1. the 1,000 newest action journal records (`ActionJournal::recent`, waiting
   at most 250 ms for a writer, then `busy`) and keeps those of this session whose tool
   is `run_command`;
2. the run journal's newest 100 events through `run::audit_existing`, a core
   reader added for this view, joined through the finish record's
   `operation_id`. It makes one attempt, never waits, and never creates the
   settings store, the journal, or a directory; absence lists no runs. A held
   lock sets `run_details` to `busy`;
3. the session journal's recorded `workspace_run_command` requests
   (`SessionInspection`), for the program and argument text.

The action records and the requests are joined by call identifier. Because
that identifier repeats across turns, requests and records that share one are
paired in order from the newest end, which is also the end the action journal
keeps when it rotates. An unmatched record shows no argument text. This
pairing is an inference, not a recorded link, and the interface does not
claim otherwise. A run journal or session journal that cannot be read does not
fail the command: the run journal sets `run_details` to `unavailable`, and an
unreadable session journal leaves the command text out. An unreadable or busy
action journal is an error (`busy`, `invalid_journal`, `internal`). An unknown
session is `not_found`.

At most 100 entries are returned, newest first, with `truncated` set when more
matched or when the action journal window was full. Each entry has a state:

- `running`: started, no finish, and this desktop has a chat turn active for
  the session;
- `unfinished`: started, no finish, and no turn is active here, for example
  after a crash; the outcome is unknown, never failed;
- `finished`, with the run's result and any limit that stopped it (memory,
  processes, CPU time, or disk growth), or a failure code when no run result
  exists;
- `refused`, with the refusal reason; the call never started.

### Fields that reach the webview

The reply carries only: the call identifier; the state; the start and finish
RFC 3339 times; the run's elapsed milliseconds; the program; the arguments;
whether any text was shortened; the argument count; the profile name; the
result kind with its exit code or signal; the limit; and a reason code. It
never carries captured output, environment values, credentials, working
directories, digests, workspace or approval identifiers, or paths other than
the program text.

Program, arguments, and profile name are untrusted. Rust escapes each to one
line with `display_safe_line`, renders a non-Unicode program path lossily, and
bounds them: 256 characters per item, 32 arguments, and 2,048 characters per
call. The program is charged first, then the profile name, so both are shown
whatever the arguments hold, and then the arguments; any cut sets the
shortened flag. The view builds text nodes only. The reason and failure codes are
validated `[a-z0-9_]` values from the action journal.

This exposure is acceptable because the transcript already delivers the same
tool request arguments to the `main` window as recorded text (the
`tool_requested` entries), the window is the one that holds them, and the
reply adds no output body and no value that the model did not send. It grants
no authority: the view has no cancel, restart, or run control, and a listed
call is evidence, never an approval or a permission.

The view polls only while the session has a `running` entry, every two seconds
through the shared poller, and stops when nothing runs, on clearing the
session, and on disposal.

## Alternatives considered

- **Add a session identifier to the run journal.** It would join runs to
  sessions directly, but it changes the strict, versioned journal format for a
  display feature, and the action journal already links a session to a run.
- **Add a request identifier to the action journal.** It would make the
  argument join exact, but it changes the journal schema and writers for the
  same reason. Revisit if the in-order pairing proves wrong in practice.
- **Show only action journal and run journal data, without argument text.**
  The program alone often cannot tell two calls apart. The session journal is
  the only record of the arguments, and the transcript already exposes them.
- **Return the captured output or environment.** Rejected: the run journal does
  not keep output, and output is the largest and least bounded injection path.
- **Treat every unfinished call as running.** Rejected: after a crash it would
  show a process that does not exist. Only an active turn in this desktop makes
  a call `running`.
- **Track live processes in the shell.** Rejected: the trusted runner owns the
  processes, and the shell would need a second, unverified source of truth.
- **Add cancel or re-run controls.** Rejected for this change: it would need new
  authority and an approval path; the existing turn cancel remains.

## Consequences

- The list covers command calls found in the 1,000 newest action records of
  all sessions and, for results and durations, in the newest 100 run journal
  events. Older calls of a busy installation are missing or lack a result; the
  entry says so through `truncated` and `run_details`, and an entry without a
  run record shows the action outcome only.
- A running call shows no run result until it finishes, because only a
  finish records the run identifier.
- The view polls every two seconds, so no read may queue behind a lock. A run
  takes the settings lock and the run journal lock only to record its start
  and its finish (`run::run` drops both before the program starts); the
  program runs holding neither. The run journal read does not wait at all, and
  the action journal read waits at most 250 ms. The shared poller skips a tick
  while its previous read is in flight, so reads do not stack.
- `run::audit` was rejected for this view: it creates an empty journal and
  waits up to 10 seconds for the locks.
- On Windows an open turn can hold the session journal, so a running call may
  show only its run journal program, or no command text, until it finishes.
- Tests: Rust unit tests of the join, the states, refused calls, the bounds, and
  the serialized fields; an integration test against real journals and the
  service; Vitest tests of the view with fake timers.

## References

- [Desktop interface](../desktop-interface.md), "Processes tab"
- [Workspace permissions](../workspace-permissions.md), "Model action journal"
- [Execution contract](../execution.md)
- [ADR-0063](0063-deny-prompt-or-allow-each-preference-wait-for-approvals-in-the-turn-and-journal-model-actions.md)
- [ADR-0072](0072-resize-and-collapse-the-main-window-panels-and-follow-the-transcript-end.md)
