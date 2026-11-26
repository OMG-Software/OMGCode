use omgcode_core::prompt::{BASE_PROMPT, ModelMessage, ModelRole};
use omgcode_core::provider::{
    LocalModelClient, LocalModelConfig, ModelReply, ProviderError, Usage,
};
use omgcode_core::turn::{CancelToken, ToolCall, ToolCallError, ToolModelReply, tool_definitions};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Mutex, PoisonError, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const SUCCESS: &str = r#"{"choices":[{"message":{"role":"assistant","content":"A plain reply."},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":4,"total_tokens":16}}"#;
const MAX_REQUEST_HEADER_BYTES: usize = 16 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 5 * 1024 * 1024;

struct Fixture {
    address: String,
    task: JoinHandle<(String, Vec<u8>)>,
}

impl Fixture {
    /// Serve one bounded local exchange without depending on production retries.
    fn start(status: &str, body: &[u8], extra_headers: &str) -> Self {
        let listener = bind_fixture_listener();
        listener
            .set_nonblocking(true)
            .expect("make fixture accept bounded");
        let address = listener.local_addr().expect("fixture address");
        let body = body.to_vec();
        let extra_headers = extra_headers.to_owned();
        let status = status.to_owned();
        let task = thread::spawn(move || {
            let accept_deadline = Instant::now() + Duration::from_secs(5);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < accept_deadline, "fixture accept timed out");
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                }
            };
            stream
                .set_nonblocking(false)
                .expect("make accepted fixture stream blocking");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("set fixture read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .expect("set fixture write timeout");
            let (head, request_body) = read_request(&mut stream).expect("read bounded request");
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write fixture response headers");
            stream.write_all(&body).expect("write fixture body");
            assert!(
                await_client_close(&mut stream),
                "the client closes the fixture connection first"
            );
            (head, request_body)
        });
        Self {
            address: format!("http://{address}/v1"),
            task,
        }
    }

    /// Observe fixture completion before returning captured headers and body.
    fn finish(self) -> (String, Vec<u8>) {
        join_bounded(self.task, "fixture completes")
    }
}

/// Fail independently when a mock worker does not terminate within its budget.
fn join_bounded<T>(task: JoinHandle<T>, message: &str) -> T {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !task.is_finished() {
        assert!(Instant::now() < deadline, "{message} within the deadline");
        thread::sleep(Duration::from_millis(2));
    }
    task.join().expect(message)
}

/// Bind a loopback fixture listener whose port stays bound until the test
/// binary exits.
///
/// On Windows a new listener can receive a port that an earlier fixture used,
/// and the client can receive its earlier local port too. That recreates a
/// closed connection's addresses: a TIME_WAIT the client holds refuses the
/// connect, and one the fixture holds can delay it until the connect timeout
/// (ADR-0078). A port that stays bound is never handed to a later fixture.
/// The held handle never accepts, so a connection that arrives after the
/// fixture finished waits in the backlog until the client's own timeout.
fn bind_fixture_listener() -> TcpListener {
    static HELD: Mutex<Vec<TcpListener>> = Mutex::new(Vec::new());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local fixture");
    let held = listener.try_clone().expect("hold the fixture port");
    HELD.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(held);
    listener
}

/// Wait, within the stream's read timeout, until a client that received a
/// complete `Connection: close` reply closes its side, and only then let the
/// caller drop the stream.
///
/// The client then holds the connection's TIME_WAIT. On Windows the next
/// client can receive the same local port, and a TIME_WAIT that the server
/// holds after a large request delays that connect until it times out
/// (ADR-0078). Call this only after a reply framed by `Content-Length`, never
/// after a reply that the close delimits, and only on a stream with a read
/// timeout. A read error or timeout ends the wait; the caller still closes.
///
/// Returns whether the client's close, orderly or abortive, was observed.
/// Any other read error, including
/// the timeout or `WouldBlock` on a stream left non-blocking, returns `false`:
/// then the fixture, not the client, closes first. A shared fixture asserts
/// the result so a regression of the close order fails loudly.
fn await_client_close(stream: &mut impl Read) -> bool {
    let mut sink = [0_u8; 512];
    loop {
        match stream.read(&mut sink) {
            Ok(0) => return true,
            Ok(_) => {}
            // An abortive close, as a cancelled client makes, leaves no
            // TIME_WAIT on either side.
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ) =>
            {
                return true;
            }
            Err(_) => return false,
        }
    }
}

/// Validate fixture framing and cap allocations before reading a declared body.
fn read_request(stream: &mut TcpStream) -> std::io::Result<(String, Vec<u8>)> {
    let mut head = Vec::new();
    let mut window = Vec::with_capacity(4);
    while window.as_slice() != b"\r\n\r\n" {
        if head.len() >= MAX_REQUEST_HEADER_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "fixture request headers exceed their byte limit",
            ));
        }
        let mut byte = [0_u8; 1];
        stream.read_exact(&mut byte)?;
        head.push(byte[0]);
        if window.len() == 4 {
            window.remove(0);
        }
        window.push(byte[0]);
    }
    let head_text = String::from_utf8(head)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let content_length = head_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>())
        })
        .transpose()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "missing content length")
        })?;
    if content_length > MAX_REQUEST_BODY_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "fixture request body exceeds its byte limit",
        ));
    }
    let mut body = vec![0; content_length];
    stream.read_exact(&mut body)?;
    Ok((head_text, body))
}

fn messages() -> Vec<ModelMessage> {
    vec![
        ModelMessage {
            role: ModelRole::System,
            content: BASE_PROMPT.to_owned(),
        },
        ModelMessage {
            role: ModelRole::User,
            content: "Saved user preferences".to_owned(),
        },
        ModelMessage {
            role: ModelRole::Assistant,
            content: "Prior assistant text".to_owned(),
        },
        ModelMessage {
            role: ModelRole::User,
            content: "Last user request".to_owned(),
        },
    ]
}

fn client(address: String, token: Option<String>) -> LocalModelClient {
    LocalModelClient::new(
        LocalModelConfig {
            base_url: address,
            model: "already-loaded-model".to_owned(),
            context_tokens: 32_768,
            max_output_tokens: 512,
        },
        token,
    )
    .expect("valid local model client")
}

