//! Approval commands of the desktop service (ADR-0046) end to end: a scripted
//! model requests a gated tool call, the operator reviews and decides through
//! the service, and the identical call in a later turn consumes the approval.

use std::fs;
use std::time::Duration;

use omgcode_app::desktop::{
    ApprovalOperationDto, ApprovalStateDto, ApprovalView, Binding, ChatState, DesktopService,
};
use omgcode_core::control::{ApprovalDeclaration, ApprovalOperation, ControlStore, now_unix};
use omgcode_core::policy::WorkspaceId;
use serde_json::{Value, json};
use uuid::Uuid;

mod common;
use common::{
    Fixture, MockServer, Step, Workspace, all_tools, assert_no_leaks, json_of, settings_json, text,
    text_of, tool_call, wait_finished, wire,
};

const EDIT_TOOL: &str = "workspace_edit_file";
const COMMAND_TOOL: &str = "workspace_run_command";
const FILE_AUDIT_NAME: &str = "file-operations-v1.jsonl";

struct Setup {
    fixture: Fixture,
    workspace: Workspace,
    service: DesktopService,
    server: MockServer,
}

fn setup(steps: Vec<Step>) -> Setup {
    let fixture = Fixture::new();
    fixture.declare_profile();
    let workspace = fixture.workspace("project", all_tools());
    let server = MockServer::start(steps);
    let service = fixture.service(&server.endpoint);
    Setup {
        fixture,
        workspace,
        service,
        server,
    }
}

fn edit_args(path: &str) -> Value {
    json!({"path": path, "old_text": "bc", "new_text": "b"})
}

fn command_args() -> Value {
    json!({"profile": "system", "program": "echo", "arguments": ["hello"]})
}

impl Setup {
    fn session(&self, title: &str) -> Uuid {
        self.service
            .session_create(wire(
                json!({"workspace_id": self.workspace.id, "title": title}),
            ))
            .expect("create session")
            .session_id
    }

    /// Send one message and wait for the turn to complete.
    fn turn(&self, session: Uuid, message: &str) {
        self.service
            .chat_send(wire(json!({
                "session_id": session,
                "message": message,
                "settings": settings_json(&self.server.endpoint),
            })))
            .expect("send");
        let done = wait_finished(&self.service, session);
        assert_eq!(done.state, ChatState::Completed, "{:?}", done.error);
    }

    fn list(&self) -> Vec<ApprovalView> {
        self.service.approvals_list().expect("approvals").approvals
    }

    fn pending(&self) -> Vec<ApprovalView> {
        self.list()
            .into_iter()
            .filter(|view| view.state == ApprovalStateDto::Pending)
            .collect()
    }

    fn detail(&self, id: Uuid) -> omgcode_app::desktop::ApprovalDetail {
        self.service
            .approval_detail(wire(json!({"id": id})))
            .expect("detail")
    }

    fn decide(
        &self,
        id: Uuid,
        decision: &str,
        writers_stopped: bool,
        token: Option<&str>,
    ) -> Result<ApprovalView, omgcode_app::desktop::DesktopError> {
        self.service.approval_decide(wire(json!({
            "id": id,
            "decision": decision,
            "writers_stopped": writers_stopped,
            "review_token": token,
        })))
    }

    fn state_of(&self, id: Uuid) -> ApprovalStateDto {
        self.list()
            .into_iter()
            .find(|view| view.id == id)
            .expect("listed approval")
            .state
    }

    fn audit_lines(&self) -> usize {
        fs::read_to_string(self.fixture.data.join(FILE_AUDIT_NAME))
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }
}

