use std::fs;
use std::path::{Path, PathBuf};

use omgcode_core::session::{
    ManifestSkill, MessageRole, RequestManifest, SessionError, SessionJournal, ToolOutcome,
};
use omgcode_core::skills::SkillName;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use uuid::Uuid;

fn setup() -> (TempDir, PathBuf, Uuid) {
    let temporary = TempDir::new().expect("private temporary directory");
    let data = temporary.path().join("application-data");
    let workspace = temporary.path().join("workspace");
    fs::create_dir_all(&workspace).expect("create workspace");
    let journal = SessionJournal::create(&data, &workspace, "Schema").expect("create session");
    let path = journal.path().to_path_buf();
    let session_id = journal.snapshot().session_id;
    drop(journal);
    (temporary, path, session_id)
}

fn read_events(path: &Path) -> Vec<Value> {
    let bytes = fs::read(path).expect("journal bytes");
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).expect("event JSON"))
        .collect()
}

fn write_events(path: &Path, events: &[Value]) {
    let mut bytes = Vec::new();
    for event in events {
        bytes.extend(serde_json::to_vec(event).expect("serialize event"));
        bytes.push(b'\n');
    }
    fs::write(path, bytes).expect("write journal");
}

fn digest(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let mut output = String::with_capacity(64);
    for byte in digest {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

fn event(session_id: Uuid, seq: u64, schema_version: u32, kind: &str, payload: Value) -> Value {
    json!({
        "schema_version": schema_version,
        "session_id": session_id,
        "event_id": Uuid::new_v4(),
        "seq": seq,
        "timestamp": "2026-11-08T00:00:00Z",
        "type": kind,
        "payload": payload
    })
}

fn first_event(path: &Path) -> Value {
    read_events(path).into_iter().next().expect("initial event")
}

fn rewrite_first_event_schema(path: &Path, schema_version: u32) -> Value {
    let mut first = first_event(path);
    first["schema_version"] = json!(schema_version);
    write_events(path, &[first.clone()]);
    first
}

fn tool_request(session_id: Uuid, seq: u64, schema: u32) -> Value {
    event(
        session_id,
        seq,
        schema,
        "tool.requested",
        json!({
            "request_id": Uuid::nil(),
            "call_id": "call-1",
            "tool": "workspace_read_file",
            "arguments": "{\"path\":\"notes.txt\"}"
        }),
    )
}

fn tool_result(session_id: Uuid, seq: u64, schema: u32) -> Value {
    event(
        session_id,
        seq,
        schema,
        "tool.completed",
        json!({
            "request_id": Uuid::nil(),
            "call_id": "call-1",
            "outcome": "succeeded",
            "output_bytes": 5,
            "output_sha256": "a".repeat(64)
        }),
    )
}

#[test]
fn tool_events_round_trip_through_replay() {
    let temporary = TempDir::new().expect("private temporary directory");
    let data = temporary.path().join("application-data");
    let workspace = temporary.path().join("workspace");
    fs::create_dir_all(&workspace).expect("create workspace");
    let mut journal = SessionJournal::create(&data, &workspace, "Tools").expect("create session");
    let request_id = journal.start_turn().expect("start turn");
    journal
        .record_tool_request(
            request_id,
            "call-1",
            "workspace_read_file",
            "{\"path\":\"notes.txt\"}",
        )
        .expect("record request");
    let text = "Tool result [call call-1]:\nhello";
    journal
        .record_tool_result(
            request_id,
            "call-1",
            ToolOutcome::Succeeded,
            None,
            u64::try_from(text.len()).expect("text length"),
            &digest(text),
        )
        .expect("record result");
    journal
        .record_tool_message("call-1", text)
        .expect("record tool message");
    journal
        .record_message(MessageRole::Assistant, "done")
        .expect("record assistant message");
    journal.complete_turn(request_id).expect("complete turn");
    let path = journal.path().to_path_buf();
    drop(journal);

    let reopened = SessionJournal::open(&path).expect("reopen v2 journal");

    assert_eq!(reopened.schema_version(), 3);
    assert_eq!(reopened.snapshot().schema_version, 3);
    assert_eq!(reopened.snapshot().active_turn, None);
    assert_eq!(reopened.snapshot().messages.len(), 2);
    assert_eq!(reopened.snapshot().messages[0].role, MessageRole::Tool);
    assert_eq!(reopened.snapshot().messages[1].role, MessageRole::Assistant);
    drop(reopened);
    let events = read_events(&path);
    assert!(events.iter().all(|event| event["schema_version"] == 3));
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "tool.requested")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "tool.completed")
            .count(),
        1
    );
}