/// Observe the listening socket so rejection proves the prompt was not sent.
fn assert_prompt_rejected_before_connect(messages: &[ModelMessage]) {
    let listener = bind_fixture_listener();
    listener
        .set_nonblocking(true)
        .expect("make no-call fixture nonblocking");
    let address = format!(
        "http://{}/v1",
        listener.local_addr().expect("fixture address")
    );
    let model = client(address, None);
    let monitor = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_millis(250);
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .expect("make accepted fixture stream blocking");
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .expect("set fixture read timeout");
                    stream
                        .set_write_timeout(Some(Duration::from_secs(1)))
                        .expect("set fixture write timeout");
                    let (head, _) = read_request(&mut stream).expect("read bounded request");
                    stream
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                SUCCESS.len(),
                                SUCCESS
                            )
                            .as_bytes(),
                        )
                        .expect("reply to any unexpected request");
                    break Some(head);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        break None;
                    }
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("fixture accept failed: {error}"),
            }
        }
    });

    let result = model.complete(messages, &CancelToken::new());
    let unexpected_request = join_bounded(monitor, "bounded listener monitor completes");

    assert_eq!(result, Err(ProviderError::InvalidConfiguration));
    assert!(
        unexpected_request.is_none(),
        "invalid prompt must not reach server"
    );
}

