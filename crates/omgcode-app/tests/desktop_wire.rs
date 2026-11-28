//! The wire contract of the desktop service: exact JSON keys and types of every
//! response, the closed request shapes, and agreement with the TypeScript
//! declarations in `crates/omgcode-desktop/ui/src/ipc/types.ts`.

use std::collections::BTreeSet;
use std::fs;

use omgcode_app::desktop::{
    AllowHostView, ApprovalDecideRequest, ApprovalDetail, ApprovalField, ApprovalList,
    ApprovalOperationDto, ApprovalRequest, ApprovalStateDto, ApprovalView, ApprovalsSummary,
    Binding, ChatEvent, ChatOutcome, ChatSendRequest, ChatSettingsDto, ChatState, ChatStatus,
    ContextMeterDto, Decision, DesktopError, EntryKind, MessageRoleDto, ReasoningEffort, SearchHit,
    SearchRequest, SearchResult, SearchRoleDto, SessionCreateRequest, SessionRequest,
    SessionStatus, SessionSummary, SessionTree, SnippetPart, ToolOutcomeDto, ToolsMode,
    TranscriptEntry, TranscriptPage, TranscriptRequest, TreeRequest, TreeSort, TreeView,
    WorkspaceChoice, WorkspaceGroup, WorkspaceList,
};
use omgcode_core::policy::Access;
use serde_json::{Value, json};
use uuid::Uuid;

mod common;
use common::{json_of, text_of, wire};

const TYPES_TS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../omgcode-desktop/ui/src/ipc/types.ts"
);
const CLIENT_TS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../omgcode-desktop/ui/src/ipc/client.ts"
);
const APPROVALS_CLIENT_TS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../omgcode-desktop/ui/src/ipc/approvals_client.ts"
);

/// Assert that `value` is an object with exactly these keys and JSON kinds.
/// A kind is `string`, `number`, `bool`, `array`, `object`, or one of those
/// followed by `|null`.
fn assert_shape(value: &Value, expected: &[(&str, &str)]) {
    let object = value.as_object().expect("a JSON object");
    let actual: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    let wanted: BTreeSet<&str> = expected.iter().map(|(key, _)| *key).collect();
    assert_eq!(actual, wanted, "exact key set of {value}");
    for (key, kind) in expected {
        let field = &object[*key];
        let (base, nullable) = kind
            .strip_suffix("|null")
            .map_or((*kind, false), |base| (base, true));
        let matches = match base {
            "string" => field.is_string(),
            "number" => field.is_u64() || field.is_i64(),
            "bool" => field.is_boolean(),
            "array" => field.is_array(),
            "object" => field.is_object(),
            other => panic!("unknown kind {other}"),
        };
        assert!(
            matches || (nullable && field.is_null()),
            "{key} must be {kind}, got {field}"
        );
    }
}

fn id() -> Uuid {
    Uuid::new_v4()
}

fn summary(created: Option<&str>, invalid: Option<&str>) -> SessionSummary {
    SessionSummary {
        session_id: id(),
        title: "Title".to_owned(),
        created_at: created.map(str::to_owned),
        updated_at: created.map(str::to_owned),
        status: SessionStatus::Idle,
        archived: false,
        event_count: 3,
        invalid_code: invalid.map(str::to_owned),
    }
}

const SUMMARY_SHAPE: &[(&str, &str)] = &[
    ("session_id", "string"),
    ("title", "string"),
    ("created_at", "string|null"),
    ("updated_at", "string|null"),
    ("status", "string"),
    ("archived", "bool"),
    ("event_count", "number"),
    ("invalid_code", "string|null"),
];

fn approval_view(with_labels: bool) -> ApprovalView {
    ApprovalView {
        id: id(),
        session_id: id(),
        session_title: with_labels.then(|| "Session".to_owned()),
        folder_display: with_labels.then(|| "/work".to_owned()),
        operation: ApprovalOperationDto::FileWrite,
        summary: "edit".to_owned(),
        state: ApprovalStateDto::Pending,
        created_unix: 1,
        expires_unix: 2,
        request_sha256: "ab".repeat(32),
        writers_stopped: false,
    }
}

