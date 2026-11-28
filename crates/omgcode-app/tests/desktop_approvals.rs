//! Approval commands of the desktop service (ADR-0046, ADR-0063) end to end:
//! a scripted model requests a gated tool call, the turn waits while the
//! operator reviews and decides through the service, and the same call then
//! completes in the same turn.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use omgcode_app::desktop::{
    AllowHostView, ApprovalOperationDto, ApprovalStateDto, ApprovalView, Binding, ChatState,
    ChatStatus, Confirmation, DesktopService, FolderPurpose, SettingsPrompt,
};
use omgcode_core::control::{ApprovalDeclaration, ApprovalOperation, ControlStore, now_unix};
use omgcode_core::policy::{Access, PermissionSet, WorkspaceId};
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
const ACTION_JOURNAL_NAME: &str = "model-actions-v1.jsonl";
/// How long a test waits for a turn to record its approval.
const PENDING_DEADLINE: Duration = Duration::from_secs(20);

struct Setup {
    fixture: Fixture,
    workspace: Workspace,
    service: DesktopService,
    server: MockServer,
}

fn setup(steps: Vec<Step>) -> Setup {
    setup_with(all_tools(), steps)
}

fn setup_with(permissions: PermissionSet, steps: Vec<Step>) -> Setup {
    let fixture = Fixture::new();
    fixture.declare_profile();
    let workspace = fixture.workspace("project", permissions);
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

    /// Send one message without waiting for the turn.
    fn start(&self, session: Uuid, message: &str) {
        self.service
            .chat_send(wire(json!({
                "session_id": session,
                "message": message,
                "settings": settings_json(&self.server.endpoint),
                "skills": [],
            })))
            .expect("send");
    }

    /// Send one message and wait for the turn to complete.
    fn turn(&self, session: Uuid, message: &str) {
        self.start(session, message);
        self.finish(session);
    }

    /// Wait for the session's turn to end and require it to have completed.
    fn finish(&self, session: Uuid) -> ChatStatus {
        let done = wait_finished(&self.service, session);
        assert_eq!(done.state, ChatState::Completed, "{:?}", done.error);
        done
    }

    /// Wait until the session's running turn has recorded a pending approval
    /// and is waiting on it, and return its ID.
    fn await_pending(&self, session: Uuid) -> Uuid {
        let deadline = Instant::now() + PENDING_DEADLINE;
        loop {
            if let Some(view) = self
                .pending()
                .into_iter()
                .find(|view| view.session_id == session)
            {
                return view.id;
            }
            if Instant::now() >= deadline {
                let status = self
                    .service
                    .chat_status(wire(json!({"session_id": session})))
                    .expect("status");
                panic!("no approval was recorded; turn {status:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Deny `id` and let its waiting turn complete.
    fn deny_and_finish(&self, id: Uuid, session: Uuid) {
        self.decide(id, "deny", false, None).expect("deny");
        self.finish(session);
    }

    /// The approval list. `busy` is the list's documented answer while a
    /// running turn holds the settings store, so it is read again until the
    /// shared deadline; any other error, or a store that stays held, fails.
    fn list(&self) -> Vec<ApprovalView> {
        let deadline = Instant::now() + PENDING_DEADLINE;
        loop {
            match self.service.approvals_list() {
                Ok(list) => return list.approvals,
                Err(error) if error.code() == "busy" && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("approvals: {error:?}"),
            }
        }
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

    /// Decide with a prompt that fails the test if any dialog opens.
    fn decide(
        &self,
        id: Uuid,
        decision: &str,
        writers_stopped: bool,
        token: Option<&str>,
    ) -> Result<ApprovalView, omgcode_app::desktop::DesktopError> {
        self.decide_with(id, decision, writers_stopped, token, &Prompt::silent())
    }

    fn decide_with(
        &self,
        id: Uuid,
        decision: &str,
        writers_stopped: bool,
        token: Option<&str>,
        prompt: &Prompt,
    ) -> Result<ApprovalView, omgcode_app::desktop::DesktopError> {
        self.service.approval_decide(
            wire(json!({
                "id": id,
                "decision": decision,
                "writers_stopped": writers_stopped,
                "review_token": token,
            })),
            prompt,
        )
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

    /// The event of every model action journal record, parsed.
    fn actions(&self) -> Vec<Value> {
        fs::read_to_string(self.fixture.data.join(ACTION_JOURNAL_NAME))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let record: Value = serde_json::from_str(line).expect("an action record");
                record["event"].clone()
            })
            .collect()
    }
}

/// Answers native confirmations from a script and records them. A dialog
/// beyond the script, or any folder dialog, fails the test.
#[derive(Default)]
struct Prompt {
    answers: RefCell<VecDeque<bool>>,
    asked: RefCell<Vec<Confirmation>>,
    /// Runs while the confirmation is open, to change state meanwhile.
    during: RefCell<Option<Box<dyn FnOnce()>>>,
}

impl Prompt {
    /// A prompt that must not be asked anything.
    fn silent() -> Self {
        Self::default()
    }

    fn answering(confirmed: bool) -> Self {
        let prompt = Self::default();
        prompt.answers.borrow_mut().push_back(confirmed);
        prompt
    }

    fn meanwhile(self, action: impl FnOnce() + 'static) -> Self {
        *self.during.borrow_mut() = Some(Box::new(action));
        self
    }

    fn asked(&self) -> Vec<Confirmation> {
        self.asked.take()
    }
}

impl SettingsPrompt for Prompt {
    fn pick_folder(&self, _purpose: FolderPurpose, _start: Option<&Path>) -> Option<PathBuf> {
        panic!("an approval never opens a folder dialog");
    }

    fn confirm(&self, confirmation: &Confirmation) -> bool {
        self.asked.borrow_mut().push(confirmation.clone());
        if let Some(action) = self.during.borrow_mut().take() {
            action();
        }
        self.answers
            .borrow_mut()
            .pop_front()
            .expect("an unexpected confirmation")
    }
}

/// The host patterns a workspace lists now, as text.
fn listed_hosts(data: &Path, workspace: Uuid) -> Vec<String> {
    let control = ControlStore::open_existing(data)
        .expect("open control")
        .expect("control store");
    let id = control
        .registered_workspace(workspace)
        .expect("registered workspace")
        .id();
    control
        .web_fetch_hosts(id)
        .iter()
        .map(|pattern| pattern.as_str().to_owned())
        .collect()
}

#[test]
fn an_edit_approval_is_reviewed_approved_and_applied_in_the_same_turn() {
    let setup = setup(vec![
        Step::reply(tool_call("edit-1", EDIT_TOOL, &edit_args("notes.txt"))),
        Step::reply(text("applied")),
    ]);
    let notes = setup.workspace.path.join("notes.txt");
    fs::write(&notes, "alpha bcc\n").expect("workspace file");
    let session = setup.session("Edits");
    let service = &setup.service;
    let mut responses = Vec::new();

    assert_eq!(service.approvals_summary().expect("summary").pending, 0);
    setup.start(session, "fix the typo");
    let id = setup.await_pending(session);
    let summary = service.approvals_summary().expect("summary");
    assert_eq!(summary.pending, 1);
    responses.push(text_of(&summary));

    // The list shows it with the session's title and folder.
    let listed = service.approvals_list().expect("list");
    responses.push(text_of(&listed));
    assert_eq!(listed.approvals.len(), 1);
    let approval = &listed.approvals[0];
    assert_eq!(approval.id, id);
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

    // Review while the turn waits: exact, with the edit's fields and a token,
    // read from the call the waiting turn published. Nothing is changed.
    let detail = setup.detail(id);
    responses.push(text_of(&detail));
    assert_eq!(detail.binding, Binding::Exact);
    assert!(detail.waiting, "the turn waits on this approval");
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

    // The waiting turn consumes the approval, applies, and completes: the
    // model was asked twice, never to repeat the call.
    setup.finish(session);
    assert_eq!(fs::read_to_string(&notes).expect("read"), "alpha bc\n");
    assert_eq!(setup.state_of(id), ApprovalStateDto::Consumed);
    responses.push(text_of(&setup.list()));

    // The action journal records the edit as authorised by this approval.
    let actions = setup.actions();
    let started = actions
        .iter()
        .find(|record| record["type"] == "action_started")
        .expect("a start record");
    assert_eq!(started["payload"]["tool"], "edit_file");
    assert_eq!(
        started["payload"]["authorization"]["payload"]["approval_id"],
        json!(id)
    );
    assert!(
        actions
            .iter()
            .any(|record| record["type"] == "action_finished"
                && record["payload"]["outcome"] == "succeeded"),
        "{actions:?}"
    );

    assert_no_leaks(&setup.fixture, &responses);
    assert_eq!(setup.server.finish(), 2);
}

#[test]
fn an_allowed_create_runs_without_an_approval_and_is_journaled() {
    let permissions = PermissionSet {
        write: Access::Allow,
        ..all_tools()
    };
    let setup = setup_with(
        permissions,
        vec![
            Step::reply(tool_call("create-1", CREATE_TOOL, &create_args("new.txt"))),
            Step::reply(text("created")),
        ],
    );
    let session = setup.session("Allowed");
    setup.turn(session, "add a file");
    assert_eq!(
        fs::read_to_string(setup.workspace.path.join("new.txt")).expect("created"),
        "hello\nworld\n"
    );
    assert!(setup.list().is_empty(), "no approval was recorded");
    let actions = setup.actions();
    let started = actions
        .iter()
        .find(|record| record["type"] == "action_started")
        .expect("a start record");
    assert_eq!(started["payload"]["tool"], "create_file");
    assert_eq!(
        started["payload"]["authorization"]["type"],
        "standing_allow"
    );
    assert_eq!(setup.server.finish(), 2);
}

#[test]
fn a_denied_tool_limit_refuses_the_call_and_journals_the_refusal() {
    let mut permissions = PermissionSet {
        write: Access::Allow,
        ..all_tools()
    };
    permissions
        .write_tools
        .set(omgcode_core::policy::WriteTool::Create, Access::Deny);
    let setup = setup_with(
        permissions,
        vec![Step::reply(tool_call(
            "create-1",
            CREATE_TOOL,
            &create_args("new.txt"),
        ))],
    );
    let session = setup.session("Limited");
    setup.start(session, "add a file");
    // The create tool was not offered, so the call fails closed and ends the
    // turn; nothing is created and no approval is recorded.
    let done = wait_finished(&setup.service, session);
    assert_eq!(done.state, ChatState::Failed);
    assert_eq!(done.error.expect("an error").code(), "permission");
    assert!(!setup.workspace.path.join("new.txt").exists());
    assert!(setup.list().is_empty());
    assert!(
        setup
            .actions()
            .iter()
            .any(|record| record["type"] == "action_refused"),
        "{:?}",
        setup.actions()
    );
    assert_eq!(setup.server.finish(), 1);
}

#[test]
fn a_command_approval_is_decided_while_the_turn_waits() {
    let setup = setup(vec![
        Step::reply(tool_call("cmd-1", COMMAND_TOOL, &command_args())),
        Step::reply(text("ran")),
    ]);
    let session = setup.session("Commands");
    setup.start(session, "say hello");
    let id = setup.await_pending(session);
    let pending = setup.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].operation, ApprovalOperationDto::Command);

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
    // Allow host belongs to web fetches only (ADR-0110).
    assert_eq!(detail.allow_host, None);

    // `writers_stopped` is ignored for a command.
    let token = detail.review_token.expect("a token");
    let approved = setup
        .decide(id, "approve", false, Some(&token))
        .expect("approve a command");
    assert_eq!(approved.state, ApprovalStateDto::Approved);
    assert!(!approved.writers_stopped);

    setup.finish(session);
    assert_eq!(setup.state_of(id), ApprovalStateDto::Consumed);
    assert_eq!(setup.server.finish(), 2);
}

#[test]
fn a_turn_that_starts_waiting_tells_the_observer_once_and_the_wait_still_needs_a_decision() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let setup = setup(vec![
        Step::reply(tool_call("cmd-1", COMMAND_TOOL, &command_args())),
        Step::reply(text("denied")),
    ]);
    let told = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&told);
    setup
        .service
        .set_approval_wait_observer(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }))
        .expect("observer");
    let session = setup.session("Observed");
    setup.start(session, "say hello");
    let id = setup.await_pending(session);

    // The record exists before the turn publishes its call and starts
    // waiting, so the observer may run just after the record appears.
    let deadline = Instant::now() + PENDING_DEADLINE;
    while told.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "the observer was not called");
        std::thread::sleep(Duration::from_millis(10));
    }
    // Telling the observer decided nothing: the turn still waits.
    assert!(setup.detail(id).waiting);
    assert_eq!(setup.state_of(id), ApprovalStateDto::Pending);

    setup.deny_and_finish(id, session);
    assert_eq!(told.load(Ordering::SeqCst), 1);
    assert_eq!(setup.server.finish(), 2);
}

