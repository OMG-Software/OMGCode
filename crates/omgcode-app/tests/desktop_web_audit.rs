//! The `session_web_audit` command of the desktop service, against a real
//! session journal and a real model action journal (ADR-0098).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use omgcode_app::desktop::{WebAuditAuthorization, WebAuditOutcome, WebAuditTarget};
use omgcode_core::action_journal::{
    ActionJournal, ActionOutcome, ActionTool, Authorization, RefusalReason, RefusedAction,
    StartedAction,
};
use omgcode_core::policy::WorkspaceId;
use omgcode_core::session::SessionJournal;
use serde_json::json;
use uuid::Uuid;

mod common;
use common::{Fixture, read_tools, session_request, text_of, wire};

const NO_ENDPOINT: &str = "http://127.0.0.1:1/v1";
const COMMAND: &str = "workspace_run_command";
const URL: &str = "https://example.com/docs?page=2";

fn authorization() -> Authorization {
    Authorization::StandingAllow {
        settings_revision: 1,
    }
}

/// Every file under `directory` with its length.
fn snapshot(directory: &Path) -> BTreeMap<PathBuf, u64> {
    let mut files = BTreeMap::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).expect("read directory").flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let length = entry.metadata().expect("metadata").len();
                files.insert(path, length);
            }
        }
    }
    files
}

#[test]
fn an_unknown_session_is_not_found() {
    let fixture = Fixture::new();
    let service = fixture.service(NO_ENDPOINT);

    let error = service
        .session_web_audit(wire(session_request(Uuid::new_v4())))
        .expect_err("unknown session");

    assert_eq!(error.code(), "not_found");
}

#[test]
fn a_session_that_never_fetched_lists_nothing_and_creates_no_file() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("quiet", read_tools());
    let service = fixture.service(NO_ENDPOINT);
    let journal =
        SessionJournal::create(&fixture.data, &workspace.path, "Quiet").expect("create session");
    let session_id = journal.snapshot().session_id;
    drop(journal);
    let before = snapshot(&fixture.data);

    let audit = service
        .session_web_audit(wire(session_request(session_id)))
        .expect("audit");

    assert!(audit.entries.is_empty());
    assert!(!audit.truncated);
    assert_eq!(snapshot(&fixture.data), before);
}

