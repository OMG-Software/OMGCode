//! Behaviour of the front-end-neutral chat turn against a local mock provider.
//!
//! Every test uses a private temporary data root and a loopback server bound to
//! an ephemeral port. No process is spawned on Unix; on Windows the private
//! data root is prepared with `icacls`, which holds no store handle.

use std::cell::{Cell, RefCell};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

mod common;

use omgcode_app::{
    ChatHooks, ChatSettings, ChatStreamEvent, ChatTurnInput, ChatTurnOutput, compact_journal,
    create_session, run_chat_turn,
};
use omgcode_core::context::ContextBudget;
use omgcode_core::control::ControlStore;
use omgcode_core::policy::{Access, PermissionSet, WriteToolAccess};
use omgcode_core::provider::{LocalModelClient, LocalModelConfig};
use omgcode_core::session::{MessageRole, SessionJournal};
use omgcode_core::turn::CancelToken;
use serde_json::{Value, json};
use tempfile::TempDir;

const MOCK_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_HTTP_BYTES: usize = 1024 * 1024;
const SECRET: &str = "fixture-token-7f3a91";

/// Serve one scripted response per connection and optionally cancel a turn first.
struct SequenceServer {
    endpoint: String,
    /// Dropping it lets the worker release the port.
    stop: mpsc::Sender<()>,
    worker: JoinHandle<Result<usize, String>>,
}

impl SequenceServer {
    /// `cancel_source` delivers the token the hook captured; the server cancels
    /// it after the first request arrives and before it replies.
    fn start(responses: Vec<Value>, cancel_source: Option<Receiver<CancelToken>>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind mock server");
        listener.set_nonblocking(true).expect("bounded accept");
        let address = listener.local_addr().expect("mock address");
        let (stop, stopped) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result = serve_sequence(&listener, responses, cancel_source.as_ref());
            common::refuse_until_stopped(&listener, &stopped);
            result
        });
        Self {
            endpoint: format!("http://{address}/v1"),
            stop,
            worker,
        }
    }

    /// Wait for the server thread and return how many requests it answered.
    fn finish(self) -> usize {
        drop(self.stop);
        self.worker
            .join()
            .expect("mock server thread")
            .expect("mock server exchange")
    }
}

/// Answer each scripted response on its own connection.
fn serve_sequence(
    listener: &TcpListener,
    responses: Vec<Value>,
    cancel_source: Option<&Receiver<CancelToken>>,
) -> Result<usize, String> {
    let mut served = 0;
    for response in responses {
        let Ok(mut stream) = accept(listener) else {
            break;
        };
        read_body(&mut stream)?;
        if served == 0
            && let Some(source) = cancel_source
        {
            let token = source
                .recv_timeout(MOCK_TIMEOUT)
                .map_err(|error| error.to_string())?;
            token.cancel();
        }
        let bytes = serde_json::to_vec(&response).map_err(|error| error.to_string())?;
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            bytes.len()
        );
        stream
            .write_all(head.as_bytes())
            .and_then(|()| stream.write_all(&bytes))
            .map_err(|error| error.to_string())?;
        common::await_client_close(&mut stream);
        served += 1;
    }
    Ok(served)
}

fn accept(listener: &TcpListener) -> Result<TcpStream, String> {
    let deadline = Instant::now() + MOCK_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .map_err(|error| error.to_string())?;
                stream
                    .set_read_timeout(Some(MOCK_TIMEOUT))
                    .map_err(|error| error.to_string())?;
                return Ok(stream);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err("no further request".to_owned());
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn read_body(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            let length: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .ok_or("missing content length")?
                .trim()
                .parse()
                .map_err(|_| "invalid content length")?;
            if length > MAX_HTTP_BYTES {
                return Err("request too large".to_owned());
            }
            while bytes.len() < end + 4 + length {
                let read = stream
                    .read(&mut buffer)
                    .map_err(|error| error.to_string())?;
                if read == 0 {
                    return Err("truncated body".to_owned());
                }
                bytes.extend_from_slice(&buffer[..read]);
            }
            return Ok(bytes[end + 4..end + 4 + length].to_vec());
        }
        if bytes.len() > MAX_HTTP_BYTES {
            return Err("headers too large".to_owned());
        }
        let read = stream
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            return Err("connection closed before headers".to_owned());
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
}

fn text(content: &str) -> Value {
    json!({
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }]
    })
}

fn tool_call(id: &str, name: &str, arguments: &Value) -> Value {
    json!({
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments.to_string()}
                }]
            },
            "finish_reason": "tool_calls"
        }]
    })
}

/// Private application data and one workspace; optionally a registered
/// workspace whose `model_tools` and `read` preferences are enabled.
struct Fixture {
    _temporary: TempDir,
    data: PathBuf,
    workspace: PathBuf,
}

impl Fixture {
    fn new(tools_enabled: bool) -> Self {
        let temporary = tempfile::tempdir().expect("private fixture root");
        let data = temporary.path().join("application-data");
        let development = temporary.path().join("development");
        let workspace = development.join("project");
        fs::create_dir_all(&workspace).expect("workspace directories");
        fs::create_dir_all(&data).expect("application data");
        if tools_enabled {
            #[cfg(windows)]
            restrict_to_current_user(&data);
            let mut control = ControlStore::open(&data).expect("private control store");
            control
                .set_development_root(&development, control.revision())
                .expect("development root");
            let id = control
                .register_workspace(&workspace, control.revision())
                .expect("registered workspace")
                .id();
            control
                .set_permissions(
                    id,
                    PermissionSet {
                        read: Access::Allow,
                        write: Access::Deny,
                        execute: Access::Deny,
                        task_network: Access::Deny,
                        model_tools: true,
                        host_services: Access::Deny,
                        write_tools: WriteToolAccess::default(),
                    },
                    control.revision(),
                )
                .expect("model tool preferences");
        }
        Self {
            _temporary: temporary,
            data,
            workspace,
        }
    }

