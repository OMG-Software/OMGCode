use super::*;
use std::io::Cursor;

fn event(line: &str) -> StreamEvent {
    parse_line(line.as_bytes()).expect("line parses")
}

const INIT: &str = r#"{"type":"system","subtype":"init","cwd":"/w","session_id":"s1","tools":["mcp__omgcode__read","mcp__omgcode__list"],"mcp_servers":[{"name":"omgcode","status":"connected","source":"dynamic"}],"model":"claude-haiku-5-5","permissionMode":"default","slash_commands":[],"apiKeySource":"none","claude_code_version":"2.1.325","agents":["claude","Explore"],"skills":[],"plugins":[],"uuid":"u1"}"#;

fn init_report() -> InitReport {
    match event(INIT) {
        StreamEvent::Init(report) => report,
        other => panic!("expected init, got {other:?}"),
    }
}

fn lines(input: &[u8], limit: usize) -> Vec<Result<Vec<u8>, StreamError>> {
    BoundedLines::with_limit(Cursor::new(input.to_vec()), limit).collect()
}

#[test]
fn splits_lines_and_strips_one_carriage_return() {
    let got = lines(b"ab\r\ncd\n\nef\r\r\n", 64);
    let got: Vec<_> = got.into_iter().map(|l| l.expect("line")).collect();
    assert_eq!(
        got,
        vec![b"ab".to_vec(), b"cd".to_vec(), Vec::new(), b"ef\r".to_vec()]
    );
}

#[test]
fn a_line_at_the_limit_is_accepted_and_one_byte_more_is_refused() {
    assert_eq!(lines(b"abcd\n", 4), vec![Ok(b"abcd".to_vec())]);
    assert_eq!(
        lines(b"abcde\nok\n", 4),
        vec![Err(StreamError::LineTooLong { limit: 4 })]
    );
}

#[test]
fn a_long_line_ends_the_iterator_without_reading_the_rest() {
    struct Endless {
        reads: usize,
    }
    impl Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            buf.fill(b'x');
            Ok(buf.len())
        }
    }
    let mut reader = BoundedLines::with_limit(Endless { reads: 0 }, 20_000);
    assert_eq!(
        reader.next(),
        Some(Err(StreamError::LineTooLong { limit: 20_000 }))
    );
    assert_eq!(reader.next(), None);
    // 20,000 bytes need three 8 KiB reads; the stream was never buffered.
    assert!(
        reader.reader.reads <= 3,
        "read {} chunks",
        reader.reader.reads
    );
    assert!(reader.chunk.len() <= READ_CHUNK_BYTES);
}

#[test]
fn a_partial_final_line_is_an_error_and_a_clean_end_is_not() {
    assert_eq!(
        lines(b"ab\ncd", 64),
        vec![Ok(b"ab".to_vec()), Err(StreamError::PartialLine)]
    );
    assert_eq!(lines(b"ab\n", 64), vec![Ok(b"ab".to_vec())]);
    assert!(lines(b"", 64).is_empty());
}

#[test]
fn a_line_split_across_reads_is_joined() {
    struct OneByte(Cursor<Vec<u8>>);
    impl Read for OneByte {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let take = buf.len().min(1);
            self.0.read(&mut buf[..take])
        }
    }
    let got: Vec<_> = BoundedLines::new(OneByte(Cursor::new(b"hello\nworld\n".to_vec()))).collect();
    assert_eq!(got, vec![Ok(b"hello".to_vec()), Ok(b"world".to_vec())]);
}

#[test]
fn a_reader_failure_is_an_error_and_interrupts_are_retried() {
    struct Flaky(u8);
    impl Read for Flaky {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.0 += 1;
            match self.0 {
                1 => Err(ErrorKind::Interrupted.into()),
                2 => {
                    buf[..2].copy_from_slice(b"a\n");
                    Ok(2)
                }
                _ => Err(ErrorKind::BrokenPipe.into()),
            }
        }
    }
    let got: Vec<_> = BoundedLines::new(Flaky(0)).collect();
    assert_eq!(
        got,
        vec![
            Ok(b"a".to_vec()),
            Err(StreamError::Io(ErrorKind::BrokenPipe))
        ]
    );
}

#[test]
fn init_is_parsed_with_unknown_keys_ignored() {
    let report = init_report();
    assert_eq!(report.version, "2.1.325");
    assert_eq!(report.model, "claude-haiku-5-5");
    assert_eq!(report.api_key_source, "none");
    assert_eq!(report.tools, ["mcp__omgcode__read", "mcp__omgcode__list"]);
    assert_eq!(
        report.mcp_servers,
        [McpServerStatus {
            name: "omgcode".into(),
            status: "connected".into()
        }]
    );
    assert!(report.plugins.is_empty() && report.skills.is_empty());
    assert!(report.slash_commands.is_empty());
}

