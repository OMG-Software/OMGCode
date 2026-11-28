//! A session's round, driven from a queue the test fills (ADR-0109).

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use omgcode_platform::process::{ProcessError, StreamingChild, UnconfirmedCleanup};
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

use super::super::ClaudeCodeFailure;
use super::super::mcp::{McpServer, McpTool};
use super::{
    EVENT_QUEUE, Event, Next, Progress, SERVER_NAME, Session, TOOL_PREFIX, TurnFolder,
    UNCONFIRMED_TURNS, WritePoint, WriteRace, raced_write, spawned,
};
use crate::control::{ClaudeCodeWatch, ControlError, wait_for_claude_code_turns};
use crate::provider::claude_code::stream::parse_line;
use crate::provider::{MAX_STREAM_BYTES, ProviderError};
use crate::turn::CancelToken;

const TOOL: &str = "workspace_list_directory";

fn line(value: &Value) -> Vec<u8> {
    value.to_string().into_bytes()
}

fn stdout(value: &Value) -> Event {
    Event::Stdout(parse_line(&line(value)))
}

fn mcp(value: &Value) -> Event {
    Event::Mcp(Ok(line(value)))
}

/// A session past its checked init report and with the whole prompt
/// written, whose relay answered `initialize`, and the sender of its queue.
fn session(runtime: &Path) -> (Session, SyncSender<Event>) {
    let (sender, receiver) = mpsc::sync_channel(EVENT_QUEUE);
    let turn = TurnFolder::create(runtime).expect("turn folder");
    let mut session = Session::new(receiver, turn, Duration::from_secs(30));
    let mut server = McpServer::new(
        SERVER_NAME,
        "0",
        vec![McpTool {
            name: TOOL.into(),
            description: "List a folder".into(),
            input_schema: json!({ "type": "object" }),
        }],
    );
    server
        .handle_line(&line(&json!({
            "jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": { "protocolVersion": "2025-11-25", "capabilities": {},
                        "clientInfo": { "name": "test", "version": "0" } },
        })))
        .expect("initialize");
    session.server = Some(server);
    session.expected_tools = vec![format!("{TOOL_PREFIX}{TOOL}")];
    session.init_seen = true;
    session.input_written = true;
    (session, sender)
}

/// The assistant message that announces `toolu_1`, then its `tools/call`
/// as request 7, so the round is complete once the call arrives.
fn queue_announced_call(sender: &SyncSender<Event>) {
    sender
        .send(stdout(&json!({
            "type": "assistant", "parent_tool_use_id": null,
            "message": { "id": "msg_1", "content": [
                { "type": "tool_use", "id": "toolu_1", "name": format!("{TOOL_PREFIX}{TOOL}"),
                  "input": { "path": "" } }
            ] },
        })))
        .expect("queue the message");
    sender
        .send(mcp(&json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": { "name": TOOL, "arguments": { "path": "" },
                        "_meta": { "claudecode/toolUseId": "toolu_1" } },
        })))
        .expect("queue the call");
}

/// A text delta of the main agent.
fn text_delta(text: &str) -> Event {
    stdout(&json!({
        "type": "stream_event", "parent_tool_use_id": null,
        "event": { "type": "content_block_delta", "index": 0,
                   "delta": { "type": "text_delta", "text": text } },
    }))
}

fn result(text: &str) -> Event {
    stdout(&json!({
        "type": "result", "subtype": "success", "is_error": false, "result": text,
        "terminal_reason": "completed", "stop_reason": "end_turn",
    }))
}

#[test]
fn a_call_cancelled_while_queued_never_reaches_the_runtime() {
    let runtime = TempDir::new().expect("runtime folder");
    let (mut session, sender) = session(runtime.path());
    // The call and its cancellation are both queued before the round reads
    // either, so the hand-off must wait until the cancellation was handled.
    queue_announced_call(&sender);
    sender
        .send(mcp(&json!({
            "jsonrpc": "2.0", "method": "notifications/cancelled",
            "params": { "requestId": 7, "reason": "user interrupt" },
        })))
        .expect("queue the cancellation");
    sender.send(result("done")).expect("queue the result");

    let progress = session
        .run_round(&CancelToken::new(), None)
        .expect("the round ends");

    match progress {
        Progress::Finished(text) => assert_eq!(text, "done"),
        Progress::Calls(calls) => panic!("a cancelled call reached the runtime: {calls:?}"),
    }
    assert!(session.unreturned.is_empty());
    assert!(session.held.is_empty());
    let server = session.server.as_ref().expect("server");
    assert_eq!(
        server.pending_calls(),
        0,
        "the cancelled request is retired"
    );
}