const APPROVAL_VIEW_SHAPE: &[(&str, &str)] = &[
    ("id", "string"),
    ("session_id", "string"),
    ("session_title", "string|null"),
    ("folder_display", "string|null"),
    ("operation", "string"),
    ("summary", "string"),
    ("state", "string"),
    ("created_unix", "number"),
    ("expires_unix", "number"),
    ("request_sha256", "string"),
    ("writers_stopped", "bool"),
];

fn outcome() -> ChatOutcome {
    ChatOutcome {
        finish_reason: "stop".to_owned(),
        tools_mode: ToolsMode::Disabled,
        tools_disabled_reason: Some("reason".to_owned()),
        rounds: 0,
        tool_calls: 0,
        withheld_tool_results: 0,
        context: Some(ContextMeterDto {
            estimated_tokens: 1,
            trigger_tokens: 2,
            context_tokens: 3,
            compacted: false,
        }),
    }
}

#[test]
fn session_summaries_have_exactly_the_declared_keys_and_nullability() {
    assert_shape(
        &json_of(&summary(Some("2026-01-01T00:00:00Z"), None)),
        SUMMARY_SHAPE,
    );
    let bare = json_of(&summary(None, Some("invalid_header")));
    assert_shape(&bare, SUMMARY_SHAPE);
    assert!(bare["created_at"].is_null() && bare["invalid_code"].is_string());
}

#[test]
fn the_tree_search_and_transcript_responses_have_exactly_the_declared_keys() {
    let group = WorkspaceGroup {
        group_id: "0123456789abcdef".to_owned(),
        folder_display: "/work".to_owned(),
        sessions: vec![summary(None, None)],
    };
    let tree = SessionTree {
        groups: vec![group],
        indexed_sessions: 1,
        skipped_entries: 0,
    };
    let tree = json_of(&tree);
    assert_shape(
        &tree,
        &[
            ("groups", "array"),
            ("indexed_sessions", "number"),
            ("skipped_entries", "number"),
        ],
    );
    assert_shape(
        &tree["groups"][0],
        &[
            ("group_id", "string"),
            ("folder_display", "string"),
            ("sessions", "array"),
        ],
    );

    let hit = |seq: Option<u64>| SearchHit {
        session: summary(None, None),
        folder_display: "/work".to_owned(),
        seq,
        role: SearchRoleDto::Summary,
        snippet: vec![SnippetPart {
            text: "x".to_owned(),
            matched: true,
        }],
    };
    let result = json_of(&SearchResult {
        hits: vec![hit(Some(4)), hit(None)],
        truncated: false,
    });
    assert_shape(&result, &[("hits", "array"), ("truncated", "bool")]);
    for (index, kind) in [(0, "number"), (1, "number|null")] {
        assert_shape(
            &result["hits"][index],
            &[
                ("session", "object"),
                ("folder_display", "string"),
                ("seq", kind),
                ("role", "string"),
                ("snippet", "array"),
            ],
        );
    }
    assert_shape(
        &result["hits"][0]["snippet"][0],
        &[("text", "string"), ("matched", "bool")],
    );

    let page = json_of(&TranscriptPage {
        session: summary(None, None),
        folder_display: "/work".to_owned(),
        entries: Vec::new(),
        next_after_seq: None,
        incomplete_tail: false,
    });
    assert_shape(
        &page,
        &[
            ("session", "object"),
            ("folder_display", "string"),
            ("entries", "array"),
            ("next_after_seq", "number|null"),
            ("incomplete_tail", "bool"),
        ],
    );
}

#[test]
fn chat_events_have_exactly_the_declared_shapes() {
    // Given one of each pushed chat event.
    let session = id();
    let events = [
        (
            ChatEvent::RequestStarted {
                session_id: session,
                request: 1,
            },
            json!({"kind": "request_started", "session_id": session, "request": 1}),
        ),
        (
            ChatEvent::Delta {
                session_id: session,
                request: 2,
                text: "hi \u{e9}".to_owned(),
            },
            json!({"kind": "delta", "session_id": session, "request": 2, "text": "hi \u{e9}"}),
        ),
        (
            ChatEvent::PreviewTruncated {
                session_id: session,
                request: 3,
            },
            json!({"kind": "preview_truncated", "session_id": session, "request": 3}),
        ),
        (
            ChatEvent::Finished {
                session_id: session,
            },
            json!({"kind": "finished", "session_id": session}),
        ),
    ];

    // When each is serialized, then it is exactly the pinned JSON.
    for (event, expected) in events {
        assert_eq!(json_of(&event), expected);
    }
}

