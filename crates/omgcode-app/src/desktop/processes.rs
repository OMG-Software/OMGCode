//! The subprocesses one session started: its model command-tool calls
//! (ADR-0086).
//!
//! The only subprocess a session starts is a `workspace_run_command` call
//! (ADR-0063). The service lists those calls newest first from three trusted,
//! read-only sources, resolved by the session identifier and never by a
//! webview-supplied path:
//!
//! - the model action journal, which records the session, the call identifier,
//!   how the call was authorized or refused, and when it started and finished.
//!   It records no argument text;
//! - the run journal, joined through the run identifier that a finished
//!   action records as its `operation_id`. It adds the resolved program, the
//!   profile name, the result, the elapsed time, and a limit that stopped the
//!   run;
//! - the session journal's recorded tool requests, matched to the action
//!   records by call identifier, for the program and argument text the model
//!   chose. The model's call identifier is unique within one turn only, so
//!   requests and records sharing an identifier are paired in order, aligned
//!   at the newest end.
//!
//! Nothing here reads a captured output body, an environment value, or a
//! credential, and nothing starts, stops, or re-runs a program. A list never
//! grants authority. The command text is untrusted model data and is escaped
//! with [`display_safe_line`] and bounded before it is returned.

use std::collections::HashMap;
use std::thread;
use std::time::{Duration, Instant};

use omgcode_core::action_journal::{
    ActionDisposition, ActionJournal, ActionJournalError, ActionOutcome, ActionRecord, ActionTool,
    MAX_RECENT_RECORDS, RefusalReason,
};
use omgcode_core::run::{self, RecentRuns, RunAuditView};
use omgcode_core::run_journal::{LimitKind, RunFinish, RunResult, RunStart};
use omgcode_core::session::{SessionInspection, TranscriptEntryKind};
use omgcode_core::turn::display_safe_line;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::DesktopService;
use super::error::DesktopError;
use super::state::windows_display_path;
use super::wire::SessionRequest;

/// Most calls one reply lists.
const MAX_ENTRIES: usize = 100;
/// Most run journal events one read inspects.
const RUN_EVENTS: usize = 100;
/// Most characters of the program or of one argument.
const MAX_ITEM_CHARS: usize = 256;
/// Most arguments one entry lists.
const MAX_ARGUMENTS: usize = 32;
/// Most characters of the program and arguments of one entry together.
const MAX_TOTAL_CHARS: usize = 2048;
/// Longest a read waits for a writer that holds the action journal. Well under
/// the view's two-second poll, so a poll never queues behind a writer.
const BUSY_WAIT: Duration = Duration::from_millis(250);
const BUSY_POLL: Duration = Duration::from_millis(10);
/// The recorded name of the command tool (`ToolName::Command`).
const COMMAND_TOOL: &str = "workspace_run_command";
/// Text for a recorded program path another operating system wrote.
const PATH_NOT_REPRESENTABLE: &str = "(path not representable on this system)";
const ELLIPSIS: char = '\u{2026}';

/// Where one listed call is.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    /// Started, not finished, and this session's chat turn is active here.
    Running,
    /// Started and finished. The result says how, when the run journal has it.
    Finished,
    /// Started with no recorded finish while no turn is active here, for
    /// example after a crash. The outcome is unknown, not failed.
    Unfinished,
    /// Never started: refused before any program ran.
    Refused,
}

/// How a recorded run ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessResultKind {
    /// The program exited; `exit_code` has its code.
    Exited,
    /// A Unix signal ended the program; `signal` has it.
    Signalled,
    /// The deadline elapsed and the tree was terminated.
    DeadlineExceeded,
    /// The turn was cancelled and the tree was terminated.
    Cancelled,
    /// A settings change withdrew access and the tree was terminated.
    Revoked,
    /// Nothing started: a check, a confinement rule, or the start failed.
    NotStarted,
    /// Supervision failed after the start.
    SupervisionFailed,
    /// The tree was not observed closed.
    CleanupUnconfirmed,
    /// The tree reached a limit and was terminated; `limit` names it.
    LimitExceeded,
}