#[test]
fn a_call_with_no_queued_cancellation_is_handed_off() {
    let runtime = TempDir::new().expect("runtime folder");
    let (mut session, sender) = session(runtime.path());
    queue_announced_call(&sender);

    let progress = session
        .run_round(&CancelToken::new(), None)
        .expect("the round ends");

    let Progress::Calls(calls) = progress else {
        panic!("the announced call was not handed off");
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, TOOL);
    assert_eq!(calls[0].tool_use_id.as_deref(), Some("toolu_1"));
    assert!(session.unreturned.is_empty());
    drop(sender);
}

#[test]
fn a_cancellation_queued_while_the_runtime_runs_the_call_withholds_its_answer() {
    let runtime = TempDir::new().expect("runtime folder");
    let (mut session, sender) = session(runtime.path());
    queue_announced_call(&sender);
    let Progress::Calls(mut calls) = session
        .run_round(&CancelToken::new(), None)
        .expect("the round ends")
    else {
        panic!("the announced call was not handed off");
    };
    let call = calls.pop().expect("one call");
    // The relay cancels request 7 while the runtime runs it, and the
    // process's result follows. The session has no relay writer, so an
    // answer it tried to write would fail the turn.
    sender
        .send(mcp(&json!({
            "jsonrpc": "2.0", "method": "notifications/cancelled",
            "params": { "requestId": 7, "reason": "user interrupt" },
        })))
        .expect("queue the cancellation");
    sender.send(result("done")).expect("queue the result");

    session
        .answer(&call, "listing", &CancelToken::new())
        .expect("the cancelled call's answer is withheld");

    // The result waited for the next round, in order.
    session.hold(Vec::new());
    match session
        .run_round(&CancelToken::new(), None)
        .expect("the round ends")
    {
        Progress::Finished(text) => assert_eq!(text, "done"),
        Progress::Calls(calls) => panic!("unexpected calls: {calls:?}"),
    }
    let server = session.server.as_ref().expect("server");
    assert_eq!(
        server.pending_calls(),
        0,
        "the cancelled request is retired"
    );
}

#[test]
fn a_ready_call_is_handed_off_while_events_keep_arriving() {
    let runtime = TempDir::new().expect("runtime folder");
    let (mut session, sender) = session(runtime.path());
    let status = json!({ "type": "system", "subtype": "status" });
    queue_announced_call(&sender);
    for _ in 2..EVENT_QUEUE {
        sender.send(stdout(&status)).expect("fill the queue");
    }
    let cancel = CancelToken::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    // Each event taken is replaced at once, so the queue is never found
    // empty: the hand-off must come at a batch's end.
    for _ in 0..4 * EVENT_QUEUE {
        match session.next_event(&cancel, deadline).expect("next") {
            Next::Calls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].tool_use_id.as_deref(), Some("toolu_1"));
                return;
            }
            Next::Event(event) => {
                sender.send(stdout(&status)).expect("top up the queue");
                match event {
                    Event::Mcp(Ok(line)) => session
                        .mcp_line(&line, &cancel, deadline)
                        .expect("the call is accepted"),
                    Event::Stdout(Ok(event)) => {
                        let _ = session.stream_event(event, &mut None).expect("output");
                    }
                    _ => panic!("unexpected event"),
                }
            }
            Next::Idle | Next::Ended => panic!("the queue was found empty"),
        }
    }
    panic!("a steady stream of events held back the ready call");
}