#[test]
fn sends_prompt_roles_without_tools_and_parses_plain_reply() {
    let fixture = Fixture::start("200 OK", SUCCESS.as_bytes(), "");
    let model = client(
        fixture.address.clone(),
        Some("local-secret-token".to_owned()),
    );

    let reply = model
        .complete(&messages(), &CancelToken::new())
        .expect("valid completion");
    let (head, body) = fixture.finish();
    let request: serde_json::Value = serde_json::from_slice(&body).expect("request JSON");

    assert!(head.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
    assert!(
        head.to_ascii_lowercase()
            .contains("authorization: bearer local-secret-token")
    );
    assert_eq!(request["model"], "already-loaded-model");
    assert_eq!(request["stream"], false);
    assert_eq!(request["max_tokens"], 512);
    assert_eq!(request["messages"][0]["role"], "system");
    assert_eq!(request["messages"][0]["content"], BASE_PROMPT);
    assert_eq!(request["messages"][1]["role"], "user");
    assert_eq!(request["messages"][2]["role"], "assistant");
    assert_eq!(request["messages"][3]["content"], "Last user request");
    assert!(request.get("tools").is_none());
    assert!(request.get("tool_choice").is_none());
    assert_eq!(
        reply,
        ModelReply {
            content: "A plain reply.".to_owned(),
            finish_reason: "stop".to_owned(),
            usage: Some(Usage {
                prompt_tokens: Some(12),
                completion_tokens: Some(4),
                total_tokens: Some(16),
            }),
        }
    );
}

#[test]
fn does_not_leak_token_on_authentication_failure() {
    let fixture = Fixture::start("401 Unauthorized", b"server echoed local-secret-token", "");
    let model = client(
        fixture.address.clone(),
        Some("local-secret-token".to_owned()),
    );
    let error = model
        .complete(&messages(), &CancelToken::new())
        .expect_err("401 is an error");
    let (head, _) = fixture.finish();

    assert_eq!(error, ProviderError::HttpStatus(401));
    assert!(!error.to_string().contains("local-secret-token"));
    assert!(!format!("{error:?}").contains("local-secret-token"));
    assert!(
        head.to_ascii_lowercase()
            .contains("authorization: bearer local-secret-token")
    );
}

#[test]
fn rejects_invalid_response_schemas_and_tool_requests() {
    let malformed = Fixture::start("200 OK", br#"{"choices":[]}"#, "");
    let error = client(malformed.address.clone(), None)
        .complete(&messages(), &CancelToken::new())
        .expect_err("empty choices are invalid");
    malformed.finish();
    assert_eq!(error, ProviderError::MalformedResponse);

    let tool_call = Fixture::start(
        "200 OK",
        br#"{"choices":[{"message":{"role":"assistant","content":"maybe","tool_calls":[{"type":"function"}]},"finish_reason":"tool_calls"}]}"#,
        "",
    );
    let error = client(tool_call.address.clone(), None)
        .complete(&messages(), &CancelToken::new())
        .expect_err("tool requests are unsupported");
    tool_call.finish();
    assert_eq!(error, ProviderError::UnsupportedToolCall);
}

#[test]
fn accepts_empty_lm_studio_tool_call_array_as_no_action() {
    let response = br#"{"choices":[{"message":{"role":"assistant","content":"READY.","tool_calls":[],"reasoning_content":""},"finish_reason":"stop"}],"usage":{"prompt_tokens":893,"completion_tokens":3,"total_tokens":896}}"#;
    let fixture = Fixture::start("200 OK", response, "");
    let model = client(fixture.address.clone(), None);

    let result = model.complete(&messages(), &CancelToken::new());
    fixture.finish();

    assert_eq!(
        result,
        Ok(ModelReply {
            content: "READY.".to_owned(),
            finish_reason: "stop".to_owned(),
            usage: Some(Usage {
                prompt_tokens: Some(893),
                completion_tokens: Some(3),
                total_tokens: Some(896),
            }),
        })
    );
}

#[test]
fn rejects_oversized_response_body() {
    let fixture = Fixture::start("200 OK", &vec![b'x'; 1024 * 1024 + 1], "");
    let error = client(fixture.address.clone(), None)
        .complete(&messages(), &CancelToken::new())
        .expect_err("oversized response is rejected");
    fixture.finish();
    assert_eq!(error, ProviderError::ResponseTooLarge);
}

#[test]
fn null_action_fields_are_absent_but_actual_or_malformed_calls_fail() {
    for (fields, expected) in [
        (
            serde_json::json!({"tool_calls":null,"function_call":null}),
            None,
        ),
        (
            serde_json::json!({"tool_calls":[{}]}),
            Some(ProviderError::UnsupportedToolCall),
        ),
        (
            serde_json::json!({"function_call":{}}),
            Some(ProviderError::UnsupportedToolCall),
        ),
        (
            serde_json::json!({"tool_calls":"not an array"}),
            Some(ProviderError::MalformedResponse),
        ),
    ] {
        // Exercise both response locations, including unexpected choice fields.
        for at_choice in [false, true] {
            let mut response = serde_json::json!({"choices":[{
                "message":{"role":"assistant","content":"plain text"},
                "finish_reason":"stop"
            }]});
            let target = if at_choice {
                response["choices"][0]
                    .as_object_mut()
                    .expect("choice object")
            } else {
                response["choices"][0]["message"]
                    .as_object_mut()
                    .expect("message object")
            };
            target.extend(fields.as_object().expect("field object").clone());
            let encoded = serde_json::to_vec(&response).expect("fixture response JSON");
            let fixture = Fixture::start("200 OK", &encoded, "");
            let result =
                client(fixture.address.clone(), None).complete(&messages(), &CancelToken::new());
            fixture.finish();
            assert_eq!(result.err(), expected);
        }
    }
}

#[test]
fn does_not_follow_redirects() {
    let fixture = Fixture::start(
        "302 Found",
        b"",
        "Location: http://127.0.0.1:9/v1/chat/completions\r\n",
    );
    let error = client(fixture.address.clone(), None)
        .complete(&messages(), &CancelToken::new())
        .expect_err("redirect response is not followed");
    fixture.finish();
    assert_eq!(error, ProviderError::HttpStatus(302));
}

#[test]
fn rejects_destinations_outside_literal_loopback_http_before_connecting() {
    for base_url in [
        "http://localhost:1234/v1",
        "http://192.0.2.1:1234/v1",
        "http://127.0.0.1/v1",
        "http://127.0.0.1:0/v1",
        "http://127.0.0.1:1234/v1/other",
        "http://user@127.0.0.1:1234/v1",
        "http://127.0.0.1:1234/v1?query=1",
        "http://127.0.0.1:1234/v1#fragment",
        // HTTPS to this computer is neither a local endpoint nor a typed
        // hosted one: a local server uses a local kind (ADR-0092).
        "https://127.0.0.1:1234/v1",
        "https://localhost:1234/v1",
    ] {
        let result = LocalModelClient::new(
            LocalModelConfig {
                base_url: base_url.to_owned(),
                model: "model".to_owned(),
                context_tokens: 4096,
                max_output_tokens: 128,
            },
            None,
        );
        assert_eq!(
            result.err(),
            Some(ProviderError::InvalidConfiguration),
            "{base_url}"
        );
    }
}

#[test]
fn context_budget_fails_before_network_request() {
    let listener = bind_fixture_listener();
    listener
        .set_nonblocking(true)
        .expect("make no-call fixture nonblocking");
    let address = format!(
        "http://{}/v1",
        listener.local_addr().expect("fixture address")
    );
    let model = LocalModelClient::new(
        LocalModelConfig {
            base_url: address,
            model: "model".to_owned(),
            context_tokens: 1,
            max_output_tokens: 1,
        },
        None,
    )
    .expect("valid limits");

    assert_eq!(
        model.complete(&messages(), &CancelToken::new()),
        Err(ProviderError::ContextLimit)
    );
    assert!(
        listener.accept().is_err(),
        "budget rejection must not connect"
    );
}

#[test]
fn requires_exact_compiled_base_as_the_only_system_message() {
    let mut missing_base = messages();
    missing_base.remove(0);
    assert_prompt_rejected_before_connect(&missing_base);

    let mut changed_base = messages();
    changed_base[0].content.push(' ');
    assert_prompt_rejected_before_connect(&changed_base);

    let mut extra_system = messages();
    extra_system.push(ModelMessage {
        role: ModelRole::System,
        content: "Untrusted extra system message".to_owned(),
    });
    assert_prompt_rejected_before_connect(&extra_system);
}

#[test]
fn wire_size_limit_fails_before_network_request() {
    let listener = bind_fixture_listener();
    listener
        .set_nonblocking(true)
        .expect("make no-call fixture nonblocking");
    let address = format!(
        "http://{}/v1",
        listener.local_addr().expect("fixture address")
    );
    let model = LocalModelClient::new(
        LocalModelConfig {
            base_url: address,
            model: "model".to_owned(),
            context_tokens: 20_000_000,
            max_output_tokens: 1,
        },
        None,
    )
    .expect("valid limits");
    let oversized = [
        ModelMessage {
            role: ModelRole::System,
            content: BASE_PROMPT.to_owned(),
        },
        ModelMessage {
            role: ModelRole::User,
            content: "x".repeat(4 * 1024 * 1024 + 1),
        },
    ];

    assert_eq!(
        model.complete(&oversized, &CancelToken::new()),
        Err(ProviderError::RequestTooLarge)
    );
    assert!(
        listener.accept().is_err(),
        "size rejection must not connect"
    );
}

#[test]
fn rejects_control_characters_in_bearer_token() {
    let result = LocalModelClient::new(
        LocalModelConfig {
            base_url: "http://127.0.0.1:1234/v1".to_owned(),
            model: "model".to_owned(),
            context_tokens: 4096,
            max_output_tokens: 128,
        },
        Some("bad\r\ntoken".to_owned()),
    );

    assert_eq!(result.err(), Some(ProviderError::InvalidToken));
}

const TOOL_CALL_REPLY: &str = r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call-1","type":"function","function":{"name":"workspace_read_file","arguments":"{\"path\":\"notes.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#;

/// Send one tool-enabled request to a fixture serving `body`.
fn tool_exchange(body: &str) -> (Result<ToolModelReply, ProviderError>, Value) {
    let fixture = Fixture::start("200 OK", body.as_bytes(), "");
    let result = client(fixture.address.clone(), None).complete_with_tools(
        &messages(),
        &tool_definitions(),
        &CancelToken::new(),
    );
    let (_, request) = fixture.finish();
    (
        result,
        serde_json::from_slice(&request).expect("request JSON"),
    )
}

#[test]
fn a_length_limited_tool_mode_text_reply_keeps_its_finish_reason() {
    let (reply, _) = tool_exchange(
        r#"{"choices":[{"message":{"role":"assistant","content":"partial"},"finish_reason":"length"}]}"#,
    );
    let reply = reply.expect("length-limited text reply");
    assert!(reply.tool_calls.is_empty());
    assert_eq!(reply.content, "partial");
    assert_eq!(reply.finish_reason, "length");

    let (reply, _) = tool_exchange(TOOL_CALL_REPLY);
    assert_eq!(reply.expect("tool-call reply").finish_reason, "tool_calls");
}

#[test]
fn tool_requests_advertise_exact_function_tools_and_return_raw_calls() {
    let (reply, request) = tool_exchange(TOOL_CALL_REPLY);
    let reply = reply.expect("tool-call reply");

    assert_eq!(request["tool_choice"], "auto");
    assert_eq!(request["stream"], false);
    let tools = request["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| {
            assert_eq!(tool["type"], "function");
            tool["function"]["name"].as_str().expect("tool name")
        })
        .collect();
    assert_eq!(names, ["workspace_read_file", "workspace_list_directory"]);
    assert_eq!(
        tools[0]["function"]["parameters"]["additionalProperties"],
        false
    );
    let roles: Vec<&str> = request["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .map(|message| message["role"].as_str().expect("role"))
        .collect();
    assert_eq!(roles, ["system", "user", "assistant", "user"]);

    assert_eq!(reply.content, "");
    assert_eq!(reply.tool_calls.len(), 1);
    // The adapter returns calls unvalidated; the runtime parses them strictly.
    assert_eq!(reply.tool_calls[0]["id"], "call-1");
    assert!(ToolCall::parse(&reply.tool_calls[0]).is_ok());
}

/// ADR-0099: an object in place of the arguments string is a shape error on
/// every adapter. The buffered adapter returns the call unvalidated, and the
/// runtime parser refuses it as `UnsupportedShape`, which ends the turn, as
/// the Responses adapter's `MalformedResponse` does.
#[test]
fn object_valued_arguments_reach_the_runtime_as_an_unsupported_shape() {
    let (reply, _) = tool_exchange(
        r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call-1","type":"function","function":{"name":"workspace_read_file","arguments":{"path":"notes.txt"}}}]},"finish_reason":"tool_calls"}]}"#,
    );
    let reply = reply.expect("the adapter returns calls unvalidated");
    assert_eq!(
        reply.tool_calls[0]["function"]["arguments"],
        serde_json::json!({"path": "notes.txt"})
    );
    assert_eq!(
        ToolCall::parse(&reply.tool_calls[0]).err(),
        Some(ToolCallError::UnsupportedShape)
    );
}

#[test]
fn tool_requests_accept_plain_text_and_empty_call_lists() {
    for body in [
        SUCCESS,
        r#"{"choices":[{"message":{"role":"assistant","content":"done","tool_calls":[]},"finish_reason":"stop"}]}"#,
    ] {
        let (reply, _) = tool_exchange(body);
        let reply = reply.expect("text reply");
        assert!(reply.tool_calls.is_empty());
        assert!(!reply.content.is_empty());
    }
}

#[test]
fn tool_responses_with_duplicate_keys_or_unsupported_shapes_are_refused() {
    let many_calls = format!(
        r#"{{"choices":[{{"message":{{"role":"assistant","content":null,"tool_calls":[{}]}},"finish_reason":"tool_calls"}}]}}"#,
        vec![r#"{"id":"x","type":"function","function":{"name":"workspace_list_directory","arguments":"{}"}}"#; 65].join(",")
    );
    let cases = [
        // Duplicate keys could smuggle a second tool name or argument string.
        r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"a","type":"function","function":{"name":"workspace_read_file","name":"workspace_delete","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
        r#"{"choices":[{"message":{"role":"assistant","role":"system","content":"x"},"finish_reason":"stop"}]}"#,
        // A non-assistant role is refused.
        r#"{"choices":[{"message":{"role":"system","content":"x"},"finish_reason":"stop"}]}"#,
        // Null content needs at least one tool call.
        r#"{"choices":[{"message":{"role":"assistant","content":null},"finish_reason":"stop"}]}"#,
        // Truncated tool-call output is never dispatched.
        r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"a","type":"function","function":{"name":"workspace_read_file","arguments":"{\"path\":"}}]},"finish_reason":"length"}]}"#,
        r#"{"choices":[{"message":{"role":"assistant","content":"x","tool_calls":{}},"finish_reason":"stop"}]}"#,
        r#"{"choices":[]}"#,
        &many_calls,
    ];
    for body in cases {
        let (reply, _) = tool_exchange(body);
        assert_eq!(
            reply.expect_err("refused tool response"),
            ProviderError::MalformedResponse,
            "{body}"
        );
    }

    for body in [
        r#"{"choices":[{"message":{"role":"assistant","content":null,"function_call":{"name":"workspace_read_file","arguments":"{}"}},"finish_reason":"function_call"}]}"#,
        r#"{"choices":[{"message":{"role":"assistant","content":"x"},"tool_calls":[{"id":"a"}],"finish_reason":"stop"}]}"#,
    ] {
        let (reply, _) = tool_exchange(body);
        assert_eq!(
            reply.expect_err("legacy or misplaced call"),
            ProviderError::UnsupportedToolCall,
            "{body}"
        );
    }
}

