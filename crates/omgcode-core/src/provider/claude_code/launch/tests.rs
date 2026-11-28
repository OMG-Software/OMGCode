//! A session's round, driven from a queue the test fills (ADR-0109).

use std::path::Path;
use std::sync::mpsc::{self, SyncSender};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

use super::super::mcp::{McpServer, McpTool};
use super::{
    EVENT_QUEUE, Event, Next, Progress, SERVER_NAME, Session, TOOL_PREFIX, TurnFolder, WriteRace,
};
use crate::provider::claude_code::stream::parse_line;
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
        match session.next_event() {
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

#[test]
fn a_write_that_finished_first_is_not_reported_as_stopped() {
    let race = WriteRace::new();
    assert!(race.running());
    assert!(race.finish(), "the writer records first");
    assert!(!race.running());
    assert!(!race.stop(), "the watcher's later stop loses");
    assert!(!race.finish(), "the end is recorded once");
}

#[test]
fn a_write_stopped_first_is_not_reported_as_finished() {
    let race = WriteRace::new();
    assert!(race.stop(), "the watcher records first");
    assert!(!race.running());
    assert!(!race.finish(), "the writer's later finish loses");
    assert!(!race.stop(), "the end is recorded once");
}
