//! Chat commands of the desktop service end to end: a real journal, a real
//! index, and a scripted loopback model server. Every wait is bounded.

use std::time::{Duration, Instant};

use omgcode_app::desktop::{
    ChatEvent, ChatState, ChatStatus, DesktopError, DesktopService, PreferencesSource,
    SessionStatus, ToolsMode,
};
use serde_json::json;
use uuid::Uuid;

mod common;
use common::{
    Fixture, MockServer, SECRET, Step, Workspace, fixed_token, gate, json_of, read_tools,
    session_request, settings_json, text, text_of, text_only, tool_call, unused_endpoint,
    wait_arrived, wait_finished, wire,
};

fn new_session(service: &DesktopService, workspace: &Workspace, title: &str) -> Uuid {
    service
        .session_create(wire(json!({"workspace_id": workspace.id, "title": title})))
        .expect("create session")
        .session_id
}

fn send(
    service: &DesktopService,
    session: Uuid,
    message: &str,
    endpoint: &str,
) -> Result<ChatStatus, DesktopError> {
    service.chat_send(wire(json!({
        "session_id": session,
        "message": message,
        "settings": settings_json(endpoint),
        "skills": [],
    })))
}

fn status(service: &DesktopService, session: Uuid) -> ChatStatus {
    service
        .chat_status(wire(session_request(session)))
        .expect("status")
}

fn texts(service: &DesktopService, session: Uuid) -> Vec<(String, String)> {
    let page = service
        .session_transcript(wire(
            json!({"session_id": session, "after_seq": null, "limit": 200}),
        ))
        .expect("transcript");
    page.entries
        .iter()
        .filter_map(|entry| {
            let value = json_of(entry);
            (value["kind"] == "message").then(|| {
                (
                    value["role"].as_str().unwrap_or_default().to_owned(),
                    value["text"].as_str().unwrap_or_default().to_owned(),
                )
            })
        })
        .collect()
}

/// A service on a fresh fixture with one workspace and its model server.
fn started(permissions: omgcode_core::policy::PermissionSet, steps: Vec<Step>) -> Started {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", permissions);
    let server = MockServer::start(steps);
    let service = fixture.service(&server.endpoint);
    Started {
        _fixture: fixture,
        workspace,
        server,
        service,
    }
}

struct Started {
    /// Keeps the private data root alive for the test.
    _fixture: Fixture,
    workspace: Workspace,
    server: MockServer,
    service: DesktopService,
}

#[test]
fn a_text_only_turn_runs_in_the_background_and_completes_with_its_outcome() {
    let started = started(text_only(), vec![Step::reply(text("pong"))]);
    let service = &started.service;
    let session = new_session(service, &started.workspace, "Chat");

    let running = send(service, session, "ping", &started.server.endpoint).expect("send");
    assert_eq!(running.state, ChatState::Running);
    assert_eq!(running.session_id, session);
    assert!(running.outcome.is_none() && running.error.is_none());

    let done = wait_finished(service, session);
    assert_eq!(done.state, ChatState::Completed);
    let outcome = done.outcome.expect("an outcome");
    assert_eq!(outcome.finish_reason, "stop");
    assert_eq!(outcome.tools_mode, ToolsMode::Disabled);
    assert!(outcome.tools_disabled_reason.is_some());
    assert_eq!((outcome.rounds, outcome.tool_calls), (0, 0));
    assert_eq!(outcome.withheld_tool_results, 0);
    let context = outcome.context.expect("a context meter");
    assert_eq!(context.context_tokens, 131_072);
    assert!(context.estimated_tokens > 0 && context.trigger_tokens > 0);
    assert!(!context.compacted);
    assert!(done.error.is_none());
    // Reading the result again returns the same one until the next send.
    assert_eq!(status(service, session).state, ChatState::Completed);

    assert_eq!(
        texts(service, session),
        [
            ("user".to_owned(), "ping".to_owned()),
            ("assistant".to_owned(), "pong".to_owned())
        ]
    );
    let tree = service
        .session_tree(wire(
            json!({"view": "active", "sort": "updated", "descending": true}),
        ))
        .expect("tree");
    let summary = &tree.groups[0].sessions[0];
    assert_eq!(summary.status, SessionStatus::Idle);
    assert!(summary.event_count > 1, "the index saw the finished turn");
    assert_eq!(started.server.finish(), 1);
}