#[test]
fn shutdown_drops_the_approval_wait_observer() {
    use std::sync::Arc;

    let setup = setup(Vec::new());
    let observer: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    let watch = Arc::downgrade(&observer);
    setup
        .service
        .set_approval_wait_observer(observer)
        .expect("observer");
    assert!(watch.upgrade().is_some(), "the service keeps the observer");
    let report = setup.service.shutdown(Duration::from_millis(100));
    assert_eq!(report.still_running, 0);
    // No turn ran, so the service held the only owner.
    assert!(watch.upgrade().is_none(), "shutdown drops the observer");
    assert_eq!(setup.server.finish(), 0);
}

#[test]
fn a_finished_turn_does_not_keep_the_observer_after_shutdown() {
    use std::sync::Arc;

    let setup = setup(vec![Step::reply(text("hello"))]);
    let observer: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    let watch = Arc::downgrade(&observer);
    setup
        .service
        .set_approval_wait_observer(observer)
        .expect("observer");
    let session = setup.session("Finished");
    setup.turn(session, "say hello");
    // The turn table keeps the finished slot, which must hold no observer.
    let report = setup.service.shutdown(Duration::from_secs(5));
    assert_eq!(report.still_running, 0);
    assert!(
        watch.upgrade().is_none(),
        "a finished turn's slot keeps the observer"
    );
    assert_eq!(setup.server.finish(), 1);
}