/// A call that is ready when a batch's last slot goes to an event is not
/// handed off before a cancellation queued right behind that event: the
/// hand-off handles the relay's queued events first, and the call never
/// reaches the runtime.
#[test]
fn a_cancellation_right_behind_a_batchs_last_event_retires_its_call() {
    let runtime = TempDir::new().expect("runtime folder");
    let (mut session, sender) = session(runtime.path());
    let cancel = CancelToken::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    queue_announced_call(&sender);
    for _ in 0..2 {
        let Next::Event(event) = session.next_event(&cancel, deadline).expect("next") else {
            panic!("the message and the call come first");
        };
        match event {
            Event::Stdout(Ok(event)) => {
                let _ = session.stream_event(event, &mut None).expect("output");
            }
            Event::Mcp(Ok(line)) => session
                .mcp_line(&line, &cancel, deadline)
                .expect("the call is accepted"),
            _ => panic!("unexpected event"),
        }
    }
    assert!(session.calls_ready(), "every announced call arrived");
    // One slot is left in the batch: a status event takes it, and the
    // relay's cancellation waits right behind it.
    session.batch_left = 1;
    sender
        .send(stdout(&json!({ "type": "system", "subtype": "status" })))
        .expect("queue the status");
    sender
        .send(mcp(&json!({
            "jsonrpc": "2.0", "method": "notifications/cancelled",
            "params": { "requestId": 7, "reason": "user interrupt" },
        })))
        .expect("queue the cancellation");
    let Next::Event(Event::Stdout(Ok(status))) =
        session.next_event(&cancel, deadline).expect("next")
    else {
        panic!("the status event takes the last slot");
    };
    let _ = session.stream_event(status, &mut None).expect("output");
    assert_eq!(session.batch_left, 0, "the batch ended");

    match session.next_event(&cancel, deadline).expect("next") {
        Next::Calls(calls) => panic!("a cancelled call reached the runtime: {calls:?}"),
        Next::Idle => {}
        Next::Event(_) | Next::Ended => panic!("nothing else was queued"),
    }
    assert!(session.unreturned.is_empty());
    let server = session.server.as_ref().expect("server");
    assert_eq!(
        server.pending_calls(),
        0,
        "the cancelled request is retired"
    );
    drop(sender);
}