#[test]
fn plugins_may_be_objects_with_a_name() {
    let line = INIT.replace(
        r#""plugins":[]"#,
        r#""plugins":[{"name":"p1","path":"/x"}]"#,
    );
    let StreamEvent::Init(report) = event(&line) else {
        panic!("init expected");
    };
    assert_eq!(report.plugins, ["p1"]);
}

#[test]
fn init_with_a_missing_or_wrong_member_is_an_error() {
    let missing = INIT.replace(r#""apiKeySource":"none","#, "");
    assert_eq!(
        parse_line(missing.as_bytes()),
        Err(StreamError::Shape("apiKeySource"))
    );
    let wrong = INIT.replace(r#""tools":["#, r#""tools":[7,"#);
    assert_eq!(
        parse_line(wrong.as_bytes()),
        Err(StreamError::Shape("tools"))
    );
}

#[test]
fn init_lists_and_names_are_bounded() {
    let many = vec!["\"t\""; MAX_LIST_ITEMS + 1].join(",");
    let line = INIT.replace(r#""skills":[]"#, &format!(r#""skills":[{many}]"#));
    assert_eq!(
        parse_line(line.as_bytes()),
        Err(StreamError::TooMany("skills"))
    );
    let long = "x".repeat(MAX_NAME_BYTES + 1);
    let line = INIT.replace("claude-haiku-5-5", &long);
    assert_eq!(
        parse_line(line.as_bytes()),
        Err(StreamError::TooLong("model"))
    );
}

#[test]
fn informational_system_events_and_unknown_types_are_ignored() {
    for line in [
        r#"{"type":"system","subtype":"status","status":"requesting"}"#,
        r#"{"type":"system","subtype":"thinking_tokens","n":3}"#,
        r#"{"type":"rate_limit_event","info":{}}"#,
        r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_01X","type":"tool_result","content":[{"type":"text","text":"x"}]}]},"parent_tool_use_id":null}"#,
        r#"{"type":"future_thing","a":1}"#,
        "",
    ] {
        assert_eq!(event(line), StreamEvent::Ignored, "{line}");
    }
}

#[test]
fn a_missing_type_or_non_object_line_is_an_error() {
    assert_eq!(parse_line(b"{}"), Err(StreamError::Shape("type")));
    assert_eq!(parse_line(b"[1]"), Err(StreamError::Shape("line")));
    assert_eq!(parse_line(b"\"x\""), Err(StreamError::Shape("line")));
}

#[test]
fn text_deltas_of_the_main_agent_are_reported() {
    let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}},"parent_tool_use_id":null,"uuid":"u"}"#;
    assert_eq!(event(line), StreamEvent::TextDelta("Hel".into()));
    let no_parent = r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":""}}}"#;
    assert_eq!(event(no_parent), StreamEvent::TextDelta(String::new()));
}

/// A tool-use block's name is reported from its first streamed event, for
/// the main agent and a sub-agent alike, so an unexpected tool is seen before
/// its input is complete.
#[test]
fn a_tool_use_block_start_reports_its_name() {
    let main = r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_01","name":"mcp__omgcode__workspace_read_file","input":{}}},"parent_tool_use_id":null}"#;
    assert_eq!(
        event(main),
        StreamEvent::ToolUseStart("mcp__omgcode__workspace_read_file".into())
    );
    let sub = r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_02","name":"Bash","input":{}}},"parent_tool_use_id":"toolu_01"}"#;
    assert_eq!(event(sub), StreamEvent::ToolUseStart("Bash".into()));
    for (line, error) in [
        (
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0}}"#,
            StreamError::Shape("content_block"),
        ),
        (
            r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"tool_use","id":"t"}}}"#,
            StreamError::Shape("name"),
        ),
        (
            r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"tool_use","name":""}}}"#,
            StreamError::Shape("tool_use"),
        ),
    ] {
        assert_eq!(parse_line(line.as_bytes()), Err(error), "{line}");
    }
}