#[test]
fn a_tool_turn_reports_rounds_and_tool_calls() {
    let started = started(
        read_tools(),
        vec![
            Step::reply(tool_call(
                "call-1",
                "workspace_list_directory",
                &json!({"path": ""}),
            )),
            Step::reply(text("listed")),
        ],
    );
    let service = &started.service;
    std::fs::write(
        started.workspace.path.join("notes.txt"),
        "alpha
",
    )
    .expect("workspace file");
    let session = new_session(service, &started.workspace, "Tools");
    send(service, session, "list", &started.server.endpoint).expect("send");
    let done = wait_finished(service, session);
    let outcome = done
        .outcome
        .clone()
        .unwrap_or_else(|| panic!("failed: {:?}", done.error));
    assert_eq!(outcome.tools_mode, ToolsMode::Enabled);
    assert_eq!(outcome.tools_disabled_reason, None);
    assert_eq!((outcome.rounds, outcome.tool_calls), (2, 1));
    assert_eq!(started.server.finish(), 2);
}

#[test]
fn concurrent_turns_are_limited_per_session_and_in_total_and_a_finished_session_can_chat_again() {
    let (release_one, gate_one) = gate();
    let (release_two, gate_two) = gate();
    let (announce, arrived) = std::sync::mpsc::channel();
    let started = started(
        text_only(),
        vec![
            Step::gated(text("one"), gate_one).announcing(announce),
            Step::gated(text("two"), gate_two),
            Step::reply(text("three")),
        ],
    );
    let service = &started.service;
    let endpoint = started.server.endpoint.clone();
    let first = new_session(service, &started.workspace, "First");
    let second = new_session(service, &started.workspace, "Second");
    let third = new_session(service, &started.workspace, "Third");

    send(service, first, "a", &endpoint).expect("first turn");
    wait_arrived(&arrived);
    send(service, second, "b", &endpoint).expect("second turn");

    // The same session again, and a third session beyond the limit of two.
    let again = send(service, first, "again", &endpoint).expect_err("one turn per session");
    assert_eq!(again.code(), "turn_running");
    let over = send(service, third, "c", &endpoint).expect_err("two turns at most");
    assert_eq!(over.code(), "too_many_turns");
    assert_eq!(status(service, third).state, ChatState::Idle);

    // While running: the tree says so, and journal access is refused.
    let tree = service
        .session_tree(wire(
            json!({"view": "active", "sort": "title", "descending": false}),
        ))
        .expect("tree");
    let state_of = |id: Uuid| {
        tree.groups
            .iter()
            .flat_map(|group| &group.sessions)
            .find(|session| session.session_id == id)
            .map(|session| session.status)
            .expect("listed")
    };
    assert_eq!(state_of(first), SessionStatus::Running);
    assert_eq!(state_of(second), SessionStatus::Running);
    assert_eq!(state_of(third), SessionStatus::Idle);
    let busy = service
        .session_transcript(wire(
            json!({"session_id": first, "after_seq": null, "limit": 5}),
        ))
        .expect_err("no inspection while a turn runs here");
    assert_eq!(busy.code(), "busy");
    for refused in [
        service.session_archive(wire(session_request(first))),
        service.session_restore(wire(session_request(first))),
        service.session_interrupt(wire(session_request(first))),
    ] {
        assert_eq!(refused.expect_err("running").code(), "turn_running");
    }

    release_one.send(()).expect("release the first model reply");
    release_two
        .send(())
        .expect("release the second model reply");
    assert_eq!(wait_finished(service, first).state, ChatState::Completed);
    assert_eq!(wait_finished(service, second).state, ChatState::Completed);

    // With the slots free, a third session can chat; a finished session can
    // chat again and its old result is replaced.
    send(service, third, "c", &endpoint).expect("third turn after the others finished");
    assert_eq!(wait_finished(service, third).state, ChatState::Completed);
    assert_eq!(started.server.finish(), 3);
}

