use omgcode_core::action_journal::{ActionFinish, Authorization};
use omgcode_core::policy::WorkspaceId;
use omgcode_core::session::NativePath;
use serde_json::json;

use super::*;

fn workspace() -> WorkspaceId {
    Uuid::new_v4().to_string().parse().expect("workspace id")
}

fn authorization() -> Authorization {
    Authorization::StandingAllow {
        settings_revision: 1,
    }
}

type Finish<'a> = (&'a str, ActionOutcome, Option<&'a str>, Option<Uuid>);

/// An executed command call with an optional finish.
fn executed(session: Uuid, call_id: &str, sequence: u64, finish: Option<Finish>) -> ActionRecord {
    ActionRecord {
        sequence,
        time: format!("2026-01-01T00:00:{sequence:02}Z"),
        session_id: session,
        workspace_id: workspace(),
        call_id: call_id.to_owned(),
        tool: ActionTool::RunCommand,
        targets: Vec::new(),
        request_sha256: None,
        after_web_content: false,
        disposition: ActionDisposition::Executed {
            action_id: Uuid::new_v4(),
            authorization: authorization(),
            finish: finish.map(|(time, outcome, code, operation_id)| ActionFinish {
                time: time.to_owned(),
                outcome,
                code: code.map(str::to_owned),
                operation_id,
            }),
        },
    }
}

fn refused(session: Uuid, call_id: &str, sequence: u64, reason: RefusalReason) -> ActionRecord {
    ActionRecord {
        disposition: ActionDisposition::Refused { reason },
        ..executed(session, call_id, sequence, None)
    }
}

fn request(call_id: &str, program: &str, arguments: &[&str]) -> CommandRequest {
    CommandRequest {
        call_id: call_id.to_owned(),
        arguments: json!({
            "profile": "rust",
            "program": program,
            "arguments": arguments,
        })
        .to_string(),
    }
}

fn native(path: &str) -> Value {
    serde_json::to_value(NativePath::from_path(std::path::Path::new(path)).expect("native"))
        .expect("json")
}

fn start_event(run_id: Uuid, program: &str, arguments: usize) -> Value {
    json!({"action": {"type": "run_started", "payload": {"start": {
        "run_id": run_id,
        "workspace_id": workspace(),
        "settings_revision": 1,
        "profile_id": Uuid::new_v4(),
        "profile_name": "rust-profile",
        "profile_revision": 1,
        "program": native(program),
        "argument_count": arguments,
        "arguments_sha256": "0".repeat(64),
        "working_directory": native("w"),
        "unix_sockets": "pairs_only",
        "model_origin": true,
    }}}})
}

fn finish_event(run_id: Uuid, result: Value, elapsed_ms: u64) -> Value {
    json!({"action": {"type": "run_finished", "payload": {
        "run_id": run_id,
        "finish": {
            "result": result,
            "confinement": null,
            "elapsed_ms": elapsed_ms,
            "stdout_bytes": 5,
            "stderr_bytes": 0,
        },
    }}})
}

fn index(events: Vec<Value>) -> RunIndex {
    run_index(&RunAuditView {
        pending: Vec::new(),
        entries: events,
    })
}

/// Build the reply from records given oldest first.
fn list(
    session: Uuid,
    chronological: Vec<ActionRecord>,
    requests: Option<Vec<CommandRequest>>,
    runs: Option<RunIndex>,
    running: bool,
) -> SessionProcesses {
    let newest_first: Vec<ActionRecord> = chronological.into_iter().rev().collect();
    let lookup = runs.map_or(RunLookup::Unavailable, RunLookup::Index);
    build(session, &newest_first, requests, lookup, running, false)
}

const DONE: &str = "2026-01-01T00:00:09Z";