/// A call that is ready at a batch's end is not handed off while output
/// that the round refuses waits behind the batch: output that ends the turn,
/// a second init report, a tool or message the lockdown refuses, a failed
/// sign-in, a result while the call would be held, or text past the budget.
/// The round handles that output first and fails, and the call never
/// reaches the runtime. A status frame, input that reached the process, and
/// accepted text or messages waiting there do not hold the call back.
#[test]
fn queued_output_that_the_round_refuses_fails_it_before_a_ready_call_is_handed_off() {
    let status = json!({ "type": "system", "subtype": "status" });
    let init: Value = serde_json::from_str(
        r#"{"type":"system","subtype":"init","cwd":"/w","session_id":"s1","tools":["mcp__omgcode__workspace_list_directory"],"mcp_servers":[{"name":"omgcode","status":"connected","source":"dynamic"}],"model":"claude-haiku-5-5","permissionMode":"default","slash_commands":[],"apiKeySource":"none","claude_code_version":"2.1.325","agents":[],"skills":[],"plugins":[],"uuid":"u1"}"#,
    )
    .expect("init report");
    let tool_start = |name: &str| {
        stdout(&json!({
            "type": "stream_event", "parent_tool_use_id": null,
            "event": { "type": "content_block_start", "index": 1,
                       "content_block": { "type": "tool_use", "id": "toolu_2", "name": name, "input": {} } },
        }))
    };
    let message = |content: Value| {
        stdout(&json!({
            "type": "assistant", "parent_tool_use_id": null,
            "message": { "id": "msg_2", "content": content },
        }))
    };
    let offered = format!("{TOOL_PREFIX}{TOOL}");
    let cases = [
        (
            "malformed",
            Event::Stdout(parse_line(b"{not json")),
            Some(ClaudeCodeFailure::Protocol),
        ),
        ("ended", Event::StdoutEnded, Some(ClaudeCodeFailure::Failed)),
        (
            "input lost",
            Event::InputWritten(false),
            Some(ClaudeCodeFailure::Failed),
        ),
        (
            "second init",
            stdout(&init),
            Some(ClaudeCodeFailure::Protocol),
        ),
        (
            "unoffered tool start",
            tool_start("Bash"),
            Some(ClaudeCodeFailure::Lockdown),
        ),
        (
            "unoffered tool use",
            message(json!([{ "type": "tool_use", "id": "toolu_2", "name": "Bash", "input": {} }])),
            Some(ClaudeCodeFailure::Lockdown),
        ),
        (
            "failed sign-in",
            stdout(&json!({
                "type": "assistant", "parent_tool_use_id": null, "error": "authentication_failed",
                "message": { "content": [{ "type": "text", "text": "Not logged in" }] },
            })),
            Some(ClaudeCodeFailure::NotSignedIn),
        ),
        ("result", result("done"), Some(ClaudeCodeFailure::Protocol)),
        ("status", stdout(&status), None),
        ("input written", Event::InputWritten(true), None),
        ("text", text_delta("Hel"), None),
        ("offered tool start", tool_start(&offered), None),
        (
            "text message",
            message(json!([{ "type": "text", "text": "Looking" }])),
            None,
        ),
    ];
    for (label, output, failure) in cases {
        if !matches!(output, Event::Stdout(Ok(_))) {
            assert_eq!(output.ends_the_turn(), failure.is_some(), "{label}");
        }
        let runtime = TempDir::new().expect("runtime folder");
        let (mut session, sender) = session(runtime.path());
        let cancel = CancelToken::new();
        let deadline = Instant::now() + Duration::from_secs(30);
        queue_announced_call(&sender);
        for _ in 0..2 {
            let Next::Event(event) = session.next_event(&cancel, deadline).expect("next") else {
                panic!("the message and the call come first");
            };
            match event {
                Event::Stdout(Ok(event)) => {
                    let _ = session.stream_event(event, &mut None).expect("output");
                }
                Event::Mcp(Ok(line)) => session
                    .mcp_line(&line, &cancel, deadline)
                    .expect("the call is accepted"),
                _ => panic!("unexpected event"),
            }
        }
        assert!(session.calls_ready(), "every announced call arrived");
        // The batch ends with output queued behind it.
        session.batch_left = 0;
        sender.send(stdout(&status)).expect("queue the status");
        sender.send(output).expect("queue the output");

        let progress = session.run_round(&cancel, None);
        match (failure, progress) {
            (None, Ok(Progress::Calls(calls))) => {
                assert_eq!(calls.len(), 1, "{label}");
                assert_eq!(calls[0].tool_use_id.as_deref(), Some("toolu_1"));
            }
            (Some(expected), Err(ProviderError::ClaudeCode(kind))) => {
                assert_eq!(kind, expected, "{label}");
                assert_eq!(session.unreturned.len(), 1, "{label}: the call stays");
            }
            (_, other) => panic!("{label}: {other:?}"),
        }
        drop(sender);
    }
}

/// Text kept back is judged against the budgets as they would stand once
/// it was handled: a delta or a message's text that would pass the limit,
/// counting the text kept back before it, holds the calls back, and text
/// just within it does not.
#[test]
fn kept_back_text_is_judged_against_the_budgets_as_they_would_stand() {
    let runtime = TempDir::new().expect("runtime folder");
    let (mut session, _sender) = session(runtime.path());
    session.streamed_bytes = MAX_STREAM_BYTES - 2;
    session.deferred.push_back(text_delta("a"));
    assert!(
        !session.kept_back_output_fails(),
        "one byte below the limit"
    );
    session.deferred.push_back(text_delta("bc"));
    assert!(
        session.kept_back_output_fails(),
        "with the earlier delta, past it"
    );

    session.deferred.clear();
    session.streamed_bytes = 0;
    // With the blank line before it, one more byte reaches the limit.
    let current = usize::try_from(MAX_STREAM_BYTES - 3).expect("size");
    session.text = "x".repeat(current);
    let message = |text: &str| {
        stdout(&json!({
            "type": "assistant", "parent_tool_use_id": null,
            "message": { "id": "msg_2", "content": [{ "type": "text", "text": text }] },
        }))
    };
    session.deferred.push_back(message("a"));
    assert!(!session.kept_back_output_fails(), "at the limit");
    session.deferred.push_back(message("b"));
    assert!(
        session.kept_back_output_fails(),
        "with the earlier text, past it"
    );
}

#[test]
fn a_write_that_finished_first_is_not_reported_as_stopped() {
    let race = WriteRace::new();
    assert!(race.running());
    assert!(race.finish(), "the writer records first");
    assert!(!race.running());
    assert!(!race.stop(), "the watcher's later stop loses");
    assert!(!race.finish(), "the end is recorded once");
}