/// Two sessions, each with a turn waiting on one pending edit approval for
/// its own file. Each turn needs one more model reply once it is decided.
fn two_pending() -> (Setup, [(Uuid, Uuid); 2]) {
    let setup = setup(vec![
        Step::reply(tool_call("edit-a", EDIT_TOOL, &edit_args("a.txt"))),
        Step::reply(tool_call("edit-b", EDIT_TOOL, &edit_args("b.txt"))),
        Step::reply(text("done")),
        Step::reply(text("done")),
    ]);
    fs::write(setup.workspace.path.join("a.txt"), "abcc\n").expect("a");
    fs::write(setup.workspace.path.join("b.txt"), "bbcc\n").expect("b");
    let first = setup.session("First");
    setup.start(first, "edit a");
    let a = setup.await_pending(first);
    let second = setup.session("Second");
    setup.start(second, "edit b");
    let b = setup.await_pending(second);
    assert_eq!(setup.pending().len(), 2);
    (setup, [(a, first), (b, second)])
}

#[test]
fn approve_needs_a_matching_unused_unexpired_review_token() {
    let (setup, [(a, first), (b, second)]) = two_pending();

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

    setup.finish(first);
    assert_eq!(
        fs::read_to_string(setup.workspace.path.join("a.txt")).expect("a"),
        "abc\n"
    );
    setup.deny_and_finish(b, second);
    assert_eq!(setup.server.finish(), 4);
}

