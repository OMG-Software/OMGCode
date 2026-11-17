//! Pushed chat events of the desktop service (ADR-0049) end to end: a real
//! journal and index, a scripted loopback model server that answers with
//! server-sent events, and a test sink that records what the service pushes.
//! Every wait is bounded and fails the test at its deadline.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use omgcode_app::AppError;
use omgcode_app::desktop::{
    ChatEvent, ChatEventSink, ChatState, ChatStatus, DesktopService, SessionStatus, ToolsMode,
};
use omgcode_core::policy::PermissionSet;
use serde_json::{Value, json};
use uuid::Uuid;

mod common;
use common::{
    Fixture, MockServer, POLL_DEADLINE, SECRET, Step, Workspace, fixed_token, gate, no_token,
    read_tools, session_request, settings_json, sse_text, sse_text_unfinished, sse_tool_call, text,
    text_only, unused_endpoint, wire,
};

type Log = Arc<Mutex<Vec<ChatEvent>>>;

/// A sink that appends every event to a log and always stays subscribed.
fn recording_sink() -> (Log, ChatEventSink) {
    let log: Log = Arc::default();
    let writer = Arc::clone(&log);
    let sink: ChatEventSink = Box::new(move |event| {
        writer.lock().expect("event log").push(event.clone());
        true
    });
    (log, sink)
}

fn subscribe(service: &DesktopService) -> Log {
    let (log, sink) = recording_sink();
    service.chat_subscribe(sink).expect("subscribe");
    log
}

fn snapshot(log: &Log) -> Vec<ChatEvent> {
    log.lock().expect("event log").clone()
}