/// The writer is held after its I/O until the watcher stopped the write,
/// as when it is descheduled between its flush and the record, for at most
/// 30 seconds. Returns whether the stop came.
fn held_until(stops: &AtomicUsize) -> bool {
    let until = Instant::now() + Duration::from_secs(30);
    while stops.load(Ordering::SeqCst) == 0 {
        if Instant::now() >= until {
            return false;
        }
        thread::sleep(Duration::from_millis(1));
    }
    true
}

/// The name of the thread that watches a relay write.
const WATCHER: &str = "claude-code-write";

/// A write whose bytes went out but that is recorded finished only after
/// its deadline passed fails the turn and stops the tree once: a write
/// counts as finished only once the writer recorded it. The deadline passes
/// only once the I/O completed, so the watcher cannot stop the write before.
#[test]
fn a_write_recorded_after_its_deadline_fails_and_stops_the_tree() {
    let expired = AtomicBool::new(false);
    let stops = AtomicUsize::new(0);
    let mut stopped_after_io = false;
    let result = raced_write(
        || {
            expired.store(true, Ordering::SeqCst);
            stopped_after_io = held_until(&stops);
            Ok(())
        },
        &CancelToken::new(),
        || expired.load(Ordering::SeqCst),
        || {
            stops.fetch_add(1, Ordering::SeqCst);
        },
        |_| {},
    );
    assert!(
        stopped_after_io,
        "the watcher stopped the write after its I/O"
    );
    assert_eq!(stops.load(Ordering::SeqCst), 1, "stopped once");
    assert!(
        matches!(
            result,
            Err(ProviderError::ClaudeCode(ClaudeCodeFailure::Failed))
        ),
        "{result:?}"
    );
}

/// As above for a cancellation raised once the I/O completed, which the
/// turn reports as cancelled.
#[test]
fn a_write_recorded_after_a_cancellation_is_cancelled_and_stops_the_tree() {
    let stops = AtomicUsize::new(0);
    let cancel = CancelToken::new();
    let mut stopped_after_io = false;
    let result = raced_write(
        || {
            cancel.cancel();
            stopped_after_io = held_until(&stops);
            Ok(())
        },
        &cancel,
        || false,
        || {
            stops.fetch_add(1, Ordering::SeqCst);
        },
        |_| {},
    );
    assert!(
        stopped_after_io,
        "the watcher stopped the write after its I/O"
    );
    assert_eq!(stops.load(Ordering::SeqCst), 1, "stopped once");
    assert!(
        matches!(result, Err(ProviderError::Cancelled)),
        "{result:?}"
    );
}

/// A write whose deadline passed during its I/O fails and stops the tree
/// although the watcher never sees the expiry, as when it is not scheduled
/// in time: the writer checks the deadline itself before it records the
/// write finished.
#[test]
fn a_write_whose_deadline_the_watcher_missed_still_fails() {
    let expired = AtomicBool::new(false);
    let stops = AtomicUsize::new(0);
    let result = raced_write(
        || {
            expired.store(true, Ordering::SeqCst);
            Ok(())
        },
        &CancelToken::new(),
        || expired.load(Ordering::SeqCst) && thread::current().name() != Some(WATCHER),
        || {
            stops.fetch_add(1, Ordering::SeqCst);
        },
        |_| {},
    );
    assert_eq!(
        stops.load(Ordering::SeqCst),
        1,
        "the writer stopped the tree"
    );
    assert!(
        matches!(
            result,
            Err(ProviderError::ClaudeCode(ClaudeCodeFailure::Failed))
        ),
        "{result:?}"
    );
}