const EVENT_STREAM: &str = "Content-Type: text/event-stream\r\n";

fn text_event(content: &str) -> String {
    format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"index":0,"delta":{"content":content},"finish_reason":null}]})
    )
}

fn finish_event(reason: &str) -> String {
    format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":reason}]})
    )
}

fn text_stream(pieces: &[&str]) -> String {
    let mut stream: String = pieces.iter().map(|piece| text_event(piece)).collect();
    stream.push_str(&finish_event("stop"));
    stream.push_str("data: [DONE]\n\n");
    stream
}

/// The outcome of one streamed text exchange.
struct StreamExchange {
    result: Result<ModelReply, ProviderError>,
    pieces: Vec<String>,
    request: Value,
    head: String,
}

fn stream_exchange(status: &str, body: &str, headers: &str) -> StreamExchange {
    let fixture = Fixture::start(status, body.as_bytes(), headers);
    let mut pieces = Vec::new();
    let result = client(fixture.address.clone(), None).complete_streaming(
        &messages(),
        &mut |text: &str| pieces.push(text.to_owned()),
        &CancelToken::new(),
    );
    let (head, request) = fixture.finish();
    StreamExchange {
        result,
        pieces,
        request: serde_json::from_slice(&request).expect("request JSON"),
        head,
    }
}

fn tool_stream_exchange(body: &str) -> (Result<ToolModelReply, ProviderError>, Vec<String>, Value) {
    let fixture = Fixture::start("200 OK", body.as_bytes(), EVENT_STREAM);
    let mut pieces = Vec::new();
    let result = client(fixture.address.clone(), None).complete_with_tools_streaming(
        &messages(),
        &tool_definitions(),
        &mut |text: &str| pieces.push(text.to_owned()),
        &CancelToken::new(),
    );
    let (_, request) = fixture.finish();
    (
        result,
        pieces,
        serde_json::from_slice(&request).expect("request JSON"),
    )
}

fn call_event(entry: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{entry}]}},\"finish_reason\":null}}]}}\n\n"
    )
}