#[test]
fn deny_needs_no_token_ends_the_wait_and_a_second_decision_is_not_pending() {
    let (setup, [(a, first), (b, second)]) = two_pending();
    let denied = setup.decide(a, "deny", false, None).expect("deny");
    assert_eq!(denied.state, ApprovalStateDto::Denied);
    // The denied call ends without a change, and the turn completes.
    setup.finish(first);
    assert_eq!(
        fs::read_to_string(setup.workspace.path.join("a.txt")).expect("a"),
        "abcc\n"
    );

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
    setup.deny_and_finish(b, second);
    assert_eq!(setup.server.finish(), 4);
}

#[test]
fn reviewing_a_non_pending_approval_reads_nothing_and_returns_stale_without_a_token() {
    let (setup, [(a, first), (b, second)]) = two_pending();
    setup.deny_and_finish(a, first);

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
    setup.deny_and_finish(b, second);
    assert_eq!(setup.server.finish(), 4);
}

#[test]
fn a_changed_target_makes_the_review_stale_and_blocks_approval() {
    let (setup, [(a, first), (b, second)]) = two_pending();
    fs::write(setup.workspace.path.join("a.txt"), "completely different\n").expect("change a");
    let detail = setup.detail(a);
    assert_eq!(detail.binding, Binding::Stale);
    assert_eq!(detail.review_token, None);
    assert!(detail.fields.is_empty());
    let refused = setup
        .decide(a, "approve", true, None)
        .expect_err("a stale review has no token");
    assert_eq!(refused.code(), "review_expired");
    setup.deny_and_finish(a, first);
    setup.deny_and_finish(b, second);
    assert_eq!(setup.server.finish(), 4);
}

#[test]
fn a_turn_running_for_another_reason_makes_the_review_busy() {
    let (release, hold) = common::gate();
    let (announce, arrived) = std::sync::mpsc::channel();
    let setup = setup(vec![
        Step::reply(tool_call("edit-a", EDIT_TOOL, &edit_args("a.txt"))),
        Step::gated(text("later"), hold).announcing(announce),
    ]);
    fs::write(setup.workspace.path.join("a.txt"), "abcc\n").expect("a");
    let session = setup.session("Busy review");
    setup.start(session, "edit");
    let id = setup.await_pending(session);

    // Cancelling interrupts the wait; the record stays pending, undecided.
    setup
        .service
        .chat_cancel(wire(json!({"session_id": session})))
        .expect("cancel");
    let cancelled = wait_finished(&setup.service, session);
    assert_eq!(cancelled.state, ChatState::Failed);
    assert_eq!(cancelled.error.expect("an error").code(), "cancelled");
    assert_eq!(setup.state_of(id), ApprovalStateDto::Pending);

    // With no turn running, the review reads the journal. The request is
    // still exact, but no turn waits on it, so approving would only allow a
    // later identical call.
    let idle = setup.detail(id);
    assert_eq!(idle.binding, Binding::Exact);
    assert!(!idle.waiting);

    // A later turn of the session holds the journal without waiting on this
    // record, so the review cannot read the request.
    setup.start(session, "again");
    common::wait_arrived(&arrived);
    let busy = setup
        .service
        .approval_detail(wire(json!({"id": id})))
        .expect_err("the journal belongs to the running turn");
    assert_eq!(busy.code(), "busy");
    release.send(()).expect("release");
    setup.finish(session);
    assert_eq!(setup.server.finish(), 2);
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
    for decision in ["deny", "allow_host"] {
        let missing = service
            .approval_decide(
                wire(json!({
                    "id": Uuid::new_v4(), "decision": decision, "writers_stopped": false, "review_token": null
                })),
                &Prompt::silent(),
            )
            .expect_err("no such approval");
        assert_eq!(missing.code(), "not_found");
    }
    assert!(json_of(&listed).get("now_unix").is_some());
}

const MULTI_EDIT_TOOL: &str = "workspace_multi_edit_file";
const CREATE_TOOL: &str = "workspace_create_file";

fn multi_edit_args(path: &str) -> Value {
    json!({
        "path": path,
        "edits": [
            {"old_text": "gamma", "new_text": "G"},
            {"old_text": "alpha", "new_text": "A"},
        ],
    })
}

fn create_args(path: &str) -> Value {
    json!({"path": path, "content": "hello\nworld\n"})
}

fn field_value<'a>(detail: &'a omgcode_app::desktop::ApprovalDetail, label: &str) -> &'a str {
    detail
        .fields
        .iter()
        .find(|field| field.label == label)
        .map(|field| field.value.as_str())
        .unwrap_or_else(|| panic!("field {label}"))
}