    fn journal(&self) -> SessionJournal {
        create_session(&self.data, &self.workspace, "Fixture session").expect("session")
    }
}

/// Give `data` the private ACL the Windows control store requires of its root.
#[cfg(windows)]
fn restrict_to_current_user(data: &Path) {
    let whoami = std::process::Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .expect("whoami");
    assert!(whoami.status.success(), "whoami fixture failed");
    let identity = String::from_utf8_lossy(&whoami.stdout).into_owned();
    let sid = identity
        .trim()
        .rsplit(',')
        .next()
        .expect("SID column")
        .trim_matches('"')
        .to_owned();
    let granted = std::process::Command::new("icacls")
        .arg(data)
        .args(["/inheritance:r", "/grant:r", &format!("*{sid}:(OI)(CI)F")])
        .output()
        .expect("icacls");
    assert!(granted.status.success(), "icacls fixture failed");
}

fn settings(endpoint: &str) -> ChatSettings {
    let mut settings = ChatSettings::new(endpoint, "fixture", 131_072);
    settings.max_output_tokens = 64;
    settings
}

fn turn(
    journal: &mut SessionJournal,
    data: &Path,
    settings: &ChatSettings,
    token: Option<&str>,
    message: &str,
    hooks: &mut ChatHooks<'_>,
) -> Result<ChatTurnOutput, omgcode_app::AppError> {
    run_chat_turn(
        journal,
        data,
        &ChatTurnInput {
            message,
            settings,
            token,
            skills: &[],
        },
        hooks,
    )
}

/// Close the journal, which holds an exclusive lock, and read its events.
fn close_and_read(journal: SessionJournal) -> Vec<Value> {
    let path = journal.path().to_owned();
    drop(journal);
    fs::read_to_string(path)
        .expect("journal file")
        .lines()
        .map(|line| serde_json::from_str(line).expect("journal event"))
        .collect()
}

fn last_failure_code(events: &[Value]) -> Option<String> {
    events
        .iter()
        .rev()
        .find(|event| event["type"] == "turn.failed")
        .and_then(|event| event["payload"]["code"].as_str().map(str::to_owned))
}

#[test]
fn an_archived_session_is_refused_with_the_published_message() {
    let fixture = Fixture::new(false);
    let mut journal = fixture.journal();
    journal.archive().expect("archive");
    let error = turn(
        &mut journal,
        &fixture.data,
        &settings(&common::unused_endpoint()),
        None,
        "hello",
        &mut ChatHooks::default(),
    )
    .expect_err("archived sessions cannot chat");
    assert_eq!(error.code(), "session");
    assert_eq!(
        error.message(),
        "Restore the archived session before chatting"
    );
}

#[test]
fn an_incomplete_previous_turn_is_refused_with_the_published_message() {
    let fixture = Fixture::new(false);
    let mut journal = fixture.journal();
    journal.start_turn().expect("start turn");
    let error = turn(
        &mut journal,
        &fixture.data,
        &settings(&common::unused_endpoint()),
        None,
        "hello",
        &mut ChatHooks::default(),
    )
    .expect_err("an active turn blocks chat");
    assert_eq!(error.code(), "session");
    assert_eq!(
        error.message(),
        "The previous turn is incomplete; inspect it, then use session interrupt to mark it failed without retrying"
    );
}

#[test]
fn reaching_the_trigger_without_auto_compact_is_a_context_error() {
    let fixture = Fixture::new(false);
    let mut journal = fixture.journal();
    let mut chat = settings(&common::unused_endpoint());
    chat.compact_threshold = 1;
    let error = turn(
        &mut journal,
        &fixture.data,
        &chat,
        None,
        "hello",
        &mut ChatHooks::default(),
    )
    .expect_err("the trigger is reached");
    assert_eq!(error.code(), "context");
    let message = error.message();
    assert!(
        message.starts_with("The conservative input estimate ("),
        "{message}"
    );
    assert!(
        message.ends_with(". Use session compact or rerun chat with --auto-compact."),
        "{message}"
    );
    assert!(
        close_and_read(journal)
            .iter()
            .all(|event| event["type"] != "turn.started"),
        "a refused preflight must not start a turn"
    );
}

#[test]
fn a_zero_threshold_is_a_configuration_error() {
    let fixture = Fixture::new(false);
    let mut journal = fixture.journal();
    let mut chat = settings(&common::unused_endpoint());
    chat.compact_threshold = 0;
    let error = turn(
        &mut journal,
        &fixture.data,
        &chat,
        None,
        "hello",
        &mut ChatHooks::default(),
    )
    .expect_err("a zero threshold is invalid");
    assert_eq!(error.code(), "configuration");
    assert_eq!(
        error.message(),
        "The compaction threshold must be a positive token count"
    );
}

#[test]
fn an_invalid_endpoint_is_a_provider_error_before_any_storage_use() {
    let fixture = Fixture::new(false);
    let mut journal = fixture.journal();
    let error = turn(
        &mut journal,
        &fixture.data,
        &settings("http://example.com/v1"),
        None,
        "hello",
        &mut ChatHooks::default(),
    )
    .expect_err("only loopback endpoints are accepted");
    assert_eq!(error.code(), "provider");
    assert_eq!(error.message(), "invalid local model configuration");
}