#[test]
fn a_streamed_text_request_sends_stream_true_and_delivers_pieces_in_order() {
    // Given a server that streams three pieces.
    let body = text_stream(&["Hel", "lo ", "there"]);

    // When a streamed text request runs.
    let exchange = stream_exchange("200 OK", &body, EVENT_STREAM);

    // Then the reply is the concatenation, the pieces arrived in order, and
    // the wire request asked for a stream without `stream_options`.
    let reply = exchange.result.expect("valid stream");
    assert_eq!(reply.content, "Hello there");
    assert_eq!(reply.finish_reason, "stop");
    assert_eq!(reply.usage, None);
    assert_eq!(exchange.pieces, ["Hel", "lo ", "there"]);
    assert_eq!(exchange.request["stream"], true);
    assert!(exchange.request.get("stream_options").is_none());
    assert!(exchange.request.get("tools").is_none());
    assert!(
        exchange
            .head
            .to_ascii_lowercase()
            .contains("accept: text/event-stream\r\n")
    );
}

#[test]
fn non_streaming_requests_still_send_stream_false_and_no_accept_header() {
    // Given the non-streaming text and tool methods.
    let fixture = Fixture::start("200 OK", SUCCESS.as_bytes(), "");
    client(fixture.address.clone(), None)
        .complete(&messages(), &CancelToken::new())
        .expect("reply");

    // When their requests are captured.
    let (head, body) = fixture.finish();
    let request: Value = serde_json::from_slice(&body).expect("request JSON");
    let (_, tool_request) = tool_exchange(
        r#"{"choices":[{"message":{"role":"assistant","content":"x"},"finish_reason":"stop"}]}"#,
    );

    // Then both send `stream: false`, no `stream_options`, and no event-stream Accept.
    assert_eq!(request["stream"], false);
    assert!(request.get("stream_options").is_none());
    assert!(!head.to_ascii_lowercase().contains("text/event-stream"));
    assert_eq!(tool_request["stream"], false);
    assert!(tool_request.get("stream_options").is_none());
}

#[test]
fn a_streamed_tool_request_sends_stream_true_with_the_tools() {
    let (result, _, request) = tool_stream_exchange(&text_stream(&["ok"]));
    assert_eq!(result.expect("valid stream").content, "ok");
    assert_eq!(request["stream"], true);
    assert_eq!(request["tool_choice"], "auto");
    assert!(
        request["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty())
    );
    assert!(request.get("stream_options").is_none());
}

#[test]
fn a_content_type_with_parameters_in_any_case_is_accepted() {
    let exchange = stream_exchange(
        "200 OK",
        &text_stream(&["a"]),
        "Content-Type: Text/Event-Stream; charset=utf-8\r\n",
    );
    assert_eq!(exchange.result.expect("valid stream").content, "a");
    assert_eq!(exchange.pieces, ["a"]);
}

#[test]
fn a_json_reply_to_a_streamed_request_is_refused() {
    let exchange = stream_exchange("200 OK", SUCCESS, "Content-Type: application/json\r\n");
    assert_eq!(exchange.result, Err(ProviderError::MalformedResponse));
    assert!(exchange.pieces.is_empty());

    let missing = stream_exchange("200 OK", &text_stream(&["a"]), "");
    assert_eq!(missing.result, Err(ProviderError::MalformedResponse));
}

#[test]
fn a_non_success_status_to_a_streamed_request_is_an_http_status_error() {
    let exchange = stream_exchange(
        "503 Service Unavailable",
        &text_stream(&["a"]),
        EVENT_STREAM,
    );
    assert_eq!(exchange.result, Err(ProviderError::HttpStatus(503)));
    assert!(exchange.pieces.is_empty());
}

#[test]
fn a_streamed_reply_without_done_is_incomplete_and_keeps_delivered_text() {
    let body = format!("{}{}", text_event("one"), text_event("two"));
    let exchange = stream_exchange("200 OK", &body, EVENT_STREAM);
    assert_eq!(exchange.result, Err(ProviderError::IncompleteStream));
    assert_eq!(exchange.pieces, ["one", "two"]);
}

#[test]
fn text_before_a_malformed_payload_stays_delivered_and_the_call_fails() {
    let body = format!(
        "{}{}data: {{broken\n\n",
        text_event("one"),
        text_event("two")
    );
    let exchange = stream_exchange("200 OK", &body, EVENT_STREAM);
    assert_eq!(exchange.result, Err(ProviderError::MalformedResponse));
    assert_eq!(exchange.pieces, ["one", "two"]);
}

#[test]
fn a_streamed_reply_over_eight_mebibytes_is_refused() {
    let mut body = text_event("start");
    while body.len() <= 8 * 1024 * 1024 {
        body.push_str(": padding padding padding padding padding padding\n");
    }
    let exchange = stream_exchange("200 OK", &body, EVENT_STREAM);
    assert_eq!(exchange.result, Err(ProviderError::ResponseTooLarge));
    assert_eq!(exchange.pieces, ["start"]);
}

#[test]
fn streamed_usage_and_crlf_framing_are_accepted() {
    let body = format!(
        ": ping\r\n\r\nevent: message\r\ndata:{}\r\n\r\ndata: {}\r\n\r\ndata: {}\r\n\r\ndata: [DONE]\r\n\r\n",
        serde_json::json!({"choices":[{"index":0,"delta":{"role":"assistant","content":"hi"}}]}),
        serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"length"}]}),
        serde_json::json!({"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}}),
    );
    let exchange = stream_exchange("200 OK", &body, EVENT_STREAM);
    assert_eq!(
        exchange.result,
        Ok(ModelReply {
            content: "hi".to_owned(),
            finish_reason: "length".to_owned(),
            usage: Some(Usage {
                prompt_tokens: Some(5),
                completion_tokens: Some(2),
                total_tokens: Some(7),
            }),
        })
    );
    assert_eq!(exchange.pieces, ["hi"]);
}

#[test]
fn the_streamed_text_method_refuses_tool_calls_and_tool_finish_reasons() {
    let with_call = call_event(
        r#"{"index":0,"id":"a","type":"function","function":{"name":"workspace_read_file","arguments":"{}"}}"#,
    );
    let exchange = stream_exchange("200 OK", &with_call, EVENT_STREAM);
    assert_eq!(exchange.result, Err(ProviderError::UnsupportedToolCall));

    let finish = format!("{}data: [DONE]\n\n", finish_event("tool_calls"));
    let exchange = stream_exchange("200 OK", &finish, EVENT_STREAM);
    assert_eq!(exchange.result, Err(ProviderError::UnsupportedToolCall));
}