#[test]
fn chat_responses_have_exactly_the_declared_keys() {
    let completed = json_of(&ChatStatus {
        session_id: id(),
        state: ChatState::Completed,
        outcome: Some(outcome()),
        error: None,
    });
    assert_shape(
        &completed,
        &[
            ("session_id", "string"),
            ("state", "string"),
            ("outcome", "object|null"),
            ("error", "object|null"),
        ],
    );
    assert_shape(
        &completed["outcome"],
        &[
            ("finish_reason", "string"),
            ("tools_mode", "string"),
            ("tools_disabled_reason", "string|null"),
            ("rounds", "number"),
            ("tool_calls", "number"),
            ("withheld_tool_results", "number"),
            ("context", "object|null"),
        ],
    );
    assert_shape(
        &completed["outcome"]["context"],
        &[
            ("estimated_tokens", "number"),
            ("trigger_tokens", "number"),
            ("context_tokens", "number"),
            ("compacted", "bool"),
        ],
    );
    assert!(completed["error"].is_null());

    // A failed status carries the error shape and no outcome.
    let failed = ChatStatus {
        session_id: id(),
        state: ChatState::Failed,
        outcome: None,
        error: Some(error_value()),
    };
    let failed = json_of(&failed);
    assert!(failed["outcome"].is_null());
    assert_shape(
        &failed["error"],
        &[("code", "string"), ("message", "string")],
    );
}

/// A `DesktopError` as the service produces it, without a public constructor:
/// a request the service refuses without touching storage.
fn error_value() -> DesktopError {
    let directory = tempfile::tempdir().expect("data root");
    let service = omgcode_app::desktop::DesktopService::open_with_token_source(
        omgcode_app::desktop::DesktopConfig {
            data_root: directory.path().to_owned(),
            endpoint: "http://127.0.0.1:1/v1".to_owned(),
            token_source: omgcode_app::TokenSource::Environment,
            stream: true,
        },
        common::no_token,
    )
    .expect("service");
    service
        .chat_send(wire::<ChatSendRequest>(json!({
            "session_id": id(),
            "message": "  ",
            "settings": common::settings_json("http://127.0.0.1:1/v1"),
            "skills": [],
        })))
        .expect_err("an empty message is refused")
}

#[test]
fn a_desktop_error_serializes_as_exactly_code_and_message() {
    let error = error_value();
    let value = json_of(&error);
    assert_shape(&value, &[("code", "string"), ("message", "string")]);
    assert_eq!(value["code"], "invalid_request");
    assert_eq!(error.code(), "invalid_request");
}

