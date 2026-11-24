//! The `session_processes` command of the desktop service, against a real
//! session journal, a real model action journal, and a real settings store
//! (ADR-0086).

use omgcode_app::desktop::{ProcessState, RunDetails};
use omgcode_core::action_journal::{
    ActionJournal, ActionOutcome, ActionTool, Authorization, RefusalReason, RefusedAction,
    StartedAction,
};
use omgcode_core::policy::WorkspaceId;
use omgcode_core::session::SessionJournal;
use serde_json::json;
use uuid::Uuid;

mod common;
use common::{Fixture, read_tools, session_request, wire};

const NO_ENDPOINT: &str = "http://127.0.0.1:1/v1";
const COMMAND: &str = "workspace_run_command";

fn authorization() -> Authorization {
    Authorization::StandingAllow {
        settings_revision: 1,
    }
}

#[test]
fn an_unknown_session_is_not_found() {
    let fixture = Fixture::new();
    let service = fixture.service(NO_ENDPOINT);

    let error = service
        .session_processes(wire(session_request(Uuid::new_v4())))
        .expect_err("unknown session");

    assert_eq!(error.code(), "not_found");
}

#[test]
fn a_session_with_no_calls_lists_nothing() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("quiet", read_tools());
    let service = fixture.service(NO_ENDPOINT);
    let journal =
        SessionJournal::create(&fixture.data, &workspace.path, "Quiet").expect("create session");
    let session_id = journal.snapshot().session_id;
    drop(journal);

    let listed = service
        .session_processes(wire(session_request(session_id)))
        .expect("processes");

    assert!(listed.entries.is_empty());
    assert!(!listed.truncated);
    // A read-only listing creates no run journal.
    assert_eq!(listed.run_details, RunDetails::Available);
    assert!(!contains_run_journal(&fixture.data));
}

fn contains_run_journal(directory: &std::path::Path) -> bool {
    std::fs::read_dir(directory)
        .expect("read directory")
        .filter_map(Result::ok)
        .any(|entry| {
            let path = entry.path();
            path.file_name().is_some_and(|name| name == "runs-v1.jsonl")
                || (path.is_dir() && contains_run_journal(&path))
        })
}

#[test]
fn recorded_calls_join_their_requests_and_exclude_other_sessions() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("busy", read_tools());
    let service = fixture.service(NO_ENDPOINT);
    let workspace_id: WorkspaceId = workspace.id.to_string().parse().expect("workspace id");

    let mut journal =
        SessionJournal::create(&fixture.data, &workspace.path, "Commands").expect("create");
    let session_id = journal.snapshot().session_id;
    let turn = journal.start_turn().expect("turn");
    journal
        .record_tool_request(
            turn,
            "call-ok",
            COMMAND,
            &json!({"profile": "rust", "program": "cargo", "arguments": ["test", "--lib"]})
                .to_string(),
        )
        .expect("request");
    journal
        .record_tool_request(
            turn,
            "call-denied",
            COMMAND,
            &json!({"profile": "rust", "program": "rm", "arguments": ["-rf", "x"]}).to_string(),
        )
        .expect("request");
    journal
        .record_tool_request(
            turn,
            "call-open",
            COMMAND,
            &json!({"profile": "rust", "program": "sleep", "arguments": ["5"]}).to_string(),
        )
        .expect("request");
    drop(journal);

    let mut actions = ActionJournal::open(&fixture.data).expect("action journal");
    let started = |call_id, session| StartedAction {
        session_id: session,
        workspace_id,
        call_id,
        tool: ActionTool::RunCommand,
        targets: &[],
        request_sha256: None,
        authorization: authorization(),
    };
    let ok = actions
        .record_started(&started("call-ok", session_id))
        .expect("start");
    actions
        .record_finished(ok, ActionOutcome::Succeeded, None, Some(Uuid::new_v4()))
        .expect("finish");
    actions
        .record_refused(&RefusedAction {
            session_id,
            workspace_id,
            call_id: "call-denied",
            tool: ActionTool::RunCommand,
            targets: &[],
            request_sha256: None,
            reason: RefusalReason::AccessDenied,
        })
        .expect("refuse");
    actions
        .record_started(&started("call-open", session_id))
        .expect("start");
    // Another session's call and another tool of this session are not listed.
    actions
        .record_started(&started("call-other", Uuid::new_v4()))
        .expect("start");
    actions
        .record_started(&StartedAction {
            tool: ActionTool::ReadFile,
            targets: &["a.txt"],
            ..started("call-read", session_id)
        })
        .expect("start");
    drop(actions);

    let listed = service
        .session_processes(wire(session_request(session_id)))
        .expect("processes");

    assert_eq!(listed.run_details, RunDetails::Available);
    assert!(!listed.truncated);
    let calls: Vec<(&str, ProcessState)> = listed
        .entries
        .iter()
        .map(|entry| (entry.call_id.as_str(), entry.state))
        .collect();
    // No turn is active in this service, so the open call is unfinished.
    assert_eq!(
        calls,
        [
            ("call-open", ProcessState::Unfinished),
            ("call-denied", ProcessState::Refused),
            ("call-ok", ProcessState::Finished),
        ]
    );
    assert_eq!(listed.entries[0].program.as_deref(), Some("sleep"));
    assert_eq!(listed.entries[1].reason.as_deref(), Some("access_denied"));
    assert_eq!(listed.entries[1].program.as_deref(), Some("rm"));
    let finished = &listed.entries[2];
    assert_eq!(finished.program.as_deref(), Some("cargo"));
    assert_eq!(
        finished.arguments.as_deref(),
        Some(&["test".to_owned(), "--lib".to_owned()][..])
    );
    // The run identifier names no run in the run journal, so there is no result.
    assert_eq!(finished.result, None);
}