#[test]
fn other_stream_events_and_sub_agent_deltas_are_ignored() {
    for line in [
        r#"{"type":"stream_event","event":{"type":"message_start","message":{}},"parent_tool_use_id":null}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
        r#"{"type":"stream_event","event":{"type":"message_delta","delta":{"stop_reason":"end_turn"}}}"#,
        r#"{"type":"stream_event","event":{"type":"message_stop"}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hm"}}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{"}}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"sub"}},"parent_tool_use_id":"toolu_01X"}"#,
    ] {
        assert_eq!(event(line), StreamEvent::Ignored, "{line}");
    }
}

#[test]
fn a_malformed_stream_event_is_an_error() {
    assert_eq!(
        parse_line(br#"{"type":"stream_event"}"#),
        Err(StreamError::Shape("event"))
    );
    assert_eq!(
        parse_line(
            br#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":5}}}"#
        ),
        Err(StreamError::Shape("text"))
    );
    assert_eq!(
        parse_line(
            br#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"x"}},"parent_tool_use_id":7}"#
        ),
        Err(StreamError::Shape("parent_tool_use_id"))
    );
}

#[test]
fn a_sub_agent_delta_is_checked_before_it_is_ignored() {
    for (line, field) in [
        (
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":null}},"parent_tool_use_id":"toolu_01X"}"#,
            "type",
        ),
        (
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{}},"parent_tool_use_id":"toolu_01X"}"#,
            "type",
        ),
        (
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":"text"},"parent_tool_use_id":"toolu_01X"}"#,
            "delta",
        ),
        (
            r#"{"type":"stream_event","event":{"type":"content_block_delta"},"parent_tool_use_id":"toolu_01X"}"#,
            "delta",
        ),
    ] {
        assert_eq!(
            parse_line(line.as_bytes()),
            Err(StreamError::Shape(field)),
            "{line}"
        );
    }
    // A well-formed sub-agent delta of any kind is still ignored.
    let thinking = r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hm"}},"parent_tool_use_id":"toolu_01X"}"#;
    assert_eq!(event(thinking), StreamEvent::Ignored);
}

fn assistant(content: &str) -> StreamEvent {
    event(&format!(
        r#"{{"type":"assistant","message":{{"id":"msg_x","model":"claude-haiku-5-5","role":"assistant","content":[{content}],"stop_reason":null}},"parent_tool_use_id":null,"session_id":"s","uuid":"u"}}"#
    ))
}

#[test]
fn assistant_tool_use_text_and_thinking_blocks() {
    let StreamEvent::Assistant(message) = assistant(
        r#"{"type":"tool_use","id":"toolu_01X","name":"mcp__omgcode__workspace_read_file","input":{"path":"a.txt"},"caller":{"type":"direct"}}"#,
    ) else {
        panic!("assistant expected");
    };
    assert_eq!(message.message_id.as_deref(), Some("msg_x"));
    assert_eq!(message.parent_tool_use_id, None);
    assert_eq!(message.error, None);
    assert_eq!(
        message.blocks,
        [AssistantBlock::ToolUse {
            id: "toolu_01X".into(),
            name: "mcp__omgcode__workspace_read_file".into(),
            input: serde_json::json!({"path": "a.txt"}),
        }]
    );
    let StreamEvent::Assistant(text) = assistant(r#"{"type":"text","text":"Hi"}"#) else {
        panic!("assistant expected");
    };
    assert_eq!(text.blocks, [AssistantBlock::Text("Hi".into())]);
    let StreamEvent::Assistant(thinking) =
        assistant(r#"{"type":"thinking","thinking":"hm","signature":"sig"}"#)
    else {
        panic!("assistant expected");
    };
    assert_eq!(thinking.blocks, [AssistantBlock::Other]);
}

#[test]
fn a_failed_sign_in_assistant_message_carries_its_error_code() {
    let line = r#"{"type":"assistant","message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"Not logged in"}]},"error":"authentication_failed","parent_tool_use_id":null}"#;
    let StreamEvent::Assistant(message) = event(line) else {
        panic!("assistant expected");
    };
    assert_eq!(message.message_id, None);
    assert_eq!(message.error.as_deref(), Some("authentication_failed"));
}

#[test]
fn a_sub_agent_assistant_message_keeps_its_parent_id() {
    let line = r#"{"type":"assistant","message":{"content":[]},"parent_tool_use_id":"toolu_P"}"#;
    let StreamEvent::Assistant(message) = event(line) else {
        panic!("assistant expected");
    };
    assert_eq!(message.parent_tool_use_id.as_deref(), Some("toolu_P"));
    assert!(message.blocks.is_empty());
}

#[test]
fn malformed_assistant_messages_are_errors() {
    for (line, expected) in [
        (r#"{"type":"assistant"}"#, StreamError::Shape("message")),
        (
            r#"{"type":"assistant","message":{}}"#,
            StreamError::Shape("content"),
        ),
        (
            r#"{"type":"assistant","message":{"content":[5]}}"#,
            StreamError::Shape("content"),
        ),
        (
            r#"{"type":"assistant","message":{"content":[{"text":"x"}]}}"#,
            StreamError::Shape("content type"),
        ),
        (
            r#"{"type":"assistant","message":{"content":[{"type":"text"}]}}"#,
            StreamError::Shape("text"),
        ),
        (
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"","name":"n","input":{}}]}}"#,
            StreamError::Shape("tool_use"),
        ),
        (
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"i","name":"n"}]}}"#,
            StreamError::Shape("input"),
        ),
        (
            r#"{"type":"assistant","message":{"content":[]},"error":{"a":1}}"#,
            StreamError::Shape("error"),
        ),
    ] {
        assert_eq!(parse_line(line.as_bytes()), Err(expected), "{line}");
    }
}