#[test]
fn a_finished_call_joins_its_run_and_lists_newest_first() {
    let session = Uuid::new_v4();
    let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
    let runs = index(vec![
        start_event(first, "cargo", 2),
        finish_event(first, json!({"kind": "exited", "code": 0}), 1500),
        start_event(second, "cargo", 1),
        finish_event(second, json!({"kind": "exited", "code": 101}), 40),
    ]);
    let records = vec![
        executed(
            session,
            "a",
            1,
            Some((DONE, ActionOutcome::Succeeded, None, Some(first))),
        ),
        executed(
            session,
            "b",
            3,
            Some((DONE, ActionOutcome::Succeeded, None, Some(second))),
        ),
    ];
    let requests = vec![
        request("a", "cargo", &["build", "--release"]),
        request("b", "cargo", &["test"]),
    ];

    let listed = list(session, records, Some(requests), Some(runs), false);

    assert_eq!(listed.run_details, RunDetails::Available);
    assert!(!listed.truncated);
    let ids: Vec<&str> = listed.entries.iter().map(|e| e.call_id.as_str()).collect();
    assert_eq!(ids, ["b", "a"]);
    let newest = &listed.entries[0];
    assert_eq!(newest.state, ProcessState::Finished);
    assert_eq!(newest.program.as_deref(), Some("cargo"));
    assert_eq!(newest.arguments.as_deref(), Some(&["test".to_owned()][..]));
    assert_eq!(newest.profile.as_deref(), Some("rust-profile"));
    assert_eq!(newest.duration_ms, Some(40));
    assert_eq!(newest.finished_at.as_deref(), Some(DONE));
    assert_eq!(
        newest.result,
        Some(ProcessResult {
            kind: ProcessResultKind::Exited,
            exit_code: Some(101),
            signal: None,
        })
    );
    assert_eq!(
        listed.entries[1].result.map(|result| result.exit_code),
        Some(Some(0))
    );
}

#[test]
fn a_limit_stop_names_the_limit() {
    let session = Uuid::new_v4();
    let run = Uuid::new_v4();
    let runs = index(vec![
        start_event(run, "hog", 0),
        finish_event(
            run,
            json!({"kind": "limit_exceeded", "limit": "memory"}),
            900,
        ),
    ]);
    let records = vec![executed(
        session,
        "a",
        1,
        Some((DONE, ActionOutcome::Succeeded, None, Some(run))),
    )];

    let listed = list(session, records, None, Some(runs), false);

    let entry = &listed.entries[0];
    assert_eq!(entry.limit, Some(ProcessLimit::Memory));
    assert_eq!(
        entry.result.map(|result| result.kind),
        Some(ProcessResultKind::LimitExceeded)
    );
    // Without the session journal the run journal still names the program.
    assert_eq!(entry.program.as_deref(), Some("hog"));
    assert_eq!(entry.arguments, None);
    assert_eq!(entry.argument_count, Some(0));
}

#[test]
fn a_refused_call_is_listed_as_refused_with_its_reason() {
    let session = Uuid::new_v4();
    let records = vec![refused(session, "a", 1, RefusalReason::AccessDenied)];

    let listed = list(
        session,
        records,
        Some(vec![request("a", "rm", &["-rf", "x"])]),
        Some(RunIndex::default()),
        false,
    );

    let entry = &listed.entries[0];
    assert_eq!(entry.state, ProcessState::Refused);
    assert_eq!(entry.reason.as_deref(), Some("access_denied"));
    assert_eq!(entry.result, None);
    assert_eq!(entry.program.as_deref(), Some("rm"));
}

#[test]
fn a_start_with_no_finish_is_running_only_while_a_turn_is_active() {
    let session = Uuid::new_v4();
    let records = || vec![executed(session, "a", 1, None)];

    let active = list(session, records(), None, None, true);
    let idle = list(session, records(), None, None, false);

    assert_eq!(active.entries[0].state, ProcessState::Running);
    assert_eq!(idle.entries[0].state, ProcessState::Unfinished);
    assert_eq!(active.run_details, RunDetails::Unavailable);
    assert_eq!(idle.entries[0].program, None);
}

#[test]
fn a_finish_without_a_run_reports_its_code_or_outcome() {
    let session = Uuid::new_v4();
    let records = vec![
        executed(
            session,
            "a",
            1,
            Some((DONE, ActionOutcome::Failed, Some("command_failed"), None)),
        ),
        executed(
            session,
            "b",
            3,
            Some((DONE, ActionOutcome::Uncertain, None, None)),
        ),
        executed(
            session,
            "c",
            5,
            Some((DONE, ActionOutcome::Succeeded, None, None)),
        ),
    ];

    let listed = list(session, records, None, Some(RunIndex::default()), false);

    let reasons: Vec<Option<&str>> = listed
        .entries
        .iter()
        .map(|entry| entry.reason.as_deref())
        .collect();
    assert_eq!(reasons, [None, Some("uncertain"), Some("command_failed")]);
}