/// The tree limit that stopped a run.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessLimit {
    /// Memory for the whole tree.
    Memory,
    /// Processes alive at once.
    Processes,
    /// CPU time summed over the tree.
    CpuTime,
    /// Net disk growth of the writable roots.
    Disk,
}

/// The recorded result of a run.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ProcessResult {
    /// How the run ended.
    pub kind: ProcessResultKind,
    /// The exit code, for `exited`.
    pub exit_code: Option<i64>,
    /// The Unix signal, for `signalled`.
    pub signal: Option<i32>,
}

/// One model command-tool call, for display. Every text field is untrusted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProcessEntry {
    /// The validated call identifier. Unique within one turn only.
    pub call_id: String,
    /// Where the call is.
    pub state: ProcessState,
    /// RFC 3339 time of the start or refusal record.
    pub started_at: String,
    /// RFC 3339 time of the finish record, when there is one.
    pub finished_at: Option<String>,
    /// Milliseconds from start to exit or termination, from the run journal.
    pub duration_ms: Option<u64>,
    /// The program as the model named it, or as the run journal resolved it.
    /// Escaped to one line and bounded. `None` when neither source is readable.
    pub program: Option<String>,
    /// The model's arguments, escaped to one line each, bounded in count and
    /// length. `None` when the session journal is unreadable or has no match.
    pub arguments: Option<Vec<String>>,
    /// Whether `program` or `arguments` were shortened or dropped to a bound.
    pub text_truncated: bool,
    /// How many arguments the call had, when known.
    pub argument_count: Option<usize>,
    /// The toolchain profile name, when recorded.
    pub profile: Option<String>,
    /// How the run ended, from the run journal.
    pub result: Option<ProcessResult>,
    /// The limit that stopped the run, from the run journal.
    pub limit: Option<ProcessLimit>,
    /// Why a call was refused, or the failure code or outcome of a finished
    /// call that has no run result.
    pub reason: Option<String>,
}

/// Whether the run journal could be read.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunDetails {
    /// It was read; entries that finished may carry a result.
    Available,
    /// It could not be read, so only action journal data is shown.
    Unavailable,
    /// Another holder had it locked just now; the next read may succeed.
    Busy,
}

/// What the run journal read found.
enum RunLookup {
    Index(RunIndex),
    Unavailable,
    Busy,
}

/// The reply to `session_processes`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SessionProcesses {
    /// At most 100 calls, newest first.
    pub entries: Vec<ProcessEntry>,
    /// Whether older calls exist that are not listed: more than 100 matched,
    /// or the 1000 newest action records, all sessions together, were full.
    pub truncated: bool,
    /// Whether run details could be joined.
    pub run_details: RunDetails,
}

/// One recorded command request of the session, in journal order.
struct CommandRequest {
    call_id: String,
    arguments: String,
}

/// Run journal records by run identifier.
#[derive(Default)]
struct RunIndex {
    starts: HashMap<Uuid, RunStart>,
    finishes: HashMap<Uuid, RunFinish>,
}

/// The model's recorded command arguments, parsed.
#[derive(Deserialize)]
struct RecordedCommand {
    profile: Option<String>,
    program: Option<String>,
    arguments: Option<Vec<String>>,
}

impl DesktopService {
    /// List the session's command-tool calls, newest first, at most 100.
    ///
    /// Read-only. The run journal and the session journal are best effort: an
    /// unreadable one is reported (`run_details`) or leaves the command text
    /// out, and the action journal data is still returned.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown session; `busy` when the action journal
    /// stays held; `invalid_journal` when it fails validation; `internal`.
    pub fn session_processes(
        &self,
        request: SessionRequest,
    ) -> Result<SessionProcesses, DesktopError> {
        let inner = &self.inner;
        let path = inner.resolve_path(request.session_id)?;
        let session_running = inner.is_running(request.session_id)?;
        let records = recent_actions(&inner.data_root)?;
        let window_full = records.len() >= MAX_RECENT_RECORDS;
        // The session journal is read only to name a listed call's command, so
        // a session with no command record in the window skips that read. A
        // read when there is one covers the whole journal, which is verified
        // as one chain; the tab polls only while a listed call runs.
        let has_calls = records.iter().any(|record| {
            record.session_id == request.session_id && record.tool == ActionTool::RunCommand
        });
        let requests = if has_calls {
            command_requests(&path)
        } else {
            Some(Vec::new())
        };
        // One attempt, no waiting, nothing created: a poll must not queue
        // behind a lock, and a read-only view must not make files.
        let runs = match run::audit_existing(&inner.data_root, RUN_EVENTS) {
            Ok(RecentRuns::Events(view)) => RunLookup::Index(run_index(&view)),
            Ok(RecentRuns::Absent) => RunLookup::Index(RunIndex::default()),
            Ok(RecentRuns::Busy) => RunLookup::Busy,
            Err(_) => RunLookup::Unavailable,
        };
        Ok(build(
            request.session_id,
            &records,
            requests,
            runs,
            session_running,
            window_full,
        ))
    }
}