#[test]
fn v2_replay_rejects_orphan_and_incomplete_tool_sequences() {
    let (_temporary, path, session_id) = setup();
    let first = rewrite_first_event_schema(&path, 2);

    let cases = [
        // A result without a matching request.
        vec![
            event(
                session_id,
                1,
                2,
                "turn.started",
                json!({"request_id": Uuid::nil()}),
            ),
            tool_result(session_id, 2, 2),
        ],
        // A completed turn with a pending request.
        vec![
            event(
                session_id,
                1,
                2,
                "turn.started",
                json!({"request_id": Uuid::nil()}),
            ),
            tool_request(session_id, 2, 2),
            event(
                session_id,
                3,
                2,
                "turn.completed",
                json!({"request_id": Uuid::nil()}),
            ),
        ],
        // A tool message before any result.
        vec![
            event(
                session_id,
                1,
                2,
                "turn.started",
                json!({"request_id": Uuid::nil()}),
            ),
            event(
                session_id,
                2,
                2,
                "message.recorded",
                json!({
                    "role": "tool",
                    "source": "core_tool",
                    "call_id": "call-1",
                    "text": "Tool result"
                }),
            ),
        ],
        // A duplicate call identifier.
        vec![
            event(
                session_id,
                1,
                2,
                "turn.started",
                json!({"request_id": Uuid::nil()}),
            ),
            tool_request(session_id, 2, 2),
            tool_request(session_id, 3, 2),
        ],
        // A completed result without the required tool message.
        vec![
            event(
                session_id,
                1,
                2,
                "turn.started",
                json!({"request_id": Uuid::nil()}),
            ),
            tool_request(session_id, 2, 2),
            tool_result(session_id, 3, 2),
            event(
                session_id,
                4,
                2,
                "turn.completed",
                json!({"request_id": Uuid::nil()}),
            ),
        ],
        // A malformed output digest.
        vec![
            event(
                session_id,
                1,
                2,
                "turn.started",
                json!({"request_id": Uuid::nil()}),
            ),
            tool_request(session_id, 2, 2),
            event(
                session_id,
                3,
                2,
                "tool.completed",
                json!({
                    "request_id": Uuid::nil(),
                    "call_id": "call-1",
                    "outcome": "succeeded",
                    "output_bytes": 5,
                    "output_sha256": "not-a-digest"
                }),
            ),
        ],
        // A result identity that does not match the recorded tool message.
        vec![
            event(
                session_id,
                1,
                2,
                "turn.started",
                json!({"request_id": Uuid::nil()}),
            ),
            tool_request(session_id, 2, 2),
            tool_result(session_id, 3, 2),
            event(
                session_id,
                4,
                2,
                "message.recorded",
                json!({
                    "role": "tool",
                    "source": "core_tool",
                    "call_id": "call-1",
                    "text": "hello"
                }),
            ),
            event(
                session_id,
                5,
                2,
                "turn.completed",
                json!({"request_id": Uuid::nil()}),
            ),
        ],
    ];

    for mut extra in cases {
        let mut events = vec![first.clone()];
        events.append(&mut extra);
        write_events(&path, &events);
        assert!(
            matches!(
                SessionJournal::open(&path),
                Err(SessionError::InvalidJournal(_))
            ),
            "malformed tool sequence must be rejected"
        );
    }
}

#[test]
fn mixed_schema_versions_are_rejected() {
    let (_temporary, path, session_id) = setup();
    let first = rewrite_first_event_schema(&path, 2);
    let mixed = event(
        session_id,
        1,
        1,
        "message.recorded",
        json!({"role": "user", "source": "user_cli", "text": "x"}),
    );
    write_events(&path, &[first, mixed]);

    assert!(matches!(
        SessionJournal::open(&path),
        Err(SessionError::InvalidJournal(_))
    ));
}