#[test]
fn workspace_and_approval_responses_have_exactly_the_declared_keys() {
    let list = json_of(&WorkspaceList {
        development_root_set: true,
        workspaces: vec![WorkspaceChoice {
            workspace_id: id(),
            folder_display: "/work".to_owned(),
            model_tools: true,
            read: Access::Allow,
            write: Access::Deny,
            execute: Access::Prompt,
        }],
    });
    assert_shape(
        &list,
        &[("development_root_set", "bool"), ("workspaces", "array")],
    );
    assert_shape(
        &list["workspaces"][0],
        &[
            ("workspace_id", "string"),
            ("folder_display", "string"),
            ("model_tools", "bool"),
            ("read", "string"),
            ("write", "string"),
            ("execute", "string"),
        ],
    );
    assert_shape(
        &json_of(&ApprovalsSummary { pending: 2 }),
        &[("pending", "number")],
    );

    assert_shape(&json_of(&approval_view(true)), APPROVAL_VIEW_SHAPE);
    let unlabelled = json_of(&approval_view(false));
    assert_shape(&unlabelled, APPROVAL_VIEW_SHAPE);
    assert!(unlabelled["session_title"].is_null() && unlabelled["folder_display"].is_null());

    let approvals = json_of(&ApprovalList {
        now_unix: 9,
        approvals: vec![approval_view(true)],
    });
    assert_shape(
        &approvals,
        &[("now_unix", "number"), ("approvals", "array")],
    );

    let detail = |binding, token: Option<&str>| ApprovalDetail {
        approval: approval_view(true),
        binding,
        tool: token.map(|_| "workspace_edit_file".to_owned()),
        fields: vec![ApprovalField {
            label: "File".to_owned(),
            value: "a".to_owned(),
            multiline: false,
        }],
        review_token: token.map(str::to_owned),
        waiting: token.is_some(),
        allow_host: token.map(|_| AllowHostView::Offered {
            host: "docs.example.com".to_owned(),
        }),
    };
    for (value, tool_kind) in [
        (detail(Binding::Exact, Some("token")), "string"),
        (detail(Binding::Stale, None), "string|null"),
    ] {
        let value = json_of(&value);
        assert_shape(
            &value,
            &[
                ("approval", "object"),
                ("binding", "string"),
                ("tool", tool_kind),
                ("fields", "array"),
                ("review_token", "string|null"),
                ("waiting", "bool"),
                ("allow_host", "object|null"),
            ],
        );
        assert_shape(
            &value["fields"][0],
            &[
                ("label", "string"),
                ("value", "string"),
                ("multiline", "bool"),
            ],
        );
    }

    // Only an offer carries a host; every other state is its tag alone.
    assert_shape(
        &json_of(&AllowHostView::Offered {
            host: "docs.example.com".to_owned(),
        }),
        &[("state", "string"), ("host", "string")],
    );
    for view in [
        AllowHostView::NotAllow,
        AllowHostView::Full,
        AllowHostView::Unavailable,
    ] {
        assert_shape(&json_of(&view), &[("state", "string")]);
    }
}

#[test]
fn every_transcript_entry_kind_has_exactly_its_declared_keys() {
    let request = id();
    let entries = [
        (
            EntryKind::Message {
                role: MessageRoleDto::Tool,
                source: "core_tool".to_owned(),
                text: "t".to_owned(),
                call_id: None,
            },
            "message",
            vec![
                ("role", "string"),
                ("source", "string"),
                ("text", "string"),
                ("call_id", "string|null"),
            ],
        ),
        (
            EntryKind::TurnStarted {
                request_id: request,
            },
            "turn_started",
            vec![("request_id", "string")],
        ),
        (
            EntryKind::TurnCompleted {
                request_id: request,
            },
            "turn_completed",
            vec![("request_id", "string")],
        ),
        (
            EntryKind::TurnFailed {
                request_id: request,
                code: Some("c".to_owned()),
            },
            "turn_failed",
            vec![("request_id", "string"), ("code", "string|null")],
        ),
        (
            EntryKind::ToolRequested {
                request_id: request,
                call_id: "c".to_owned(),
                tool: "t".to_owned(),
                arguments: "{}".to_owned(),
            },
            "tool_requested",
            vec![
                ("request_id", "string"),
                ("call_id", "string"),
                ("tool", "string"),
                ("arguments", "string"),
            ],
        ),
        (
            EntryKind::ToolCompleted {
                request_id: request,
                call_id: "c".to_owned(),
                outcome: ToolOutcomeDto::Denied,
                code: None,
                output_bytes: 4,
            },
            "tool_completed",
            vec![
                ("request_id", "string"),
                ("call_id", "string"),
                ("outcome", "string"),
                ("code", "string|null"),
                ("output_bytes", "number"),
            ],
        ),
        (
            EntryKind::RequestManifest {
                request_id: request,
                model: "m".to_owned(),
                prompt_version: "v".to_owned(),
                tools: vec!["a".to_owned()],
            },
            "request_manifest",
            vec![
                ("request_id", "string"),
                ("model", "string"),
                ("prompt_version", "string"),
                ("tools", "array"),
            ],
        ),
        (
            EntryKind::ContextCompacted {
                summary: "s".to_owned(),
                compacted_messages: 2,
                model: "m".to_owned(),
            },
            "context_compacted",
            vec![
                ("summary", "string"),
                ("compacted_messages", "number"),
                ("model", "string"),
            ],
        ),
        (EntryKind::SessionArchived, "session_archived", Vec::new()),
        (EntryKind::SessionRestored, "session_restored", Vec::new()),
    ];
    let ts = fs::read_to_string(TYPES_TS).expect("types.ts");
    for (kind, name, fields) in entries {
        let value = json_of(&TranscriptEntry {
            seq: 3,
            timestamp: "2026-01-01T00:00:00Z".to_owned(),
            kind,
        });
        let mut expected = vec![
            ("seq", "number"),
            ("timestamp", "string"),
            ("kind", "string"),
        ];
        expected.extend(fields);
        assert_shape(&value, &expected);
        assert_eq!(value["kind"], name);
        // The TypeScript variant declares the same fields.
        let declared = variant_fields(&ts, name);
        let serialized: BTreeSet<String> =
            value.as_object().expect("object").keys().cloned().collect();
        assert_eq!(declared, serialized, "TypeScript variant {name}");
    }
}