#[test]
fn an_edit_approval_is_reviewed_approved_and_consumed_by_the_identical_call() {
    let setup = setup(vec![
        Step::reply(tool_call("edit-1", EDIT_TOOL, &edit_args("notes.txt"))),
        Step::reply(text("waiting for approval")),
        Step::reply(tool_call("edit-2", EDIT_TOOL, &edit_args("notes.txt"))),
        Step::reply(text("applied")),
    ]);
    let notes = setup.workspace.path.join("notes.txt");
    fs::write(&notes, "alpha bcc\n").expect("workspace file");
    let session = setup.session("Edits");
    let service = &setup.service;
    let mut responses = Vec::new();

    assert_eq!(service.approvals_summary().expect("summary").pending, 0);
    setup.turn(session, "fix the typo");
    let summary = service.approvals_summary().expect("summary");
    assert_eq!(summary.pending, 1);
    responses.push(text_of(&summary));

    // The list shows it with the session's title and folder.
    let listed = service.approvals_list().expect("list");
    responses.push(text_of(&listed));
    assert_eq!(listed.approvals.len(), 1);
    let approval = &listed.approvals[0];
    assert_eq!(approval.session_id, session);
    assert_eq!(approval.session_title.as_deref(), Some("Edits"));
    assert!(
        approval
            .folder_display
            .as_deref()
            .is_some_and(|folder| folder.ends_with("project"))
    );
    assert_eq!(approval.operation, ApprovalOperationDto::FileWrite);
    assert_eq!(approval.state, ApprovalStateDto::Pending);
    assert!(!approval.writers_stopped);
    assert_eq!(approval.request_sha256.len(), 64);
    assert!(listed.now_unix <= approval.expires_unix);
    let id = approval.id;

    // Review: exact, with the edit's fields and a token. Nothing is changed.
    let detail = setup.detail(id);
    responses.push(text_of(&detail));
    assert_eq!(detail.binding, Binding::Exact);
    assert_eq!(detail.tool.as_deref(), Some(EDIT_TOOL));
    let field = |label: &str| {
        detail
            .fields
            .iter()
            .find(|field| field.label == label)
            .unwrap_or_else(|| panic!("field {label}"))
    };
    assert_eq!(field("File").value, "notes.txt");
    assert_eq!(field("Old text").value, "bc");
    assert!(field("Old text").multiline);
    assert_eq!(field("New text").value, "b");
    assert_eq!(field("Size").value, "10 -> 9 bytes");
    assert_eq!(fs::read_to_string(&notes).expect("read"), "alpha bcc\n");
    let first_token = detail.review_token.clone().expect("a review token");

    // Approve without the writers-stopped acknowledgement is refused, and the
    // refused attempt consumed the token.
    let refused = setup
        .decide(id, "approve", false, Some(&first_token))
        .expect_err("a file write needs the acknowledgement");
    assert_eq!(refused.code(), "writers_stopped_required");
    assert_eq!(setup.state_of(id), ApprovalStateDto::Pending);
    let reused = setup
        .decide(id, "approve", true, Some(&first_token))
        .expect_err("the token is single use");
    assert_eq!(reused.code(), "review_expired");

    // A fresh review gives a fresh token, and with the acknowledgement it works.
    let again = setup.detail(id);
    let second_token = again.review_token.expect("a fresh token");
    assert_ne!(second_token, first_token);
    let approved = setup
        .decide(id, "approve", true, Some(&second_token))
        .expect("approve");
    responses.push(text_of(&approved));
    assert_eq!(approved.state, ApprovalStateDto::Approved);
    assert!(approved.writers_stopped);
    assert_eq!(service.approvals_summary().expect("summary").pending, 0);
    assert_eq!(fs::read_to_string(&notes).expect("read"), "alpha bcc\n");

    // The model repeats the identical call; core consumes the approval and applies.
    setup.turn(session, "try again");
    assert_eq!(fs::read_to_string(&notes).expect("read"), "alpha bc\n");
    assert_eq!(setup.state_of(id), ApprovalStateDto::Consumed);
    responses.push(text_of(&setup.list()));

    assert_no_leaks(&setup.fixture, &responses);
    assert_eq!(setup.server.finish(), 4);
}