#[test]
fn other_sessions_and_other_tools_are_not_listed() {
    let session = Uuid::new_v4();
    let mut other_tool = executed(session, "t", 2, None);
    other_tool.tool = ActionTool::ReadFile;
    let records = vec![
        executed(Uuid::new_v4(), "x", 1, None),
        other_tool,
        executed(session, "mine", 3, None),
    ];

    let listed = list(session, records, None, None, false);

    assert_eq!(listed.entries.len(), 1);
    assert_eq!(listed.entries[0].call_id, "mine");
}

#[test]
fn a_session_with_no_records_lists_nothing() {
    let listed = list(Uuid::new_v4(), Vec::new(), Some(Vec::new()), None, false);

    assert!(listed.entries.is_empty());
    assert!(!listed.truncated);
}

#[test]
fn a_repeated_call_identifier_pairs_requests_from_the_newest_end() {
    let session = Uuid::new_v4();
    let records = vec![
        executed(session, "call_1", 1, None),
        executed(session, "call_1", 3, None),
    ];
    // The oldest request has no action record any more.
    let requests = vec![
        request("call_1", "oldest", &[]),
        request("call_1", "older", &[]),
        request("call_1", "newer", &[]),
    ];

    let listed = list(session, records, Some(requests), None, false);

    let programs: Vec<Option<&str>> = listed
        .entries
        .iter()
        .map(|entry| entry.program.as_deref())
        .collect();
    assert_eq!(programs, [Some("newer"), Some("older")]);
}

#[test]
fn more_records_than_requests_leave_the_oldest_unmatched() {
    let session = Uuid::new_v4();
    let records = vec![
        executed(session, "call_1", 1, None),
        executed(session, "call_1", 3, None),
    ];

    let listed = list(
        session,
        records,
        Some(vec![request("call_1", "only", &[])]),
        None,
        false,
    );

    assert_eq!(listed.entries[0].program.as_deref(), Some("only"));
    assert_eq!(listed.entries[1].program, None);
}

#[test]
fn more_than_one_hundred_calls_are_cut_and_flagged() {
    let session = Uuid::new_v4();
    let records: Vec<ActionRecord> = (0..130)
        .map(|n| executed(session, &format!("c{n}"), n + 1, None))
        .collect();

    let listed = list(session, records, None, None, false);

    assert_eq!(listed.entries.len(), MAX_ENTRIES);
    assert!(listed.truncated);
    assert_eq!(listed.entries[0].call_id, "c129");
    assert_eq!(listed.entries[MAX_ENTRIES - 1].call_id, "c30");
}

#[test]
fn a_full_action_window_is_flagged_even_when_few_calls_match() {
    let listed = build(
        Uuid::new_v4(),
        &[],
        None,
        RunLookup::Unavailable,
        false,
        true,
    );

    assert!(listed.truncated);
}

#[test]
fn text_is_escaped_to_one_line_and_bounded() {
    let session = Uuid::new_v4();
    let long = "x".repeat(1000);
    let rtl = format!("{}rtl", char::from_u32(0x202e).expect("bidi override"));
    let many: Vec<String> = (0..40).map(|n| format!("a{n}")).collect();
    let many_refs: Vec<&str> = many.iter().map(String::as_str).collect();
    let records = vec![
        executed(session, "a", 1, None),
        executed(session, "b", 3, None),
    ];
    let requests = vec![
        request("a", "echo", &["line1\nline2", rtl.as_str(), long.as_str()]),
        request("b", "echo", &many_refs),
    ];

    let listed = list(session, records, Some(requests), None, false);

    let many_entry = &listed.entries[0];
    assert_eq!(many_entry.argument_count, Some(40));
    assert_eq!(
        many_entry.arguments.as_ref().map(Vec::len),
        Some(MAX_ARGUMENTS)
    );
    assert!(many_entry.text_truncated);
    let entry = &listed.entries[1];
    let arguments = entry.arguments.as_ref().expect("arguments");
    assert!(!arguments[0].contains('\n'));
    assert!(arguments[0].contains("u{000a}"));
    assert!(!arguments[1].contains(char::from_u32(0x202e).expect("bidi override")));
    assert!(arguments[1].contains("u{202e}"));
    assert_eq!(arguments[2].chars().count(), MAX_ITEM_CHARS);
    assert!(arguments[2].ends_with(ELLIPSIS));
    assert!(entry.text_truncated);
}