/// The newest action records, waiting at most a quarter second for a writer.
fn recent_actions(data_root: &std::path::Path) -> Result<Vec<ActionRecord>, DesktopError> {
    let started = Instant::now();
    loop {
        match ActionJournal::recent(data_root, MAX_RECENT_RECORDS) {
            Ok(records) => return Ok(records),
            Err(ActionJournalError::Busy) if started.elapsed() < BUSY_WAIT => {
                thread::sleep(BUSY_POLL);
            }
            Err(ActionJournalError::Busy) => return Err(DesktopError::busy()),
            Err(ActionJournalError::Corrupt(_)) => return Err(DesktopError::invalid_journal()),
            Err(_) => {
                return Err(DesktopError::internal(
                    "The model action journal could not be read.",
                ));
            }
        }
    }
}

/// The session's recorded command requests, oldest first, or `None` when the
/// session journal cannot be read (for example while a turn holds it).
fn command_requests(path: &std::path::Path) -> Option<Vec<CommandRequest>> {
    let inspection = SessionInspection::inspect(path).ok()?;
    Some(
        inspection
            .entries
            .into_iter()
            .filter_map(|entry| match entry.kind {
                TranscriptEntryKind::ToolRequested {
                    call_id,
                    tool,
                    arguments,
                    ..
                } if tool == COMMAND_TOOL => Some(CommandRequest { call_id, arguments }),
                _ => None,
            })
            .collect(),
    )
}

/// Index run starts and finishes by run identifier. An event this build cannot
/// parse is skipped, never guessed at.
fn run_index(view: &RunAuditView) -> RunIndex {
    let mut index = RunIndex::default();
    for entry in &view.entries {
        let Some(action) = entry.get("action") else {
            continue;
        };
        let Some(payload) = action.get("payload") else {
            continue;
        };
        match action.get("type").and_then(Value::as_str) {
            Some("run_started") => {
                if let Some(start) = payload
                    .get("start")
                    .and_then(|value| serde_json::from_value::<RunStart>(value.clone()).ok())
                {
                    index.starts.insert(start.run_id, start);
                }
            }
            Some("run_finished") => {
                let run_id = payload
                    .get("run_id")
                    .and_then(|value| serde_json::from_value::<Uuid>(value.clone()).ok());
                let finish = payload
                    .get("finish")
                    .and_then(|value| serde_json::from_value::<RunFinish>(value.clone()).ok());
                if let (Some(run_id), Some(finish)) = (run_id, finish) {
                    index.finishes.insert(run_id, finish);
                }
            }
            _ => {}
        }
    }
    index
}