#[test]
fn a_command_approval_is_consumed_by_the_identical_call() {
    let setup = setup(vec![
        Step::reply(tool_call("cmd-1", COMMAND_TOOL, &command_args())),
        Step::reply(text("waiting")),
        Step::reply(tool_call("cmd-2", COMMAND_TOOL, &command_args())),
        Step::reply(text("ran")),
    ]);
    let session = setup.session("Commands");
    setup.turn(session, "say hello");
    let pending = setup.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].operation, ApprovalOperationDto::Command);
    let id = pending[0].id;

    let detail = setup.detail(id);
    assert_eq!(detail.binding, Binding::Exact);
    assert_eq!(detail.tool.as_deref(), Some(COMMAND_TOOL));
    let value_of = |label: &str| {
        detail
            .fields
            .iter()
            .find(|field| field.label == label)
            .map(|field| field.value.as_str())
    };
    assert_eq!(value_of("Profile"), Some("system"));
    assert_eq!(value_of("Program"), Some("echo"));
    assert_eq!(value_of("Arguments"), Some("1: hello"));

    // `writers_stopped` is ignored for a command.
    let token = detail.review_token.expect("a token");
    let approved = setup
        .decide(id, "approve", false, Some(&token))
        .expect("approve a command");
    assert_eq!(approved.state, ApprovalStateDto::Approved);
    assert!(!approved.writers_stopped);

    setup.turn(session, "again");
    assert_eq!(setup.state_of(id), ApprovalStateDto::Consumed);
    assert_eq!(setup.server.finish(), 4);
}

/// Two sessions, each with one pending edit approval on its own file.
fn two_pending() -> (Setup, Uuid, Uuid) {
    let setup = setup(vec![
        Step::reply(tool_call("edit-a", EDIT_TOOL, &edit_args("a.txt"))),
        Step::reply(text("a asked")),
        Step::reply(tool_call("edit-b", EDIT_TOOL, &edit_args("b.txt"))),
        Step::reply(text("b asked")),
    ]);
    fs::write(setup.workspace.path.join("a.txt"), "abcc\n").expect("a");
    fs::write(setup.workspace.path.join("b.txt"), "bbcc\n").expect("b");
    let first = setup.session("First");
    setup.turn(first, "edit a");
    let second = setup.session("Second");
    setup.turn(second, "edit b");
    let pending = setup.pending();
    assert_eq!(pending.len(), 2);
    let id_for = |session: Uuid| {
        pending
            .iter()
            .find(|view| view.session_id == session)
            .expect("approval for session")
            .id
    };
    let (a, b) = (id_for(first), id_for(second));
    (setup, a, b)
}

#[test]
fn approve_needs_a_matching_unused_unexpired_review_token() {
    let (setup, a, b) = two_pending();

    // No token at all.
    let none = setup
        .decide(a, "approve", true, None)
        .expect_err("no token");
    assert_eq!(none.code(), "review_expired");

    // A wrong token is a mismatch, and it burns the right one.
    let token = setup.detail(a).review_token.expect("token");
    let wrong = setup
        .decide(a, "approve", true, Some("not-the-token"))
        .expect_err("wrong token");
    assert_eq!(wrong.code(), "review_mismatch");
    let burnt = setup
        .decide(a, "approve", true, Some(&token))
        .expect_err("the right token no longer works");
    assert_eq!(burnt.code(), "review_expired");

    // A token for approval A cannot approve B, and does not disturb A's own.
    let token_a = setup.detail(a).review_token.expect("token for a");
    let foreign = setup
        .decide(b, "approve", true, Some(&token_a))
        .expect_err("a's token on b");
    assert_eq!(foreign.code(), "review_expired");
    let token_b = setup.detail(b).review_token.expect("token for b");
    let crossed = setup
        .decide(b, "approve", true, Some(&token_a))
        .expect_err("a's token on b with b reviewed");
    assert_eq!(crossed.code(), "review_mismatch");
    assert_eq!(setup.state_of(b), ApprovalStateDto::Pending);
    let still = setup
        .decide(a, "approve", true, Some(&token_a))
        .expect("a's own token still works");
    assert_eq!(still.state, ApprovalStateDto::Approved);
    // B's token was burnt by the crossed attempt.
    let burnt_b = setup
        .decide(b, "approve", true, Some(&token_b))
        .expect_err("b's token was removed by the failed attempt");
    assert_eq!(burnt_b.code(), "review_expired");
}