#[test]
fn v1_journals_reject_tool_events_and_tool_manifest_fields() {
    // A schema-v1 journal must not gain tool events.
    let (_temporary, path, session_id) = setup();
    let first = rewrite_first_event_schema(&path, 1);
    let events = vec![
        first.clone(),
        event(
            session_id,
            1,
            1,
            "turn.started",
            json!({"request_id": Uuid::nil()}),
        ),
        tool_request(session_id, 2, 1),
    ];
    write_events(&path, &events);
    assert!(matches!(
        SessionJournal::open(&path),
        Err(SessionError::InvalidJournal(_))
    ));

    // The journal API also refuses tool events on a reopened v1 journal.
    let (_temporary, path, _session_id) = setup();
    let first = rewrite_first_event_schema(&path, 1);
    write_events(&path, &[first]);
    let mut journal = SessionJournal::open(&path).expect("open v1 journal");
    let request_id = journal.start_turn().expect("start v1 turn");
    assert!(matches!(
        journal.record_tool_request(
            request_id,
            "call-1",
            "workspace_read_file",
            "{\"path\":\"notes.txt\"}"
        ),
        Err(SessionError::InvalidValue(_))
    ));

    // A v1 manifest must not carry schema-v2 tool fields.
    let (_temporary, path, session_id) = setup();
    let first = rewrite_first_event_schema(&path, 1);
    let manifest = json!({
        "request_id": Uuid::nil(),
        "manifest": {
            "base_prompt": "base",
            "prompt_version": "1",
            "prompt_sha256": "a".repeat(64),
            "custom_instructions": "",
            "model": "model",
            "base_url": "http://127.0.0.1:1234/v1",
            "context_tokens": 1000,
            "max_output_tokens": 100,
            "tool_schema_version": "0.1.0",
            "tools": ["workspace_read_file"]
        }
    });
    write_events(
        &path,
        &[
            first,
            event(
                session_id,
                1,
                1,
                "turn.started",
                json!({"request_id": Uuid::nil()}),
            ),
            event(session_id, 2, 1, "request.manifest", manifest),
        ],
    );
    assert!(matches!(
        SessionJournal::open(&path),
        Err(SessionError::InvalidJournal(_))
    ));
}

#[test]
fn failed_turns_release_pending_call_ids_for_later_turns() {
    let (_temporary, path, session_id) = setup();
    let mut first = first_event(&path);
    first["schema_version"] = json!(2);
    let first_request = Uuid::new_v4();
    let second_request = Uuid::new_v4();
    let text = "Tool result [call call-1]:\nok";
    let text_bytes = u64::try_from(text.len()).expect("text length");

    let mut events = vec![
        first,
        event(
            session_id,
            1,
            2,
            "turn.started",
            json!({"request_id": first_request}),
        ),
        event(
            session_id,
            2,
            2,
            "tool.requested",
            json!({
                "request_id": first_request,
                "call_id": "call-1",
                "tool": "workspace_read_file",
                "arguments": "{\"path\":\"notes.txt\"}"
            }),
        ),
        event(
            session_id,
            3,
            2,
            "turn.failed",
            json!({"request_id": first_request, "code": "cancelled"}),
        ),
    ];
    write_events(&path, &events);
    let reopened = SessionJournal::open(&path).expect("failed turn with pending intent reopens");
    assert_eq!(reopened.snapshot().active_turn, None);
    drop(reopened);

    events.extend([
        event(
            session_id,
            4,
            2,
            "turn.started",
            json!({"request_id": second_request}),
        ),
        event(
            session_id,
            5,
            2,
            "tool.requested",
            json!({
                "request_id": second_request,
                "call_id": "call-1",
                "tool": "workspace_read_file",
                "arguments": "{\"path\":\"notes.txt\"}"
            }),
        ),
        event(
            session_id,
            6,
            2,
            "tool.completed",
            json!({
                "request_id": second_request,
                "call_id": "call-1",
                "outcome": "succeeded",
                "output_bytes": text_bytes,
                "output_sha256": digest(text)
            }),
        ),
        event(
            session_id,
            7,
            2,
            "message.recorded",
            json!({
                "role": "tool",
                "source": "core_tool",
                "call_id": "call-1",
                "text": text
            }),
        ),
        event(
            session_id,
            8,
            2,
            "turn.completed",
            json!({"request_id": second_request}),
        ),
    ]);
    write_events(&path, &events);
    let reopened = SessionJournal::open(&path).expect("later turn may reuse the call identifier");
    assert_eq!(reopened.snapshot().active_turn, None);
    assert_eq!(reopened.snapshot().schema_version, 2);
    assert!(
        reopened
            .snapshot()
            .messages
            .iter()
            .any(|message| message.role == MessageRole::Tool)
    );
}