#[test]
fn only_this_sessions_fetches_and_later_actions_are_listed_newest_first_with_command_text() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("web", read_tools());
    let service = fixture.service(NO_ENDPOINT);
    let workspace_id: WorkspaceId = workspace.id.to_string().parse().expect("workspace id");

    let mut journal =
        SessionJournal::create(&fixture.data, &workspace.path, "Web").expect("create");
    let session_id = journal.snapshot().session_id;
    let turn = journal.start_turn().expect("turn");
    journal
        .record_tool_request(
            turn,
            "call-run",
            COMMAND,
            &json!({"profile": "rust", "program": "curl", "arguments": ["-s", "--max-time", "5"]})
                .to_string(),
        )
        .expect("request");
    drop(journal);

    let other_session = Uuid::new_v4();
    let mut actions = ActionJournal::open(&fixture.data).expect("action journal");
    let started = |call_id, tool, session, targets, after_web_content| StartedAction {
        session_id: session,
        workspace_id,
        call_id,
        tool,
        targets,
        request_sha256: None,
        authorization: authorization(),
        after_web_content,
    };
    // A read before any fetch is not listed.
    let early = actions
        .record_started(&started(
            "call-early",
            ActionTool::ReadFile,
            session_id,
            &["a.txt"],
            false,
        ))
        .expect("start");
    actions
        .record_finished(early, ActionOutcome::Succeeded, None, None)
        .expect("finish");
    // The fetch, then actions made after it.
    let fetch = actions
        .record_started(&started(
            "call-fetch",
            ActionTool::Network,
            session_id,
            &[URL],
            false,
        ))
        .expect("start");
    actions
        .record_finished(fetch, ActionOutcome::Succeeded, None, None)
        .expect("finish");
    let edit = actions
        .record_started(&started(
            "call-edit",
            ActionTool::EditFile,
            session_id,
            &["src/main.rs"],
            true,
        ))
        .expect("start");
    actions
        .record_finished(edit, ActionOutcome::Failed, Some("not_found"), None)
        .expect("finish");
    actions
        .record_refused(&RefusedAction {
            session_id,
            workspace_id,
            call_id: "call-refused",
            tool: ActionTool::DeleteFile,
            targets: &["secrets.txt"],
            request_sha256: None,
            reason: RefusalReason::AccessDenied,
            after_web_content: true,
        })
        .expect("refuse");
    let run = actions
        .record_started(&started(
            "call-run",
            ActionTool::RunCommand,
            session_id,
            &[],
            true,
        ))
        .expect("start");
    actions
        .record_finished(run, ActionOutcome::Succeeded, None, None)
        .expect("finish");
    // Another session's fetch, and an absolute path of this session that
    // failed validation.
    let foreign = actions
        .record_started(&started(
            "call-foreign",
            ActionTool::Network,
            other_session,
            &[URL],
            false,
        ))
        .expect("start");
    actions
        .record_finished(foreign, ActionOutcome::Succeeded, None, None)
        .expect("finish");
    actions
        .record_refused(&RefusedAction {
            session_id,
            workspace_id,
            call_id: "call-hostile",
            tool: ActionTool::ReadFile,
            targets: &["/etc/passwd"],
            request_sha256: None,
            reason: RefusalReason::InvalidPath,
            after_web_content: true,
        })
        .expect("refuse");
    drop(actions);
    let before = snapshot(&fixture.data);

    let audit = service
        .session_web_audit(wire(session_request(session_id)))
        .expect("audit");

    assert!(!audit.truncated);
    let calls: Vec<&str> = audit
        .entries
        .iter()
        .map(|entry| entry.call_id.as_str())
        .collect();
    assert_eq!(
        calls,
        [
            "call-hostile",
            "call-run",
            "call-refused",
            "call-edit",
            "call-fetch"
        ]
    );

    let hostile = &audit.entries[0];
    assert_eq!(hostile.outcome, WebAuditOutcome::Refused);
    assert_eq!(hostile.reason.as_deref(), Some("invalid_path"));
    assert_eq!(hostile.authorization, None);
    // The path failed validation: only its length is returned, never a digest.
    assert!(matches!(
        hostile.targets.as_slice(),
        [WebAuditTarget::Withheld { bytes: 11 }]
    ));
    assert!(!text_of(&audit).contains("sha256"));

    let command = &audit.entries[1];
    assert_eq!(command.tool, "run_command");
    assert_eq!(command.program.as_deref(), Some("curl"));
    assert_eq!(
        command.arguments.as_deref(),
        Some(&["-s".to_owned(), "--max-time".to_owned(), "5".to_owned()][..])
    );
    assert_eq!(command.argument_count, Some(3));
    assert!(command.targets.is_empty());

    let edit = &audit.entries[3];
    assert_eq!(edit.outcome, WebAuditOutcome::Failed);
    assert_eq!(edit.reason.as_deref(), Some("not_found"));
    assert!(edit.after_web_content);
    assert_eq!(
        edit.targets,
        [WebAuditTarget::Text {
            text: "src/main.rs".to_owned()
        }]
    );

    let fetch = &audit.entries[4];
    assert!(fetch.is_fetch);
    assert_eq!(fetch.tool, "network");
    assert_eq!(fetch.outcome, WebAuditOutcome::Succeeded);
    assert_eq!(
        fetch.authorization,
        Some(WebAuditAuthorization::StandingAllow)
    );
    assert_eq!(
        fetch.targets,
        [WebAuditTarget::Text {
            text: URL.to_owned()
        }]
    );
    // A read-only listing writes nothing.
    assert_eq!(snapshot(&fixture.data), before);
}

#[test]
fn more_than_one_hundred_fetches_list_the_newest_hundred_and_say_truncated() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("many", read_tools());
    let service = fixture.service(NO_ENDPOINT);
    let workspace_id: WorkspaceId = workspace.id.to_string().parse().expect("workspace id");
    let journal = SessionJournal::create(&fixture.data, &workspace.path, "Many").expect("create");
    let session_id = journal.snapshot().session_id;
    drop(journal);
    let mut actions = ActionJournal::open(&fixture.data).expect("action journal");
    for number in 0..105 {
        let call_id = format!("call-{number:03}");
        let target = format!("https://example.com/page/{number}");
        let id = actions
            .record_started(&StartedAction {
                session_id,
                workspace_id,
                call_id: &call_id,
                tool: ActionTool::Network,
                targets: &[target.as_str()],
                request_sha256: None,
                authorization: authorization(),
                after_web_content: false,
            })
            .expect("start");
        actions
            .record_finished(id, ActionOutcome::Succeeded, None, None)
            .expect("finish");
    }
    drop(actions);

    let audit = service
        .session_web_audit(wire(session_request(session_id)))
        .expect("audit");

    assert_eq!(audit.entries.len(), 100);
    assert!(audit.truncated);
    assert_eq!(audit.entries[0].call_id, "call-104");
    assert_eq!(audit.entries[99].call_id, "call-005");
}