#[test]
fn too_many_blocks_are_refused() {
    let blocks = vec![r#"{"type":"thinking"}"#; MAX_BLOCKS + 1].join(",");
    let line = format!(r#"{{"type":"assistant","message":{{"content":[{blocks}]}}}}"#);
    assert_eq!(
        parse_line(line.as_bytes()),
        Err(StreamError::TooMany("content"))
    );
}

#[test]
fn a_success_result_is_parsed() {
    let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"final text","num_turns":2,"terminal_reason":"completed","stop_reason":"end_turn","permission_denials":[]}"#;
    assert_eq!(
        event(line),
        StreamEvent::Result(ResultReport {
            is_error: false,
            subtype: "success".into(),
            result: Some("final text".into()),
            terminal_reason: Some("completed".into()),
            stop_reason: Some("end_turn".into()),
        })
    );
}

#[test]
fn a_failed_sign_in_result_is_an_error_despite_its_success_subtype() {
    let line = r#"{"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login","terminal_reason":"api_error"}"#;
    let StreamEvent::Result(report) = event(line) else {
        panic!("result expected");
    };
    assert_eq!(report.subtype, "success");
    assert!(report.is_error);
    assert_eq!(report.terminal_reason.as_deref(), Some("api_error"));
    assert_eq!(report.stop_reason, None);
}

#[test]
fn a_result_without_is_error_or_with_a_non_string_result_is_an_error() {
    assert_eq!(
        parse_line(br#"{"type":"result","subtype":"success"}"#),
        Err(StreamError::Shape("is_error"))
    );
    assert_eq!(
        parse_line(br#"{"type":"result","subtype":"success","is_error":false,"result":3}"#),
        Err(StreamError::Shape("result"))
    );
    assert_eq!(
        parse_line(br#"{"type":"result","is_error":false}"#),
        Err(StreamError::Shape("subtype"))
    );
}

#[test]
fn invalid_and_duplicate_key_lines_are_rejected() {
    for line in [
        &b"not json"[..],
        b"{\"type\":\"result\"",
        b"{\"type\":\"result\"} trailing",
        br#"{"type":"result","type":"assistant"}"#,
        br#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"a","id":"b","name":"n","input":{}}]}}"#,
        br#"{"type":"future","nested":{"k":1,"k":2}}"#,
    ] {
        assert_eq!(parse_line(line), Err(StreamError::InvalidJson), "{line:?}");
    }
}

const TOOLS: [&str; 2] = ["read", "list"];

#[test]
fn lockdown_accepts_exactly_the_expected_session() {
    assert_eq!(check_lockdown(&init_report(), "omgcode", &TOOLS), Ok(()));
}

#[test]
fn lockdown_accepts_tools_in_any_order() {
    let mut report = init_report();
    report.tools.reverse();
    assert_eq!(check_lockdown(&report, "omgcode", &TOOLS), Ok(()));
}

#[test]
fn lockdown_refuses_an_extra_tool() {
    let mut report = init_report();
    report.tools.push("mcp__omgcode__write".into());
    assert_eq!(
        check_lockdown(&report, "omgcode", &TOOLS),
        Err(LockdownViolation::UnexpectedTool(
            "mcp__omgcode__write".into()
        ))
    );
}

#[test]
fn lockdown_refuses_a_built_in_tool() {
    let mut report = init_report();
    report.tools.push("Bash".into());
    assert_eq!(
        check_lockdown(&report, "omgcode", &TOOLS),
        Err(LockdownViolation::UnexpectedTool("Bash".into()))
    );
}

#[test]
fn lockdown_refuses_a_missing_tool() {
    let mut report = init_report();
    report.tools.pop();
    assert_eq!(
        check_lockdown(&report, "omgcode", &TOOLS),
        Err(LockdownViolation::MissingTool("mcp__omgcode__list".into()))
    );
}

#[test]
fn lockdown_refuses_a_repeated_tool() {
    let mut report = init_report();
    report.tools.push("mcp__omgcode__read".into());
    assert_eq!(
        check_lockdown(&report, "omgcode", &TOOLS),
        Err(LockdownViolation::DuplicateTool)
    );
}

#[test]
fn lockdown_refuses_tools_of_another_server_name() {
    assert_eq!(
        check_lockdown(&init_report(), "other", &TOOLS),
        Err(LockdownViolation::UnexpectedTool(
            "mcp__omgcode__read".into()
        ))
    );
}

#[test]
fn lockdown_checks_the_mcp_servers() {
    let mut report = init_report();
    report.mcp_servers[0].status = "failed".into();
    assert_eq!(
        check_lockdown(&report, "omgcode", &TOOLS),
        Err(LockdownViolation::McpServerStatus("failed".into()))
    );
    let mut report = init_report();
    report.mcp_servers[0].name = "evil".into();
    assert_eq!(
        check_lockdown(&report, "omgcode", &TOOLS),
        Err(LockdownViolation::McpServerName("evil".into()))
    );
    let mut report = init_report();
    report.mcp_servers.push(McpServerStatus {
        name: "extra".into(),
        status: "connected".into(),
    });
    assert_eq!(
        check_lockdown(&report, "omgcode", &TOOLS),
        Err(LockdownViolation::McpServerCount(2))
    );
    let mut report = init_report();
    report.mcp_servers.clear();
    assert_eq!(
        check_lockdown(&report, "omgcode", &TOOLS),
        Err(LockdownViolation::McpServerCount(0))
    );
}

#[test]
fn lockdown_refuses_plugins_skills_and_slash_commands() {
    let mut report = init_report();
    report.plugins.push("p".into());
    assert_eq!(
        check_lockdown(&report, "omgcode", &TOOLS),
        Err(LockdownViolation::PluginsPresent(1))
    );
    let mut report = init_report();
    report.skills.push("s".into());
    assert_eq!(
        check_lockdown(&report, "omgcode", &TOOLS),
        Err(LockdownViolation::SkillsPresent(1))
    );
    let mut report = init_report();
    report.slash_commands.push("compact".into());
    assert_eq!(
        check_lockdown(&report, "omgcode", &TOOLS),
        Err(LockdownViolation::SlashCommandsPresent(1))
    );
}

#[test]
fn lockdown_refuses_any_api_key_source_but_none() {
    for source in ["ANTHROPIC_API_KEY", "apiKeyHelper", "", "None"] {
        let mut report = init_report();
        report.api_key_source = source.into();
        assert_eq!(
            check_lockdown(&report, "omgcode", &TOOLS),
            Err(LockdownViolation::ApiKeySource(source.into())),
            "{source}"
        );
    }
}

#[test]
fn a_missing_or_non_string_discriminator_is_an_error() {
    for (line, field) in [
        (r#"{"type":"system"}"#, "subtype"),
        (r#"{"type":"system","subtype":3}"#, "subtype"),
        (r#"{"type":5}"#, "type"),
        (r#"{"type":"stream_event","event":{}}"#, "type"),
        (r#"{"type":"stream_event","event":{"type":7}}"#, "type"),
        (
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{}}}"#,
            "type",
        ),
        (
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":null}}}"#,
            "type",
        ),
        (
            r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{}}}"#,
            "type",
        ),
        (
            r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":false}}}"#,
            "type",
        ),
    ] {
        assert_eq!(
            parse_line(line.as_bytes()),
            Err(StreamError::Shape(field)),
            "{line}"
        );
    }
}

#[test]
fn a_line_over_the_limit_is_refused_before_parsing() {
    let line = vec![b' '; MAX_STREAM_LINE_BYTES + 1];
    assert_eq!(
        parse_line(&line),
        Err(StreamError::LineTooLong {
            limit: MAX_STREAM_LINE_BYTES
        })
    );
    let mut at_limit = br#"{"type":"future_thing"}"#.to_vec();
    at_limit.resize(MAX_STREAM_LINE_BYTES, b' ');
    assert_eq!(parse_line(&at_limit), Ok(StreamEvent::Ignored));
}