#[test]
fn a_transport_failure_is_recorded_and_never_mentions_the_token() {
    let fixture = Fixture::new(false);
    let mut journal = fixture.journal();
    let error = turn(
        &mut journal,
        &fixture.data,
        &settings(&common::unused_endpoint()),
        Some(SECRET),
        "hello",
        &mut ChatHooks::default(),
    )
    .expect_err("nothing is listening");
    assert_eq!(error.code(), "provider");
    assert_eq!(error.message(), "local model transport failed");
    assert!(!format!("{error:?}{error}").contains(SECRET));
    assert_eq!(journal.snapshot().active_turn, None);
    let events = close_and_read(journal);
    assert_eq!(
        last_failure_code(&events).as_deref(),
        Some("provider_error")
    );
}

#[test]
fn a_text_only_turn_serializes_the_published_object_and_calls_the_cancel_hook_once() {
    let fixture = Fixture::new(false);
    let mut journal = fixture.journal();
    let server = SequenceServer::start(vec![text(&format!("pong {SECRET}"))], None);
    let mut calls = 0_usize;
    let mut on_cancel_token = |token: CancelToken| {
        assert!(!token.is_cancelled());
        calls += 1;
    };
    let output = turn(
        &mut journal,
        &fixture.data,
        &settings(&server.endpoint),
        Some(SECRET),
        &format!("ping {SECRET}"),
        &mut ChatHooks {
            approval_wait: None,
            cancel: None,
            on_cancel_token: Some(&mut on_cancel_token),
            on_stream: None,
        },
    )
    .expect("text-only turn");
    assert_eq!(server.finish(), 1);
    // A request that is not streamed can be interrupted too (ADR-0051).
    assert_eq!(calls, 1);

    assert!(!output.tools_enabled());
    assert_eq!(output.disabled_reason(), Some("settings_absent"));
    assert_eq!(output.reply(), "pong [REDACTED]");
    assert_eq!(output.finish_reason(), "stop");
    assert_eq!(output.rounds(), None);
    assert_eq!(output.withheld_tool_results(), 0);
    assert_eq!(output.context().estimate_scope(), "pre_turn_conservative");

    let serialized = serde_json::to_string(&output).expect("serialize");
    assert!(!serialized.contains(SECRET));
    let value: Value = serde_json::from_str(&serialized).expect("JSON");
    assert_eq!(value["reply"]["content"], "pong [REDACTED]");
    assert_eq!(value["reply"]["finish_reason"], "stop");
    assert_eq!(
        value["model_tools"],
        json!({"mode": "disabled", "reason": "settings_absent", "withheld_tool_results": 0})
    );
    let mut keys: Vec<&str> = value
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["context", "model_tools", "reply"]);
    // The recorded user message is the redacted one.
    let events = close_and_read(journal);
    assert!(!format!("{events:?}").contains(SECRET));
}

#[test]
fn a_tool_turn_calls_the_cancel_hook_once_and_serializes_the_published_object() {
    let fixture = Fixture::new(true);
    let mut journal = fixture.journal();
    let server = SequenceServer::start(vec![text("done")], None);
    let mut calls = 0_usize;
    let mut on_cancel_token = |token: CancelToken| {
        assert!(!token.is_cancelled());
        calls += 1;
    };
    let output = turn(
        &mut journal,
        &fixture.data,
        &settings(&server.endpoint),
        None,
        "hello",
        &mut ChatHooks {
            approval_wait: None,
            cancel: None,
            on_cancel_token: Some(&mut on_cancel_token),
            on_stream: None,
        },
    )
    .expect("tool turn");
    assert_eq!(server.finish(), 1);
    assert_eq!(calls, 1);

    assert!(output.tools_enabled());
    assert_eq!(output.disabled_reason(), None);
    assert_eq!(output.reply(), "done");
    assert_eq!(output.rounds(), Some(1));
    assert_eq!(output.tool_calls(), Some(0));
    assert_eq!(output.tool_bytes(), Some(0));
    assert_eq!(output.withheld_tool_results(), 0);
    assert!(output.workspace_id().is_some());

    let serialized = serde_json::to_string(&output).expect("serialize");
    let reply_at = serialized.find("\"reply\"").expect("reply key");
    let tools_at = serialized.find("\"model_tools\"").expect("tools key");
    let context_at = serialized.find("\"context\"").expect("context key");
    assert!(reply_at < tools_at && tools_at < context_at, "{serialized}");
    let value: Value = serde_json::from_str(&serialized).expect("JSON");
    assert_eq!(value["model_tools"]["mode"], "enabled");
    assert_eq!(value["model_tools"]["rounds"], 1);
    assert_eq!(value["model_tools"]["tool_calls"], 0);
    assert_eq!(value["model_tools"]["tool_bytes"], 0);
    assert_eq!(value["model_tools"]["withheld_tool_results"], 0);
    assert_eq!(value["reply"]["content"], "done");
}