#[test]
fn a_multi_edit_approval_is_reviewed_approved_and_applied_in_the_same_turn() {
    let setup = setup(vec![
        Step::reply(tool_call(
            "multi-1",
            MULTI_EDIT_TOOL,
            &multi_edit_args("notes.txt"),
        )),
        Step::reply(text("applied")),
    ]);
    let notes = setup.workspace.path.join("notes.txt");
    fs::write(&notes, "alpha beta gamma\n").expect("workspace file");
    let session = setup.session("Multi");
    setup.start(session, "change two things");
    let id = setup.await_pending(session);

    let pending = setup.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].operation, ApprovalOperationDto::FileWrite);
    let detail = setup.detail(id);
    assert_eq!(detail.binding, Binding::Exact);
    assert_eq!(detail.tool.as_deref(), Some(MULTI_EDIT_TOOL));
    assert_eq!(field_value(&detail, "File"), "notes.txt");
    assert_eq!(field_value(&detail, "Edits"), "2");
    assert_eq!(field_value(&detail, "Old text 1"), "gamma");
    assert_eq!(field_value(&detail, "New text 2"), "A");
    assert_eq!(field_value(&detail, "Size"), "17 -> 9 bytes");
    assert_eq!(
        fs::read_to_string(&notes).expect("read"),
        "alpha beta gamma\n"
    );
    let token = detail.review_token.clone().expect("a review token");
    setup
        .decide(id, "approve", true, Some(&token))
        .expect("approve");

    setup.finish(session);
    assert_eq!(fs::read_to_string(&notes).expect("read"), "A beta G\n");
    assert_eq!(setup.state_of(id), ApprovalStateDto::Consumed);
    assert_eq!(setup.server.finish(), 2);
}

#[test]
fn a_changed_multi_edit_target_makes_the_review_stale() {
    let setup = setup(vec![
        Step::reply(tool_call(
            "multi-1",
            MULTI_EDIT_TOOL,
            &multi_edit_args("notes.txt"),
        )),
        Step::reply(text("stopped")),
    ]);
    fs::write(setup.workspace.path.join("notes.txt"), "alpha beta gamma\n").expect("file");
    let session = setup.session("Multi");
    setup.start(session, "change two things");
    let id = setup.await_pending(session);
    assert_eq!(setup.detail(id).binding, Binding::Exact);
    fs::write(
        setup.workspace.path.join("notes.txt"),
        "alpha beta gamma!\n",
    )
    .expect("change");
    let detail = setup.detail(id);
    assert_eq!(detail.binding, Binding::Stale);
    assert_eq!(detail.review_token, None);
    assert_eq!(
        setup
            .decide(id, "approve", true, None)
            .expect_err("a stale review has no token")
            .code(),
        "review_expired"
    );
    setup.deny_and_finish(id, session);
    assert_eq!(setup.server.finish(), 2);
}

#[test]
fn a_create_approval_is_reviewed_approved_and_applied_in_the_same_turn() {
    let setup = setup(vec![
        Step::reply(tool_call("create-1", CREATE_TOOL, &create_args("new.txt"))),
        Step::reply(text("created")),
    ]);
    let created = setup.workspace.path.join("new.txt");
    let session = setup.session("Create");
    setup.start(session, "add a file");
    let id = setup.await_pending(session);

    assert_eq!(setup.pending().len(), 1);
    let detail = setup.detail(id);
    assert_eq!(detail.binding, Binding::Exact);
    assert_eq!(detail.tool.as_deref(), Some(CREATE_TOOL));
    assert_eq!(field_value(&detail, "File"), "new.txt");
    assert_eq!(field_value(&detail, "Size"), "12 bytes");
    assert_eq!(field_value(&detail, "Content"), "hello\nworld\n");
    assert!(!created.exists());
    let token = detail.review_token.clone().expect("a review token");
    setup
        .decide(id, "approve", true, Some(&token))
        .expect("approve");

    setup.finish(session);
    assert_eq!(
        fs::read_to_string(&created).expect("read"),
        "hello\nworld\n"
    );
    assert_eq!(setup.state_of(id), ApprovalStateDto::Consumed);
    assert_eq!(setup.server.finish(), 2);
}

#[test]
fn a_create_review_becomes_stale_once_the_file_exists() {
    let setup = setup(vec![
        Step::reply(tool_call("create-1", CREATE_TOOL, &create_args("new.txt"))),
        Step::reply(text("stopped")),
    ]);
    let session = setup.session("Create");
    setup.start(session, "add a file");
    let id = setup.await_pending(session);
    assert_eq!(setup.detail(id).binding, Binding::Exact);
    fs::write(setup.workspace.path.join("new.txt"), "someone else").expect("other writer");
    let detail = setup.detail(id);
    assert_eq!(detail.binding, Binding::Stale);
    assert_eq!(detail.review_token, None);
    setup.deny_and_finish(id, session);
    assert_eq!(setup.server.finish(), 2);
}

const RENAME_TOOL: &str = "workspace_rename_file";
const DELETE_TOOL: &str = "workspace_delete_file";