#[test]
fn deny_needs_no_token_and_a_second_decision_is_not_pending() {
    let (setup, a, b) = two_pending();
    let denied = setup.decide(a, "deny", false, None).expect("deny");
    assert_eq!(denied.state, ApprovalStateDto::Denied);

    for decision in ["deny", "approve"] {
        let twice = setup
            .decide(a, decision, true, Some("whatever"))
            .expect_err("already decided");
        assert_eq!(twice.code(), "not_pending");
    }
    let unknown = setup
        .decide(Uuid::new_v4(), "deny", false, None)
        .expect_err("unknown approval");
    assert_eq!(unknown.code(), "not_found");

    // The list shows pending first, then the resolved one.
    let listed = setup.list();
    assert_eq!(listed[0].id, b);
    assert_eq!(listed[1].id, a);
    assert_eq!(
        setup.service.approvals_summary().expect("summary").pending,
        1
    );
}

#[test]
fn reviewing_a_non_pending_approval_reads_nothing_and_returns_stale_without_a_token() {
    let (setup, a, b) = two_pending();
    setup.decide(a, "deny", false, None).expect("deny");

    let before = setup.audit_lines();
    let detail = setup.detail(a);
    assert_eq!(detail.binding, Binding::Stale);
    assert_eq!(detail.tool, None);
    assert!(detail.fields.is_empty());
    assert_eq!(detail.review_token, None);
    assert_eq!(detail.approval.state, ApprovalStateDto::Denied);
    assert_eq!(setup.audit_lines(), before, "no file was read");

    // A pending review reads exactly the target file once, through the audit.
    let exact = setup.detail(b);
    assert_eq!(exact.binding, Binding::Exact);
    assert!(
        setup.audit_lines() > before,
        "the exact review read the file"
    );

    let unknown = setup
        .service
        .approval_detail(wire(json!({"id": Uuid::new_v4()})))
        .expect_err("unknown approval");
    assert_eq!(unknown.code(), "not_found");
}

#[test]
fn a_changed_target_makes_the_review_stale_and_blocks_approval() {
    let (setup, a, _) = two_pending();
    fs::write(setup.workspace.path.join("a.txt"), "completely different\n").expect("change a");
    let detail = setup.detail(a);
    assert_eq!(detail.binding, Binding::Stale);
    assert_eq!(detail.review_token, None);
    assert!(detail.fields.is_empty());
    let refused = setup
        .decide(a, "approve", true, None)
        .expect_err("a stale review has no token");
    assert_eq!(refused.code(), "review_expired");
}

#[test]
fn a_turn_running_for_the_session_makes_the_review_busy() {
    let (release, hold) = common::gate();
    let (announce, arrived) = std::sync::mpsc::channel();
    let setup = setup(vec![
        Step::reply(tool_call("edit-a", EDIT_TOOL, &edit_args("a.txt"))),
        Step::reply(text("asked")),
        Step::gated(text("later"), hold).announcing(announce),
    ]);
    fs::write(setup.workspace.path.join("a.txt"), "abcc\n").expect("a");
    let session = setup.session("Busy review");
    setup.turn(session, "edit");
    let id = setup.pending()[0].id;

    setup
        .service
        .chat_send(wire(json!({
            "session_id": session,
            "message": "again",
            "settings": settings_json(&setup.server.endpoint),
        })))
        .expect("send");
    common::wait_arrived(&arrived);
    let busy = setup
        .service
        .approval_detail(wire(json!({"id": id})))
        .expect_err("the journal belongs to the running turn");
    assert_eq!(busy.code(), "busy");
    release.send(()).expect("release");
    wait_finished(&setup.service, session);
}