#[test]
fn a_cancelled_token_ends_the_tool_turn_with_the_core_cancelled_failure() {
    let fixture = Fixture::new(true);
    let mut journal = fixture.journal();
    let (sender, receiver) = mpsc::channel::<CancelToken>();
    let server = SequenceServer::start(
        vec![tool_call(
            "call-1",
            "workspace_list_directory",
            &json!({"path": "."}),
        )],
        Some(receiver),
    );
    let mut calls = 0_usize;
    let mut on_cancel_token = |token: CancelToken| {
        calls += 1;
        sender.send(token).expect("hand the token to the server");
    };
    let error = turn(
        &mut journal,
        &fixture.data,
        &settings(&server.endpoint),
        None,
        "list the files",
        &mut ChatHooks {
            approval_wait: None,
            cancel: None,
            on_cancel_token: Some(&mut on_cancel_token),
            on_stream: None,
        },
    )
    .expect_err("cancellation stops the turn");
    assert_eq!(server.finish(), 1);
    assert_eq!(calls, 1);
    assert_eq!(error.code(), "cancelled");
    assert_eq!(error.message(), "turn was cancelled");
    assert_eq!(journal.snapshot().active_turn, None);
    let events = close_and_read(journal);
    assert_eq!(last_failure_code(&events).as_deref(), Some("cancelled"));
}

/// Record stream events as text: `request_started` and `text:<piece>`.
fn capture(log: &mut Vec<String>) -> impl FnMut(ChatStreamEvent<'_>) + '_ {
    move |event| {
        log.push(match event {
            ChatStreamEvent::RequestStarted => "request_started".to_owned(),
            ChatStreamEvent::Text(text) => format!("text:{text}"),
        });
    }
}

/// The event kinds of a journal, with role and text for each recorded message.
/// Identifiers, times, and the endpoint in the manifest are left out.
fn journal_shape(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            let kind = event["type"].as_str().unwrap_or("?");
            if kind == "message.recorded" {
                format!(
                    "{kind}|{}|{}",
                    event["payload"]["role"], event["payload"]["text"]
                )
            } else {
                kind.to_owned()
            }
        })
        .collect()
}

fn request_body(recorded: &common::Recorded) -> Value {
    serde_json::from_str(&recorded.body).expect("request JSON")
}

#[test]
fn a_streamed_text_turn_delivers_pieces_and_journals_like_a_non_streaming_turn() {
    // Given the same reply served once as one JSON document and once as a stream.
    let plain_fixture = Fixture::new(false);
    let mut plain_journal = plain_fixture.journal();
    let plain_server = SequenceServer::start(vec![text("Hello streamed world")], None);
    let plain = turn(
        &mut plain_journal,
        &plain_fixture.data,
        &settings(&plain_server.endpoint),
        None,
        "greet me",
        &mut ChatHooks::default(),
    )
    .expect("non-streaming turn");
    assert_eq!(plain_server.finish(), 1);

    let fixture = Fixture::new(false);
    let mut journal = fixture.journal();
    let server = common::MockServer::start(vec![common::Step::stream(common::sse_text(&[
        "Hello ",
        "streamed ",
        "world",
    ]))]);
    let mut log = Vec::new();

    // When the streaming turn runs with a stream hook.
    let output = {
        let mut on_stream = capture(&mut log);
        turn(
            &mut journal,
            &fixture.data,
            &settings(&server.endpoint),
            None,
            "greet me",
            &mut ChatHooks {
                on_stream: Some(&mut on_stream),
                ..ChatHooks::default()
            },
        )
        .expect("streamed turn")
    };
    let (served, recorded) = server.finish_recorded();

    // Then the request started once, the pieces arrived in order, and the
    // reply and journal match the non-streaming turn.
    assert_eq!(
        log,
        [
            "request_started",
            "text:Hello ",
            "text:streamed ",
            "text:world"
        ]
    );
    assert_eq!(served, 1);
    let request = request_body(&recorded[0]);
    assert_eq!(request["stream"], true);
    assert!(request.get("stream_options").is_none());
    assert_eq!(
        recorded[0].header("accept").as_deref(),
        Some("text/event-stream")
    );
    assert_eq!(output.reply(), plain.reply());
    assert_eq!(output.finish_reason(), plain.finish_reason());
    assert_eq!(
        journal_shape(&close_and_read(journal)),
        journal_shape(&close_and_read(plain_journal))
    );
}

#[test]
fn a_token_split_across_pieces_never_reaches_events_or_the_journal() {
    // Given a token split across two pieces and used again whole.
    let fixture = Fixture::new(false);
    let mut journal = fixture.journal();
    let server = common::MockServer::start(vec![common::Step::stream(common::sse_text(&[
        "key is fixture-tok",
        "en-7f3a91 and again ",
        SECRET,
        " end",
    ]))]);
    let mut log = Vec::new();

    // When the streamed turn runs with the token configured.
    let output = {
        let mut on_stream = capture(&mut log);
        turn(
            &mut journal,
            &fixture.data,
            &settings(&server.endpoint),
            Some(SECRET),
            "what is the key",
            &mut ChatHooks {
                on_stream: Some(&mut on_stream),
                ..ChatHooks::default()
            },
        )
        .expect("streamed turn")
    };
    server.finish();

    // Then neither the events nor the journal hold it, and the reply is redacted.
    let shown: String = log
        .iter()
        .filter_map(|event| event.strip_prefix("text:"))
        .collect();
    assert_eq!(shown, "key is [REDACTED] and again [REDACTED] end");
    assert!(!log.join("\n").contains(SECRET));
    assert!(!log.join("\n").contains("fixture-tok"));
    assert_eq!(output.reply(), "key is [REDACTED] and again [REDACTED] end");
    let events = close_and_read(journal);
    assert!(!format!("{events:?}").contains(SECRET));
    assert!(format!("{events:?}").contains("[REDACTED]"));
}