#[test]
fn a_rename_approval_is_reviewed_approved_and_applied_in_the_same_turn() {
    let args = json!({"from": "a.txt", "to": "b.txt"});
    let setup = setup(vec![
        Step::reply(tool_call("rename-1", RENAME_TOOL, &args)),
        Step::reply(text("renamed")),
    ]);
    let source = setup.workspace.path.join("a.txt");
    fs::write(&source, "alpha\n").expect("workspace file");
    let session = setup.session("Rename");
    setup.start(session, "rename a file");
    let id = setup.await_pending(session);

    let pending = setup.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].operation, ApprovalOperationDto::FileWrite);
    let detail = setup.detail(id);
    assert_eq!(detail.binding, Binding::Exact);
    assert_eq!(detail.tool.as_deref(), Some(RENAME_TOOL));
    assert_eq!(field_value(&detail, "From"), "a.txt");
    assert_eq!(field_value(&detail, "To"), "b.txt");
    assert_eq!(field_value(&detail, "Size"), "6 bytes");
    assert!(source.exists());
    let token = detail.review_token.clone().expect("a review token");
    setup
        .decide(id, "approve", true, Some(&token))
        .expect("approve");

    setup.finish(session);
    assert!(!source.exists());
    assert_eq!(
        fs::read_to_string(setup.workspace.path.join("b.txt")).expect("read"),
        "alpha\n"
    );
    assert_eq!(setup.state_of(id), ApprovalStateDto::Consumed);
    assert_eq!(setup.server.finish(), 2);
}

#[test]
fn a_rename_review_becomes_stale_once_the_destination_exists() {
    let args = json!({"from": "a.txt", "to": "b.txt"});
    let setup = setup(vec![
        Step::reply(tool_call("rename-1", RENAME_TOOL, &args)),
        Step::reply(text("stopped")),
    ]);
    fs::write(setup.workspace.path.join("a.txt"), "alpha\n").expect("file");
    let session = setup.session("Rename");
    setup.start(session, "rename a file");
    let id = setup.await_pending(session);
    assert_eq!(setup.detail(id).binding, Binding::Exact);
    fs::write(setup.workspace.path.join("b.txt"), "someone else").expect("other writer");
    let detail = setup.detail(id);
    assert_eq!(detail.binding, Binding::Stale);
    assert_eq!(detail.review_token, None);
    setup.deny_and_finish(id, session);
    assert_eq!(setup.server.finish(), 2);
}

#[test]
fn a_delete_approval_is_reviewed_approved_and_applied_in_the_same_turn() {
    let args = json!({"path": "old.txt"});
    let setup = setup(vec![
        Step::reply(tool_call("delete-1", DELETE_TOOL, &args)),
        Step::reply(text("deleted")),
    ]);
    let target = setup.workspace.path.join("old.txt");
    fs::write(&target, "alpha\n").expect("workspace file");
    let session = setup.session("Delete");
    setup.start(session, "delete a file");
    let id = setup.await_pending(session);

    assert_eq!(setup.pending().len(), 1);
    let detail = setup.detail(id);
    assert_eq!(detail.binding, Binding::Exact);
    assert_eq!(detail.tool.as_deref(), Some(DELETE_TOOL));
    assert_eq!(field_value(&detail, "File"), "old.txt");
    assert_eq!(field_value(&detail, "Size"), "6 bytes");
    assert!(target.exists());
    let token = detail.review_token.clone().expect("a review token");
    setup
        .decide(id, "approve", true, Some(&token))
        .expect("approve");

    setup.finish(session);
    assert!(!target.exists());
    assert_eq!(setup.state_of(id), ApprovalStateDto::Consumed);
    assert_eq!(setup.server.finish(), 2);
}

#[test]
fn a_delete_review_becomes_stale_once_the_file_changes() {
    let args = json!({"path": "old.txt"});
    let setup = setup(vec![
        Step::reply(tool_call("delete-1", DELETE_TOOL, &args)),
        Step::reply(text("stopped")),
    ]);
    fs::write(setup.workspace.path.join("old.txt"), "alpha\n").expect("file");
    let session = setup.session("Delete");
    setup.start(session, "delete a file");
    let id = setup.await_pending(session);
    assert_eq!(setup.detail(id).binding, Binding::Exact);
    fs::write(setup.workspace.path.join("old.txt"), "alpha changed\n").expect("change");
    let detail = setup.detail(id);
    assert_eq!(detail.binding, Binding::Stale);
    assert_eq!(detail.review_token, None);
    setup.deny_and_finish(id, session);
    assert_eq!(setup.server.finish(), 2);
}

const WEB_FETCH_TOOL: &str = "web_fetch";

/// Model tools with `web_fetch` set to ask for every fetch (ADR-0098).
fn fetching() -> PermissionSet {
    PermissionSet {
        web_fetch: Access::Prompt,
        ..all_tools()
    }
}