#[test]
fn the_list_orders_pending_first_caps_resolved_ones_and_escapes_summaries() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", all_tools());
    let service = fixture.service("http://127.0.0.1:1/v1");
    let workspace_id: WorkspaceId = {
        let control = ControlStore::open(&fixture.data).expect("control");
        control
            .workspaces()
            .find(|registered| *registered.id().as_uuid() == workspace.id)
            .expect("registered")
            .id()
    };
    let session = Uuid::new_v4();
    let mut ids = Vec::new();
    {
        let mut control = ControlStore::open(&fixture.data).expect("control");
        let now = now_unix();
        for number in 0..62_u32 {
            let summary = if number == 61 {
                "hostile \u{202e}summary".to_owned()
            } else {
                format!("network request {number}")
            };
            let status = control
                .request_approval(
                    &ApprovalDeclaration {
                        session_id: session,
                        workspace_id,
                        operation: ApprovalOperation::Network,
                        request_sha256: format!("{number:064x}"),
                        summary,
                        ttl_secs: 600,
                    },
                    now,
                )
                .expect("request an approval");
            ids.push(status.id);
        }
        // Deny all but the first and the last: 60 resolved, 2 pending.
        for id in &ids[1..61] {
            control.decide_approval(*id, false, now).expect("deny");
        }
    }

    let listed = service.approvals_list().expect("list").approvals;
    assert_eq!(listed.len(), 2 + 50);
    assert_eq!(listed[0].id, ids[0]);
    assert_eq!(listed[1].id, ids[61]);
    assert_eq!(
        listed[1].summary,
        omgcode_core::turn::display_safe_line("hostile \u{202e}summary")
    );
    assert!(listed[1].summary.contains("\\u{202e}"));
    // The 50 most recent resolved ones, in store order: numbers 11 through 60.
    let resolved: Vec<Uuid> = listed[2..].iter().map(|view| view.id).collect();
    assert_eq!(resolved, ids[11..61].to_vec());
    assert!(
        listed[2..]
            .iter()
            .all(|view| view.state == ApprovalStateDto::Denied)
    );
    // An unknown session has no labels.
    assert!(listed[0].session_title.is_none() && listed[0].folder_display.is_none());
    assert_eq!(listed[0].operation, ApprovalOperationDto::Network);
    assert_eq!(service.approvals_summary().expect("summary").pending, 2);

    // A network record has no reviewable tool: its review is `not_found`, not an error.
    let detail = service
        .approval_detail(wire(json!({"id": ids[0]})))
        .expect("detail");
    assert_eq!(detail.binding, Binding::NotFound);
    assert!(detail.review_token.is_none());
}

#[test]
fn the_summary_serves_its_last_count_while_the_store_is_held_and_the_list_reports_busy() {
    let fixture = Fixture::new();
    let service = fixture.service("http://127.0.0.1:1/v1");
    // No approvals yet; remember the count of zero, then add one and read it.
    assert_eq!(service.approvals_summary().expect("summary").pending, 0);
    let workspace = fixture.workspace("project", all_tools());
    {
        let mut control = ControlStore::open(&fixture.data).expect("control");
        let id = control
            .workspaces()
            .find(|registered| *registered.id().as_uuid() == workspace.id)
            .expect("registered")
            .id();
        control
            .request_approval(
                &ApprovalDeclaration {
                    session_id: Uuid::new_v4(),
                    workspace_id: id,
                    operation: ApprovalOperation::Network,
                    request_sha256: "a".repeat(64),
                    summary: "one".to_owned(),
                    ttl_secs: 600,
                },
                now_unix(),
            )
            .expect("request");
    }
    assert_eq!(service.approvals_summary().expect("summary").pending, 1);

    let held = ControlStore::open(&fixture.data).expect("hold the settings store");
    let begun = std::time::Instant::now();
    let cached = service
        .approvals_summary()
        .expect("a busy store is not an error");
    assert_eq!(cached.pending, 1, "the last count read is served");
    assert!(
        begun.elapsed() >= Duration::from_millis(1500),
        "bounded retry"
    );
    let busy = service
        .approvals_list()
        .expect_err("the list needs the store");
    assert_eq!(busy.code(), "busy");
    let busy = service
        .workspace_list()
        .expect_err("the workspace list needs the store");
    assert_eq!(busy.code(), "busy");
    drop(held);
    assert_eq!(service.approvals_summary().expect("summary").pending, 1);
}

#[test]
fn without_a_settings_store_there_are_no_approvals() {
    let fixture = Fixture::bare();
    let service = fixture.service("http://127.0.0.1:1/v1");
    assert_eq!(service.approvals_summary().expect("summary").pending, 0);
    let listed = service.approvals_list().expect("list");
    assert!(listed.approvals.is_empty());
    let missing = service
        .approval_detail(wire(json!({"id": Uuid::new_v4()})))
        .expect_err("no such approval");
    assert_eq!(missing.code(), "not_found");
    let missing = service
        .approval_decide(wire(json!({
            "id": Uuid::new_v4(), "decision": "deny", "writers_stopped": false, "review_token": null
        })))
        .expect_err("no such approval");
    assert_eq!(missing.code(), "not_found");
    assert!(json_of(&listed).get("now_unix").is_some());
}