/// A write whose deadline passes after its I/O and just before its record,
/// as when the writer is descheduled there, fails and stops the tree once
/// although the watcher never sees the expiry: the writer's own sample,
/// taken just before the record, decides the write.
#[test]
fn a_write_whose_deadline_passes_just_before_its_record_fails() {
    let expired = AtomicBool::new(false);
    let stops = AtomicUsize::new(0);
    let result = raced_write(
        || Ok(()),
        &CancelToken::new(),
        || expired.load(Ordering::SeqCst) && thread::current().name() != Some(WATCHER),
        || {
            stops.fetch_add(1, Ordering::SeqCst);
        },
        |point| {
            if point == WritePoint::BeforeRecord {
                expired.store(true, Ordering::SeqCst);
            }
        },
    );
    assert_eq!(
        stops.load(Ordering::SeqCst),
        1,
        "the writer stopped the tree"
    );
    assert!(
        matches!(
            result,
            Err(ProviderError::ClaudeCode(ClaudeCodeFailure::Failed))
        ),
        "{result:?}"
    );
}

/// A write recorded in time succeeds without stopping the tree when its
/// deadline passes, or the turn is cancelled, just after the record, as when
/// the writer is descheduled there: the sample taken before the record
/// decided the write, and a later reading of the clock does not undo it.
/// The watcher sees the expiry too, and cannot stop a recorded write.
#[test]
fn a_write_whose_deadline_or_cancellation_comes_just_after_its_record_succeeds() {
    for by_cancellation in [false, true] {
        let cancel = CancelToken::new();
        let expired = AtomicBool::new(false);
        let stops = AtomicUsize::new(0);
        let mut seen = Vec::new();
        let result = raced_write(
            || Ok(()),
            &cancel,
            || expired.load(Ordering::SeqCst),
            || {
                stops.fetch_add(1, Ordering::SeqCst);
            },
            |point| {
                seen.push(point);
                if point == WritePoint::AfterRecord {
                    if by_cancellation {
                        cancel.cancel();
                    } else {
                        expired.store(true, Ordering::SeqCst);
                    }
                }
            },
        );
        assert!(result.is_ok(), "{by_cancellation}: {result:?}");
        assert_eq!(
            stops.load(Ordering::SeqCst),
            0,
            "{by_cancellation}: the tree was never stopped"
        );
        assert_eq!(seen, [WritePoint::BeforeRecord, WritePoint::AfterRecord]);
    }
}

/// A write blocked in its I/O, as when the relay reads nothing, is released
/// by the watcher once the turn is cancelled or its deadline passes. The
/// writer signals that it is blocked and returns only once the stop came,
/// failing as a channel that the tree's end closed fails it; the turn then
/// reports the cancellation or the failure, and the tree is stopped once.
#[test]
fn a_write_blocked_in_its_io_is_released_by_a_cancellation_or_its_deadline() {
    for by_cancellation in [true, false] {
        let cancel = CancelToken::new();
        let expired = AtomicBool::new(false);
        let stops = AtomicUsize::new(0);
        let (blocked, is_blocked) = mpsc::channel::<()>();
        let mut released = false;
        let result = thread::scope(|scope| {
            let (cancel, expired) = (&cancel, &expired);
            scope.spawn(move || {
                // Bounded, so a failed test cannot leave it behind.
                if is_blocked.recv_timeout(Duration::from_secs(30)).is_ok() {
                    if by_cancellation {
                        cancel.cancel();
                    } else {
                        expired.store(true, Ordering::SeqCst);
                    }
                }
            });
            raced_write(
                || {
                    blocked.send(()).expect("the releaser waits");
                    released = held_until(&stops);
                    Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
                },
                cancel,
                || expired.load(Ordering::SeqCst),
                || {
                    stops.fetch_add(1, Ordering::SeqCst);
                },
                |_| {},
            )
        });
        assert!(released, "the stop released the blocked write");
        assert_eq!(stops.load(Ordering::SeqCst), 1, "stopped once");
        if by_cancellation {
            assert!(
                matches!(result, Err(ProviderError::Cancelled)),
                "{result:?}"
            );
        } else {
            assert!(
                matches!(
                    result,
                    Err(ProviderError::ClaudeCode(ClaudeCodeFailure::Failed))
                ),
                "{result:?}"
            );
        }
    }
}