#[test]
fn documented_streamed_tool_call_pieces_assemble_into_a_valid_call() {
    // Given the first piece with id, type, name, and empty arguments, then fragments.
    let mut body = text_event("Reading.");
    body.push_str(&call_event(
        r#"{"index":0,"id":"call_9","type":"function","function":{"name":"workspace_read_file","arguments":""}}"#,
    ));
    body.push_str(&call_event(
        r#"{"index":0,"function":{"arguments":"{\"path\":"}}"#,
    ));
    body.push_str(&call_event(
        r#"{"index":0,"function":{"arguments":"\"notes.txt\"}"}}"#,
    ));
    body.push_str(&finish_event("tool_calls"));
    body.push_str("data: [DONE]\n\n");

    // When the streamed tool request runs.
    let (result, pieces, _) = tool_stream_exchange(&body);

    // Then one raw call parses with the real validator and the text was delivered.
    let reply = result.expect("valid tool stream");
    assert_eq!(reply.content, "Reading.");
    assert_eq!(reply.finish_reason, "tool_calls");
    assert_eq!(pieces, ["Reading."]);
    let [raw_call] = reply.tool_calls.as_slice() else {
        panic!("exactly one call: {:?}", reply.tool_calls);
    };
    assert!(ToolCall::parse(raw_call).is_ok());
}

/// Stream the given call pieces and a finish reason through the tool method.
fn assembled(entries: &[&str], reason: &str) -> Result<ToolModelReply, ProviderError> {
    let mut body: String = entries.iter().map(|entry| call_event(entry)).collect();
    body.push_str(&finish_event(reason));
    body.push_str("data: [DONE]\n\n");
    tool_stream_exchange(&body).0
}