#[test]
fn a_streamed_tool_turn_starts_two_requests_and_runs_the_tool() {
    // Given a streamed tool call followed by a streamed text reply.
    let fixture = Fixture::new(true);
    let mut journal = fixture.journal();
    let server = common::MockServer::start(vec![
        common::Step::stream(common::sse_tool_call(
            "call-1",
            "workspace_list_directory",
            &json!({"path": ""}),
        )),
        common::Step::stream(common::sse_text(&["all ", "done"])),
    ]);
    let mut log = Vec::new();

    // When the tool-enabled streamed turn runs.
    let output = {
        let mut on_stream = capture(&mut log);
        turn(
            &mut journal,
            &fixture.data,
            &settings(&server.endpoint),
            None,
            "list the files",
            &mut ChatHooks {
                on_stream: Some(&mut on_stream),
                ..ChatHooks::default()
            },
        )
        .expect("streamed tool turn")
    };
    let (served, recorded) = server.finish_recorded();

    // Then each request announced itself, tool-call pieces were never shown,
    // and the tool ran as in the non-streaming test.
    assert_eq!(
        log,
        [
            "request_started",
            "request_started",
            "text:all ",
            "text:done"
        ]
    );
    assert_eq!(served, 2);
    for request in &recorded {
        assert_eq!(request_body(request)["stream"], true);
    }
    assert_eq!(output.reply(), "all done");
    assert_eq!(output.rounds(), Some(2));
    assert_eq!(output.tool_calls(), Some(1));
}

#[test]
fn a_stream_that_fails_after_two_pieces_keeps_them_and_records_a_provider_failure() {
    // Given a stream that ends after two pieces without `[DONE]`.
    let fixture = Fixture::new(false);
    let mut journal = fixture.journal();
    let server =
        common::MockServer::start(vec![common::Step::stream(common::sse_text_unfinished(&[
            "first-piece ",
            "second-piece",
        ]))]);
    let mut log = Vec::new();

    // When the streamed turn runs.
    let error = {
        let mut on_stream = capture(&mut log);
        turn(
            &mut journal,
            &fixture.data,
            &settings(&server.endpoint),
            None,
            "say something",
            &mut ChatHooks {
                on_stream: Some(&mut on_stream),
                ..ChatHooks::default()
            },
        )
        .expect_err("incomplete stream fails the turn")
    };
    server.finish();

    // Then the two pieces were delivered, nothing followed, and the failure
    // is the provider error recorded as `provider_error`.
    assert_eq!(
        log,
        ["request_started", "text:first-piece ", "text:second-piece"]
    );
    assert_eq!(error.code(), "provider");
    assert_eq!(
        error.message(),
        "local model stream ended before completion"
    );
    assert_eq!(journal.snapshot().active_turn, None);
    let events = close_and_read(journal);
    assert_eq!(
        last_failure_code(&events).as_deref(),
        Some("provider_error")
    );
    assert!(!format!("{events:?}").contains("second-piece"));
}

#[test]
fn a_turn_without_a_stream_hook_sends_stream_false() {
    for tools_enabled in [false, true] {
        let fixture = Fixture::new(tools_enabled);
        let mut journal = fixture.journal();
        let server = common::MockServer::start(vec![common::Step::reply(common::text("plain"))]);
        let output = turn(
            &mut journal,
            &fixture.data,
            &settings(&server.endpoint),
            None,
            "hello",
            &mut ChatHooks::default(),
        )
        .expect("plain turn");
        let (served, recorded) = server.finish_recorded();
        assert_eq!(served, 1);
        assert_eq!(output.reply(), "plain");
        let request = request_body(&recorded[0]);
        assert_eq!(request["stream"], false, "tools_enabled={tools_enabled}");
        assert!(request.get("stream_options").is_none());
        assert_ne!(
            recorded[0].header("accept").as_deref(),
            Some("text/event-stream")
        );
    }
}

// ---- Cancelling a streamed turn (ADR-0050) ----

/// The slowest acceptable cancellation, timed from the moment the token is
/// cancelled until the turn or compaction returns. The request deadline is 30
/// seconds.
///
/// The interval starts at the cancel, as in the desktop cancellation tests.
/// Before it, a turn reads its settings and makes several synced journal
/// appends. Their duration depends on the host's storage, not on cancellation:
/// on a loaded Windows runner they once took 5.9 seconds.
const CANCEL_BOUND: Duration = Duration::from_secs(5);

/// What a streamed turn did when its token was cancelled by the first piece.
struct CancelledTurn {
    error: omgcode_app::AppError,
    log: Vec<String>,
    handed: usize,
    /// From the stream hook's cancel until the turn returned.
    after_cancel: Duration,
    served: usize,
    has_active_turn: bool,
    events: Vec<Value>,
}