#[test]
fn a_refused_send_starts_no_thread_and_records_nothing() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let endpoint = unused_endpoint();
    let service = fixture.service(&endpoint);
    let session = new_session(&service, &workspace, "Refusals");

    // A different endpoint than the one fixed at launch.
    let other = send(&service, session, "hi", "http://127.0.0.1:9/v1").expect_err("endpoint");
    assert_eq!(other.code(), "invalid_request");
    assert_eq!(other.message(), "The endpoint is fixed by the application.");

    // Nothing to send, or too much.
    let blank = send(&service, session, " \n\t", &endpoint).expect_err("blank");
    assert_eq!(blank.code(), "invalid_request");
    let huge = "x".repeat(65 * 1024);
    let large = send(&service, session, &huge, &endpoint).expect_err("oversized");
    assert_eq!(large.code(), "invalid_request");

    // Bad model settings are a synchronous provider error.
    let mut settings = settings_json(&endpoint);
    settings["model"] = json!("");
    let bad_model = service
        .chat_send(wire(
            json!({"session_id": session, "message": "hi", "settings": settings, "skills": []}),
        ))
        .expect_err("an empty model");
    assert_eq!(bad_model.code(), "provider");
    let mut settings = settings_json(&endpoint);
    settings["context_tokens"] = json!(0);
    let bad_context = service
        .chat_send(wire(
            json!({"session_id": session, "message": "hi", "settings": settings, "skills": []}),
        ))
        .expect_err("a zero context");
    assert_eq!(bad_context.code(), "provider");

    // An unknown session.
    let unknown = send(&service, Uuid::new_v4(), "hi", &endpoint).expect_err("unknown");
    assert_eq!(unknown.code(), "not_found");

    // None of those started a turn or touched the journal.
    assert_eq!(status(&service, session).state, ChatState::Idle);
    let report = service.shutdown(Duration::from_secs(5));
    assert_eq!((report.finished, report.still_running), (0, 0));
    assert!(texts(&service, session).is_empty());
}

#[test]
fn an_archived_session_and_an_open_turn_refuse_a_send() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let endpoint = unused_endpoint();
    let service = fixture.service(&endpoint);

    let archived = new_session(&service, &workspace, "Archived");
    service
        .session_archive(wire(session_request(archived)))
        .expect("archive");
    let error = send(&service, archived, "hi", &endpoint).expect_err("archived");
    assert_eq!(error.code(), "archived");

    let open = new_session(&service, &workspace, "Open turn");
    // Leave a turn open in the journal, as a crashed process would.
    let path = omgcode_core::session::locate(&fixture.data, open)
        .expect("locate")
        .expect("journal");
    {
        let mut journal = omgcode_core::session::SessionJournal::open(&path).expect("open");
        journal
            .record_message(omgcode_core::session::MessageRole::User, "q")
            .expect("message");
        journal.start_turn().expect("start turn");
    }
    let error = send(&service, open, "hi", &endpoint).expect_err("open turn");
    assert_eq!(error.code(), "turn_open");
}

#[test]
fn a_provider_transport_failure_ends_failed_with_code_provider_and_no_token() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let endpoint = unused_endpoint();
    let service = fixture.service_with(&endpoint, fixed_token);
    let session = new_session(&service, &workspace, "Unreachable");

    send(&service, session, &format!("hello {SECRET}"), &endpoint).expect("send");
    let done = wait_finished(&service, session);
    assert_eq!(done.state, ChatState::Failed);
    assert!(done.outcome.is_none());
    let error = done.error.as_ref().expect("an error");
    assert_eq!(error.code(), "provider");
    let serialized = text_of(&done);
    assert!(!serialized.contains(SECRET), "{serialized}");
    assert!(!format!("{done:?}").contains(SECRET));

    // The journal recorded the failed turn and the redacted message only.
    let page = service
        .session_transcript(wire(
            json!({"session_id": session, "after_seq": null, "limit": 50}),
        ))
        .expect("transcript");
    let all = text_of(&page);
    assert!(!all.contains(SECRET), "the token was recorded: {all}");
    assert!(all.contains("[REDACTED]"));
    assert!(all.contains("turn_failed"));
}

#[test]
fn the_server_token_never_reaches_a_response_even_when_the_model_echoes_it() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let server = MockServer::start(vec![Step::reply(text(&format!("echo {SECRET}")))]);
    let service = fixture.service_with(&server.endpoint, fixed_token);
    let session = new_session(&service, &workspace, "Echo");
    send(
        &service,
        session,
        &format!("say {SECRET}"),
        &server.endpoint,
    )
    .expect("send");
    let done = wait_finished(&service, session);
    assert_eq!(done.state, ChatState::Completed);

    let mut responses = vec![text_of(&done)];
    responses.push(text_of(
        &service
            .session_transcript(wire(
                json!({"session_id": session, "after_seq": null, "limit": 50}),
            ))
            .expect("transcript"),
    ));
    responses.push(text_of(
        &service
            .session_tree(wire(
                json!({"view": "active", "sort": "updated", "descending": true}),
            ))
            .expect("tree"),
    ));
    responses.push(text_of(
        &service
            .session_search(wire(
                json!({"query": "echo", "view": "active", "limit": 10}),
            ))
            .expect("search"),
    ));
    responses.push(text_of(&service.workspace_list().expect("workspaces")));
    responses.push(text_of(&service.approvals_list().expect("approvals")));
    responses.push(text_of(&service.chat_defaults()));
    common::assert_no_leaks(&fixture, &responses);
    assert!(responses[1].contains("[REDACTED]"));
    assert_eq!(server.finish(), 1);
}

