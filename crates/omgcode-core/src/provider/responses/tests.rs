use super::*;
use crate::prompt::{AnsweredCall, BASE_PROMPT};
use crate::provider::{AccessTokenSource, LocalModelClient, LocalModelConfig};
use crate::test_support::await_client_close;
use crate::turn::{CancelToken, tool_definitions};
use omgcode_platform::credentials::Zeroizing;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

fn messages() -> Vec<ModelMessage> {
    vec![
        ModelMessage {
            role: ModelRole::System,
            content: BASE_PROMPT.to_owned(),
        },
        ModelMessage {
            role: ModelRole::User,
            content: "List the files.".to_owned(),
        },
        ModelMessage {
            role: ModelRole::Assistant,
            content: "Looking.".to_owned(),
        },
    ]
}

/// Feed `stream` one byte at a time, so every rule holds for any chunking.
fn read(stream: &str, mode: ResponsesMode) -> (Result<ResponsesOutcome, ProviderError>, String) {
    let mut parser = ResponsesParser::new(mode);
    let mut shown = String::new();
    for byte in stream.as_bytes() {
        match parser.push(std::slice::from_ref(byte), &mut |text| shown.push_str(text)) {
            Ok(ResponsesFlow::Continue) => {}
            Ok(ResponsesFlow::Completed) => return (parser.finish(), shown),
            Err(error) => return (Err(error), shown),
        }
    }
    (parser.finish(), shown)
}