/// Run a streamed turn against a server that sends one piece and holds the
/// rest of the body on a gate nobody opens until the turn has returned. The
/// stream hook cancels the turn's token when it sees the first piece.
fn cancel_after_the_first_piece(tools_enabled: bool) -> CancelledTurn {
    let fixture = Fixture::new(tools_enabled);
    let mut journal = fixture.journal();
    let (release, gate) = common::gate();
    let server = common::MockServer::start(vec![common::Step::stream_held(
        common::sse_text_unfinished(&["one "]),
        gate,
        common::sse_text(&["second-piece "]),
    )]);
    let slot: RefCell<Option<CancelToken>> = RefCell::new(None);
    let cancelled_at: Cell<Option<Instant>> = Cell::new(None);
    let mut handed = 0_usize;
    let mut log = Vec::new();
    let mut on_cancel_token = |token: CancelToken| {
        handed += 1;
        *slot.borrow_mut() = Some(token);
    };
    let mut on_stream = |event: ChatStreamEvent<'_>| match event {
        ChatStreamEvent::RequestStarted => log.push("request_started".to_owned()),
        ChatStreamEvent::Text(text) => {
            log.push(format!("text:{text}"));
            if let Some(token) = slot.borrow().as_ref() {
                // The first cancel starts the measured interval.
                if cancelled_at.get().is_none() {
                    cancelled_at.set(Some(Instant::now()));
                }
                token.cancel();
            }
        }
    };

    let result = turn(
        &mut journal,
        &fixture.data,
        &settings(&server.endpoint),
        None,
        "say something",
        &mut ChatHooks {
            approval_wait: None,
            cancel: None,
            on_cancel_token: Some(&mut on_cancel_token),
            on_stream: Some(&mut on_stream),
        },
    );
    let returned = Instant::now();
    let after_cancel = returned.saturating_duration_since(
        cancelled_at
            .get()
            .expect("the stream hook cancelled the turn's token"),
    );
    // Let the server finish; it never saw the rest of its body read.
    drop(release);
    let served = server.finish();
    let has_active_turn = journal.snapshot().active_turn.is_some();
    CancelledTurn {
        error: result.expect_err("the cancelled turn fails"),
        log,
        handed,
        after_cancel,
        served,
        has_active_turn,
        events: close_and_read(journal),
    }
}

/// Whether the journal recorded any assistant message.
fn has_assistant_message(events: &[Value]) -> bool {
    events
        .iter()
        .any(|event| event["type"] == "message.recorded" && event["payload"]["role"] == "assistant")
}

#[test]
fn a_streamed_text_turn_cancelled_mid_stream_is_recorded_as_cancelled() {
    // Given a streamed text-only turn whose server holds the body after the
    // first piece, and a stream hook that cancels on that piece.
    // When the turn runs.
    let ended = cancel_after_the_first_piece(false);

    // Then it fails as cancelled, promptly, and the one piece stays delivered.
    assert_eq!(ended.error.code(), "cancelled");
    assert_eq!(ended.error.message(), "turn was cancelled");
    assert_eq!(ended.handed, 1);
    assert_eq!(ended.log, ["request_started", "text:one "]);
    assert!(
        ended.after_cancel < CANCEL_BOUND,
        "cancellation took {:?}",
        ended.after_cancel
    );

    // And the journal holds a cancelled failure, no reply, and no open turn.
    assert_eq!(
        last_failure_code(&ended.events).as_deref(),
        Some("cancelled")
    );
    assert!(!has_assistant_message(&ended.events));
    assert!(!ended.has_active_turn);
    assert!(!format!("{:?}", ended.events).contains("second-piece"));
}

#[test]
fn a_streamed_tool_turn_cancelled_in_its_first_request_runs_no_tool() {
    // Given a streamed tool-enabled turn held after its first piece.
    // When the stream hook cancels on that piece.
    let ended = cancel_after_the_first_piece(true);

    // Then the outcome is the same as on the text-only path, and no tool ran.
    assert_eq!(ended.error.code(), "cancelled");
    assert_eq!(ended.error.message(), "turn was cancelled");
    assert_eq!(ended.handed, 1);
    assert_eq!(ended.log, ["request_started", "text:one "]);
    assert_eq!(ended.served, 1);
    assert!(
        ended.after_cancel < CANCEL_BOUND,
        "cancellation took {:?}",
        ended.after_cancel
    );
    assert_eq!(
        last_failure_code(&ended.events).as_deref(),
        Some("cancelled")
    );
    assert!(!has_assistant_message(&ended.events));
    assert!(!ended.has_active_turn);
    assert!(
        ended
            .events
            .iter()
            .all(|event| !event["type"].as_str().unwrap_or("").starts_with("tool.")),
        "no tool was requested or run"
    );
}

#[test]
fn a_streamed_text_turn_that_is_not_cancelled_hands_over_a_live_token_and_completes() {
    // Given a streamed text-only turn and a hook that keeps the token.
    let fixture = Fixture::new(false);
    let mut journal = fixture.journal();
    let server =
        common::MockServer::start(vec![common::Step::stream(common::sse_text(&["Hel", "lo"]))]);
    let slot: RefCell<Option<CancelToken>> = RefCell::new(None);
    let mut handed = 0_usize;
    let mut log = Vec::new();
    let mut on_cancel_token = |token: CancelToken| {
        handed += 1;
        *slot.borrow_mut() = Some(token);
    };

    // When the turn runs.
    let output = {
        let mut on_stream = capture(&mut log);
        turn(
            &mut journal,
            &fixture.data,
            &settings(&server.endpoint),
            None,
            "greet me",
            &mut ChatHooks {
                approval_wait: None,
                cancel: None,
                on_cancel_token: Some(&mut on_cancel_token),
                on_stream: Some(&mut on_stream),
            },
        )
        .expect("streamed turn")
    };
    server.finish();

    // Then one token was handed over, it was never cancelled, and the turn
    // completed as it does without a hook.
    assert_eq!(handed, 1);
    assert!(
        slot.borrow()
            .as_ref()
            .is_some_and(|token| !token.is_cancelled())
    );
    assert_eq!(output.reply(), "Hello");
    assert_eq!(log, ["request_started", "text:Hel", "text:lo"]);
    assert_eq!(journal.snapshot().active_turn, None);
}