/// Join the sources into the reply. Pure, so tests need no journals.
///
/// `records` is newest first, as [`ActionJournal::recent`] returns it.
/// `requests` is the session's recorded command requests, oldest first.
fn build(
    session_id: Uuid,
    records: &[ActionRecord],
    requests: Option<Vec<CommandRequest>>,
    runs: RunLookup,
    session_running: bool,
    window_full: bool,
) -> SessionProcesses {
    let mine: Vec<&ActionRecord> = records
        .iter()
        .rev()
        .filter(|record| record.session_id == session_id && record.tool == ActionTool::RunCommand)
        .collect();
    let truncated = window_full || mine.len() > MAX_ENTRIES;

    let mut by_call: HashMap<&str, Vec<&CommandRequest>> = HashMap::new();
    for request in requests.iter().flatten() {
        by_call.entry(&request.call_id).or_default().push(request);
    }
    let mut record_counts: HashMap<&str, usize> = HashMap::new();
    for record in &mine {
        *record_counts.entry(&record.call_id).or_default() += 1;
    }

    // Pair the k-th record of a call identifier with the request at the same
    // position counted from the newest end.
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let (run_details, runs) = match runs {
        RunLookup::Index(index) => (RunDetails::Available, index),
        RunLookup::Unavailable => (RunDetails::Unavailable, RunIndex::default()),
        RunLookup::Busy => (RunDetails::Busy, RunIndex::default()),
    };
    let mut entries: Vec<ProcessEntry> = Vec::with_capacity(mine.len().min(MAX_ENTRIES));
    for record in &mine {
        let position = seen.entry(&record.call_id).or_default();
        let request = by_call.get(record.call_id.as_str()).and_then(|list| {
            let records_total = record_counts.get(record.call_id.as_str()).copied()?;
            // A record with no request at its position (fewer requests than
            // records, for example after a rotation) stays unmatched.
            (list.len() + *position)
                .checked_sub(records_total)
                .and_then(|index| list.get(index).copied())
        });
        *position += 1;
        entries.push(entry_of(record, request, &runs, session_running));
    }
    entries.reverse();
    entries.truncate(MAX_ENTRIES);
    SessionProcesses {
        entries,
        truncated,
        run_details,
    }
}

/// One listed call.
fn entry_of(
    record: &ActionRecord,
    request: Option<&CommandRequest>,
    runs: &RunIndex,
    session_running: bool,
) -> ProcessEntry {
    let mut entry = ProcessEntry {
        call_id: record.call_id.clone(),
        state: ProcessState::Refused,
        started_at: record.time.clone(),
        finished_at: None,
        duration_ms: None,
        program: None,
        arguments: None,
        text_truncated: false,
        argument_count: None,
        profile: None,
        result: None,
        limit: None,
        reason: None,
    };
    let recorded = request
        .and_then(|request| serde_json::from_str::<RecordedCommand>(&request.arguments).ok());
    let mut run_start: Option<&RunStart> = None;
    match &record.disposition {
        ActionDisposition::Refused { reason } => {
            entry.reason = Some(refusal_code(*reason).to_owned());
        }
        ActionDisposition::Executed {
            finish: Some(finish),
            ..
        } => {
            entry.state = ProcessState::Finished;
            entry.finished_at = Some(finish.time.clone());
            let run_id = finish.operation_id;
            run_start = run_id.and_then(|id| runs.starts.get(&id));
            if let Some(run) = run_id.and_then(|id| runs.finishes.get(&id)) {
                entry.duration_ms = Some(run.elapsed_ms);
                entry.result = Some(result_of(run.result));
                if let RunResult::LimitExceeded { limit } = run.result {
                    entry.limit = Some(limit_of(limit));
                }
            }
            if entry.result.is_none() {
                entry.reason = finish.code.clone().or_else(|| match finish.outcome {
                    ActionOutcome::Succeeded => None,
                    ActionOutcome::Failed => Some("failed".to_owned()),
                    ActionOutcome::Uncertain => Some("uncertain".to_owned()),
                });
            }
        }
        ActionDisposition::Executed { finish: None, .. } => {
            entry.state = if session_running {
                ProcessState::Running
            } else {
                ProcessState::Unfinished
            };
        }
    }
    fill_command(&mut entry, recorded.as_ref(), run_start);
    entry
}