#[test]
fn a_schema_v2_journal_keeps_recording_and_replaying_tool_manifests() {
    let (_temporary, path, _session_id) = setup();
    rewrite_first_event_schema(&path, 2);
    let manifest = RequestManifest {
        base_prompt: "base instructions".into(),
        prompt_version: "1".into(),
        prompt_sha256: "a".repeat(64),
        custom_instructions: String::new(),
        model: "local-model".into(),
        base_url: "http://127.0.0.1:1234/v1".into(),
        context_tokens: 32_000,
        max_output_tokens: 2_000,
        tool_schema_version: Some("1".into()),
        tools: Some(vec!["workspace_read_file".into()]),
        skills: Vec::new(),
    };

    let mut journal = SessionJournal::open(&path).expect("open schema-v2 journal");
    assert_eq!(journal.schema_version(), 2);
    let request_id = journal.start_turn().expect("start turn");
    journal
        .record_message(MessageRole::User, "read notes")
        .expect("user message");
    journal
        .record_manifest(request_id, manifest.clone())
        .expect("schema v2 accepts a tool manifest");
    journal.complete_turn(request_id).expect("complete turn");
    drop(journal);

    let reopened = SessionJournal::open(&path).expect("replay a schema-v2 tool manifest");
    assert_eq!(reopened.schema_version(), 2);
    reopened
        .preflight_request("next", &manifest)
        .expect("preflight a later tool-enabled turn");
    drop(reopened);
    let events = read_events(&path);
    assert!(events.iter().all(|event| event["schema_version"] == 2));
}

#[test]
fn given_a_version_2_journal_when_a_skill_manifest_is_recorded_or_replayed_then_it_is_refused() {
    let skills = vec![ManifestSkill {
        skill_id: Uuid::new_v4(),
        name: SkillName::new("demo-skill").expect("skill name"),
        skill_md_sha256: "b".repeat(64),
    }];
    let manifest = RequestManifest {
        base_prompt: "base instructions".into(),
        prompt_version: "1".into(),
        prompt_sha256: "a".repeat(64),
        custom_instructions: String::new(),
        model: "local-model".into(),
        base_url: "http://127.0.0.1:1234/v1".into(),
        context_tokens: 32_000,
        max_output_tokens: 2_000,
        tool_schema_version: None,
        tools: None,
        skills,
    };

    // The journal API refuses the field on a writable version 2 journal.
    let (_temporary, path, session_id) = setup();
    let first = rewrite_first_event_schema(&path, 2);
    let mut journal = SessionJournal::open(&path).expect("open schema-v2 journal");
    assert!(!journal.supports_skill_manifests());
    let request_id = journal.start_turn().expect("start turn");
    journal
        .record_message(MessageRole::User, "review")
        .expect("user message");
    assert!(matches!(
        journal.record_manifest(request_id, manifest.clone()),
        Err(SessionError::InvalidValue(_))
    ));
    drop(journal);

    // Replay refuses a version 2 line that carries it.
    let payload = json!({
        "request_id": Uuid::nil(),
        "manifest": serde_json::to_value(&manifest).expect("manifest JSON"),
    });
    write_events(
        &path,
        &[
            first,
            event(
                session_id,
                1,
                2,
                "turn.started",
                json!({"request_id": Uuid::nil()}),
            ),
            event(session_id, 2, 2, "request.manifest", payload),
        ],
    );
    assert!(matches!(
        SessionJournal::open(&path),
        Err(SessionError::InvalidJournal(_))
    ));

    // A version 3 journal accepts the same manifest.
    let (_temporary, path, _session_id) = setup();
    let mut journal = SessionJournal::open(&path).expect("open schema-v3 journal");
    assert!(journal.supports_skill_manifests());
    let request_id = journal.start_turn().expect("start turn");
    journal
        .record_message(MessageRole::User, "review")
        .expect("user message");
    journal
        .record_manifest(request_id, manifest)
        .expect("schema v3 accepts a skill manifest");
}