#[test]
fn a_cancel_seen_only_after_the_reply_arrived_still_ends_the_turn_as_cancelled() {
    // Given a streamed text-only turn whose whole reply arrives at once and
    // whose hook cancels on its piece.
    let fixture = Fixture::new(false);
    let mut journal = fixture.journal();
    let server = common::MockServer::start(vec![common::Step::stream(common::sse_text(&["done"]))]);
    let slot: RefCell<Option<CancelToken>> = RefCell::new(None);
    let mut on_cancel_token = |token: CancelToken| *slot.borrow_mut() = Some(token);
    let mut on_stream = |event: ChatStreamEvent<'_>| {
        if matches!(event, ChatStreamEvent::Text(_))
            && let Some(token) = slot.borrow().as_ref()
        {
            token.cancel();
        }
    };

    // When the turn runs.
    let error = turn(
        &mut journal,
        &fixture.data,
        &settings(&server.endpoint),
        None,
        "say it",
        &mut ChatHooks {
            approval_wait: None,
            cancel: None,
            on_cancel_token: Some(&mut on_cancel_token),
            on_stream: Some(&mut on_stream),
        },
    )
    .expect_err("the cancelled turn fails");
    server.finish();

    // Then the turn is cancelled whether the stream or the checkpoint saw the
    // token, and no reply was recorded.
    assert_eq!(error.code(), "cancelled");
    assert_eq!(journal.snapshot().active_turn, None);
    let events = close_and_read(journal);
    assert_eq!(last_failure_code(&events).as_deref(), Some("cancelled"));
    assert!(!has_assistant_message(&events));
}

// ---- Cancelling a non-streamed turn and its compaction (ADR-0051) ----

/// What a turn did when its token was cancelled while the server held the
/// reply of its first request.
struct HeldTurn {
    error: omgcode_app::AppError,
    /// From the helper thread's cancel until the turn returned.
    after_cancel: Duration,
    served: usize,
    has_active_turn: bool,
    events_before: usize,
    events: Vec<Value>,
}

/// Run a non-streamed turn whose token comes from the caller. The server
/// holds the reply of the first request, which is the compaction request when
/// `chat` compacts, on a gate nobody opens until the turn has returned; a
/// helper thread cancels the token once that request has arrived.
fn cancel_while_a_reply_is_held(
    tools_enabled: bool,
    chat: impl FnOnce(&str) -> ChatSettings,
    prepare: impl FnOnce(&mut SessionJournal),
) -> HeldTurn {
    let fixture = Fixture::new(tools_enabled);
    let mut journal = fixture.journal();
    prepare(&mut journal);
    let path = journal.path().to_owned();
    let events_before = close_and_read(journal).len();
    let mut journal = SessionJournal::open(&path).expect("reopen the session");
    let (release, gate) = common::gate();
    let (arrived, arrival) = mpsc::channel();
    let server = common::MockServer::start(vec![
        common::Step::gated(common::text("too late"), gate).announcing(arrived),
    ]);
    let cancel = CancelToken::new();
    let canceller = {
        let cancel = cancel.clone();
        thread::spawn(move || {
            common::wait_arrived(&arrival);
            let cancelled_at = Instant::now();
            cancel.cancel();
            cancelled_at
        })
    };
    let result = turn(
        &mut journal,
        &fixture.data,
        &chat(&server.endpoint),
        None,
        "hello",
        &mut ChatHooks {
            approval_wait: None,
            cancel: Some(cancel),
            on_cancel_token: None,
            on_stream: None,
        },
    );
    let returned = Instant::now();
    let cancelled_at = canceller.join().expect("canceller thread");
    let after_cancel = returned.saturating_duration_since(cancelled_at);
    // Let the server finish; its held reply finds the connection closed.
    drop(release);
    let served = server.finish();
    let has_active_turn = journal.snapshot().active_turn.is_some();
    HeldTurn {
        error: result.expect_err("the cancelled turn fails"),
        after_cancel,
        served,
        has_active_turn,
        events_before,
        events: close_and_read(journal),
    }
}

#[test]
fn a_non_streamed_text_turn_cancelled_while_waiting_is_recorded_as_cancelled() {
    // Given a non-streamed text-only turn whose server holds the reply.
    // When the token is cancelled once the request has arrived.
    let ended = cancel_while_a_reply_is_held(false, settings, |_| {});

    // Then the request stops far inside its 30-second deadline, and the
    // journal holds a cancelled failure, no reply, and no open turn.
    assert_eq!(ended.error.code(), "cancelled");
    assert_eq!(ended.error.message(), "turn was cancelled");
    assert!(
        ended.after_cancel < CANCEL_BOUND,
        "cancellation took {:?}",
        ended.after_cancel
    );
    assert_eq!(ended.served, 1);
    assert_eq!(
        last_failure_code(&ended.events).as_deref(),
        Some("cancelled")
    );
    assert!(!has_assistant_message(&ended.events));
    assert!(!ended.has_active_turn);
    assert!(!format!("{:?}", ended.events).contains("too late"));
}