#[test]
fn the_total_text_of_one_entry_is_bounded() {
    let session = Uuid::new_v4();
    let chunk = "y".repeat(MAX_ITEM_CHARS);
    let arguments: Vec<&str> = (0..MAX_ARGUMENTS).map(|_| chunk.as_str()).collect();
    let records = vec![executed(session, "a", 1, None)];

    let listed = list(
        session,
        records,
        Some(vec![request("a", "p", &arguments)]),
        None,
        false,
    );

    let entry = &listed.entries[0];
    let program: usize = entry.program.iter().map(|text| text.chars().count()).sum();
    let profile: usize = entry.profile.iter().map(|text| text.chars().count()).sum();
    let rest: usize = entry
        .arguments
        .iter()
        .flatten()
        .map(|text| text.chars().count())
        .sum();
    let total = program + profile + rest;
    assert!(total <= MAX_TOTAL_CHARS, "{total}");
    // The profile is charged before the arguments, so it is shown in full.
    assert_eq!(entry.profile.as_deref(), Some("rust"));
    assert!(entry.text_truncated);
}

#[test]
fn a_cut_profile_name_is_bounded_and_flagged() {
    let session = Uuid::new_v4();
    let records = vec![executed(session, "a", 1, None)];
    let long = "p".repeat(1000);
    let requests = vec![CommandRequest {
        call_id: "a".to_owned(),
        arguments: json!({"profile": long, "program": "echo", "arguments": []}).to_string(),
    }];

    let listed = list(session, records, Some(requests), None, false);

    let entry = &listed.entries[0];
    let profile = entry.profile.as_deref().expect("profile");
    assert_eq!(profile.chars().count(), MAX_ITEM_CHARS);
    assert!(profile.ends_with(ELLIPSIS));
    assert!(entry.text_truncated);
}

#[test]
fn a_malformed_recorded_request_leaves_the_command_unknown() {
    let session = Uuid::new_v4();
    let records = vec![executed(session, "a", 1, None)];
    let bad = CommandRequest {
        call_id: "a".to_owned(),
        arguments: "not json".to_owned(),
    };

    let listed = list(session, records, Some(vec![bad]), None, true);

    assert_eq!(listed.entries[0].program, None);
    assert_eq!(listed.entries[0].arguments, None);
}

#[test]
fn unparseable_run_events_are_skipped() {
    let run = Uuid::new_v4();
    let events = vec![
        json!({"action": {"type": "created"}}),
        json!({"action": {"type": "run_finished", "payload": {"run_id": run, "finish": {"bogus": 1}}}}),
        json!("garbage"),
    ];

    let runs = index(events);

    assert!(runs.starts.is_empty());
    assert!(runs.finishes.is_empty());
}

#[test]
fn an_entry_serializes_only_the_documented_fields() {
    let session = Uuid::new_v4();
    let listed = list(
        session,
        vec![executed(session, "a", 1, None)],
        None,
        None,
        true,
    );

    let value = serde_json::to_value(&listed).expect("json");

    let mut keys: Vec<&str> = value["entries"][0]
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "argument_count",
            "arguments",
            "call_id",
            "duration_ms",
            "finished_at",
            "limit",
            "profile",
            "program",
            "reason",
            "result",
            "started_at",
            "state",
            "text_truncated",
        ]
    );
    assert_eq!(value["entries"][0]["state"], "running");
}

#[test]
fn a_busy_run_journal_is_reported_and_the_action_data_still_lists() {
    let session = Uuid::new_v4();
    let records = [executed(session, "a", 1, None)];

    let listed = build(session, &records, None, RunLookup::Busy, true, false);

    assert_eq!(listed.run_details, RunDetails::Busy);
    assert_eq!(listed.entries[0].state, ProcessState::Running);
}