/// A write already cancelled or past its deadline never starts, and still
/// stops the tree.
#[test]
fn a_write_already_cancelled_or_late_never_starts() {
    for (cancelled, late) in [(true, false), (false, true)] {
        let cancel = CancelToken::new();
        if cancelled {
            cancel.cancel();
        }
        let stops = AtomicUsize::new(0);
        let mut started = false;
        let result = raced_write(
            || {
                started = true;
                Ok(())
            },
            &cancel,
            || late,
            || {
                stops.fetch_add(1, Ordering::SeqCst);
            },
            |_| {},
        );
        assert!(!started, "the I/O never started");
        assert_eq!(stops.load(Ordering::SeqCst), 1, "stopped once");
        if cancelled {
            assert!(
                matches!(result, Err(ProviderError::Cancelled)),
                "{result:?}"
            );
        } else {
            assert!(
                matches!(
                    result,
                    Err(ProviderError::ClaudeCode(ClaudeCodeFailure::Failed))
                ),
                "{result:?}"
            );
        }
    }
}

/// A write recorded before its deadline succeeds and never stops the tree,
/// and a failed one is a protocol failure.
#[test]
fn a_write_recorded_in_time_succeeds_without_stopping_the_tree() {
    let stops = AtomicUsize::new(0);
    let cancel = CancelToken::new();
    let stop = || {
        stops.fetch_add(1, Ordering::SeqCst);
    };
    raced_write(|| Ok(()), &cancel, || false, stop, |_| {}).expect("written");
    let failed = raced_write(
        || Err(std::io::Error::other("broken channel")),
        &cancel,
        || false,
        stop,
        |_| {},
    );
    assert!(
        matches!(
            failed,
            Err(ProviderError::ClaudeCode(ClaudeCodeFailure::Protocol))
        ),
        "{failed:?}"
    );
    assert_eq!(
        stops.load(Ordering::SeqCst),
        0,
        "the tree was never stopped"
    );
}

/// A session holding the connection's turn lock and its turn record, as a
/// launch leaves it.
fn held_session(data: &Path, runtime: &Path) -> (Session, SyncSender<Event>, Uuid) {
    let profile_id = Uuid::new_v4();
    let watch = ClaudeCodeWatch::for_tests(data.to_owned(), profile_id, data);
    let (mut session, sender) = session(runtime);
    session.turn_lock = Some(watch.lock_turn().expect("lock").expect("free"));
    session.turn_record = Some(watch.record_turn().expect("record"));
    session.turn_watch = Some(watch);
    (session, sender, profile_id)
}

/// A session whose tree was not observed ended keeps the connection's turn
/// lock and its turn record, so a revocation fails while this process runs
/// and also after it exits: both for a tree whose end its session could not
/// confirm and for one the launch could not stop before the session owned
/// it. A session whose tree ended clears its record and releases the lock.
#[test]
fn an_unconfirmed_tree_end_keeps_the_turn_lock_and_its_record() {
    let _serial = UNCONFIRMED_SERIAL
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let data = TempDir::new().expect("data folder");
    fs::create_dir(data.path().join("claude-code")).expect("runtime parent");
    let short = Duration::from_millis(200);
    // (the tree was observed ended, the launch could not stop a tree)
    for (ended, unstopped) in [(true, false), (false, false), (true, true)] {
        let runtime = TempDir::new().expect("runtime folder");
        let (mut session, _sender, profile_id) = held_session(data.path(), runtime.path());
        session.unconfirmed = unstopped;
        let released = session.release_turn(ended, Vec::new());
        drop(session);
        let waited = wait_for_claude_code_turns(data.path(), profile_id, short);
        if ended && !unstopped {
            assert!(released);
            waited.expect("an observed end clears the record and releases the lock");
            continue;
        }
        assert!(!released);
        assert!(
            matches!(waited, Err(ControlError::ClaudeCodeTurnsRunning)),
            "the lock is held while this process runs: {waited:?}"
        );
        // This process's exit releases the held lock; the tests that leave
        // an unconfirmed end run one at a time, so the last one held is this.
        let held = UNCONFIRMED_TURNS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop();
        assert!(held.is_some(), "the lock was kept");
        drop(held);
        let waited = wait_for_claude_code_turns(data.path(), profile_id, short);
        assert!(
            matches!(waited, Err(ControlError::ClaudeCodeCleanupUnconfirmed(id)) if id == profile_id),
            "the record outlives the lock: {waited:?}"
        );
    }
}

/// The tests that leave an unconfirmed end and then release the lock it
/// holds, as this process's exit would, run one at a time.
static UNCONFIRMED_SERIAL: Mutex<()> = Mutex::new(());