/// These tests never approve a waiting fetch: the desktop service builds its
/// turns with the production fetcher, which would reach the network.
#[test]
fn a_network_approval_is_reviewed_while_the_turn_waits_and_a_denial_ends_the_call() {
    let args = json!({"url": "https://Docs.Example.com/guide#top", "offset": 10});
    let setup = setup_with(
        fetching(),
        vec![
            Step::reply(tool_call("fetch-1", WEB_FETCH_TOOL, &args)),
            Step::reply(text("stopped")),
        ],
    );
    let session = setup.session("Fetch");
    setup.start(session, "read the docs");
    let id = setup.await_pending(session);

    let pending = setup.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].operation, ApprovalOperationDto::Network);
    assert!(!pending[0].writers_stopped);
    assert_eq!(pending[0].summary, "Fetch https://docs.example.com/guide");

    let detail = setup.detail(id);
    assert_eq!(detail.binding, Binding::Exact);
    assert!(detail.waiting);
    assert_eq!(detail.tool.as_deref(), Some(WEB_FETCH_TOOL));
    assert_eq!(
        field_value(&detail, "URL"),
        "https://docs.example.com/guide"
    );
    assert_eq!(field_value(&detail, "Host"), "docs.example.com");
    assert_eq!(field_value(&detail, "Host on the allowlist now"), "no");
    assert_eq!(field_value(&detail, "Offset"), "10");
    assert_eq!(field_value(&detail, "Maximum bytes"), "12288");
    assert!(detail.review_token.is_some());
    // Under `prompt` a listed host would still ask, so Allow host is not
    // offered, and the review says why (ADR-0110).
    assert_eq!(detail.allow_host, Some(AllowHostView::NotAllow));

    // A denial needs no token and ends the wait without a fetch.
    setup.deny_and_finish(id, session);
    assert_eq!(setup.state_of(id), ApprovalStateDto::Denied);
    assert_eq!(setup.server.finish(), 2);
}

#[test]
fn a_network_approval_is_approved_with_a_review_token_and_no_writers_stopped_acknowledgement() {
    let args = json!({"url": "https://docs.example.com/guide"});
    let setup = setup_with(
        fetching(),
        vec![Step::reply(tool_call("fetch-1", WEB_FETCH_TOOL, &args))],
    );
    let session = setup.session("Fetch");
    setup.start(session, "read the docs");
    let id = setup.await_pending(session);

    // Cancelling interrupts the wait and leaves the record pending, so the
    // approval below spends nothing and nothing is fetched.
    setup
        .service
        .chat_cancel(wire(json!({"session_id": session})))
        .expect("cancel");
    let cancelled = wait_finished(&setup.service, session);
    assert_eq!(cancelled.state, ChatState::Failed);
    assert_eq!(setup.state_of(id), ApprovalStateDto::Pending);

    let detail = setup.detail(id);
    assert_eq!(detail.binding, Binding::Exact);
    assert!(!detail.waiting);
    let refused = setup
        .decide(id, "approve", true, None)
        .expect_err("approval needs a review token");
    assert_eq!(refused.code(), "review_expired");

    let detail = setup.detail(id);
    let token = detail.review_token.expect("a fresh token");
    let approved = setup
        .decide(id, "approve", false, Some(&token))
        .expect("a network approval needs no writers-stopped acknowledgement");
    assert_eq!(approved.state, ApprovalStateDto::Approved);
    assert_eq!(approved.operation, ApprovalOperationDto::Network);
    assert!(!approved.writers_stopped);
    assert_eq!(setup.server.finish(), 1);
}

/// Model tools with `web_fetch` at `allow`: a listed host runs without an
/// approval, and any other host asks (ADR-0098).
fn fetching_allowed() -> PermissionSet {
    PermissionSet {
        web_fetch: Access::Allow,
        ..all_tools()
    }
}

/// Start a turn whose one fetch asks for approval, then cancel it so the
/// record stays pending and nothing can be fetched (see above). Returns the
/// setup, the approval, and a fresh exact review of it.
fn cancelled_fetch(
    permissions: PermissionSet,
    url: &str,
    before: impl FnOnce(&Setup),
) -> (Setup, Uuid, omgcode_app::desktop::ApprovalDetail) {
    let args = json!({ "url": url });
    let setup = setup_with(
        permissions,
        vec![Step::reply(tool_call("fetch-1", WEB_FETCH_TOOL, &args))],
    );
    before(&setup);
    let session = setup.session("Fetch");
    setup.start(session, "read the docs");
    let id = setup.await_pending(session);
    setup
        .service
        .chat_cancel(wire(json!({"session_id": session})))
        .expect("cancel");
    assert_eq!(
        wait_finished(&setup.service, session).state,
        ChatState::Failed
    );
    let detail = setup.detail(id);
    assert_eq!(detail.binding, Binding::Exact);
    (setup, id, detail)
}

/// Add a host pattern through the trusted store, as the CLI would.
fn add_host(data: &Path, workspace: Uuid, pattern: &str) {
    let mut control = ControlStore::open_existing(data)
        .expect("open control")
        .expect("control store");
    let id = control
        .registered_workspace(workspace)
        .expect("registered workspace")
        .id();
    control
        .add_web_fetch_host(id, pattern, control.revision())
        .expect("add host");
}