/// Set the program and arguments from the model's recorded request, falling
/// back to the run journal's program and argument count, and the profile from
/// the run journal's resolved name, falling back to the requested one. The
/// per-call text budget is charged to the program first, then the profile,
/// then the arguments.
fn fill_command(
    entry: &mut ProcessEntry,
    recorded: Option<&RecordedCommand>,
    run_start: Option<&RunStart>,
) {
    let mut budget = MAX_TOTAL_CHARS;
    let program = recorded
        .and_then(|command| command.program.clone())
        .or_else(|| run_start.map(|start| native_text(&start.program)));
    if let Some(program) = program {
        let (text, cut) = bounded(&program, MAX_ITEM_CHARS.min(budget));
        budget = budget.saturating_sub(text.chars().count());
        entry.text_truncated |= cut;
        entry.program = Some(text);
    }
    // The profile name names the call, so it is charged before the arguments:
    // it is always shown, and the per-call bound covers it too (ADR-0086).
    let profile = run_start
        .map(|start| start.profile_name.clone())
        .or_else(|| recorded.and_then(|command| command.profile.clone()));
    if let Some(name) = profile {
        let (text, cut) = bounded(&name, MAX_ITEM_CHARS.min(budget));
        budget = budget.saturating_sub(text.chars().count());
        entry.text_truncated |= cut;
        entry.profile = Some(text);
    }
    if let Some(arguments) = recorded.and_then(|command| command.arguments.as_ref()) {
        entry.argument_count = Some(arguments.len());
        let mut shown = Vec::new();
        for argument in arguments {
            if shown.len() >= MAX_ARGUMENTS || budget == 0 {
                entry.text_truncated = true;
                break;
            }
            let (text, cut) = bounded(argument, MAX_ITEM_CHARS.min(budget));
            budget = budget.saturating_sub(text.chars().count());
            entry.text_truncated |= cut;
            shown.push(text);
        }
        entry.arguments = Some(shown);
    } else {
        entry.argument_count = run_start.map(|start| start.argument_count);
    }
}

/// A recorded native path as lossy display text.
fn native_text(path: &omgcode_core::session::NativePath) -> String {
    path.to_path_buf().map_or_else(
        |_| PATH_NOT_REPRESENTABLE.to_owned(),
        |path| windows_display_path(&path.to_string_lossy()),
    )
}

/// Escape `text` to one safe line and keep at most `max` characters, marking a
/// cut with an ellipsis. Returns whether it was cut.
fn bounded(text: &str, max: usize) -> (String, bool) {
    let safe = display_safe_line(text);
    if safe.chars().count() <= max {
        return (safe, false);
    }
    let keep = max.saturating_sub(1);
    let mut cut: String = safe.chars().take(keep).collect();
    cut.push(ELLIPSIS);
    (cut, true)
}

const fn refusal_code(reason: RefusalReason) -> &'static str {
    match reason {
        RefusalReason::AccessDenied => "access_denied",
        RefusalReason::ApprovalRequired { .. } => "approval_required",
        RefusalReason::ApprovalDenied { .. } => "approval_denied",
        RefusalReason::ApprovalExpired { .. } => "approval_expired",
        RefusalReason::InvalidRequest => "invalid_request",
        RefusalReason::PolicyUnavailable => "policy_unavailable",
        RefusalReason::Cancelled => "cancelled",
    }
}

const fn limit_of(limit: LimitKind) -> ProcessLimit {
    match limit {
        LimitKind::Memory => ProcessLimit::Memory,
        LimitKind::Processes => ProcessLimit::Processes,
        LimitKind::CpuTime => ProcessLimit::CpuTime,
        LimitKind::Disk => ProcessLimit::Disk,
    }
}

const fn result_of(result: RunResult) -> ProcessResult {
    let (kind, exit_code, signal) = match result {
        RunResult::Exited { code } => (ProcessResultKind::Exited, Some(code), None),
        RunResult::Signalled { signal } => (ProcessResultKind::Signalled, None, Some(signal)),
        RunResult::DeadlineExceeded => (ProcessResultKind::DeadlineExceeded, None, None),
        RunResult::Cancelled => (ProcessResultKind::Cancelled, None, None),
        RunResult::Revoked => (ProcessResultKind::Revoked, None, None),
        RunResult::NotStarted => (ProcessResultKind::NotStarted, None, None),
        RunResult::SupervisionFailed => (ProcessResultKind::SupervisionFailed, None, None),
        RunResult::CleanupUnconfirmed => (ProcessResultKind::CleanupUnconfirmed, None, None),
        RunResult::LimitExceeded { .. } => (ProcessResultKind::LimitExceeded, None, None),
    };
    ProcessResult {
        kind,
        exit_code,
        signal,
    }
}

#[cfg(test)]
mod tests;
