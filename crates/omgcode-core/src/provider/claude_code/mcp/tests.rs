use super::*;

fn server() -> McpServer {
    McpServer::new(
        "omgcode",
        "0.1.0",
        vec![McpTool {
            name: "workspace_read_file".into(),
            description: "Read a file".into(),
            input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
        }],
    )
}

fn initialized() -> McpServer {
    let mut s = server();
    s.handle_line(br#"{"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"claude-code","version":"2.1.325"}},"jsonrpc":"2.0","id":900000}"#)
        .expect("initialize");
    s
}

fn reply_json(outputs: &[McpOutput]) -> Value {
    let [McpOutput::Reply(line)] = outputs else {
        panic!("one reply expected, got {outputs:?}");
    };
    assert_eq!(line.last(), Some(&b'\n'));
    assert_eq!(line.iter().filter(|b| **b == b'\n').count(), 1);
    serde_json::from_slice(line).expect("reply is JSON")
}

fn call_line(id: &str, name: &str, arguments: &str) -> Vec<u8> {
    format!(
        r#"{{"method":"tools/call","params":{{"name":"{name}","arguments":{arguments},"_meta":{{"claudecode/toolUseId":"toolu_01X","progressToken":2}}}},"jsonrpc":"2.0","id":{id}}}"#
    )
    .into_bytes()
}

fn only_call(outputs: Vec<McpOutput>) -> PendingCall {
    let mut outputs = outputs;
    match (outputs.pop(), outputs.is_empty()) {
        (Some(McpOutput::Call(call)), true) => call,
        other => panic!("one call expected, got {other:?}"),
    }
}

#[test]
fn discover_is_answered_method_not_found_with_its_string_id() {
    let mut s = server();
    let out = s
        .handle_line(br#"{"jsonrpc":"2.0","id":"server-discover-omgcode-1","method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#)
        .expect("handled");
    let reply = reply_json(&out);
    assert_eq!(reply["id"], "server-discover-omgcode-1");
    assert_eq!(reply["error"]["code"], -32601);
    assert!(!s.is_initialized());
}

#[test]
fn initialize_answers_the_supported_version_and_server_info() {
    let mut s = server();
    let out = s
        .handle_line(br#"{"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{"roots":{"listChanged":true},"elicitation":{"form":{},"url":{}}},"clientInfo":{"name":"claude-code","version":"2.1.325"}},"jsonrpc":"2.0","id":0}"#)
        .expect("handled");
    assert_eq!(
        reply_json(&out),
        json!({"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"omgcode","version":"0.1.0"}}})
    );
    assert!(s.is_initialized());
}

#[test]
fn initialize_for_another_version_still_answers_the_supported_one() {
    let mut s = server();
    let out = s
        .handle_line(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2099-01-01"}}"#)
        .expect("handled");
    assert_eq!(
        reply_json(&out)["result"]["protocolVersion"],
        PROTOCOL_VERSION
    );
}

#[test]
fn initialize_without_a_version_or_twice_is_an_error_reply() {
    let mut s = server();
    let out = s
        .handle_line(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
        .expect("handled");
    assert_eq!(reply_json(&out)["error"]["code"], -32602);
    assert!(!s.is_initialized());
    let mut s = initialized();
    let out = s
        .handle_line(br#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#)
        .expect("handled");
    assert_eq!(reply_json(&out)["error"]["code"], -32600);
}

#[test]
fn notifications_produce_nothing() {
    let mut s = initialized();
    for line in [
        &br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#[..],
        br#"{"jsonrpc":"2.0","method":"notifications/roots/list_changed"}"#,
        br#"{"jsonrpc":"2.0","method":"anything","params":[1]}"#,
        br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":99}}"#,
        br#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#,
    ] {
        assert_eq!(s.handle_line(line), Ok(Vec::new()), "{line:?}");
    }
}

#[test]
fn ping_works_before_and_after_initialize() {
    for mut s in [server(), initialized()] {
        let out = s
            .handle_line(br#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#)
            .expect("handled");
        assert_eq!(
            reply_json(&out),
            json!({"jsonrpc":"2.0","id":7,"result":{}})
        );
    }
}

#[test]
fn tools_list_returns_the_tools_and_ignores_a_cursor() {
    let mut s = initialized();
    for (id, line) in [
        (1, &br#"{"method":"tools/list","jsonrpc":"2.0","id":1}"#[..]),
        (
            2,
            br#"{"method":"tools/list","jsonrpc":"2.0","id":2,"params":{"cursor":"abc"}}"#,
        ),
    ] {
        let out = s.handle_line(line).expect("handled");
        assert_eq!(
            reply_json(&out),
            json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{"name":"workspace_read_file","description":"Read a file","inputSchema":{"type":"object","properties":{"path":{"type":"string"}}}}]}})
        );
    }
}

#[test]
fn a_request_before_initialize_gets_error_32002() {
    let mut s = server();
    for line in [
        &br#"{"method":"tools/list","jsonrpc":"2.0","id":1}"#[..],
        &call_line("2", "workspace_read_file", "{}"),
    ] {
        let out = s.handle_line(line).expect("handled");
        assert_eq!(reply_json(&out)["error"]["code"], -32002);
    }
    assert_eq!(s.pending_calls(), 0);
}

#[test]
fn unknown_methods_get_method_not_found() {
    let mut s = initialized();
    let out = s
        .handle_line(br#"{"jsonrpc":"2.0","id":"a","method":"resources/list"}"#)
        .expect("handled");
    let reply = reply_json(&out);
    assert_eq!(reply["id"], "a");
    assert_eq!(reply["error"]["code"], -32601);
}

#[test]
fn tools_call_with_meta_becomes_a_pending_call_with_compact_arguments() {
    let mut s = initialized();
    let call = only_call(
        s.handle_line(&call_line(
            "2",
            "workspace_read_file",
            r#"{ "path" : "a.txt" }"#,
        ))
        .expect("handled"),
    );
    assert_eq!(call.key, RequestKey::Number(2));
    assert_eq!(call.name, "workspace_read_file");
    assert_eq!(call.arguments_json, r#"{"path":"a.txt"}"#);
    assert_eq!(call.tool_use_id.as_deref(), Some("toolu_01X"));
    assert_eq!(s.pending_calls(), 1);
}

#[test]
fn tools_call_without_meta_or_arguments_has_no_tool_use_id_and_empty_arguments() {
    let mut s = initialized();
    let call = only_call(
        s.handle_line(br#"{"jsonrpc":"2.0","id":"c-1","method":"tools/call","params":{"name":"workspace_read_file"}}"#)
            .expect("handled"),
    );
    assert_eq!(call.key, RequestKey::Text("c-1".into()));
    assert_eq!(call.arguments_json, "{}");
    assert_eq!(call.tool_use_id, None);
}

#[test]
fn unknown_tool_names_and_non_object_arguments_are_forwarded() {
    let mut s = initialized();
    let call = only_call(
        s.handle_line(&call_line("1", "no_such_tool", "[1, 2]"))
            .expect("handled"),
    );
    assert_eq!(call.name, "no_such_tool");
    assert_eq!(call.arguments_json, "[1,2]");
    let call = only_call(
        s.handle_line(&call_line("2", "workspace_read_file", "\"text\""))
            .expect("handled"),
    );
    assert_eq!(call.arguments_json, "\"text\"");
}

#[test]
fn an_unsafe_tool_use_id_is_dropped() {
    let mut s = initialized();
    let long = "x".repeat(MAX_TOOL_USE_ID_BYTES + 1);
    for (n, id) in [
        r"a\nb".to_owned(),
        long,
        String::new(),
        r"a\u0000b".to_owned(),
    ]
    .iter()
    .enumerate()
    {
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":{n},"method":"tools/call","params":{{"name":"t","_meta":{{"claudecode/toolUseId":"{id}"}}}}}}"#
        );
        let call = only_call(s.handle_line(line.as_bytes()).expect("handled"));
        assert_eq!(call.tool_use_id, None, "{id:?}");
    }
}

#[test]
fn a_tools_call_with_bad_parameters_is_an_error_reply_and_not_pending() {
    let mut s = initialized();
    for line in [
        &br#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#[..],
        br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":3}}"#,
        br#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":[]}"#,
    ] {
        let out = s.handle_line(line).expect("handled");
        assert_eq!(reply_json(&out)["error"]["code"], -32602);
    }
    assert_eq!(s.pending_calls(), 0);
}

#[test]
fn answering_builds_the_result_line_with_escaping() {
    let mut s = initialized();
    let call = only_call(
        s.handle_line(&call_line("2", "workspace_read_file", "{}"))
            .expect("handled"),
    );
    let text = "line1\nquote \" slash \\ tab\t \u{1F600}";
    let Answer::Line(line) = s.answer(&call.key, text, false).expect("answered") else {
        panic!("a line expected");
    };
    assert_eq!(line.iter().filter(|b| **b == b'\n').count(), 1);
    let reply: Value = serde_json::from_slice(&line).expect("JSON");
    assert_eq!(
        reply,
        json!({"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":text}],"isError":false}})
    );
    assert_eq!(s.pending_calls(), 0);
}

#[test]
fn an_error_answer_sets_is_error_and_string_ids_round_trip() {
    let mut s = initialized();
    let call = only_call(
        s.handle_line(
            br#"{"jsonrpc":"2.0","id":"abc","method":"tools/call","params":{"name":"t"}}"#,
        )
        .expect("handled"),
    );
    let Answer::Line(line) = s.answer(&call.key, "refused", true).expect("answered") else {
        panic!("a line expected");
    };
    let reply: Value = serde_json::from_slice(&line).expect("JSON");
    assert_eq!(reply["id"], "abc");
    assert_eq!(reply["result"]["isError"], true);
}

#[test]
fn answering_twice_or_an_unknown_key_is_an_error() {
    let mut s = initialized();
    let call = only_call(s.handle_line(&call_line("2", "t", "{}")).expect("handled"));
    s.answer(&call.key, "ok", false).expect("first answer");
    assert_eq!(
        s.answer(&call.key, "ok", false),
        Err(McpError::UnknownRequest)
    );
    assert_eq!(
        s.answer(&RequestKey::Number(99), "ok", false),
        Err(McpError::UnknownRequest)
    );
}

#[test]
fn an_oversized_answer_is_refused_and_the_call_stays_pending() {
    let mut s = initialized();
    let call = only_call(s.handle_line(&call_line("2", "t", "{}")).expect("handled"));
    let big = "x".repeat(MAX_ANSWER_TEXT_BYTES + 1);
    assert_eq!(
        s.answer(&call.key, &big, false),
        Err(McpError::AnswerTooLarge)
    );
    assert!(matches!(
        s.answer(&call.key, "short", false),
        Ok(Answer::Line(_))
    ));
}

#[test]
fn a_duplicate_pending_id_is_fatal_and_a_string_id_differs_from_an_integer() {
    let mut s = initialized();
    s.handle_line(&call_line("5", "t", "{}")).expect("first");
    assert_eq!(
        s.handle_line(&call_line("5", "t", "{}")),
        Err(McpError::DuplicatePending)
    );
    assert!(s.handle_line(&call_line("\"5\"", "t", "{}")).is_ok());
}

#[test]
fn an_answered_id_may_not_be_reused() {
    let mut s = initialized();
    let call = only_call(s.handle_line(&call_line("5", "t", "{}")).expect("first"));
    s.answer(&call.key, "ok", false).expect("answered");
    assert_eq!(
        s.handle_line(&call_line("5", "t", "{}")),
        Err(McpError::RequestIdReused)
    );
    assert!(s.handle_line(&call_line("\"5\"", "t", "{}")).is_ok());
}

#[test]
fn a_ping_may_not_reuse_an_unanswered_call_id() {
    let mut s = initialized();
    s.handle_line(&call_line("5", "t", "{}")).expect("first");
    let ping = br#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#;
    assert_eq!(s.handle_line(ping), Err(McpError::DuplicatePending));
}

#[test]
fn any_request_method_may_not_reuse_an_id() {
    let mut s = initialized();
    let ping = br#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#;
    assert!(s.handle_line(ping).is_ok());
    assert_eq!(s.handle_line(ping), Err(McpError::RequestIdReused));
    let bad = br#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{}}"#;
    assert_eq!(s.handle_line(bad), Err(McpError::RequestIdReused));
}

#[test]
fn a_full_record_of_used_ids_refuses_further_requests() {
    let mut s = initialized();
    let used = MAX_USED_REQUEST_IDS - 1;
    for id in 0..used {
        let line = format!(r#"{{"jsonrpc":"2.0","id":{},"method":"ping"}}"#, id + 1000);
        assert!(s.handle_line(line.as_bytes()).is_ok());
    }
    let next = br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    assert_eq!(s.handle_line(next), Err(McpError::TooManyRequestIds));
}

#[test]
fn more_than_the_maximum_pending_calls_is_fatal() {
    let mut s = initialized();
    for id in 0..MAX_PENDING_CALLS {
        s.handle_line(&call_line(&id.to_string(), "t", "{}"))
            .expect("within limit");
    }
    assert_eq!(
        s.handle_line(&call_line("100", "t", "{}")),
        Err(McpError::TooManyPending)
    );
    assert_eq!(s.pending_calls(), MAX_PENDING_CALLS);
}

#[test]
fn cancelling_a_pending_call_reports_it_once_and_drops_its_answer() {
    let mut s = initialized();
    let call = only_call(s.handle_line(&call_line("3", "t", "{}")).expect("call"));
    let cancel = br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":3,"reason":"user"}}"#;
    assert_eq!(
        s.handle_line(cancel),
        Ok(vec![McpOutput::Cancelled(RequestKey::Number(3))])
    );
    assert_eq!(s.handle_line(cancel), Ok(Vec::new()));
    assert_eq!(s.pending_calls(), 1);
    assert_eq!(s.answer(&call.key, "late", false), Ok(Answer::Cancelled));
    assert_eq!(s.pending_calls(), 0);
    assert_eq!(
        s.answer(&call.key, "late", false),
        Err(McpError::UnknownRequest)
    );
}

#[test]
fn cancelling_a_string_id_call_works() {
    let mut s = initialized();
    s.handle_line(br#"{"jsonrpc":"2.0","id":"c","method":"tools/call","params":{"name":"t"}}"#)
        .expect("call");
    assert_eq!(
        s.handle_line(
            br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"c"}}"#
        ),
        Ok(vec![McpOutput::Cancelled(RequestKey::Text("c".into()))])
    );
}

#[test]
fn fatal_message_errors() {
    let mut s = initialized();
    let long = vec![b' '; MAX_MCP_LINE_BYTES + 1];
    for (line, expected) in [
        (&b"[]"[..], McpError::BatchRejected),
        (
            br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
            McpError::BatchRejected,
        ),
        (b"not json", McpError::InvalidJson),
        (b"", McpError::InvalidJson),
        (
            br#"{"jsonrpc":"2.0","jsonrpc":"2.0","id":1,"method":"ping"}"#,
            McpError::InvalidJson,
        ),
        (b"3", McpError::NotAnObject),
        (br#"{"id":1,"method":"ping"}"#, McpError::InvalidVersion),
        (
            br#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
            McpError::InvalidVersion,
        ),
        (
            br#"{"jsonrpc":2,"id":1,"method":"ping"}"#,
            McpError::InvalidVersion,
        ),
        (
            br#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
            McpError::UnexpectedResponse,
        ),
        (
            br#"{"jsonrpc":"2.0","id":1,"method":5}"#,
            McpError::InvalidMethod,
        ),
        (
            br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
            McpError::InvalidId,
        ),
        (
            br#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#,
            McpError::InvalidId,
        ),
        (
            br#"{"jsonrpc":"2.0","id":{"a":1},"method":"ping"}"#,
            McpError::InvalidId,
        ),
        (
            br#"{"jsonrpc":"2.0","id":true,"method":"ping"}"#,
            McpError::InvalidId,
        ),
        (
            br#"{"jsonrpc":"2.0","id":18446744073709551615,"method":"ping"}"#,
            McpError::InvalidId,
        ),
        (&long[..], McpError::LineTooLong),
    ] {
        assert_eq!(s.handle_line(line), Err(expected), "{line:?}");
    }
}

#[test]
fn an_over_long_string_id_is_fatal() {
    let mut s = initialized();
    let id = "x".repeat(MAX_ID_BYTES + 1);
    let line = format!(r#"{{"jsonrpc":"2.0","id":"{id}","method":"ping"}}"#);
    assert_eq!(s.handle_line(line.as_bytes()), Err(McpError::InvalidId));
}

#[test]
fn a_line_at_the_limit_is_processed() {
    let mut s = initialized();
    let prefix = br#"{"jsonrpc":"2.0","id":1,"method":"ping","pad":""#;
    let suffix = br#""}"#;
    let mut line = prefix.to_vec();
    line.resize(MAX_MCP_LINE_BYTES - suffix.len(), b'a');
    line.extend_from_slice(suffix);
    assert_eq!(line.len(), MAX_MCP_LINE_BYTES);
    assert!(s.handle_line(&line).is_ok());
}

#[test]
fn a_thirty_two_kib_argument_call_is_carried_whole() {
    let mut s = initialized();
    let payload = "a".repeat(32 * 1024);
    let line = call_line(
        "1",
        "workspace_create_file",
        &format!(r#"{{"content":"{payload}"}}"#),
    );
    let call = only_call(s.handle_line(&line).expect("handled"));
    let arguments: Value = serde_json::from_str(&call.arguments_json).expect("arguments");
    assert_eq!(arguments["content"], payload);
}