#[test]
fn allow_host_approves_the_fetch_and_lists_its_exact_host_after_a_native_confirmation() {
    let (setup, id, detail) = cancelled_fetch(
        fetching_allowed(),
        "https://Docs.Example.com/guide?page=2",
        |setup| add_host(&setup.fixture.data, setup.workspace.id, "other.example.org"),
    );
    assert_eq!(field_value(&detail, "Host on the allowlist now"), "no");
    assert_eq!(
        detail.allow_host,
        Some(AllowHostView::Offered {
            host: "docs.example.com".to_owned()
        })
    );
    let token = detail.review_token.expect("a token");

    let prompt = Prompt::answering(true);
    let approved = setup
        .decide_with(id, "allow_host", true, Some(&token), &prompt)
        .expect("allow host");
    assert_eq!(approved.state, ApprovalStateDto::Approved);
    // The acknowledgement belongs to file writes and is ignored here.
    assert!(!approved.writers_stopped);
    assert_eq!(
        listed_hosts(&setup.fixture.data, setup.workspace.id),
        ["other.example.org", "docs.example.com"],
        "the exact host, appended, and no wildcard"
    );

    let asked = prompt.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].title, "Allow a web host");
    let message = &asked[0].message;
    assert!(message.contains("docs.example.com"), "{message}");
    // The workspace folder, named `project` by the fixture.
    assert!(message.contains("project"), "{message}");
    assert!(!message.contains("Docs.Example"), "{message}");
    assert!(
        message.contains("whatever their path or query"),
        "{message}"
    );
    assert!(message.contains("subdomains are not included"), "{message}");

    // The record was approved under the revision before the host was listed,
    // so a later review finds it decided and offers nothing.
    let again = setup.detail(id);
    assert_eq!(again.binding, Binding::Stale);
    assert_eq!(again.allow_host, None);
    assert_eq!(setup.server.finish(), 1);
}

#[test]
fn allow_host_without_an_offer_is_refused_before_any_dialog() {
    let (setup, id, detail) = cancelled_fetch(fetching(), "https://docs.example.com/guide", |_| ());
    assert_eq!(detail.allow_host, Some(AllowHostView::NotAllow));
    let token = detail.review_token.expect("a token");

    let refused = setup
        .decide(id, "allow_host", false, Some(&token))
        .expect_err("prompt offers no host");
    assert_eq!(refused.code(), "allow_host_unavailable");
    // A missing token is refused the same way as for Approve.
    let refused = setup
        .decide(id, "allow_host", false, None)
        .expect_err("no token");
    assert_eq!(refused.code(), "review_expired");
    assert_eq!(setup.state_of(id), ApprovalStateDto::Pending);
    assert!(listed_hosts(&setup.fixture.data, setup.workspace.id).is_empty());
    assert_eq!(setup.server.finish(), 1);
}

#[test]
fn a_declined_allow_host_changes_nothing_and_spends_the_token() {
    let (setup, id, detail) =
        cancelled_fetch(fetching_allowed(), "https://docs.example.com/guide", |_| ());
    let token = detail.review_token.expect("a token");

    let prompt = Prompt::answering(false);
    let refused = setup
        .decide_with(id, "allow_host", false, Some(&token), &prompt)
        .expect_err("declined");
    assert_eq!(refused.code(), "cancelled");
    assert_eq!(prompt.asked().len(), 1);
    assert_eq!(setup.state_of(id), ApprovalStateDto::Pending);
    assert!(listed_hosts(&setup.fixture.data, setup.workspace.id).is_empty());

    // The token was spent before the dialog, so neither decision reuses it.
    for decision in ["allow_host", "approve"] {
        let reused = setup
            .decide(id, decision, false, Some(&token))
            .expect_err("a spent token");
        assert_eq!(reused.code(), "review_expired", "{decision}");
    }
    assert_eq!(setup.server.finish(), 1);
}

#[test]
fn a_settings_change_while_allow_host_asks_refuses_the_change() {
    let (setup, id, detail) =
        cancelled_fetch(fetching_allowed(), "https://docs.example.com/guide", |_| ());
    let token = detail.review_token.expect("a token");

    // The store is released while the dialog is open, so another trusted
    // writer can change the settings meanwhile.
    let data = setup.fixture.data.clone();
    let workspace = setup.workspace.id;
    let prompt =
        Prompt::answering(true).meanwhile(move || add_host(&data, workspace, "other.example.org"));
    let refused = setup
        .decide_with(id, "allow_host", false, Some(&token), &prompt)
        .expect_err("the review went out of date");
    assert_eq!(refused.code(), "review_mismatch");
    assert_eq!(setup.state_of(id), ApprovalStateDto::Pending);
    assert_eq!(
        listed_hosts(&setup.fixture.data, setup.workspace.id),
        ["other.example.org"]
    );
    assert_eq!(setup.detail(id).binding, Binding::Stale);
    assert_eq!(setup.server.finish(), 1);
}

#[test]
fn a_full_allowlist_is_reported_and_offers_no_host() {
    let (setup, id, detail) = cancelled_fetch(
        fetching_allowed(),
        "https://docs.example.com/guide",
        |setup| {
            for index in 0..omgcode_core::control::MAX_WEB_FETCH_HOSTS {
                add_host(
                    &setup.fixture.data,
                    setup.workspace.id,
                    &format!("host{index}.example.org"),
                );
            }
        },
    );
    assert_eq!(detail.allow_host, Some(AllowHostView::Full));
    let token = detail.review_token.expect("a token");
    let refused = setup
        .decide(id, "allow_host", false, Some(&token))
        .expect_err("full");
    assert_eq!(refused.code(), "allow_host_unavailable");
    assert_eq!(setup.state_of(id), ApprovalStateDto::Pending);
    assert_eq!(setup.server.finish(), 1);
}