/// Argument JSON of exactly `bytes` bytes whose second JSON escaping inside the
/// `tool.requested` event is as large as the strict-JSON grammar allows.
fn worst_case_arguments(shape: &str, bytes: usize) -> String {
    let arguments = match shape {
        // Each backslash pair in the text becomes four bytes in the event.
        "backslashes" => format!("{{\"a\":\"{}\"}}", "\\\\".repeat((bytes - 8) / 2)),
        // Each escaped quote in the text becomes four bytes in the event.
        "quotes" => format!("{{\"a\":\"{}\"}}", "\\\"".repeat((bytes - 8) / 2)),
        // Whitespace between tokens is the only raw control character JSON
        // allows; each newline becomes two bytes in the event.
        "newlines" => format!("{{{}}}", "\n".repeat(bytes - 2)),
        // A control character inside a string must be a six-byte escape in
        // the text, which the event escapes again as seven bytes.
        "control_escapes" => format!(
            "{{\"a\":\"{}\"}}",
            format!("{}u0001", '\\').repeat((bytes - 8) / 6)
        ),
        other => panic!("unknown shape {other}"),
    };
    assert!(arguments.len() <= bytes && arguments.len() + 6 > bytes);
    arguments
}

#[test]
fn the_worst_case_tool_requested_event_with_32_kib_of_arguments_fits_the_event_bound() {
    let temporary = TempDir::new().expect("private temporary directory");
    let data = temporary.path().join("application-data");
    let workspace = temporary.path().join("workspace");
    fs::create_dir_all(&workspace).expect("create workspace");
    let mut journal = SessionJournal::create(&data, &workspace, "Bound").expect("create session");
    let request_id = journal.start_turn().expect("start turn");
    let shapes = ["backslashes", "quotes", "newlines", "control_escapes"];
    for (index, shape) in shapes.into_iter().enumerate() {
        // The longest identifier is all backslashes, which the event escapes.
        let call_id = format!("{}{index}", "\\".repeat(127));
        let tool = "w".repeat(64);
        let arguments = worst_case_arguments(shape, 32 * 1024);
        journal
            .record_tool_request(request_id, &call_id, &tool, &arguments)
            .unwrap_or_else(|error| panic!("{shape} arguments are recorded: {error:?}"));
        let text = "Tool error [call x]:\n{}";
        journal
            .record_tool_result(
                request_id,
                &call_id,
                ToolOutcome::Failed,
                Some("arguments_too_large"),
                u64::try_from(text.len()).expect("text length"),
                &digest(text),
            )
            .expect("record result");
        journal
            .record_tool_message(&call_id, text)
            .expect("record tool message");
    }
    journal
        .record_message(MessageRole::Assistant, "done")
        .expect("assistant message");
    journal.complete_turn(request_id).expect("complete turn");
    let path = journal.path().to_path_buf();
    drop(journal);

    let sizes: Vec<(&str, usize)> = fs::read(&path)
        .expect("journal bytes")
        .split(|byte| *byte == b'\n')
        .filter(|line| contains(line, b"\"tool.requested\""))
        .zip(shapes)
        .map(|(line, shape)| (shape, line.len() + 1))
        .collect();
    assert_eq!(sizes.len(), 4);
    for (shape, size) in &sizes {
        assert!(*size <= 80 * 1024, "{shape} event is {size} bytes");
    }
    let largest = sizes.iter().map(|(_, size)| *size).max().expect("sizes");
    assert!(
        largest > 64 * 1024,
        "the worst case ({largest} bytes) is why the old 64 KiB bound is not enough"
    );
    // The journal, with its large events, reads back.
    let reopened = SessionJournal::open(&path).expect("reopen with 32 KiB requests");
    assert_eq!(reopened.tool_requests().len(), 4);
    assert!(
        reopened
            .tool_requests()
            .iter()
            .all(|request| request.arguments.len() > 32 * 1024 - 8)
    );
}

#[test]
fn an_intent_record_refuses_arguments_over_32_kib() {
    let temporary = TempDir::new().expect("private temporary directory");
    let data = temporary.path().join("application-data");
    let workspace = temporary.path().join("workspace");
    fs::create_dir_all(&workspace).expect("create workspace");
    let mut journal = SessionJournal::create(&data, &workspace, "Bound").expect("create session");
    let request_id = journal.start_turn().expect("start turn");
    let arguments = worst_case_arguments("newlines", 32 * 1024 + 8);

    let error = journal
        .record_tool_request(request_id, "call-1", "workspace_create_file", &arguments)
        .expect_err("33 KiB arguments are refused");

    assert!(matches!(error, SessionError::InvalidValue(_)), "{error:?}");
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