/// A cancel was taken by a running turn. The request in flight stops within
/// about 25 ms (ADR-0051), so the turn may already have ended as failed by the
/// time its status is read; the caller checks the terminal error separately.
fn assert_cancel_accepted(state: ChatState) {
    assert!(
        matches!(state, ChatState::Cancelling | ChatState::Failed),
        "a cancel left the turn {state:?}"
    );
}

#[test]
fn cancelling_a_tool_turn_reports_cancelling_then_fails_with_the_core_error() {
    let (release, hold) = gate();
    let (announce, arrived) = std::sync::mpsc::channel();
    let started = started(
        read_tools(),
        vec![
            Step::gated(
                tool_call("call-1", "workspace_list_directory", &json!({"path": "."})),
                hold,
            )
            .announcing(announce),
        ],
    );
    let service = &started.service;
    let session = new_session(service, &started.workspace, "Cancel me");
    send(service, session, "list", &started.server.endpoint).expect("send");
    wait_arrived(&arrived);

    let cancelling = service
        .chat_cancel(wire(session_request(session)))
        .expect("cancel");
    assert_cancel_accepted(cancelling.state);
    assert_cancel_accepted(status(service, session).state);
    // The turn may already have ended, so nobody may be receiving.
    let _ignored = release.send(());

    let done = wait_finished(service, session);
    assert_eq!(done.state, ChatState::Failed);
    let error = done.error.expect("an error");
    assert_eq!(error.code(), "cancelled");
    assert_eq!(error.message(), "turn was cancelled");
    let page = service
        .session_transcript(wire(
            json!({"session_id": session, "after_seq": null, "limit": 50}),
        ))
        .expect("transcript");
    let failure = page
        .entries
        .iter()
        .map(json_of)
        .find(|entry| entry["kind"] == "turn_failed")
        .expect("a failed turn is recorded");
    assert_eq!(failure["code"], "cancelled");
    assert_eq!(started.server.finish(), 1);
}

#[test]
fn cancelling_a_non_streamed_text_turn_stops_its_request() {
    // Given a text-only turn whose request is not streamed and whose reply the
    // server holds until the test ends.
    let (release, hold) = gate();
    let (announce, arrived) = std::sync::mpsc::channel();
    let started = started(
        text_only(),
        vec![Step::gated(text("late answer"), hold).announcing(announce)],
    );
    let service = &started.service;
    let session = new_session(service, &started.workspace, "Cancellable");
    send(service, session, "hi", &started.server.endpoint).expect("send");
    wait_arrived(&arrived);

    // When the turn is cancelled while the request waits.
    let begun = Instant::now();
    let after = service
        .chat_cancel(wire(session_request(session)))
        .expect("cancel");

    // Then the turn has a token to act on, its request stops without the
    // reply, and the cancellation is recorded (ADR-0051).
    assert_cancel_accepted(after.state);
    let done = wait_finished(service, session);
    assert!(
        begun.elapsed() < Duration::from_secs(5),
        "cancellation took {:?}",
        begun.elapsed()
    );
    assert_eq!(done.state, ChatState::Failed);
    let error = done.error.expect("an error");
    assert_eq!(error.code(), "cancelled");
    let page = service
        .session_transcript(wire(
            json!({"session_id": session, "after_seq": null, "limit": 50}),
        ))
        .expect("transcript");
    let entries: Vec<_> = page.entries.iter().map(json_of).collect();
    let failure = entries
        .iter()
        .find(|entry| entry["kind"] == "turn_failed")
        .expect("a failed turn is recorded");
    assert_eq!(failure["code"], "cancelled");
    assert!(!format!("{entries:?}").contains("late answer"));

    // Cancelling something that is not running or unknown is idle or unchanged.
    let unknown = service
        .chat_cancel(wire(session_request(Uuid::new_v4())))
        .expect("cancel unknown");
    assert_eq!(unknown.state, ChatState::Idle);
    assert_eq!(
        service
            .chat_cancel(wire(session_request(session)))
            .expect("cancel finished")
            .state,
        ChatState::Failed
    );
    drop(release);
    assert_eq!(started.server.finish(), 1);
}