/// The field names that `types.ts` declares for the transcript variant `kind`,
/// including the shared `seq` and `timestamp`.
fn variant_fields(ts: &str, kind: &str) -> BTreeSet<String> {
    let marker = format!("kind: \"{kind}\"");
    let at = ts.find(&marker).unwrap_or_else(|| panic!("variant {kind}"));
    let start = ts[..at].rfind('{').expect("opening brace");
    let end = at + ts[at..].find('}').expect("closing brace");
    let mut names: BTreeSet<String> = declared_names(&ts[start + 1..end]);
    names.insert("seq".to_owned());
    names.insert("timestamp".to_owned());
    names
}

/// Names before a colon in a `;`, `,`, or newline separated field list.
fn declared_names(body: &str) -> BTreeSet<String> {
    body.split([';', ',', '\n'])
        .filter_map(|piece| {
            let (name, _) = piece.trim().split_once(':')?;
            let name = name.trim().trim_end_matches('?');
            (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
                .then(|| name.to_owned())
        })
        .collect()
}

/// The field names of `export interface <name>`.
fn interface_fields(ts: &str, name: &str) -> BTreeSet<String> {
    let header = format!("export interface {name} {{");
    let start = ts
        .find(&header)
        .unwrap_or_else(|| panic!("interface {name}"));
    let body = &ts[start + header.len()..];
    let end = body.find("\n}").expect("end of interface");
    declared_names(&body[..end])
}

fn keys_of(value: &Value) -> BTreeSet<String> {
    value.as_object().expect("object").keys().cloned().collect()
}

#[test]
fn every_serialized_interface_has_exactly_the_fields_types_ts_declares() {
    let ts = fs::read_to_string(TYPES_TS).expect("types.ts");
    let error = json_of(&error_value());
    let tree = json_of(&SessionTree {
        groups: vec![WorkspaceGroup {
            group_id: "g".to_owned(),
            folder_display: "f".to_owned(),
            sessions: vec![summary(None, None)],
        }],
        indexed_sessions: 0,
        skipped_entries: 0,
    });
    let hit = json_of(&SearchHit {
        session: summary(None, None),
        folder_display: "f".to_owned(),
        seq: None,
        role: SearchRoleDto::Title,
        snippet: vec![SnippetPart {
            text: "t".to_owned(),
            matched: false,
        }],
    });
    let page = json_of(&TranscriptPage {
        session: summary(None, None),
        folder_display: "f".to_owned(),
        entries: Vec::new(),
        next_after_seq: None,
        incomplete_tail: false,
    });
    let status = json_of(&ChatStatus {
        session_id: id(),
        state: ChatState::Idle,
        outcome: Some(outcome()),
        error: None,
    });
    let settings = json_of(&ChatSettingsDto {
        endpoint: "e".to_owned(),
        model: "m".to_owned(),
        context_tokens: 1,
        max_output_tokens: 1,
        max_tool_calls: 128,
        turn_timeout_seconds: 120,
        instructions: String::new(),
        compact_threshold: 1,
        auto_compact: false,
        // An effort is omitted when none is chosen (ADR-0102); a chosen one
        // shows the optional key `types.ts` declares.
        reasoning_effort: Some(ReasoningEffort::High),
    });
    let workspaces = json_of(&WorkspaceList {
        development_root_set: false,
        workspaces: vec![WorkspaceChoice {
            workspace_id: id(),
            folder_display: "f".to_owned(),
            model_tools: false,
            read: Access::Deny,
            write: Access::Deny,
            execute: Access::Deny,
        }],
    });
    let detail = json_of(&ApprovalDetail {
        approval: approval_view(false),
        binding: Binding::NotFound,
        tool: None,
        fields: vec![ApprovalField {
            label: "l".to_owned(),
            value: "v".to_owned(),
            multiline: true,
        }],
        review_token: None,
        waiting: false,
        allow_host: None,
    });
    let list = json_of(&ApprovalList {
        now_unix: 1,
        approvals: vec![approval_view(false)],
    });
    let checks: Vec<(&str, BTreeSet<String>)> = vec![
        ("IpcError", keys_of(&error)),
        ("SessionSummary", keys_of(&tree["groups"][0]["sessions"][0])),
        ("WorkspaceGroup", keys_of(&tree["groups"][0])),
        ("SessionTree", keys_of(&tree)),
        ("SnippetPart", keys_of(&hit["snippet"][0])),
        ("SearchHit", keys_of(&hit)),
        (
            "SearchResult",
            keys_of(&json_of(&SearchResult {
                hits: Vec::new(),
                truncated: false,
            })),
        ),
        ("TranscriptPage", keys_of(&page)),
        ("ChatSettings", keys_of(&settings)),
        ("ContextMeter", keys_of(&status["outcome"]["context"])),
        ("ChatOutcome", keys_of(&status["outcome"])),
        ("ChatStatus", keys_of(&status)),
        ("WorkspaceChoice", keys_of(&workspaces["workspaces"][0])),
        ("WorkspaceList", keys_of(&workspaces)),
        ("ApprovalView", keys_of(&list["approvals"][0])),
        ("ApprovalList", keys_of(&list)),
        ("ApprovalField", keys_of(&detail["fields"][0])),
        ("ApprovalDetail", keys_of(&detail)),
    ];
    for (name, serialized) in checks {
        assert_eq!(interface_fields(&ts, name), serialized, "interface {name}");
    }
}

#[test]
fn closed_unions_serialize_to_the_values_types_ts_declares() {
    let ts = fs::read_to_string(TYPES_TS).expect("types.ts");
    let union = |name: &str| -> BTreeSet<String> {
        let header = format!("export type {name} = ");
        let start = ts.find(&header).expect("type alias") + header.len();
        let end = start + ts[start..].find(';').expect("semicolon");
        ts[start..end]
            .split('|')
            .map(|part| part.trim().trim_matches('"').to_owned())
            .collect()
    };
    let strings = |values: Vec<Value>| -> BTreeSet<String> {
        values
            .iter()
            .map(|value| value.as_str().expect("string").to_owned())
            .collect()
    };
    assert_eq!(
        union("ReasoningEffort"),
        strings(ReasoningEffort::ALL.iter().map(json_of).collect())
    );
    assert_eq!(
        union("SessionStatus"),
        strings(
            [
                SessionStatus::Idle,
                SessionStatus::Running,
                SessionStatus::TurnOpen,
                SessionStatus::Busy,
                SessionStatus::Invalid
            ]
            .iter()
            .map(json_of)
            .collect()
        )
    );
    assert_eq!(
        union("ApprovalState"),
        strings(
            [
                ApprovalStateDto::Pending,
                ApprovalStateDto::Approved,
                ApprovalStateDto::Denied,
                ApprovalStateDto::Consumed,
                ApprovalStateDto::Expired
            ]
            .iter()
            .map(json_of)
            .collect()
        )
    );
    // Every decision the window can send is one Rust accepts, and no more.
    let decisions: Vec<Decision> = union("ApprovalDecision")
        .iter()
        .map(|decision| serde_json::from_value(json!(decision)).expect("a decision"))
        .collect();
    assert_eq!(
        decisions,
        [Decision::AllowHost, Decision::Approve, Decision::Deny],
        "sorted by wire name"
    );
    assert_eq!(
        union("AllowHostState"),
        strings(
            [
                AllowHostView::Offered {
                    host: "docs.example.com".to_owned()
                },
                AllowHostView::NotAllow,
                AllowHostView::Full,
                AllowHostView::Unavailable
            ]
            .iter()
            .map(|view| json_of(view)["state"].clone())
            .collect()
        )
    );
    assert_eq!(
        union("ApprovalOperation"),
        strings(
            [
                ApprovalOperationDto::Command,
                ApprovalOperationDto::FileRead,
                ApprovalOperationDto::FileWrite,
                ApprovalOperationDto::GitPush,
                ApprovalOperationDto::Network
            ]
            .iter()
            .map(json_of)
            .collect()
        )
    );
    assert_eq!(
        union("TreeView"),
        strings(
            [TreeView::Active, TreeView::Archived]
                .iter()
                .map(json_of)
                .collect()
        )
    );
    assert_eq!(
        union("TreeSort"),
        strings(
            [TreeSort::Updated, TreeSort::Created, TreeSort::Title]
                .iter()
                .map(json_of)
                .collect()
        )
    );
    let chat_states: BTreeSet<String> = strings(
        [
            ChatState::Idle,
            ChatState::Running,
            ChatState::Cancelling,
            ChatState::Completed,
            ChatState::Failed,
        ]
        .iter()
        .map(json_of)
        .collect(),
    );
    assert!(
        ts.contains("state: \"idle\" | \"running\" | \"cancelling\" | \"completed\" | \"failed\";")
    );
    assert_eq!(chat_states.len(), 5);
    assert!(ts.contains("binding: \"exact\" | \"stale\" | \"not_found\";"));
    assert_eq!(json_of(&Binding::NotFound), "not_found");
    assert!(ts.contains("role: \"user\" | \"assistant\" | \"tool\" | \"summary\" | \"title\";"));
    assert_eq!(json_of(&SearchRoleDto::Summary), "summary");
}

#[test]
fn the_summary_command_returns_the_object_the_client_declares() {
    let client = fs::read_to_string(CLIENT_TS).expect("client.ts");
    assert!(client.contains("invokeCommand<{ pending: number }>(\"approvals_summary\""));
    assert_shape(
        &json_of(&ApprovalsSummary { pending: 0 }),
        &[("pending", "number")],
    );
}

#[test]
fn requests_match_the_argument_objects_the_client_sends() {
    let client = fs::read_to_string(CLIENT_TS).expect("client.ts");
    let approvals = fs::read_to_string(APPROVALS_CLIENT_TS).expect("approvals_client.ts");
    let session = id();

    assert!(client.contains("\"session_tree\", { view, sort, descending }"));
    let tree: TreeRequest = wire(json!({"view": "active", "sort": "updated", "descending": true}));
    assert_eq!(tree.sort, TreeSort::Updated);

    assert!(client.contains("\"session_search\", { query, view, limit }"));
    let search: SearchRequest = wire(json!({"query": "q", "view": "archived", "limit": 20}));
    assert_eq!(search.view, TreeView::Archived);

    assert!(client.contains("session_id: sessionId,\n      after_seq: afterSeq,\n      limit,"));
    let transcript: TranscriptRequest =
        wire(json!({"session_id": session, "after_seq": null, "limit": 50}));
    assert_eq!(transcript.after_seq, None);
    let transcript: TranscriptRequest =
        wire(json!({"session_id": session, "after_seq": 7, "limit": 50}));
    assert_eq!(transcript.after_seq, Some(7));

    assert!(client.contains("\"session_archive\", { session_id: sessionId }"));
    let by_session: SessionRequest = wire(json!({"session_id": session}));
    assert_eq!(by_session.session_id, session);

    assert!(client.contains("{ workspace_id: workspaceId, title }"));
    let create: SessionCreateRequest = wire(json!({"workspace_id": session, "title": "t"}));
    assert_eq!(create.title, "t");

    assert!(client.contains("{ session_id: sessionId, message, settings, skills }"));
    let send: ChatSendRequest = wire(json!({
        "session_id": session,
        "message": "m",
        "settings": common::settings_json("http://127.0.0.1:1/v1"),
        "skills": [],
    }));
    assert_eq!(send.message, "m");

    assert!(approvals.contains("\"approval_detail\", { id }"));
    let detail: ApprovalRequest = wire(json!({"id": session}));
    assert_eq!(detail.id, session);

    assert!(
        approvals.contains("writers_stopped: writersStopped,\n      review_token: reviewToken,")
    );
    let decide: ApprovalDecideRequest = wire(json!({
        "id": session,
        "decision": "approve",
        "writers_stopped": true,
        "review_token": null,
    }));
    assert_eq!(decide.decision, Decision::Approve);
    assert_eq!(decide.review_token, None);
    // Allow host sends a decision and a token, never a host (ADR-0110).
    assert!(approvals.contains("decision: ApprovalDecision,"));
    let allow: ApprovalDecideRequest = wire(json!({
        "id": session,
        "decision": "allow_host",
        "writers_stopped": false,
        "review_token": "t",
    }));
    assert_eq!(allow.decision, Decision::AllowHost);
    let with_host: Result<ApprovalDecideRequest, _> = serde_json::from_value(json!({
        "id": session,
        "decision": "allow_host",
        "writers_stopped": false,
        "review_token": "t",
        "host": "docs.example.com",
    }));
    assert!(with_host.is_err(), "a host from the webview is refused");
}

#[test]
fn requests_reject_unknown_fields_and_wrong_values() {
    let session = id();
    let reject = |result: Result<Value, serde_json::Error>, what: &str| {
        assert!(result.is_err(), "{what} must be rejected");
    };
    macro_rules! parse {
        ($type:ty, $value:expr) => {
            serde_json::from_value::<$type>($value).map(|_| Value::Null)
        };
    }
    reject(
        parse!(
            TreeRequest,
            json!({"view": "active", "sort": "updated", "descending": false, "path": "/x"})
        ),
        "an extra tree field",
    );
    reject(
        parse!(
            TreeRequest,
            json!({"view": "all", "sort": "updated", "descending": false})
        ),
        "a wrong view",
    );
    reject(
        parse!(
            TreeRequest,
            json!({"view": "active", "sort": "size", "descending": false})
        ),
        "a wrong sort",
    );
    reject(
        parse!(
            SearchRequest,
            json!({"query": "q", "view": "active", "limit": -1})
        ),
        "a negative limit",
    );
    reject(
        parse!(SessionRequest, json!({"session_id": "not-a-uuid"})),
        "a malformed id",
    );
    reject(
        parse!(
            SessionRequest,
            json!({"session_id": session, "path": "C:\\x"})
        ),
        "a path field",
    );
    reject(
        parse!(
            SessionCreateRequest,
            json!({"workspace_id": session, "title": "t", "path": "/x"})
        ),
        "a path on session_create",
    );
    reject(
        parse!(
            ApprovalDecideRequest,
            json!({
                "id": session, "decision": "maybe", "writers_stopped": false, "review_token": null
            })
        ),
        "a wrong decision",
    );
    reject(
        parse!(
            ApprovalDecideRequest,
            json!({
                "id": session, "decision": "deny", "writers_stopped": false, "review_token": null, "extra": 1
            })
        ),
        "an extra decide field",
    );
    reject(
        parse!(
            ChatSendRequest,
            json!({
                "session_id": session, "message": "m", "settings": {"endpoint": "e"}, "skills": []
            })
        ),
        "incomplete settings",
    );
    let mut settings = common::settings_json("e");
    settings["secret"] = json!("x");
    reject(
        parse!(
            ChatSendRequest,
            json!({"session_id": session, "message": "m", "settings": settings, "skills": []})
        ),
        "an extra settings field",
    );
}

#[test]
fn a_session_create_request_has_no_way_to_carry_a_path() {
    // Compile-time: this destructuring lists every field of the request type.
    let SessionCreateRequest {
        workspace_id: _,
        title: _,
    } = wire(json!({"workspace_id": Uuid::new_v4(), "title": "t"}));
}

#[test]
fn an_error_serializes_code_before_message() {
    // The serialized form of an error is stable text with no path markers.
    let text = text_of(&error_value());
    assert!(text.starts_with("{\"code\":\"invalid_request\",\"message\":"));
}