/// Poll the log until `done` holds, or fail at the deadline.
fn wait_events(log: &Log, done: impl Fn(&[ChatEvent]) -> bool) -> Vec<ChatEvent> {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let seen = snapshot(log);
        if done(&seen) {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for events; saw {} events",
            seen.len()
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn has_finished(events: &[ChatEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, ChatEvent::Finished { .. }))
}

fn delta_text(events: &[ChatEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            ChatEvent::Delta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn truncations(events: &[ChatEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, ChatEvent::PreviewTruncated { .. }))
        .count()
}

fn request_numbers(events: &[ChatEvent]) -> Vec<u32> {
    events
        .iter()
        .filter_map(|event| match event {
            ChatEvent::RequestStarted { request, .. } => Some(*request),
            _ => None,
        })
        .collect()
}

struct Started {
    /// Keeps the private data root alive for the test.
    _fixture: Fixture,
    workspace: Workspace,
    server: MockServer,
    service: DesktopService,
}

fn started(
    permissions: PermissionSet,
    steps: Vec<Step>,
    source: fn() -> Result<Option<String>, AppError>,
    stream: bool,
) -> Started {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", permissions);
    let server = MockServer::start(steps);
    let service = fixture.service_streaming(&server.endpoint, source, stream);
    Started {
        _fixture: fixture,
        workspace,
        server,
        service,
    }
}

fn new_session(started: &Started) -> Uuid {
    started
        .service
        .session_create(wire(
            json!({"workspace_id": started.workspace.id, "title": "Events"}),
        ))
        .expect("create session")
        .session_id
}

fn send(started: &Started, session: Uuid, message: &str) {
    started
        .service
        .chat_send(wire(json!({
            "session_id": session,
            "message": message,
            "settings": settings_json(&started.server.endpoint),
        })))
        .expect("send");
}

fn status(service: &DesktopService, session: Uuid) -> ChatStatus {
    service
        .chat_status(wire(session_request(session)))
        .expect("status")
}

#[test]
fn a_streamed_turn_pushes_started_deltas_and_finished_and_status_is_settled() {
    // Given a subscriber and a model that streams three pieces.
    let started = started(
        text_only(),
        vec![Step::stream(sse_text(&["Hel", "lo ", "there"]))],
        no_token,
        true,
    );
    let session = new_session(&started);
    let log = subscribe(&started.service);

    // When a turn is sent and its `finished` event is observed.
    send(&started, session, "hi");
    let seen = wait_events(&log, has_finished);

    // Then the events are one request, the text, and the end, in that order.
    assert_eq!(
        seen.first(),
        Some(&ChatEvent::RequestStarted {
            session_id: session,
            request: 1
        })
    );
    assert_eq!(
        seen.last(),
        Some(&ChatEvent::Finished {
            session_id: session
        })
    );
    let middle = &seen[1..seen.len() - 1];
    assert!(!middle.is_empty());
    assert!(
        middle
            .iter()
            .all(|event| matches!(event, ChatEvent::Delta { request: 1, session_id, .. } if *session_id == session))
    );
    assert_eq!(delta_text(&seen), "Hello there");

    // And the status read right after `finished` is already settled.
    assert_eq!(
        status(&started.service, session).state,
        ChatState::Completed
    );
    assert_eq!(started.server.finish(), 1);
}

#[test]
fn a_streamed_tool_turn_announces_each_model_request() {
    // Given a model that calls a tool and then answers, both streamed.
    let started = started(
        read_tools(),
        vec![
            Step::stream(sse_tool_call(
                "call-1",
                "workspace_list_directory",
                &json!({"path": ""}),
            )),
            Step::stream(sse_text(&["listed"])),
        ],
        no_token,
        true,
    );
    let session = new_session(&started);
    let log = subscribe(&started.service);

    // When the turn runs.
    send(&started, session, "list");
    let seen = wait_events(&log, has_finished);

    // Then there is a `request_started` for each of the two requests, and the
    // text of the second follows its announcement.
    assert_eq!(request_numbers(&seen), [1, 2]);
    assert_eq!(delta_text(&seen), "listed");
    let second = seen
        .iter()
        .position(|event| matches!(event, ChatEvent::RequestStarted { request: 2, .. }))
        .expect("second request announced");
    assert!(matches!(
        seen.get(second + 1),
        Some(ChatEvent::Delta { request: 2, .. })
    ));
    let done = status(&started.service, session);
    assert_eq!(done.state, ChatState::Completed);
    assert_eq!(
        done.outcome.expect("outcome").tools_mode,
        ToolsMode::Enabled
    );
    assert_eq!(started.server.finish(), 2);
}

#[test]
fn a_token_split_across_pieces_never_reaches_a_subscriber() {
    // Given a server token and a model that echoes it across two pieces.
    let half = SECRET.len() / 2;
    let (head, tail) = SECRET.split_at(half);
    let first = format!("the key is {head}");
    let second = format!("{tail} and more");
    let started = started(
        text_only(),
        vec![Step::stream(sse_text(&[&first, &second]))],
        fixed_token,
        true,
    );
    let session = new_session(&started);
    let log = subscribe(&started.service);

    // When the turn runs.
    send(&started, session, "say it");
    let seen = wait_events(&log, has_finished);

    // Then no event holds the token or either half joined to the other, and
    // text before the token was delivered.
    let wire_text = serde_json::to_string(
        &seen
            .iter()
            .map(|event| serde_json::to_value(event).expect("event serializes"))
            .collect::<Vec<Value>>(),
    )
    .expect("serialize events");
    assert!(!wire_text.contains(SECRET), "{wire_text}");
    assert!(!delta_text(&seen).contains(SECRET));
    assert!(delta_text(&seen).starts_with("the key is "));
    assert_eq!(started.server.finish(), 1);
}

#[test]
fn a_reply_over_the_preview_bound_is_cut_once_and_the_turn_ends_by_the_message_rule() {
    // Given a reply of 20,000 bytes, streamed in 1,000-byte pieces.
    let pieces: Vec<String> = (0..20).map(|_| "a".repeat(1000)).collect();
    let borrowed: Vec<&str> = pieces.iter().map(String::as_str).collect();
    let started = started(
        text_only(),
        vec![Step::stream(sse_text(&borrowed))],
        no_token,
        true,
    );
    let session = new_session(&started);
    let log = subscribe(&started.service);

    // When the turn runs.
    send(&started, session, "long");
    let seen = wait_events(&log, has_finished);

    // Then 16,384 bytes were pushed, the cut was reported once, and nothing
    // for the request came after it.
    assert_eq!(delta_text(&seen).len(), 16 * 1024);
    assert_eq!(truncations(&seen), 1);
    let cut = seen
        .iter()
        .position(|event| matches!(event, ChatEvent::PreviewTruncated { .. }))
        .expect("truncation reported");
    assert!(
        seen[cut + 1..]
            .iter()
            .all(|event| matches!(event, ChatEvent::Finished { .. })),
        "events after the cut: {:?}",
        &seen[cut + 1..]
    );

    // And the turn ends as the 16 KiB session message rule says.
    let done = status(&started.service, session);
    assert_eq!(done.state, ChatState::Failed);
    let error = done.error.expect("an error");
    assert!(error.message().contains("16 KiB"), "{}", error.message());
    assert_eq!(started.server.finish(), 1);
}

#[test]
fn every_delta_is_at_most_a_kilobyte_of_valid_text_for_multibyte_replies() {
    // Given a reply of three- and four-byte characters, in one piece.
    let reply = "\u{20ac}\u{1F600}x".repeat(1500);
    let started = started(
        text_only(),
        vec![Step::stream(sse_text(&[&reply]))],
        no_token,
        true,
    );
    let session = new_session(&started);
    let log = subscribe(&started.service);

    // When the turn runs.
    send(&started, session, "wide");
    let seen = wait_events(&log, has_finished);

    // Then each delta is within the bound and the pieces rebuild the reply.
    let mut count = 0;
    for event in &seen {
        if let ChatEvent::Delta { text, .. } = event {
            assert!(text.len() <= 1024, "{} bytes", text.len());
            count += 1;
        }
    }
    assert!(count > 1, "the reply was not split");
    assert_eq!(delta_text(&seen), reply);
    assert_eq!(
        status(&started.service, session).state,
        ChatState::Completed
    );
    assert_eq!(started.server.finish(), 1);
}

#[test]
fn subscribing_mid_stream_replays_the_text_so_far_and_the_rest_follows() {
    // Given a model that holds its reply open after the first piece.
    let (release, held) = gate();
    let started = started(
        text_only(),
        vec![Step::stream_held(
            sse_text_unfinished(&["Hello "]),
            held,
            sse_text(&["world"]),
        )],
        no_token,
        true,
    );
    let session = new_session(&started);
    let first = subscribe(&started.service);
    send(&started, session, "go");
    wait_events(&first, |events| delta_text(events) == "Hello ");
    let first_count = snapshot(&first).len();

    // When a second subscriber arrives while the reply is still open.
    let second = subscribe(&started.service);

    // Then it is brought up to date at once.
    assert_eq!(
        snapshot(&second),
        [
            ChatEvent::RequestStarted {
                session_id: session,
                request: 1
            },
            ChatEvent::Delta {
                session_id: session,
                request: 1,
                text: "Hello ".to_owned()
            },
        ]
    );

    // And when the model finishes, the rest continues the same request on the
    // new subscriber only.
    release.send(()).expect("release the reply");
    let seen = wait_events(&second, has_finished);
    assert_eq!(delta_text(&seen), "Hello world");
    assert_eq!(request_numbers(&seen), [1]);
    assert_eq!(snapshot(&first).len(), first_count);
    assert_eq!(
        status(&started.service, session).state,
        ChatState::Completed
    );
    assert_eq!(started.server.finish(), 1);
}

#[test]
fn a_second_subscriber_replaces_the_first() {
    // Given two subscriptions made one after the other.
    let started = started(
        text_only(),
        vec![Step::stream(sse_text(&["one"]))],
        no_token,
        true,
    );
    let session = new_session(&started);
    let first = subscribe(&started.service);
    let second = subscribe(&started.service);

    // When a turn runs.
    send(&started, session, "hi");
    let seen = wait_events(&second, has_finished);

    // Then the second sees it and the first never saw anything.
    assert_eq!(delta_text(&seen), "one");
    assert!(snapshot(&first).is_empty());
    assert_eq!(started.server.finish(), 1);
}

#[test]
fn a_sink_that_reports_it_is_gone_gets_nothing_more_and_the_turn_completes() {
    // Given a sink that refuses its first event.
    let started = started(
        text_only(),
        vec![Step::stream(sse_text(&["a", "b", "c"]))],
        no_token,
        true,
    );
    let session = new_session(&started);
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    started
        .service
        .chat_subscribe(Box::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            false
        }))
        .expect("subscribe");

    // When the turn runs to its end.
    send(&started, session, "hi");
    let done = common::wait_finished(&started.service, session);

    // Then the turn completed and the sink was called exactly once.
    assert_eq!(done.state, ChatState::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(started.server.finish(), 1);
}

#[test]
fn a_failing_stream_pushes_its_text_and_then_finished_and_the_turn_fails() {
    // Given a stream that ends without its `[DONE]` marker.
    let started = started(
        text_only(),
        vec![Step::stream(sse_text_unfinished(&["partial"]))],
        no_token,
        true,
    );
    let session = new_session(&started);
    let log = subscribe(&started.service);

    // When the turn runs.
    send(&started, session, "hi");
    let seen = wait_events(&log, has_finished);

    // Then the provisional text was pushed, `finished` came last, and the
    // status is failed.
    assert_eq!(delta_text(&seen), "partial");
    assert_eq!(
        seen.last(),
        Some(&ChatEvent::Finished {
            session_id: session
        })
    );
    assert_eq!(status(&started.service, session).state, ChatState::Failed);
    assert_eq!(started.server.finish(), 1);
}

#[test]
fn with_streaming_off_the_request_is_not_streamed_and_only_finished_is_pushed() {
    // Given a service with streaming off and a model that answers in JSON.
    let started = started(
        text_only(),
        vec![Step::reply(text("pong"))],
        no_token,
        false,
    );
    let session = new_session(&started);
    let log = subscribe(&started.service);

    // When the turn runs.
    send(&started, session, "ping");
    let seen = wait_events(&log, has_finished);

    // Then the only event is `finished`, and the request said `stream` false.
    assert_eq!(
        seen,
        [ChatEvent::Finished {
            session_id: session
        }]
    );
    assert_eq!(
        status(&started.service, session).state,
        ChatState::Completed
    );
    let (served, requests) = started.server.finish_recorded();
    assert_eq!(served, 1);
    let body: Value = serde_json::from_str(&requests[0].body).expect("request body");
    assert_eq!(body["stream"], json!(false));
}

#[test]
fn a_send_refused_synchronously_pushes_no_event() {
    // Given a subscriber and a service that will never be contacted.
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let endpoint = unused_endpoint();
    let service = fixture.service_streaming(&endpoint, no_token, true);
    let session = service
        .session_create(wire(
            json!({"workspace_id": workspace.id, "title": "Refused"}),
        ))
        .expect("create session")
        .session_id;
    let log = subscribe(&service);

    // When an empty message is sent.
    let refused = service
        .chat_send(wire(json!({
            "session_id": session,
            "message": "   ",
            "settings": settings_json(&endpoint),
        })))
        .expect_err("an empty message is refused");

    // Then it failed before any turn started, and nothing was pushed.
    assert_eq!(refused.code(), "invalid_request");
    assert!(snapshot(&log).is_empty());
    assert_eq!(status(&service, session).state, ChatState::Idle);
    let tree = service
        .session_tree(wire(
            json!({"view": "active", "sort": "title", "descending": false}),
        ))
        .expect("tree");
    assert!(
        tree.groups
            .iter()
            .flat_map(|group| group.sessions.iter())
            .all(|summary| summary.status != SessionStatus::Running)
    );
}

// ---- Cancelling and shutting down a streamed turn (ADR-0050) ----

/// The slowest acceptable cancellation. The request deadline is 30 seconds.
const CANCEL_BOUND: Duration = Duration::from_secs(5);

/// The status of one session in the active session tree.
fn tree_status(service: &DesktopService, session: Uuid) -> SessionStatus {
    service
        .session_tree(wire(
            json!({"view": "active", "sort": "title", "descending": false}),
        ))
        .expect("tree")
        .groups
        .iter()
        .flat_map(|group| group.sessions.iter())
        .find(|summary| summary.session_id == session)
        .map(|summary| summary.status)
        .expect("the session is listed")
}

#[test]
fn chat_cancel_stops_a_streamed_text_turn_held_mid_stream() {
    // Given a streamed text-only turn whose server sent one piece and holds
    // the rest of the body, and a second scripted reply for a later turn.
    let (release, hold) = gate();
    let started = started(
        text_only(),
        vec![
            Step::stream_held(sse_text_unfinished(&["first "]), hold, sse_text(&["rest"])),
            Step::stream(sse_text(&["again"])),
        ],
        no_token,
        true,
    );
    let session = new_session(&started);
    let log = subscribe(&started.service);
    send(&started, session, "hi");
    wait_events(&log, |events| delta_text(events) == "first ");

    // When the turn is cancelled.
    let begun = Instant::now();
    let after = started
        .service
        .chat_cancel(wire(session_request(session)))
        .expect("cancel");

    // Then the status goes `cancelling`, then `failed` with the code
    // `cancelled`, and a `finished` event is pushed well inside the deadline.
    assert_eq!(after.state, ChatState::Cancelling);
    let seen = wait_events(&log, has_finished);
    let elapsed = begun.elapsed();
    assert!(elapsed < CANCEL_BOUND, "cancellation took {elapsed:?}");
    assert_eq!(delta_text(&seen), "first ");
    let done = status(&started.service, session);
    assert_eq!(done.state, ChatState::Failed);
    assert_eq!(
        done.error.as_ref().map(|error| error.code()),
        Some("cancelled")
    );
    assert_eq!(
        done.error.as_ref().map(|error| error.message()),
        Some("turn was cancelled")
    );
    assert!(done.outcome.is_none());

    // And the journal has no open turn, so the next send is accepted. The
    // server takes its next request once the first reply is released.
    assert_eq!(tree_status(&started.service, session), SessionStatus::Idle);
    drop(release);
    send(&started, session, "once more");
    wait_events(&log, |events| {
        events
            .iter()
            .filter(|event| matches!(event, ChatEvent::Finished { .. }))
            .count()
            == 2
    });
    assert_eq!(
        status(&started.service, session).state,
        ChatState::Completed
    );
    assert_eq!(started.server.finish(), 2);
}

#[test]
fn shutdown_ends_a_streamed_turn_held_mid_stream_inside_its_grace() {
    // Given a streamed text-only turn held mid-stream.
    let (release, hold) = gate();
    let started = started(
        text_only(),
        vec![Step::stream_held(
            sse_text_unfinished(&["first "]),
            hold,
            sse_text(&["rest"]),
        )],
        no_token,
        true,
    );
    let session = new_session(&started);
    let log = subscribe(&started.service);
    send(&started, session, "hi");
    wait_events(&log, |events| delta_text(events) == "first ");

    // When the service shuts down with a long grace period.
    let begun = Instant::now();
    let report = started.service.shutdown(Duration::from_secs(20));
    let elapsed = begun.elapsed();

    // Then the turn finished well inside the grace period, as cancelled, and
    // left no open turn.
    assert_eq!((report.finished, report.still_running), (1, 0));
    assert!(elapsed < CANCEL_BOUND, "shutdown took {elapsed:?}");
    let done = status(&started.service, session);
    assert_eq!(done.state, ChatState::Failed);
    assert_eq!(
        done.error.as_ref().map(|error| error.code()),
        Some("cancelled")
    );
    assert_eq!(tree_status(&started.service, session), SessionStatus::Idle);
    drop(release);
    assert_eq!(started.server.finish(), 1);
}