#[test]
fn shutdown_does_not_wait_past_its_grace_and_later_reports_the_turn_finished() {
    // Every model request can now be cancelled (ADR-0051), so a turn that
    // outlasts the grace period must be held in a step that cannot be: here
    // the turn thread is held inside the event sink when its request starts,
    // standing in for a tool call in progress.
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    // The cancelled turn never sends its request.
    let server = common::NoCallServer::start();
    let service = &fixture.service_streaming(&server.endpoint, common::no_token, true);
    let (release, hold) = gate();
    let (held, holding) = std::sync::mpsc::channel();
    let hold = std::sync::Mutex::new(Some((hold, held)));
    service
        .chat_subscribe(Box::new(move |event| {
            if matches!(event, ChatEvent::RequestStarted { .. })
                && let Some((hold, held)) = hold.lock().ok().and_then(|mut slot| slot.take())
            {
                // The test may have failed and stopped listening.
                held.send(()).ok();
                // A release and a dropped sender both let the turn go on.
                hold.recv_timeout(Duration::from_secs(20)).ok();
            }
            true
        }))
        .expect("subscribe");
    let session = new_session(service, &workspace, "Slow");
    send(service, session, "hi", &server.endpoint).expect("send");
    holding
        .recv_timeout(Duration::from_secs(10))
        .expect("the turn reaches its request");

    let grace = Duration::from_millis(150);
    let begun = Instant::now();
    let report = service.shutdown(grace);
    let elapsed = begun.elapsed();
    assert_eq!((report.finished, report.still_running), (0, 1));
    assert!(elapsed >= grace, "waited the grace period: {elapsed:?}");
    assert!(
        elapsed < grace + Duration::from_secs(2),
        "did not block past the grace period: {elapsed:?}"
    );
    // The turn holds a token, so it is reported as cancelling.
    assert_eq!(status(service, session).state, ChatState::Cancelling);
    let after =
        send(service, session, "more", &server.endpoint).expect_err("no new turn after shutdown");
    assert_eq!(after.code(), "shutting_down");

    // Once released, the thread sees the cancellation before it sends the
    // request, ends, and shutdown reports it finished.
    release.send(()).expect("release the turn");
    let report = service.shutdown(Duration::from_secs(20));
    assert_eq!((report.finished, report.still_running), (1, 0));
    let done = status(service, session);
    assert_eq!(done.state, ChatState::Failed);
    assert_eq!(done.error.expect("an error").code(), "cancelled");
    server.assert_untouched();
}

#[test]
fn shutdown_with_no_turns_returns_at_once() {
    let fixture = Fixture::bare();
    let service = fixture.service(&unused_endpoint());
    let begun = Instant::now();
    let report = service.shutdown(Duration::from_secs(30));
    assert_eq!((report.finished, report.still_running), (0, 0));
    assert!(begun.elapsed() < Duration::from_secs(2));
}

#[test]
fn chat_defaults_are_the_cli_budgets_with_desktop_compaction_and_the_fixed_endpoint() {
    let fixture = Fixture::bare();
    let endpoint = "http://127.0.0.1:1234/v1";
    let reply = fixture.service(endpoint).chat_defaults();
    assert_eq!(reply.preferences, PreferencesSource::Defaults);
    assert!(reply.enter_to_send, "Enter sends by default");
    let defaults = reply.settings;
    assert_eq!(defaults.endpoint, endpoint);
    assert_eq!(defaults.model, "");
    assert_eq!(defaults.context_tokens, 0);
    assert_eq!(defaults.max_output_tokens, 1024);
    assert_eq!(defaults.instructions, "");
    // The desktop compacts by default at 400K; the CLI keeps 200K and an
    // opt-in --auto-compact (ADR-0091).
    assert_eq!(defaults.compact_threshold, 400_000);
    assert!(defaults.auto_compact, "auto-compaction is on by default");
    // The slider takes its range and step from this reply, so the default must
    // be one of its stops to be shown and saved unchanged.
    assert!(
        (reply.compact_threshold_min..=reply.compact_threshold_max)
            .contains(&defaults.compact_threshold)
    );
    assert!(
        (defaults.compact_threshold - reply.compact_threshold_min)
            .is_multiple_of(reply.compact_threshold_step),
        "the default sits on the reported step"
    );
}