fn event(payload: &str) -> String {
    let kind = payload
        .split("\"type\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or("message");
    format!("event: {kind}\ndata: {payload}\n\n")
}

const CREATED: &str = r#"{"type":"response.created","sequence_number":0,"response":{"id":"resp_1","status":"in_progress","output":[]}}"#;
const TEXT_DELTA_1: &str = r#"{"type":"response.output_text.delta","sequence_number":3,"item_id":"msg_1","output_index":1,"content_index":0,"delta":"Hello, "}"#;
const TEXT_DELTA_2: &str = r#"{"type":"response.output_text.delta","sequence_number":4,"item_id":"msg_1","output_index":1,"content_index":0,"delta":"world!"}"#;
const REASONING_DONE: &str = r#"{"type":"response.output_item.done","sequence_number":2,"output_index":0,"item":{"type":"reasoning","id":"rs_1","summary":[]}}"#;
const MESSAGE_DONE: &str = r#"{"type":"response.output_item.done","sequence_number":5,"output_index":1,"item":{"type":"message","id":"msg_1","status":"completed","role":"assistant","content":[{"type":"output_text","text":"Hello, world!","annotations":[]}]}}"#;
const COMPLETED: &str = r#"{"type":"response.completed","sequence_number":6,"response":{"id":"resp_1","status":"completed","usage":{"input_tokens":21,"output_tokens":5,"total_tokens":26}}}"#;
const CALL_DONE: &str = r#"{"type":"response.output_item.done","sequence_number":5,"output_index":1,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","namespace":"omgcode","name":"workspace_list_directory","arguments":"{\"path\":\"src\"}","status":"completed"}}"#;

fn text_stream() -> String {
    [
        CREATED,
        REASONING_DONE,
        TEXT_DELTA_1,
        TEXT_DELTA_2,
        MESSAGE_DONE,
        COMPLETED,
    ]
    .iter()
    .map(|payload| event(payload))
    .collect()
}

#[test]
fn a_completed_text_response_returns_the_finished_message_and_usage() {
    let (outcome, shown) = read(&text_stream(), ResponsesMode::Text);
    let outcome = outcome.expect("completed");
    assert_eq!(shown, "Hello, world!");
    assert_eq!(outcome.content, "Hello, world!");
    assert_eq!(outcome.finish_reason, "stop");
    assert!(outcome.tool_calls.is_empty());
    assert_eq!(
        outcome.usage,
        Some(Usage {
            prompt_tokens: Some(21),
            completion_tokens: Some(5),
            total_tokens: Some(26),
        })
    );
}

#[test]
fn a_namespaced_function_call_becomes_a_chat_completions_call() {
    let stream: String = [CREATED, CALL_DONE, COMPLETED]
        .iter()
        .map(|payload| event(payload))
        .collect();
    let (outcome, _) = read(&stream, ResponsesMode::Tools);
    let outcome = outcome.expect("completed");
    assert_eq!(outcome.finish_reason, "tool_calls");
    assert_eq!(
        outcome.tool_calls,
        vec![serde_json::json!({
            "id": "call_1",
            "type": "function",
            "function": {"name": "workspace_list_directory", "arguments": "{\"path\":\"src\"}"}
        })]
    );
    // The converted call passes the runtime's strict parser unchanged.
    assert!(crate::turn::ToolCall::parse(&outcome.tool_calls[0]).is_ok());

    let (text_only, _) = read(&stream, ResponsesMode::Text);
    assert_eq!(text_only, Err(ProviderError::UnsupportedToolCall));
}

#[test]
fn a_call_outside_the_namespace_or_a_hosted_tool_call_is_refused() {
    for item in [
        r#"{"type":"function_call","call_id":"c","namespace":"other","name":"workspace_read_file","arguments":"{}"}"#,
        r#"{"type":"web_search_call","id":"ws_1","status":"completed"}"#,
    ] {
        let done =
            format!(r#"{{"type":"response.output_item.done","output_index":0,"item":{item}}}"#);
        let stream = event(&done) + &event(COMPLETED);
        assert_eq!(
            read(&stream, ResponsesMode::Tools).0,
            Err(ProviderError::UnsupportedToolCall),
            "{item}"
        );
    }
    for item in [
        r#"{"type":"function_call","call_id":"","name":"x","arguments":"{}"}"#,
        r#"{"type":"function_call","call_id":"c","name":"x"}"#,
        // An object in place of the arguments string ends the turn, as the
        // runtime parser's `UnsupportedShape` does for Chat Completions.
        r#"{"type":"function_call","call_id":"c","name":"workspace_read_file","arguments":{"path":"notes.txt"}}"#,
        r#"{"type":"message","role":"user","content":[]}"#,
        r#"{"type":"message","role":"assistant","content":[{"type":"input_text","text":"x"}]}"#,
        r#"{"type":"image"}"#,
    ] {
        let done =
            format!(r#"{{"type":"response.output_item.done","output_index":0,"item":{item}}}"#);
        let stream = event(&done) + &event(COMPLETED);
        assert_eq!(
            read(&stream, ResponsesMode::Tools).0,
            Err(ProviderError::MalformedResponse),
            "{item}"
        );
    }
}

/// A usage limit can arrive after text has streamed; the text stays
/// delivered and provisional, and no reply exists.
#[test]
fn a_usage_limit_after_streamed_text_fails_with_its_code() {
    let failed = r#"{"type":"response.failed","response":{"status":"failed","error":{"code":"subscription_sharing_usage_limit_exceeded","message":"limit"}}}"#;
    let stream = event(CREATED) + &event(TEXT_DELTA_1) + &event(failed);
    let (outcome, shown) = read(&stream, ResponsesMode::Text);
    assert_eq!(
        outcome,
        Err(ProviderError::ResponseFailed(
            ResponseFailure::UsageLimitExceeded
        ))
    );
    assert_eq!(shown, "Hello, ");
    assert_eq!(
        ProviderError::ResponseFailed(ResponseFailure::UsageLimitExceeded).to_string(),
        "ChatGPT plan request failed: usage_limit_exceeded"
    );
}

#[test]
fn failed_incomplete_and_error_events_end_the_request() {
    let cases = [
        (
            r#"{"type":"response.failed","response":{"status":"failed","error":{"code":"server_error"}}}"#,
            ResponseFailure::Failed,
        ),
        (
            r#"{"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}}"#,
            ResponseFailure::Incomplete,
        ),
        (
            r#"{"type":"error","code":"subscription_sharing_unsupported_capability","message":"x","param":"tools"}"#,
            ResponseFailure::UnsupportedCapability,
        ),
        (
            r#"{"type":"error","error":{"code":"chatpass_v2_scope_not_authorized"}}"#,
            ResponseFailure::NotAuthorized,
        ),
        (
            r#"{"type":"error","message":"no code"}"#,
            ResponseFailure::Failed,
        ),
    ];
    for (payload, failure) in cases {
        let stream = event(CREATED) + &event(payload) + &event(COMPLETED);
        assert_eq!(
            read(&stream, ResponsesMode::Text).0,
            Err(ProviderError::ResponseFailed(failure)),
            "{payload}"
        );
    }
}

#[test]
fn a_stream_that_ends_or_says_done_before_completion_is_incomplete() {
    let stream = event(CREATED) + &event(TEXT_DELTA_1) + &event(MESSAGE_DONE);
    assert_eq!(
        read(&stream, ResponsesMode::Text).0,
        Err(ProviderError::IncompleteStream)
    );
    let stream = event(CREATED) + "data: [DONE]\n\n";
    assert_eq!(
        read(&stream, ResponsesMode::Text).0,
        Err(ProviderError::IncompleteStream)
    );
}

#[test]
fn an_event_is_dispatched_at_its_blank_line_with_its_data_lines_joined() {
    // One event whose JSON spans two `data:` lines.
    let split = "event: response.completed\ndata: {\"type\":\"response.completed\",\ndata: \"response\":{\"status\":\"completed\"}}\n\n";
    let stream = event(CREATED) + &event(MESSAGE_DONE) + split;
    let outcome = read(&stream, ResponsesMode::Text).0.expect("completed");
    assert_eq!(outcome.content, "Hello, world!");
    // An empty first `data:` line still adds its LF, so the payload is
    // "\n[DONE]": not the done marker, and not valid JSON either.
    let stream = event(CREATED) + "data:\ndata: [DONE]\n\n";
    assert_eq!(
        read(&stream, ResponsesMode::Text).0,
        Err(ProviderError::MalformedResponse)
    );
    // Without the blank line the event is never dispatched.
    let unfinished = event(CREATED) + &event(MESSAGE_DONE) + "data: " + COMPLETED + "\n";
    assert_eq!(
        read(&unfinished, ResponsesMode::Text).0,
        Err(ProviderError::IncompleteStream)
    );
}

#[test]
fn a_completed_output_must_match_the_finished_items() {
    let listing = |output: &str| {
        format!(
            r#"{{"type":"response.completed","response":{{"status":"completed","output":{output}}}}}"#
        )
    };
    let reasoning = r#"{"type":"reasoning","id":"rs_1","summary":[]}"#;
    let message = |id: &str, text: &str| {
        format!(
            r#"{{"type":"message","id":"{id}","status":"completed","role":"assistant","content":[{{"type":"output_text","text":"{text}","annotations":[]}}]}}"#
        )
    };
    let same = message("msg_1", "Hello, world!");
    let finished = event(CREATED) + &event(REASONING_DONE) + &event(MESSAGE_DONE);
    // The same items, an empty list, or none at all are accepted.
    for output in [format!("[{reasoning},{same}]"), "[]".into(), "null".into()] {
        let stream = finished.clone() + &event(&listing(&output));
        let outcome = read(&stream, ResponsesMode::Text).0;
        assert_eq!(outcome.expect(&output).content, "Hello, world!");
    }
    // A listed item that never finished, another type, another identifier,
    // other text, or an item listed without its identifier is refused.
    for output in [
        format!("[{reasoning},{same},{same}]"),
        format!(r#"[{reasoning},{{"type":"function_call","id":"msg_1"}}]"#),
        format!("[{reasoning},{}]", message("msg_2", "Hello, world!")),
        format!("[{reasoning},{}]", message("msg_1", "Goodbye.")),
        r#"[{"type":"reasoning"},{"type":"message"}]"#.into(),
        same.clone(),
    ] {
        let stream = finished.clone() + &event(&listing(&output));
        assert_eq!(
            read(&stream, ResponsesMode::Text).0,
            Err(ProviderError::MalformedResponse),
            "{output}"
        );
    }
    // A finished item at another index than its listed one is refused.
    let stream = event(CREATED) + &event(MESSAGE_DONE) + &event(&listing(&format!("[{same}]")));
    assert_eq!(
        read(&stream, ResponsesMode::Text).0,
        Err(ProviderError::MalformedResponse)
    );
    // A call listed with other arguments is refused; the same call passes.
    let call = |arguments: &str| {
        format!(
            r#"[{reasoning},{{"type":"function_call","id":"fc_1","call_id":"call_1","namespace":"omgcode","name":"workspace_list_directory","arguments":"{arguments}","status":"completed"}}]"#
        )
    };
    let calls = event(CREATED) + &event(REASONING_DONE) + &event(CALL_DONE);
    let stream = calls.clone() + &event(&listing(&call(r#"{\"path\":\"src\"}"#)));
    assert_eq!(
        read(&stream, ResponsesMode::Tools)
            .0
            .expect("the same call")
            .tool_calls
            .len(),
        1
    );
    let stream = calls + &event(&listing(&call(r#"{\"path\":\"..\"}"#)));
    assert_eq!(
        read(&stream, ResponsesMode::Tools).0,
        Err(ProviderError::MalformedResponse)
    );
}

#[test]
fn a_malformed_line_or_event_is_refused() {
    for stream in [
        "data: {\"type\":\"response.created\",\"type\":\"x\"}\n\n".to_owned(),
        "data: {\"sequence_number\":1}\n\n".to_owned(),
        "data: [1]\n\n".to_owned(),
        "comment: x\n\n".to_owned(),
        event(r#"{"type":"response.output_text.delta","delta":7}"#),
        event(r#"{"type":"response.output_item.done","item":{"type":"reasoning"}}"#),
        event(r#"{"type":"response.completed","response":{"status":"failed"}}"#),
        event(
            r#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":-1}}}"#,
        ),
        event(REASONING_DONE) + &event(REASONING_DONE),
    ] {
        assert_eq!(
            read(&stream, ResponsesMode::Text).0,
            Err(ProviderError::MalformedResponse),
            "{stream}"
        );
    }
}

#[test]
fn bytes_after_completion_are_not_read() {
    let stream = text_stream() + "garbage that is not an event\n";
    let (outcome, _) = read(&stream, ResponsesMode::Text);
    assert!(outcome.is_ok());
}

#[test]
fn a_stream_over_the_byte_limit_is_refused() {
    let mut parser = ResponsesParser::new(ResponsesMode::Text);
    let comment = vec![b':'; 1024 * 1024];
    let mut result = Ok(ResponsesFlow::Continue);
    for _ in 0..9 {
        result = parser.push(&comment, &mut |_| {});
        if result.is_err() {
            break;
        }
    }
    assert_eq!(result, Err(ProviderError::ResponseTooLarge));
}

#[test]
fn the_request_body_uses_instructions_input_items_and_one_namespace() {
    let tools = tool_definitions();
    let body = request_body("gpt-test", &messages(), Some(&tools)).expect("body");
    let value: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(value["model"], "gpt-test");
    assert_eq!(value["instructions"], BASE_PROMPT);
    assert_eq!(value["store"], false);
    assert_eq!(value["stream"], true);
    assert_eq!(value["tool_choice"], "auto");
    assert_eq!(
        value["input"],
        serde_json::json!([
            {"type": "message", "role": "user", "content": "List the files."},
            {"type": "message", "role": "assistant", "content": "Looking."}
        ])
    );
    let namespace = &value["tools"][0];
    assert_eq!(value["tools"].as_array().map(Vec::len), Some(1));
    assert_eq!(namespace["type"], "namespace");
    assert_eq!(namespace["name"], TOOL_NAMESPACE);
    let functions = namespace["tools"].as_array().expect("functions");
    assert_eq!(functions.len(), tools.len());
    assert_eq!(functions[0]["type"], "function");
    assert_eq!(functions[0]["name"], tools[0].name);
    assert_eq!(functions[0]["strict"], false);
    // Fields the plan route refuses are never sent.
    for field in [
        "max_output_tokens",
        "temperature",
        "top_p",
        "previous_response_id",
        "metadata",
        "user",
        "truncation",
        "include",
        "reasoning",
    ] {
        assert!(value.get(field).is_none(), "{field}");
    }

    let text = request_body("gpt-test", &messages(), None).expect("text body");
    let value: Value = serde_json::from_slice(&text).expect("json");
    assert!(value.get("tools").is_none());
    assert!(value.get("tool_choice").is_none());

    let mut second_system = messages();
    second_system.push(ModelMessage {
        role: ModelRole::System,
        content: "late".to_owned(),
    });
    assert_eq!(
        request_body("gpt-test", &second_system, None),
        Err(ProviderError::InvalidConfiguration)
    );
}

/// A tool turn after one call: the request, the model's interim text, and the
/// source-labelled result of `list-1` (ADR-0097).
fn tool_turn_messages(arguments: &str, result: &str) -> Vec<ModelMessage> {
    let mut messages = messages();
    messages.push(ModelMessage {
        role: ModelRole::ToolResult(AnsweredCall::new(
            "call_list_1",
            "workspace_list_directory",
            arguments,
        )),
        content: result.to_owned(),
    });
    messages
}

#[test]
fn with_tools_a_tool_result_is_sent_as_its_call_and_output_items() {
    let tools = tool_definitions();
    let body = request_body(
        "gpt-test",
        &tool_turn_messages("{\"path\":\".\"}", "Tool result [call call_list_1]:\n[]"),
        Some(&tools),
    )
    .expect("body");
    let value: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        value["input"],
        serde_json::json!([
            {"type": "message", "role": "user", "content": "List the files."},
            {"type": "message", "role": "assistant", "content": "Looking."},
            {
                "type": "function_call",
                "call_id": "call_list_1",
                "namespace": TOOL_NAMESPACE,
                "name": "workspace_list_directory",
                "arguments": "{\"path\":\".\"}"
            },
            {
                "type": "function_call_output",
                "call_id": "call_list_1",
                "output": "Tool result [call call_list_1]:\n[]"
            }
        ])
    );
}

#[test]
fn without_tools_a_tool_result_stays_a_labelled_user_message() {
    // No call item may name a function the request does not advertise.
    let body = request_body(
        "gpt-test",
        &tool_turn_messages("{\"path\":\".\"}", "Tool result [call call_list_1]:\n[]"),
        None,
    )
    .expect("body");
    let value: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        value["input"][2],
        serde_json::json!({
            "type": "message",
            "role": "user",
            "content": "Tool result [call call_list_1]:\n[]"
        })
    );
    assert_eq!(value["input"].as_array().map(Vec::len), Some(3));
}

#[test]
fn a_tool_results_size_estimate_covers_its_encoded_items() {
    // The encoder sends `call_id` in both items and escapes the argument
    // JSON again, so quotes, backslashes, and newlines double in the body.
    let tools = tool_definitions();
    let arguments = serde_json::json!({
        "path": "notes.txt",
        "old_text": "\"quoted\"\n".repeat(500),
        "new_text": "\\".repeat(2_000),
    })
    .to_string();
    let content = "Tool result [call call_list_1]:\nok";
    let with = tool_turn_messages(&arguments, content);
    let without = messages();
    let added_body = request_body("gpt-test", &with, Some(&tools))
        .expect("body")
        .len()
        - request_body("gpt-test", &without, Some(&tools))
            .expect("body")
            .len();
    let shared = |messages: &[ModelMessage]| {
        crate::provider::estimate_serialized_request_size(messages, "gpt-test").expect("estimate")
    };
    let added_estimate =
        shared(&with) - shared(&without) + answered_call_bytes(&with).expect("call bytes");
    assert!(
        added_estimate >= u64::try_from(added_body).expect("size"),
        "estimated {added_estimate} bytes for {added_body} encoded"
    );
}

#[test]
fn a_tool_results_call_counts_against_the_context_estimate() {
    // The result text fits; the echoed arguments push the request past the
    // context. No token is asked for and nothing is sent.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}/v1", listener.local_addr().expect("addr"));
    drop(listener);
    let tokens = token(Ok("t"));
    let client = client(&base, Arc::clone(&tokens));
    let arguments = format!("{{\"path\":\"{}\"}}", "a".repeat(120_000));
    let messages = tool_turn_messages(&arguments, &"x".repeat(20_000));
    assert_eq!(
        client
            .complete_with_tools(&messages, &tool_definitions(), &CancelToken::new())
            .map(|reply| reply.content),
        Err(ProviderError::ContextLimit)
    );
    assert_eq!(tokens.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn only_a_documented_code_in_an_error_body_is_used() {
    assert_eq!(
        failure_in_error_body(
            br#"{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"x"}}"#
        ),
        Some(ResponseFailure::UsageLimitExceeded)
    );
    assert_eq!(failure_in_error_body(br#"{"detail":"no"}"#), None);
    assert_eq!(
        failure_in_error_body(br#"{"error":{"code":"rate_limit_exceeded"}}"#),
        None
    );
    assert_eq!(failure_in_error_body(b"not json"), None);
}

/// A token source that counts how often it is asked.
struct FixedToken {
    calls: AtomicUsize,
    result: Result<&'static str, ProviderError>,
}

impl AccessTokenSource for FixedToken {
    fn access_token(&self, cancel: &CancelToken) -> Result<Zeroizing<String>, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        self.result.map(|token| Zeroizing::new(token.to_owned()))
    }
}

fn token(result: Result<&'static str, ProviderError>) -> Arc<FixedToken> {
    Arc::new(FixedToken {
        calls: AtomicUsize::new(0),
        result,
    })
}

fn client(base_url: &str, tokens: Arc<FixedToken>) -> LocalModelClient {
    LocalModelClient::responses_with_timeout(
        LocalModelConfig {
            base_url: base_url.to_owned(),
            model: "gpt-test".to_owned(),
            context_tokens: 131_072,
            max_output_tokens: 1024,
        },
        tokens,
        Duration::from_secs(10),
    )
    .expect("client")
}

/// Serve one response and record the request head and body. `None` sends no
/// `Content-Type` header at all, as OpenAI's plan route does (ADR-0096).
fn serve_once(
    status: &'static str,
    content_type: Option<&'static str>,
    body: String,
) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}/v1", listener.local_addr().expect("addr"));
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        let request = read_request(&mut stream);
        let content_type = content_type
            .map(|media_type| format!("Content-Type: {media_type}\r\n"))
            .unwrap_or_default();
        let reply = format!(
            "HTTP/1.1 {status}\r\n{content_type}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(reply.as_bytes()).expect("reply");
        await_client_close(&mut stream);
        request
    });
    (base, handle)
}

/// Read one request: the head, then a `Content-Length` body.
fn read_request(stream: &mut impl Read) -> String {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let count = stream.read(&mut chunk).expect("read");
        assert!(count > 0, "request ended early");
        bytes.extend_from_slice(&chunk[..count]);
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let length = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().expect("length"))
                })
                .unwrap_or(0);
            if body.len() >= length {
                return text;
            }
        }
    }
}

#[test]
fn the_client_posts_to_responses_with_the_sources_bearer_token() {
    let (base, server) = serve_once("200 OK", Some("text/event-stream"), text_stream());
    let tokens = token(Ok("test-access-token"));
    let client = client(&base, Arc::clone(&tokens));
    let mut shown = String::new();
    let reply = client
        .complete_streaming(
            &messages(),
            &mut |text| shown.push_str(text),
            &CancelToken::new(),
        )
        .expect("reply");
    assert_eq!(reply.content, "Hello, world!");
    assert_eq!(shown, "Hello, world!");
    assert_eq!(tokens.calls.load(Ordering::SeqCst), 1);
    let request = server.join().expect("server");
    assert!(
        request.starts_with("POST /v1/responses HTTP/1.1\r\n"),
        "{request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer test-access-token\r\n"),
        "{request}"
    );
    assert!(request.contains("\"store\":false"));
    assert!(request.contains("\"stream\":true"));
    assert!(client.is_chatgpt());
}

#[test]
fn a_non_streaming_call_reads_the_stream_and_returns_tool_calls() {
    let stream: String = [CREATED, CALL_DONE, COMPLETED]
        .iter()
        .map(|payload| event(payload))
        .collect();
    let (base, server) = serve_once("200 OK", Some("text/event-stream"), stream);
    let client = client(&base, token(Ok("t")));
    let reply = client
        .complete_with_tools(&messages(), &tool_definitions(), &CancelToken::new())
        .expect("reply");
    assert_eq!(reply.finish_reason, "tool_calls");
    assert_eq!(reply.tool_calls.len(), 1);
    let request = server.join().expect("server");
    assert!(request.contains("\"type\":\"namespace\""), "{request}");
}

/// The events OpenAI's plan route sent for a short text reply on 2026-11-24,
/// in order and with their observed fields (ADR-0096). Identifiers are
/// shortened, the echoed request is left out of the lifecycle events, and the
/// moderation metadata and token attribution are cut down.
fn observed_plan_stream() -> String {
    const DELTA: &str = r#""item_id":"msg_1","output_index":0,"content_index":0"#;
    let delta = |sequence: u8, text: &str| {
        format!(
            r#"{{"type":"response.output_text.delta","sequence_number":{sequence},{DELTA},"delta":"{text}","logprobs":[],"safety_buffering":false,"obfuscation":"x"}}"#
        )
    };
    let message = r#"{"type":"message","id":"msg_1","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Hello, world!","annotations":[],"logprobs":[]}],"phase":"final_answer"}"#;
    [
        r#"{"type":"response.created","sequence_number":0,"response":{"id":"resp_1","object":"response","status":"in_progress","output":[],"usage":null}}"#.to_owned(),
        r#"{"type":"response.in_progress","sequence_number":1,"response":{"id":"resp_1","object":"response","status":"in_progress","output":[],"usage":null}}"#.to_owned(),
        r#"{"type":"response.output_item.added","sequence_number":2,"output_index":0,"item":{"type":"message","id":"msg_1","role":"assistant","status":"in_progress","content":[],"phase":"final_answer"}}"#.to_owned(),
        format!(r#"{{"type":"response.content_part.added","sequence_number":3,{DELTA},"part":{{"type":"output_text","text":"","annotations":[],"logprobs":[]}}}}"#),
        delta(4, "Hello"),
        delta(5, ","),
        delta(6, " world"),
        delta(7, "!"),
        r#"{"type":"response.metadata","sequence_number":8,"response_id":"resp_1","metadata":{"openai_chatgpt_moderation_metadata":{}}}"#.to_owned(),
        format!(r#"{{"type":"response.output_text.done","sequence_number":9,{DELTA},"text":"Hello, world!","logprobs":[]}}"#),
        format!(r#"{{"type":"response.content_part.done","sequence_number":10,{DELTA},"part":{{"type":"output_text","text":"Hello, world!","annotations":[],"logprobs":[]}}}}"#),
        format!(r#"{{"type":"response.output_item.done","sequence_number":11,"output_index":0,"item":{message}}}"#),
        format!(r#"{{"type":"response.completed","sequence_number":12,"response":{{"id":"resp_1","object":"response","status":"completed","output":[{message}],"usage":{{"input_tokens":1965,"input_tokens_details":{{"cached_tokens":0}},"output_tokens":8,"output_tokens_details":{{"reasoning_tokens":0}},"total_tokens":1973,"attribution":{{"items":{{}}}}}}}}}}"#),
    ]
    .iter()
    .map(|payload| event(payload))
    .collect()
}

#[test]
fn a_plan_stream_without_a_content_type_is_read() {
    // The plan route answers a streamed request with no `Content-Type` at
    // all; the head check must not refuse it before the strict reader runs.
    let (base, server) = serve_once("200 OK", None, observed_plan_stream());
    let client = client(&base, token(Ok("t")));
    let mut shown = String::new();
    let reply = client
        .complete_streaming(
            &messages(),
            &mut |text| shown.push_str(text),
            &CancelToken::new(),
        )
        .expect("reply");
    assert_eq!(reply.content, "Hello, world!");
    assert_eq!(shown, "Hello, world!");
    assert_eq!(
        reply.usage,
        Some(Usage {
            prompt_tokens: Some(1965),
            completion_tokens: Some(8),
            total_tokens: Some(1973),
        })
    );
    server.join().expect("server");
}

#[test]
fn a_plan_reply_that_names_another_content_type_is_refused() {
    for media_type in ["application/json", "text/plain; charset=utf-8", ""] {
        let (base, server) = serve_once("200 OK", Some(media_type), observed_plan_stream());
        let mut shown = String::new();
        let result = client(&base, token(Ok("t"))).complete_streaming(
            &messages(),
            &mut |text| shown.push_str(text),
            &CancelToken::new(),
        );
        assert_eq!(
            result,
            Err(ProviderError::MalformedResponse),
            "{media_type:?}"
        );
        assert!(shown.is_empty(), "{media_type:?}");
        server.join().expect("server");
    }
}

#[test]
fn a_plan_body_without_a_content_type_still_passes_the_strict_reader() {
    // Accepting an absent header does not accept an arbitrary body: a JSON
    // document or a page is not a stream that reaches `response.completed`.
    for (body, expected) in [
        (
            r#"{"id":"resp_1","status":"completed"}"#.to_owned(),
            ProviderError::IncompleteStream,
        ),
        (
            "<html>\n<body>\n".to_owned(),
            ProviderError::MalformedResponse,
        ),
    ] {
        let (base, server) = serve_once("200 OK", None, body);
        let result = client(&base, token(Ok("t"))).complete(&messages(), &CancelToken::new());
        assert_eq!(result, Err(expected));
        server.join().expect("server");
    }
}

#[test]
fn a_usage_limit_status_body_becomes_its_code_and_other_statuses_stay_statuses() {
    let (base, server) = serve_once(
        "429 Too Many Requests",
        Some("application/json"),
        r#"{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"m"}}"#
            .to_owned(),
    );
    let result = client(&base, token(Ok("t"))).complete(&messages(), &CancelToken::new());
    assert_eq!(
        result,
        Err(ProviderError::ResponseFailed(
            ResponseFailure::UsageLimitExceeded
        ))
    );
    server.join().expect("server");

    let (base, server) = serve_once(
        "401 Unauthorized",
        Some("application/json"),
        r#"{"detail":"Unauthorized"}"#.to_owned(),
    );
    let result = client(&base, token(Ok("t"))).complete(&messages(), &CancelToken::new());
    assert_eq!(result, Err(ProviderError::HttpStatus(401)));
    server.join().expect("server");
}

#[test]
fn a_token_failure_or_a_set_cancel_token_sends_nothing() {
    // No server listens here; a connection attempt would fail as Transport.
    let base = "http://127.0.0.1:9/v1";
    let tokens = token(Err(ProviderError::SignInRequired));
    let result = client(base, Arc::clone(&tokens)).complete(&messages(), &CancelToken::new());
    assert_eq!(result, Err(ProviderError::SignInRequired));
    assert_eq!(tokens.calls.load(Ordering::SeqCst), 1);

    let tokens = token(Ok("t"));
    let cancel = CancelToken::new();
    cancel.cancel();
    let result = client(base, Arc::clone(&tokens)).complete(&messages(), &cancel);
    assert_eq!(result, Err(ProviderError::Cancelled));
    assert_eq!(tokens.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn cancelling_a_stream_that_is_waiting_for_more_bytes_returns_promptly() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}/v1", listener.local_addr().expect("addr"));
    let release = Arc::new(Mutex::new(()));
    let held = release.lock().expect("lock");
    let server_release = Arc::clone(&release);
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        read_request(&mut stream);
        let first = event(CREATED) + &event(TEXT_DELTA_1);
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{first}\r\n",
            first.len()
        );
        stream.write_all(head.as_bytes()).expect("head");
        // Hold the stream open until the test is done.
        drop(server_release.lock());
    });
    let cancel = CancelToken::new();
    let client = client(&base, token(Ok("t")));
    let canceller = cancel.clone();
    let mut started = None;
    let result = client.complete_streaming(
        &messages(),
        &mut |_| {
            if started.is_none() {
                started = Some(Instant::now());
                canceller.cancel();
            }
        },
        &cancel,
    );
    assert_eq!(result, Err(ProviderError::Cancelled));
    let elapsed = started.expect("text arrived").elapsed();
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    drop(held);
    server.join().expect("server");
}

#[test]
fn the_public_constructor_accepts_only_the_openai_endpoint() {
    let config = |base_url: &str| LocalModelConfig {
        base_url: base_url.to_owned(),
        model: "gpt-test".to_owned(),
        context_tokens: 131_072,
        max_output_tokens: 1024,
    };
    let tokens: Arc<dyn AccessTokenSource> = token(Ok("t"));
    assert!(
        LocalModelClient::chatgpt(config("https://api.openai.com/v1"), Arc::clone(&tokens)).is_ok()
    );
    for refused in [
        "http://127.0.0.1:1234/v1",
        "https://api.deepseek.com",
        "https://api.openai.com/v2",
    ] {
        assert_eq!(
            LocalModelClient::chatgpt(config(refused), Arc::clone(&tokens)).err(),
            Some(ProviderError::InvalidConfiguration),
            "{refused}"
        );
    }
}

/// ADR-0099: a rejected call is echoed with `{}` and its recoverable result,
/// so the route still sees each call item paired with an output item.
#[test]
fn a_rejected_calls_placeholder_arguments_and_result_are_sent_as_a_paired_call_and_output() {
    let tools = tool_definitions();
    let mut messages = messages();
    messages.push(ModelMessage {
        role: ModelRole::ToolResult(AnsweredCall::new("call_big_1", "workspace_create_file", "{}")),
        content: "Tool error [call call_big_1; source core_observed; untrusted workspace data]:\n{\"error\":\"arguments_too_large\"}".to_owned(),
    });
    messages.push(ModelMessage {
        role: ModelRole::ToolResult(AnsweredCall::new("call_odd_1", "unsupported_tool", "{}")),
        content: "Tool error [call call_odd_1; source core_observed; untrusted workspace data]:\n{\"error\":\"unsupported_tool\"}".to_owned(),
    });

    let body = request_body("gpt-test", &messages, Some(&tools)).expect("body");

    let value: Value = serde_json::from_slice(&body).expect("json");
    let items = value["input"].as_array().expect("input items");
    let pairs: Vec<(&str, &str, &str)> = items
        .iter()
        .skip(2)
        .map(|item| {
            (
                item["type"].as_str().expect("type"),
                item["call_id"].as_str().expect("call id"),
                item.get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("(output)"),
            )
        })
        .collect();
    assert_eq!(
        pairs,
        [
            ("function_call", "call_big_1", "{}"),
            ("function_call_output", "call_big_1", "(output)"),
            ("function_call", "call_odd_1", "{}"),
            ("function_call_output", "call_odd_1", "(output)"),
        ]
    );
    assert_eq!(items[2]["name"], "workspace_create_file");
}

/// ADR-0099: ten answered 30 KiB create calls are echoed in one plan-route
/// request of about 600 KiB, which the context budget allows.
#[test]
fn a_plan_route_request_echoing_ten_large_calls_is_sent_when_the_context_budget_allows_it() {
    let stream: String = [CREATED, CALL_DONE, COMPLETED]
        .iter()
        .map(|payload| event(payload))
        .collect();
    let (base, server) = serve_once("200 OK", Some("text/event-stream"), stream);
    let client = LocalModelClient::responses_with_timeout(
        LocalModelConfig {
            base_url: base,
            model: "gpt-test".to_owned(),
            context_tokens: 1_000_000,
            max_output_tokens: 1024,
        },
        token(Ok("t")),
        Duration::from_secs(10),
    )
    .expect("client");
    let mut messages = messages();
    for index in 0..10 {
        let arguments =
            serde_json::json!({"path": format!("f{index}.rs"), "content": "a".repeat(30 * 1024)})
                .to_string();
        messages.push(ModelMessage {
            role: ModelRole::ToolResult(AnsweredCall::new(
                &format!("call_{index}"),
                "workspace_create_file",
                &arguments,
            )),
            content: format!("Tool result [call call_{index}]:\n{{\"status\":\"created\"}}"),
        });
    }

    let reply = client
        .complete_with_tools(&messages, &tool_definitions(), &CancelToken::new())
        .expect("a request over the old 256 KiB cap is sent");

    assert_eq!(reply.tool_calls.len(), 1);
    let request = server.join().expect("server");
    assert!(request.len() > 300 * 1024, "{} bytes", request.len());
    assert_eq!(
        request.matches("\"type\":\"function_call_output\"").count(),
        10
    );
}