/// A launch whose platform reports a tree it started and could not stop
/// goes through the launch's own error path: the turn fails, keeps the
/// connection's lock while this process runs, and keeps its record after
/// this process exits, so every later wait fails until acknowledged. Any
/// other launch failure started nothing and leaves neither.
#[test]
fn a_launch_that_could_not_stop_its_tree_blocks_waits_past_exit() {
    let _serial = UNCONFIRMED_SERIAL
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let data = TempDir::new().expect("data folder");
    fs::create_dir(data.path().join("claude-code")).expect("runtime parent");
    let short = Duration::from_millis(200);
    for unstopped in [true, false] {
        let runtime = TempDir::new().expect("runtime folder");
        let (mut session, _sender, profile_id) = held_session(data.path(), runtime.path());
        let error = if unstopped {
            ProcessError::CleanupUnconfirmed(UnconfirmedCleanup::new("a test tree"))
        } else {
            ProcessError::InvalidSpec("a test launch")
        };
        let launched = spawned(&mut session, Err::<StreamingChild, _>(error));
        assert!(
            matches!(
                launched,
                Err(ProviderError::ClaudeCode(ClaudeCodeFailure::Failed))
            ),
            "{launched:?}"
        );
        // A failed launch drops its session, which ends the turn.
        drop(session);
        let waited = wait_for_claude_code_turns(data.path(), profile_id, short);
        if !unstopped {
            waited.expect("nothing started, so the turn left nothing");
            continue;
        }
        assert!(
            matches!(waited, Err(ControlError::ClaudeCodeTurnsRunning)),
            "the lock is held while this process runs: {waited:?}"
        );
        let held = UNCONFIRMED_TURNS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop();
        assert!(held.is_some(), "the lock was kept");
        drop(held);
        let waited = wait_for_claude_code_turns(data.path(), profile_id, short);
        assert!(
            matches!(waited, Err(ControlError::ClaudeCodeCleanupUnconfirmed(id)) if id == profile_id),
            "the record outlives the lock: {waited:?}"
        );
    }
}

/// A tree observed ended clears its record at once, but the turn lock waits
/// for a worker still running after the grace, so a revocation's wait ends
/// only once that worker ended too, and then finds no record.
#[test]
fn an_outstanding_worker_keeps_the_turn_lock_until_it_ends() {
    let data = TempDir::new().expect("data folder");
    fs::create_dir(data.path().join("claude-code")).expect("runtime parent");
    let runtime = TempDir::new().expect("runtime folder");
    let (mut session, _sender, profile_id) = held_session(data.path(), runtime.path());
    let (release, released) = mpsc::channel::<()>();
    let worker = thread::spawn(move || {
        // Bounded, so a failed test cannot leave it behind.
        let _ended = released.recv_timeout(Duration::from_secs(30));
    });
    assert!(
        !session.release_turn(true, vec![worker]),
        "the lock is still held"
    );
    drop(session);
    let waited = wait_for_claude_code_turns(data.path(), profile_id, Duration::from_millis(200));
    assert!(
        matches!(waited, Err(ControlError::ClaudeCodeTurnsRunning)),
        "the worker keeps the lock: {waited:?}"
    );
    release.send(()).expect("end the worker");
    wait_for_claude_code_turns(data.path(), profile_id, Duration::from_secs(30))
        .expect("the lock is released once the worker ended");
}

/// A worker that panicked has ended: it is reported, but nothing waits for
/// it.
#[test]
fn a_worker_that_panicked_counts_as_ended() {
    let runtime = TempDir::new().expect("runtime folder");
    let (mut session, _sender) = session(runtime.path());
    let worker = thread::Builder::new()
        .name("a worker that panics on purpose".into())
        .spawn(|| panic!("on purpose"))
        .expect("worker");
    session.workers.push(worker);
    let stopped = session.stop_workers();
    assert!(stopped.panicked);
    assert!(stopped.outstanding.is_empty());
}

#[test]
fn a_write_stopped_first_is_not_reported_as_finished() {
    let race = WriteRace::new();
    assert!(race.stop(), "the watcher records first");
    assert!(!race.running());
    assert!(!race.finish(), "the writer's later finish loses");
    assert!(!race.stop(), "the end is recorded once");
}