#[test]
fn streamed_tool_call_pieces_follow_the_assembly_rules() {
    // A name sent in pieces, a repeated equal id, and finish `stop` with calls.
    let reply = assembled(
        &[
            r#"{"index":0,"id":"a","function":{"name":"workspace_list_"}}"#,
            r#"{"index":0,"id":"a","type":"function","function":{"name":"directory","arguments":"{\"path\":\"\"}"}}"#,
        ],
        "stop",
    )
    .expect("assembled");
    let [raw_call] = reply.tool_calls.as_slice() else {
        panic!("exactly one call: {:?}", reply.tool_calls);
    };
    assert!(ToolCall::parse(raw_call).is_ok());

    // A call that never received an id has no `id` member and fails validation.
    let reply = assembled(
        &[r#"{"index":0,"type":"function","function":{"name":"x","arguments":"{}"}}"#],
        "tool_calls",
    )
    .expect("assembled");
    let [raw_call] = reply.tool_calls.as_slice() else {
        panic!("exactly one call: {:?}", reply.tool_calls);
    };
    assert!(raw_call.get("id").is_none());
    assert!(ToolCall::parse(raw_call).is_err());
}

#[test]
fn streamed_tool_call_pieces_that_break_the_rules_are_refused() {
    let refused: [&[&str]; 5] = [
        &[r#"{"index":0,"id":"a"}"#, r#"{"index":0,"id":"b"}"#],
        &[r#"{"index":2,"id":"a"}"#],
        &[r#"{"index":0,"surprise":true}"#],
        &[r#"{"index":0,"type":"custom"}"#],
        &[r#"{"index":0,"function":{"extra":"x"}}"#],
    ];
    for entries in refused {
        assert_eq!(
            assembled(entries, "tool_calls").expect_err("refused"),
            ProviderError::MalformedResponse,
            "{entries:?}"
        );
    }

    // A reply cut short by the token limit cannot carry tool calls.
    assert_eq!(
        assembled(&[r#"{"index":0,"id":"a"}"#], "length").expect_err("refused"),
        ProviderError::MalformedResponse
    );

    // Sixty-four calls are accepted; index 64 is refused.
    let many: Vec<String> = (0..65)
        .map(|index| format!(r#"{{"index":{index},"id":"c{index}"}}"#))
        .collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    assert_eq!(
        assembled(&many, "tool_calls").expect_err("refused"),
        ProviderError::MalformedResponse
    );
    assert_eq!(
        assembled(many.get(..64).expect("sixty-four"), "tool_calls")
            .expect("sixty-four calls")
            .tool_calls
            .len(),
        64
    );
}

// ---- Cancellation of a streamed request (ADR-0050) ----

/// The response head of a streamed reply whose body ends only when the server closes.
const STREAM_HEAD: &str =
    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";

/// How long the fixtures hold a connection open for a client that never closes it.
const HOLD: Duration = Duration::from_secs(5);

/// The slowest acceptable cancellation. The request deadline is 30 seconds.
const CANCEL_BOUND: Duration = Duration::from_secs(2);

/// Accept one connection within a bound.
fn accept_bounded(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "fixture accept timed out");
                thread::sleep(Duration::from_millis(2));
            }
            Err(error) => panic!("fixture accept failed: {error}"),
        }
    }
}

/// Serve one connection: read the request, write `response` (when any), then
/// hold the connection open. The task returns whether the client closed it
/// within [`HOLD`]. `request_read` is signalled once the whole request has been
/// read, which is the moment the client starts waiting for a response head.
fn holding_server(
    response: Option<Vec<u8>>,
    request_read: mpsc::Sender<()>,
) -> (String, JoinHandle<bool>) {
    let listener = bind_fixture_listener();
    listener
        .set_nonblocking(true)
        .expect("make fixture accept bounded");
    let address = format!("http://{}/v1", listener.local_addr().expect("address"));
    let task = thread::spawn(move || {
        let mut stream = accept_bounded(&listener);
        stream
            .set_nonblocking(false)
            .expect("make accepted fixture stream blocking");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("set fixture read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .expect("set fixture write timeout");
        read_request(&mut stream).expect("read bounded request");
        // A call that cancels on delivered pieces does not listen for this, so
        // a failed send carries no information.
        let _ignored = request_read.send(());
        if let Some(bytes) = response {
            stream.write_all(&bytes).expect("write fixture response");
            stream.flush().expect("flush fixture response");
        }
        stream
            .set_read_timeout(Some(Duration::from_millis(25)))
            .expect("set fixture poll timeout");
        let end = Instant::now() + HOLD;
        let mut buffer = [0_u8; 64];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => return true,
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if Instant::now() >= end {
                        return false;
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::BrokenPipe
                    ) =>
                {
                    return true;
                }
                Err(error) => panic!("fixture read failed: {error}"),
            }
        }
    });
    (address, task)
}

/// What one cancelled-or-not streamed call did.
struct HeldCall<R> {
    result: Result<R, ProviderError>,
    pieces: Vec<String>,
    elapsed: Duration,
    /// Whether the server saw the client close the connection.
    closed: bool,
}

/// Run one streamed call against a server that writes `response` and then
/// holds the body open. A helper thread cancels the call once
/// `pieces_before_cancel` pieces were delivered, so the call is waiting for the
/// next piece of the body when it is cancelled. With `None` the thread cancels
/// once the server has read the whole request, so the call is waiting for a
/// response head however slow the host is.
fn held_call<R>(
    response: Option<Vec<u8>>,
    pieces_before_cancel: Option<usize>,
    run: impl FnOnce(&LocalModelClient, &mut dyn FnMut(&str), &CancelToken) -> Result<R, ProviderError>,
) -> HeldCall<R> {
    let (request_read, request_seen) = mpsc::channel::<()>();
    let (address, server) = holding_server(response, request_read);
    let cancel = CancelToken::new();
    let (delivered, seen) = mpsc::channel::<()>();
    let canceller = {
        let cancel = cancel.clone();
        thread::spawn(move || match pieces_before_cancel {
            Some(count) => {
                for _ in 0..count {
                    seen.recv_timeout(Duration::from_secs(5))
                        .expect("the pieces arrive before the bound");
                }
                cancel.cancel();
            }
            None => {
                request_seen
                    .recv_timeout(Duration::from_secs(5))
                    .expect("the server reads the request before the bound");
                cancel.cancel();
            }
        })
    };
    let mut pieces = Vec::new();
    let started = Instant::now();
    let result = run(
        &client(address, None),
        &mut |text: &str| {
            pieces.push(text.to_owned());
            // The helper drops its receiver once it has cancelled, so a send after
            // that point fails by design and carries no information.
            let _ignored = delivered.send(());
        },
        &cancel,
    );
    let elapsed = started.elapsed();
    join_bounded(canceller, "canceller completes");
    let closed = join_bounded(server, "fixture completes");
    HeldCall {
        result,
        pieces,
        elapsed,
        closed,
    }
}

fn held_body(head_and_events: &str) -> Option<Vec<u8>> {
    Some(format!("{STREAM_HEAD}{head_and_events}").into_bytes())
}

#[test]
fn a_token_cancelled_before_the_call_makes_no_connection() {
    // Given a listening socket and a token that is already cancelled.
    let listener = bind_fixture_listener();
    listener
        .set_nonblocking(true)
        .expect("make no-call fixture nonblocking");
    let address = format!("http://{}/v1", listener.local_addr().expect("address"));
    let model = client(address, None);
    let cancel = CancelToken::new();
    cancel.cancel();
    let mut pieces = Vec::new();

    // When each method, streamed or not, is called.
    let plain = model.complete(&messages(), &cancel);
    let plain_tools = model.complete_with_tools(&messages(), &tool_definitions(), &cancel);
    let text = model.complete_streaming(
        &messages(),
        &mut |piece: &str| pieces.push(piece.to_owned()),
        &cancel,
    );
    let tools = model.complete_with_tools_streaming(
        &messages(),
        &tool_definitions(),
        &mut |piece: &str| pieces.push(piece.to_owned()),
        &cancel,
    );

    // Then all are cancelled, nothing was delivered, and nothing connected.
    assert_eq!(plain, Err(ProviderError::Cancelled));
    assert_eq!(plain_tools.err(), Some(ProviderError::Cancelled));
    assert_eq!(text, Err(ProviderError::Cancelled));
    assert_eq!(tools.err(), Some(ProviderError::Cancelled));
    assert!(pieces.is_empty());
    assert!(
        matches!(
            listener.accept(),
            Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock
        ),
        "no connection was made"
    );
}

#[test]
fn cancelling_while_the_response_head_is_awaited_stops_the_request() {
    // Given a server that accepts and never sends a response head.
    // When the token is cancelled once the server has read the request.
    let call = held_call(None, None, |model, on_text, cancel| {
        model.complete_streaming(&messages(), on_text, cancel)
    });

    // Then the call is cancelled far inside the 30-second deadline, and the
    // server saw the connection close.
    assert_eq!(call.result, Err(ProviderError::Cancelled));
    assert!(call.pieces.is_empty());
    assert!(
        call.elapsed < CANCEL_BOUND,
        "cancellation took {:?}",
        call.elapsed
    );
    assert!(call.closed, "the connection was closed on cancellation");
}

#[test]
fn cancelling_while_a_text_body_is_held_open_keeps_the_delivered_pieces() {
    // Given a server that sends two pieces and holds the body open.
    let body = held_body(&format!("{}{}", text_event("one"), text_event("two")));

    // When the token is cancelled after both pieces were delivered.
    let call = held_call(body, Some(2), |model, on_text, cancel| {
        model.complete_streaming(&messages(), on_text, cancel)
    });

    // Then both pieces stay delivered, the call is cancelled promptly, and the
    // connection was closed.
    assert_eq!(call.result, Err(ProviderError::Cancelled));
    assert_eq!(call.pieces, ["one", "two"]);
    assert!(
        call.elapsed < CANCEL_BOUND,
        "cancellation took {:?}",
        call.elapsed
    );
    assert!(call.closed, "the connection was closed on cancellation");
}

#[test]
fn cancelling_a_streamed_tool_request_discards_a_partly_sent_call() {
    // Given a server that sends text and the first piece of a tool call, then
    // holds the body open.
    let mut events = text_event("Reading.");
    events.push_str(&call_event(
        r#"{"index":0,"id":"call_9","type":"function","function":{"name":"workspace_read_file","arguments":"{\"path\":"}}"#,
    ));

    // When the token is cancelled after the text was delivered.
    let call = held_call(held_body(&events), Some(1), |model, on_text, cancel| {
        model.complete_with_tools_streaming(&messages(), &tool_definitions(), on_text, cancel)
    });

    // Then the call is cancelled, no reply and no call exist, and the text stays delivered.
    assert_eq!(call.result.err(), Some(ProviderError::Cancelled));
    assert_eq!(call.pieces, ["Reading."]);
    assert!(
        call.elapsed < CANCEL_BOUND,
        "cancellation took {:?}",
        call.elapsed
    );
    assert!(call.closed, "the connection was closed on cancellation");
}

#[test]
fn a_cancel_after_done_was_processed_does_not_change_the_reply() {
    // Given a complete stream, sent in one write, whose server then keeps the
    // connection open so that the client must not wait for the body to end.
    let body = held_body(&text_stream(&["one", "two"]));

    // When the token is cancelled while the pieces are delivered.
    let call = held_call(body, Some(2), |model, on_text, cancel| {
        model.complete_streaming(&messages(), on_text, cancel)
    });

    // Then the reply is the validated one, and nothing waited for the server.
    let reply = call.result.expect("a reply that was complete");
    assert_eq!(reply.content, "onetwo");
    assert_eq!(reply.finish_reason, "stop");
    assert_eq!(call.pieces, ["one", "two"]);
    assert!(
        call.elapsed < CANCEL_BOUND,
        "the call waited {:?}",
        call.elapsed
    );
}

#[test]
fn a_cancel_after_done_does_not_change_a_streamed_tool_reply() {
    // Given a complete tool stream with a call, held open after `[DONE]`.
    let mut events = text_event("Reading.");
    events.push_str(&call_event(
        r#"{"index":0,"id":"call_9","type":"function","function":{"name":"workspace_read_file","arguments":"{\"path\":\"notes.txt\"}"}}"#,
    ));
    events.push_str(&finish_event("tool_calls"));
    events.push_str("data: [DONE]\n\n");

    // When the token is cancelled while the text is delivered.
    let call = held_call(held_body(&events), Some(1), |model, on_text, cancel| {
        model.complete_with_tools_streaming(&messages(), &tool_definitions(), on_text, cancel)
    });

    // Then the reply with its call is returned.
    let reply = call.result.expect("a reply that was complete");
    assert_eq!(reply.tool_calls.len(), 1);
    assert_eq!(reply.finish_reason, "tool_calls");
    assert!(call.elapsed < CANCEL_BOUND);
}

#[test]
fn a_token_that_is_never_cancelled_does_not_disturb_a_slow_stream() {
    // Given a server that sends its pieces more than one poll interval apart.
    let listener = bind_fixture_listener();
    listener.set_nonblocking(true).expect("bounded accept");
    let address = format!("http://{}/v1", listener.local_addr().expect("address"));
    let server = thread::spawn(move || {
        let mut stream = accept_bounded(&listener);
        stream
            .set_nonblocking(false)
            .expect("make accepted fixture stream blocking");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("set fixture read timeout");
        read_request(&mut stream).expect("read bounded request");
        stream
            .write_all(STREAM_HEAD.as_bytes())
            .expect("write head");
        for event in [text_event("a"), text_event("b"), text_event("c")] {
            thread::sleep(Duration::from_millis(80));
            stream.write_all(event.as_bytes()).expect("write piece");
            stream.flush().expect("flush piece");
        }
        thread::sleep(Duration::from_millis(80));
        stream
            .write_all(format!("{}data: [DONE]\n\n", finish_event("stop")).as_bytes())
            .expect("write end");
    });
    let mut pieces = Vec::new();

    // When the streamed request runs with a token nobody cancels.
    let result = client(address, None).complete_streaming(
        &messages(),
        &mut |piece: &str| pieces.push(piece.to_owned()),
        &CancelToken::new(),
    );
    join_bounded(server, "fixture completes");

    // Then the polling did not restart or disturb the request.
    assert_eq!(result.expect("a complete stream").content, "abc");
    assert_eq!(pieces, ["a", "b", "c"]);
}

// ---- Cancellation of a non-streamed request (ADR-0051) ----

#[test]
fn cancelling_a_non_streamed_request_while_the_head_is_awaited_stops_it() {
    // Given a server that accepts each request and never sends a head.
    // When the token is cancelled once the server has read the request.
    let text = held_call(None, None, |model, _on_text, cancel| {
        model.complete(&messages(), cancel)
    });
    let tools = held_call(None, None, |model, _on_text, cancel| {
        model.complete_with_tools(&messages(), &tool_definitions(), cancel)
    });

    // Then each call is cancelled far inside the 30-second deadline, and its
    // server saw the connection close.
    assert_eq!(text.result, Err(ProviderError::Cancelled));
    assert_eq!(tools.result.err(), Some(ProviderError::Cancelled));
    for (elapsed, closed) in [(text.elapsed, text.closed), (tools.elapsed, tools.closed)] {
        assert!(elapsed < CANCEL_BOUND, "cancellation took {elapsed:?}");
        assert!(closed, "the connection was closed on cancellation");
    }
}

#[test]
fn a_token_that_is_never_cancelled_does_not_disturb_a_slow_non_streamed_reply() {
    // Given a server that sends its head and body in pieces more than one
    // poll interval apart.
    let listener = bind_fixture_listener();
    listener.set_nonblocking(true).expect("bounded accept");
    let address = format!("http://{}/v1", listener.local_addr().expect("address"));
    let server = thread::spawn(move || {
        let mut stream = accept_bounded(&listener);
        stream
            .set_nonblocking(false)
            .expect("make accepted fixture stream blocking");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("set fixture read timeout");
        read_request(&mut stream).expect("read bounded request");
        thread::sleep(Duration::from_millis(80));
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            SUCCESS.len()
        );
        stream.write_all(head.as_bytes()).expect("write head");
        stream.flush().expect("flush head");
        for piece in SUCCESS.as_bytes().chunks(SUCCESS.len().div_ceil(3)) {
            thread::sleep(Duration::from_millis(80));
            stream.write_all(piece).expect("write piece");
            stream.flush().expect("flush piece");
        }
        assert!(
            await_client_close(&mut stream),
            "the client closes the fixture connection first"
        );
    });

    // When the request runs with a token nobody cancels.
    let result = client(address, None).complete(&messages(), &CancelToken::new());
    join_bounded(server, "fixture completes");

    // Then the polling did not restart or disturb the request.
    let reply = result.expect("a complete reply");
    assert_eq!(reply.content, "A plain reply.");
    assert_eq!(reply.finish_reason, "stop");
}

/// ADR-0099: the context budget, not a 256 KiB byte cap, bounds a request.
#[test]
fn a_chat_completions_request_over_256_kib_is_sent_when_the_context_budget_allows_it() {
    let fixture = Fixture::start("200 OK", SUCCESS.as_bytes(), "");
    let model = LocalModelClient::new(
        LocalModelConfig {
            base_url: fixture.address.clone(),
            model: "already-loaded-model".to_owned(),
            context_tokens: 1_000_000,
            max_output_tokens: 512,
        },
        None,
    )
    .expect("client");
    let mut conversation = vec![ModelMessage {
        role: ModelRole::System,
        content: BASE_PROMPT.to_owned(),
    }];
    for _ in 0..20 {
        conversation.push(ModelMessage {
            role: ModelRole::User,
            content: "a".repeat(16 * 1024),
        });
    }

    let reply = model
        .complete(&conversation, &CancelToken::new())
        .expect("a 320 KiB request is within the budget");

    assert_eq!(reply.content, "A plain reply.");
    let (_, request) = fixture.finish();
    assert!(request.len() > 256 * 1024, "{} bytes", request.len());
}