#[test]
fn a_non_streamed_tool_turn_cancelled_while_waiting_runs_no_tool() {
    // Given a non-streamed tool-enabled turn whose server holds the reply.
    // When the token is cancelled once the request has arrived.
    let ended = cancel_while_a_reply_is_held(true, settings, |_| {});

    // Then the outcome is the one of the text-only path, and no tool ran.
    assert_eq!(ended.error.code(), "cancelled");
    assert_eq!(ended.error.message(), "turn was cancelled");
    assert!(
        ended.after_cancel < CANCEL_BOUND,
        "cancellation took {:?}",
        ended.after_cancel
    );
    assert_eq!(
        last_failure_code(&ended.events).as_deref(),
        Some("cancelled")
    );
    assert!(!has_assistant_message(&ended.events));
    assert!(!ended.has_active_turn);
    assert!(
        ended
            .events
            .iter()
            .all(|event| !event["type"].as_str().unwrap_or("").starts_with("tool.")),
        "no tool was requested or run"
    );
}

/// Two completed exchanges, so that the older one can be compacted.
fn two_exchanges(journal: &mut SessionJournal) {
    for index in 0..2 {
        let request_id = journal.start_turn().expect("turn");
        journal
            .record_message(MessageRole::User, &format!("question {index}"))
            .expect("user message");
        journal
            .record_message(MessageRole::Assistant, &format!("answer {index}"))
            .expect("assistant message");
        journal.complete_turn(request_id).expect("complete turn");
    }
}

/// Settings that compact before every turn.
fn compacting(endpoint: &str) -> ChatSettings {
    let mut chat = settings(endpoint);
    chat.compact_threshold = 1;
    chat.auto_compact = true;
    chat
}

#[test]
fn cancelling_the_compaction_before_a_turn_records_nothing() {
    // Given a session over the compaction trigger with auto-compaction, and a
    // server that holds the summarizer's reply.
    // When the token is cancelled once the summarizer request has arrived.
    let ended = cancel_while_a_reply_is_held(false, compacting, two_exchanges);

    // Then the summarizer request stops promptly, and neither a compaction nor
    // a turn was recorded: the journal is exactly as it was.
    assert_eq!(ended.error.code(), "cancelled");
    assert_eq!(
        ended.error.message(),
        "Compaction was cancelled; nothing was recorded"
    );
    assert!(
        ended.after_cancel < CANCEL_BOUND,
        "cancellation took {:?}",
        ended.after_cancel
    );
    assert_eq!(ended.served, 1);
    assert!(!ended.has_active_turn);
    assert_eq!(ended.events.len(), ended.events_before);
}

#[test]
fn a_token_set_before_the_turn_records_nothing_and_sends_nothing() {
    for tools_enabled in [false, true] {
        // Given a token that is already cancelled and a server that must not
        // be contacted.
        let fixture = Fixture::new(tools_enabled);
        let mut journal = fixture.journal();
        let server = common::NoCallServer::start();
        let cancel = CancelToken::new();
        cancel.cancel();
        let mut handed = 0_usize;
        let mut on_cancel_token = |token: CancelToken| {
            assert!(token.is_cancelled(), "the caller's token is handed over");
            handed += 1;
        };

        // When the turn runs.
        let error = turn(
            &mut journal,
            &fixture.data,
            &settings(&server.endpoint),
            None,
            "hello",
            &mut ChatHooks {
                approval_wait: None,
                cancel: Some(cancel),
                on_cancel_token: Some(&mut on_cancel_token),
                on_stream: None,
            },
        )
        .expect_err("a cancelled turn fails");

        // Then it is cancelled before it starts: no request and no turn.
        assert_eq!(error.code(), "cancelled", "tools: {tools_enabled}");
        assert_eq!(handed, 1);
        server.assert_untouched();
        assert!(
            close_and_read(journal)
                .iter()
                .all(|event| event["type"] != "turn.started"),
            "tools: {tools_enabled}"
        );
    }
}

#[test]
fn a_cancelled_compaction_through_the_command_path_records_nothing() {
    // Given a session with two exchanges and a server that holds the summary.
    let fixture = Fixture::new(false);
    let mut journal = fixture.journal();
    two_exchanges(&mut journal);
    let (release, gate) = common::gate();
    let (arrived, arrival) = mpsc::channel();
    let server = common::MockServer::start(vec![
        common::Step::gated(common::text("summary"), gate).announcing(arrived),
    ]);
    let client = LocalModelClient::new(
        LocalModelConfig {
            base_url: server.endpoint.clone(),
            model: "fixture".to_owned(),
            context_tokens: 131_072,
            max_output_tokens: 256,
        },
        None,
    )
    .expect("summary client");
    let cancel = CancelToken::new();
    let canceller = {
        let cancel = cancel.clone();
        thread::spawn(move || {
            common::wait_arrived(&arrival);
            let cancelled_at = Instant::now();
            cancel.cancel();
            cancelled_at
        })
    };

    // When the token is cancelled while the summarizer request waits.
    let error = compact_journal(
        &mut journal,
        &fixture.data,
        &client,
        None,
        ContextBudget {
            threshold: u64::MAX,
            context_tokens: 131_072,
            max_output_tokens: 256,
        },
        &cancel,
    )
    .expect_err("the cancelled compaction fails");
    let returned = Instant::now();
    let cancelled_at = canceller.join().expect("canceller thread");
    let after_cancel = returned.saturating_duration_since(cancelled_at);
    drop(release);
    server.finish();

    // Then it is cancelled promptly and no compaction was recorded.
    assert_eq!(error.code(), "cancelled");
    assert!(
        after_cancel < CANCEL_BOUND,
        "cancellation took {after_cancel:?}"
    );
    assert!(journal.snapshot().compaction.is_none());
    assert!(
        close_and_read(journal)
            .iter()
            .all(|event| event["type"] != "context.compacted"),
        "no compaction event"
    );
}
