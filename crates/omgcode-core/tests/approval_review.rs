//! Approval review: core finds the recorded request an approval is bound to,
//! using the dispatcher's own parser, request construction, and digests.
//!
//! These tests drive real turns with a scripted model, real files, and the
//! real control store. Edit applies need a private data root; on Windows the
//! temporary directory must be on NTFS (set `TEMP` and `TMP` accordingly).

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use omgcode_core::control::{
    ApprovalOperation, ApprovalState, ApprovalStatus, ControlStore, ProfileDeclaration, now_unix,
};
use omgcode_core::policy::{PermissionSet, WorkspaceId};
use omgcode_core::prompt::{self, BASE_PROMPT, BASE_PROMPT_VERSION};
use omgcode_core::session::{RecordedToolRequest, RequestManifest, SessionJournal};
use omgcode_core::turn::{
    ApprovalReview, ModelError, ReviewCandidate, ReviewError, ReviewField, ReviewedRequest,
    StaleReason, ToolModel, ToolModelReply, ToolModelRequest, TurnLimits, TurnRuntime,
    review_approval,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

const FILE_AUDIT_NAME: &str = "file-operations-v1.jsonl";
const COMMAND_TOOL: &str = "workspace_run_command";
const EDIT_TOOL: &str = "workspace_edit_file";

struct Fixture {
    _temporary: TempDir,
    data: PathBuf,
    workspace: PathBuf,
    workspace_id: WorkspaceId,
    journal: SessionJournal,
}

impl Fixture {
    fn new() -> Self {
        let temporary = TempDir::new().expect("private temporary directory");
        let data = temporary.path().join("application-data");
        let root = temporary.path().join("development-root");
        let workspace = root.join("workspace");
        let tools = temporary.path().join("tools");
        fs::create_dir(&root).expect("create development root");
        fs::create_dir(&workspace).expect("create workspace");
        fs::create_dir(&tools).expect("create toolchain root");
        // Checkpointed edits need a private data root on Windows.
        #[cfg(windows)]
        make_private_root(&data);

        let mut control = ControlStore::open(&data).expect("open control store");
        control
            .set_development_root(&root, control.revision())
            .expect("select development root");
        let workspace_id = control
            .register_workspace(&workspace, control.revision())
            .expect("register workspace")
            .id();
        control
            .set_permissions(
                workspace_id,
                PermissionSet {
                    model_tools: true,
                    execute: true,
                    read: true,
                    write: true,
                    ..PermissionSet::default()
                },
                control.revision(),
            )
            .expect("enable the command and edit tools");
        control
            .add_profile(
                &ProfileDeclaration {
                    name: "system".into(),
                    read_only_roots: vec![tools],
                    path_entries: Vec::new(),
                    environment: Vec::new(),
                },
                control.revision(),
            )
            .expect("declare a toolchain profile");
        drop(control);
        let journal =
            SessionJournal::create_with_workspace_id(&data, &workspace, "Review", workspace_id)
                .expect("create session");
        Self {
            _temporary: temporary,
            data,
            workspace,
            workspace_id,
            journal,
        }
    }

    /// Run one turn in which the model makes `calls` and then replies.
    fn turn(&mut self, calls: Vec<Value>) {
        let mut model = ScriptedModel {
            replies: VecDeque::from([
                Ok(ToolModelReply {
                    content: String::new(),
                    tool_calls: calls,
                    finish_reason: "tool_calls".to_owned(),
                }),
                Ok(ToolModelReply {
                    content: "reported".to_owned(),
                    tool_calls: Vec::new(),
                    finish_reason: "stop".to_owned(),
                }),
            ]),
        };
        let mut runtime = TurnRuntime::new(
            &mut self.journal,
            &self.data,
            self.workspace_id,
            TurnLimits::default(),
        )
        .expect("build runtime");
        runtime
            .run(&mut model, "go", "", manifest())
            .expect("turn completes");
    }

    fn recorded(&self) -> Vec<RecordedToolRequest> {
        self.journal.tool_requests().to_vec()
    }

    fn approvals(&self) -> Vec<ApprovalStatus> {
        ControlStore::open_existing(&self.data)
            .expect("open control")
            .expect("control store")
            .approval_statuses(now_unix())
    }

    fn pending(&self, operation: ApprovalOperation) -> ApprovalStatus {
        let found: Vec<_> = self
            .approvals()
            .into_iter()
            .filter(|status| {
                status.operation == operation && status.state == ApprovalState::Pending
            })
            .collect();
        assert_eq!(found.len(), 1, "one pending approval: {found:?}");
        found.into_iter().next().expect("one pending approval")
    }

    fn state(&self, id: Uuid) -> ApprovalState {
        ControlStore::open_existing(&self.data)
            .expect("open control")
            .expect("control store")
            .approval_status(id, now_unix())
            .expect("approval status")
            .state
    }

    fn review(
        &self,
        approval: &ApprovalStatus,
        recorded: &[RecordedToolRequest],
    ) -> Result<ApprovalReview, ReviewError> {
        let candidates = candidates(recorded);
        review_approval(&self.data, approval, &candidates)
    }

    fn audit_reads(&self) -> usize {
        read_events(&self.data.join(FILE_AUDIT_NAME))
            .iter()
            .filter(|event| {
                event["action"]["type"] == "operation_started"
                    && event["action"]["payload"]["operation"] == "read"
            })
            .count()
    }

    fn checkpoint_operations(&self) -> usize {
        let mut control = ControlStore::open_existing(&self.data)
            .expect("open control")
            .expect("control store");
        omgcode_core::files::FileBroker::new(&mut control)
            .checkpoint_inspect()
            .expect("inspect checkpoints")
            .operations
            .len()
    }

    fn write_file(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.workspace.join(name);
        fs::write(&path, contents).expect("write workspace file");
        path
    }
}

/// Gives the data root a protected DACL for the current user only, as the
/// Windows checkpoint store requires of its private root.
#[cfg(windows)]
fn make_private_root(data: &Path) {
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
    fs::create_dir(data).expect("application data");
    let granted = std::process::Command::new("icacls")
        .arg(data)
        .args(["/inheritance:r", "/grant:r", &format!("*{sid}:(OI)(CI)F")])
        .output()
        .expect("icacls");
    assert!(granted.status.success(), "icacls fixture failed");
}

struct ScriptedModel {
    replies: VecDeque<Result<ToolModelReply, ModelError>>,
}

impl ToolModel for ScriptedModel {
    fn complete(&mut self, _request: ToolModelRequest<'_>) -> Result<ToolModelReply, ModelError> {
        self.replies
            .pop_front()
            .unwrap_or(Err(ModelError::InvalidResponse))
    }
}

fn manifest() -> RequestManifest {
    RequestManifest {
        base_prompt: BASE_PROMPT.to_owned(),
        prompt_version: BASE_PROMPT_VERSION.to_owned(),
        prompt_sha256: prompt::base_prompt_sha256(),
        custom_instructions: String::new(),
        model: "mock-model".into(),
        base_url: "http://127.0.0.1:1234/v1".into(),
        context_tokens: 32_000,
        max_output_tokens: 1_000,
        tool_schema_version: None,
        tools: None,
    }
}

fn call(id: &str, name: &str, arguments: Value) -> Value {
    json!({
        "id": id,
        "type": "function",
        "function": {"name": name, "arguments": arguments.to_string()}
    })
}

fn command_call(id: &str, arguments: Value) -> Value {
    call(id, COMMAND_TOOL, arguments)
}

fn edit_call(id: &str, path: &str, old_text: &str, new_text: &str) -> Value {
    call(
        id,
        EDIT_TOOL,
        json!({"path": path, "old_text": old_text, "new_text": new_text}),
    )
}

fn candidates(recorded: &[RecordedToolRequest]) -> Vec<ReviewCandidate<'_>> {
    recorded
        .iter()
        .map(|request| ReviewCandidate {
            tool: &request.tool,
            arguments: &request.arguments,
        })
        .collect()
}

fn read_events(path: &Path) -> Vec<Value> {
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).expect("bounded JSONL event"))
        .collect()
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn exact(review: Result<ApprovalReview, ReviewError>) -> ReviewedRequest {
    match review.expect("review completes") {
        ApprovalReview::Exact(request) => request,
        other => panic!("expected an exact match, found {other:?}"),
    }
}

fn field<'a>(request: &'a ReviewedRequest, label: &str) -> &'a ReviewField {
    request
        .fields
        .iter()
        .find(|field| field.label == label)
        .unwrap_or_else(|| panic!("no {label:?} field in {request:?}"))
}

/// A value the review may show: no control or hidden character but `\n`.
fn assert_display_clean(text: &str) {
    for character in text.chars() {
        let hidden = matches!(
            character,
            '\u{061c}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{2069}'
                | '\u{feff}'
        );
        assert!(
            character == '\n' || (!character.is_control() && !hidden),
            "unsafe character {character:?} in {text:?}"
        );
    }
}

/// A workspace identifier that is registered in no store this test opens.
fn foreign_workspace_id() -> WorkspaceId {
    let temporary = TempDir::new().expect("private temporary directory");
    let root = temporary.path().join("root");
    let workspace = root.join("other");
    fs::create_dir_all(&workspace).expect("create workspace");
    let mut control = ControlStore::open(&temporary.path().join("data")).expect("open store");
    control
        .set_development_root(&root, control.revision())
        .expect("select development root");
    control
        .register_workspace(&workspace, control.revision())
        .expect("register workspace")
        .id()
}

fn statuses_json(statuses: &[ApprovalStatus]) -> String {
    serde_json::to_string(statuses).expect("serialize statuses")
}

// ---------------------------------------------------------------- commands

#[test]
fn a_command_approval_reviews_exact_fields_and_changes_nothing() {
    let mut fixture = Fixture::new();
    fixture.turn(vec![command_call(
        "cmd-1",
        json!({
            "profile": "system",
            "program": "echo",
            "arguments": ["hello", "two words", ""],
            "cwd": "sub/dir",
        }),
    )]);
    let approval = fixture.pending(ApprovalOperation::Command);
    let recorded = fixture.recorded();
    let before = statuses_json(&fixture.approvals());

    let request = exact(fixture.review(&approval, &recorded));

    assert_eq!(request.tool, COMMAND_TOOL);
    let values: Vec<(&str, &str, bool)> = request
        .fields
        .iter()
        .map(|field| (field.label, field.value.as_str(), field.multiline))
        .collect();
    assert_eq!(
        values,
        [
            ("Profile", "system", false),
            ("Program", "echo", false),
            ("Arguments", "1: hello\n2: two words\n3: ", true),
            ("Working directory", "sub/dir", false),
            ("Network", "off", false),
            ("Host services", "off", false),
            ("Time limit", "20 seconds", false),
            ("Output limit", "4096 bytes per stream", false),
        ]
    );
    // Reviewing is read-only: the record is still pending and unchanged, and
    // nothing ran.
    assert_eq!(statuses_json(&fixture.approvals()), before);
    assert_eq!(fixture.state(approval.id), ApprovalState::Pending);
    assert!(!fixture.data.join("runs-v1.jsonl").exists());
}

/// The review digest and the dispatch digest are the same value: after an
/// exact review, approving the record lets the identical call consume it and
/// reach the runner. As in the existing approved-command test the fixture
/// profile has no PATH entry, so the run is refused after consumption
/// (`command_failed`); that proves the gate and the hand-off, and this
/// suite does not qualify native confinement.
#[test]
fn an_exactly_reviewed_command_is_the_call_that_consumes_the_approval() {
    let mut fixture = Fixture::new();
    let arguments = json!({"profile": "system", "program": "echo", "arguments": ["hello"]});
    fixture.turn(vec![command_call("cmd-1", arguments.clone())]);
    let approval = fixture.pending(ApprovalOperation::Command);
    let recorded = fixture.recorded();
    exact(fixture.review(&approval, &recorded));

    ControlStore::open_existing(&fixture.data)
        .expect("open control")
        .expect("control store")
        .decide_approval(approval.id, true, now_unix())
        .expect("approve");
    fixture.turn(vec![command_call("cmd-2", arguments)]);

    assert_eq!(fixture.state(approval.id), ApprovalState::Consumed);
    // The journal file is locked while the session is open, so read the
    // recorded tool result from its snapshot.
    assert!(
        fixture
            .journal
            .snapshot()
            .messages
            .iter()
            .any(|message| message.text.contains("\"status\":\"command_failed\"")),
        "the approved call did not reach the runner"
    );
}

#[test]
fn an_earlier_matching_command_is_reviewed_not_the_newest_candidate() {
    let mut fixture = Fixture::new();
    fixture.turn(vec![command_call(
        "cmd-1",
        json!({"profile": "system", "program": "echo"}),
    )]);
    let approval = fixture.pending(ApprovalOperation::Command);
    let mut recorded = fixture.recorded();
    // Newer requests that were never approved, one valid and one malformed.
    for (index, arguments) in [
        r#"{"profile":"system","program":"rm","arguments":["-rf","x"]}"#,
        r#"{"profile":"system","program":"echo","unexpected":true}"#,
        "not json at all",
    ]
    .into_iter()
    .enumerate()
    {
        recorded.push(RecordedToolRequest {
            request_id: Uuid::new_v4(),
            call_id: format!("later-{index}"),
            tool: COMMAND_TOOL.to_owned(),
            arguments: arguments.to_owned(),
        });
    }

    let request = exact(fixture.review(&approval, &recorded));

    assert_eq!(field(&request, "Program").value, "echo");
    assert_eq!(field(&request, "Arguments").value, "(none)");
    assert_eq!(
        field(&request, "Working directory").value,
        "(workspace root)"
    );
}

#[test]
fn a_command_review_is_stale_or_not_found_when_nothing_matches() {
    let mut fixture = Fixture::new();
    fixture.turn(vec![command_call(
        "cmd-1",
        json!({"profile": "system", "program": "echo", "arguments": ["a"]}),
    )]);
    let approval = fixture.pending(ApprovalOperation::Command);
    let recorded = fixture.recorded();

    // Another session's approval for identical arguments is a different digest.
    let other_session = ApprovalStatus {
        session_id: Uuid::new_v4(),
        ..approval.clone()
    };
    assert_eq!(
        fixture.review(&other_session, &recorded).expect("review"),
        ApprovalReview::Stale(StaleReason::NoMatchingRequest)
    );
    // Candidates of another tool do not count.
    let edits_only = [RecordedToolRequest {
        request_id: Uuid::new_v4(),
        call_id: "e".into(),
        tool: EDIT_TOOL.into(),
        arguments: r#"{"path":"a","old_text":"b","new_text":"c"}"#.into(),
    }];
    assert_eq!(
        fixture.review(&approval, &edits_only).expect("review"),
        ApprovalReview::NotFound
    );
    assert_eq!(
        fixture.review(&approval, &[]).expect("review"),
        ApprovalReview::NotFound
    );
    // Operations without a reviewable tool.
    for operation in [ApprovalOperation::GitPush, ApprovalOperation::Network] {
        let unreviewable = ApprovalStatus {
            operation,
            ..approval.clone()
        };
        assert_eq!(
            fixture.review(&unreviewable, &recorded).expect("review"),
            ApprovalReview::NotFound
        );
    }
    // An unregistered workspace.
    let unknown = ApprovalStatus {
        workspace_id: foreign_workspace_id(),
        ..approval.clone()
    };
    assert_eq!(
        fixture.review(&unknown, &recorded).expect("review"),
        ApprovalReview::NotFound
    );

    // A settings change bumps the revision: the record can never be consumed.
    let mut control = ControlStore::open_existing(&fixture.data)
        .expect("open control")
        .expect("control store");
    control
        .set_permissions(
            fixture.workspace_id,
            PermissionSet {
                model_tools: true,
                execute: true,
                read: true,
                write: true,
                host_services: true,
                ..PermissionSet::default()
            },
            control.revision(),
        )
        .expect("change settings");
    drop(control);
    assert_eq!(
        fixture.review(&approval, &recorded).expect("review"),
        ApprovalReview::Stale(StaleReason::SettingsChanged)
    );
}

#[test]
fn a_missing_settings_store_is_not_found() {
    let empty = TempDir::new().expect("empty data directory");
    let approval = ApprovalStatus {
        id: Uuid::new_v4(),
        session_id: Uuid::new_v4(),
        workspace_id: foreign_workspace_id(),
        operation: ApprovalOperation::Command,
        request_sha256: "0".repeat(64),
        summary: "run".into(),
        settings_revision: 1,
        created_unix: 0,
        expires_unix: 0,
        state: ApprovalState::Pending,
        writers_stopped: false,
    };
    let candidate = ReviewCandidate {
        tool: COMMAND_TOOL,
        arguments: r#"{"profile":"system","program":"echo"}"#,
    };
    assert_eq!(
        review_approval(empty.path(), &approval, &[candidate]).expect("review"),
        ApprovalReview::NotFound
    );
    assert!(
        !empty.path().join("control-v1.jsonl").exists(),
        "nothing created"
    );
}

#[test]
fn a_held_settings_store_makes_the_review_busy() {
    let mut fixture = Fixture::new();
    fixture.turn(vec![command_call(
        "cmd-1",
        json!({"profile": "system", "program": "echo"}),
    )]);
    let approval = fixture.pending(ApprovalOperation::Command);
    let recorded = fixture.recorded();
    let held = ControlStore::open_existing(&fixture.data)
        .expect("open control")
        .expect("control store");

    let result = fixture.review(&approval, &recorded);
    assert!(matches!(result, Err(ReviewError::Busy)), "{result:?}");
    let message = result.expect_err("busy").to_string();
    assert!(!message.contains(fixture.data.to_string_lossy().as_ref()));

    drop(held);
    exact(fixture.review(&approval, &recorded));
}

#[test]
fn hostile_command_text_is_escaped_and_cannot_add_lines() {
    let mut fixture = Fixture::new();
    let arguments = [
        "plain",
        "a\u{202e}b",
        "zero\u{200b}width",
        "cr\rtab\tend",
        "line\n9: forged argument",
        r"literal \u{202e} text",
    ];
    fixture.turn(vec![command_call(
        "cmd-1",
        json!({"profile": "system", "program": "echo", "arguments": arguments}),
    )]);
    let approval = fixture.pending(ApprovalOperation::Command);
    let recorded = fixture.recorded();

    let request = exact(fixture.review(&approval, &recorded));

    for field in &request.fields {
        assert_display_clean(&field.value);
    }
    let shown = field(&request, "Arguments");
    let lines: Vec<&str> = shown.value.lines().collect();
    assert_eq!(
        lines.len(),
        arguments.len(),
        "one line per argument: {lines:?}"
    );
    assert_eq!(lines[1], "2: a\\u{202e}b");
    assert_eq!(lines[2], "3: zero\\u{200b}width");
    assert_eq!(lines[3], "4: cr\\u{000d}tab\\u{0009}end");
    assert_eq!(lines[4], "5: line\\u{000a}9: forged argument");
    assert_eq!(lines[5], "6: literal \\u{5c}u{202e} text");
}

// ------------------------------------------------------------------- edits

#[test]
fn an_edit_approval_reviews_exact_fields_without_touching_the_file() {
    let mut fixture = Fixture::new();
    let target = fixture.write_file("notes.txt", "alpha bcc\n");
    fixture.turn(vec![edit_call("edit-1", "notes.txt", "bc", "b")]);
    let approval = fixture.pending(ApprovalOperation::FileWrite);
    let recorded = fixture.recorded();
    let before = statuses_json(&fixture.approvals());
    let reads = fixture.audit_reads();

    let request = exact(fixture.review(&approval, &recorded));

    assert_eq!(request.tool, EDIT_TOOL);
    let values: Vec<(&str, String, bool)> = request
        .fields
        .iter()
        .map(|field| (field.label, field.value.clone(), field.multiline))
        .collect();
    assert_eq!(
        values,
        [
            ("File", "notes.txt".to_owned(), false),
            ("Old text", "bc".to_owned(), true),
            ("New text", "b".to_owned(), true),
            ("Size", "10 -> 9 bytes".to_owned(), false),
            ("Current SHA-256", sha256(b"alpha bcc\n"), false),
            ("Proposed SHA-256", sha256(b"alpha bc\n"), false),
        ]
    );
    assert_eq!(fs::read(&target).expect("read"), b"alpha bcc\n");
    assert_eq!(fixture.checkpoint_operations(), 0);
    assert_eq!(statuses_json(&fixture.approvals()), before);
    assert_eq!(fixture.audit_reads(), reads + 1, "one audited read");
}

/// After an exact review and an approval with the writers-stopped
/// acknowledgement, the identical call applies the edit: the review digest is
/// the dispatch digest.
#[test]
fn an_exactly_reviewed_edit_is_the_call_that_applies() {
    let mut fixture = Fixture::new();
    let target = fixture.write_file("notes.txt", "alpha bcc\n");
    let arguments = edit_call("edit-1", "notes.txt", "bc", "b");
    fixture.turn(vec![arguments.clone()]);
    let approval = fixture.pending(ApprovalOperation::FileWrite);
    let recorded = fixture.recorded();
    exact(fixture.review(&approval, &recorded));

    ControlStore::open_existing(&fixture.data)
        .expect("open control")
        .expect("control store")
        .approve_with_writers_stopped(approval.id, now_unix())
        .expect("approve the edit");
    fixture.turn(vec![edit_call("edit-2", "notes.txt", "bc", "b")]);

    assert_eq!(fs::read_to_string(&target).expect("read"), "alpha bc\n");
    assert_eq!(fixture.state(approval.id), ApprovalState::Consumed);
    assert_eq!(fixture.checkpoint_operations(), 1);
}

#[test]
fn an_edit_review_is_stale_after_the_file_changes() {
    let mut fixture = Fixture::new();
    let target = fixture.write_file("notes.txt", "xa");
    fixture.turn(vec![edit_call("edit-1", "notes.txt", "x", "")]);
    let approval = fixture.pending(ApprovalOperation::FileWrite);
    let recorded = fixture.recorded();
    exact(fixture.review(&approval, &recorded));

    fs::write(&target, "ax").expect("change the file after the request");

    assert_eq!(
        fixture.review(&approval, &recorded).expect("review"),
        ApprovalReview::Stale(StaleReason::NoMatchingRequest)
    );
    // A removed file is also simply not a match.
    fs::remove_file(&target).expect("remove the file");
    assert_eq!(
        fixture.review(&approval, &recorded).expect("review"),
        ApprovalReview::Stale(StaleReason::NoMatchingRequest)
    );
    assert_eq!(fixture.checkpoint_operations(), 0);
}

fn recorded_edit(index: usize, arguments: String) -> RecordedToolRequest {
    RecordedToolRequest {
        request_id: Uuid::new_v4(),
        call_id: format!("edit-{index}"),
        tool: EDIT_TOOL.to_owned(),
        arguments,
    }
}

fn distinct_edit_arguments(index: usize) -> String {
    json!({"path": "notes.txt", "old_text": "alpha", "new_text": format!("other{index}")})
        .to_string()
}

#[test]
fn at_most_eight_distinct_edit_candidates_are_read_newest_first() {
    let mut fixture = Fixture::new();
    fixture.write_file("notes.txt", "alpha bcc\n");
    fixture.turn(vec![edit_call("edit-1", "notes.txt", "bc", "b")]);
    let approval = fixture.pending(ApprovalOperation::FileWrite);
    let matching = fixture.recorded();

    // Seven newer non-matching candidates: the matching one is the eighth.
    let mut within = matching.clone();
    for index in 0..7 {
        within.push(recorded_edit(index, distinct_edit_arguments(index)));
    }
    let reads = fixture.audit_reads();
    exact(fixture.review(&approval, &within));
    assert_eq!(fixture.audit_reads() - reads, 8);

    // Eight newer non-matching candidates push it past the bound. Repeats of
    // one arguments string count once, so the bound is on distinct requests.
    let mut beyond = matching;
    for index in 0..8 {
        beyond.push(recorded_edit(index, distinct_edit_arguments(index)));
        beyond.push(recorded_edit(index + 100, distinct_edit_arguments(index)));
    }
    let reads = fixture.audit_reads();
    let result = fixture.review(&approval, &beyond).expect("review");
    assert_eq!(
        result,
        ApprovalReview::Stale(StaleReason::NoMatchingRequest)
    );
    assert!(
        fixture.audit_reads() - reads <= 8,
        "{} audited reads",
        fixture.audit_reads() - reads
    );
    assert_eq!(fixture.audit_reads() - reads, 8);
}

#[test]
fn hostile_edit_text_is_escaped_and_a_literal_escape_stays_distinguishable() {
    let mut fixture = Fixture::new();
    // The file holds every kind of hostile character, including a NUL.
    let contents = "x\u{202e}rtl\u{200b}zw\r\ttab\0nul \\u{202e} literal\nsecond\n";
    fixture.write_file("odd.txt", contents);
    let replacement = "ok\u{2028}\u{feff}\r\\u{200b}";
    fixture.turn(vec![edit_call("edit-1", "odd.txt", contents, replacement)]);
    let approval = fixture.pending(ApprovalOperation::FileWrite);
    let recorded = fixture.recorded();

    let request = exact(fixture.review(&approval, &recorded));

    for field in &request.fields {
        assert_display_clean(&field.value);
    }
    let old = &field(&request, "Old text").value;
    assert_eq!(
        old,
        "x\\u{202e}rtl\\u{200b}zw\\u{000d}\\u{0009}tab\\u{0000}nul \\u{5c}u{202e} literal\nsecond\n"
    );
    assert!(old.contains("\\u{202e}rtl"), "the real override is shown");
    assert!(
        old.contains("\\u{5c}u{202e}"),
        "the forged literal is marked"
    );
    assert_eq!(
        field(&request, "New text").value,
        "ok\\u{2028}\\u{feff}\\u{000d}\\u{5c}u{200b}"
    );
}

// ------------------------------------------------- multi-edit and create

const MULTI_EDIT_TOOL: &str = "workspace_multi_edit_file";
const CREATE_TOOL: &str = "workspace_create_file";

fn multi_call(id: &str, path: &str, edits: &[(&str, &str)]) -> Value {
    let edits: Vec<Value> = edits
        .iter()
        .map(|(old, new)| json!({"old_text": old, "new_text": new}))
        .collect();
    call(id, MULTI_EDIT_TOOL, json!({"path": path, "edits": edits}))
}

fn create_call(id: &str, path: &str, content: &str) -> Value {
    call(id, CREATE_TOOL, json!({"path": path, "content": content}))
}

fn approve(fixture: &Fixture, approval: &ApprovalStatus) {
    ControlStore::open_existing(&fixture.data)
        .expect("open control")
        .expect("control store")
        .approve_with_writers_stopped(approval.id, now_unix())
        .expect("approve");
}

#[test]
fn a_multi_edit_approval_reviews_exact_fields_without_touching_the_file() {
    let mut fixture = Fixture::new();
    let target = fixture.write_file("notes.txt", "alpha beta gamma\n");
    fixture.turn(vec![multi_call(
        "multi-1",
        "notes.txt",
        &[("gamma", "G"), ("alpha", "A")],
    )]);
    let approval = fixture.pending(ApprovalOperation::FileWrite);
    let recorded = fixture.recorded();
    let before = statuses_json(&fixture.approvals());
    let reads = fixture.audit_reads();

    let request = exact(fixture.review(&approval, &recorded));

    assert_eq!(request.tool, MULTI_EDIT_TOOL);
    let values: Vec<(&str, String, bool)> = request
        .fields
        .iter()
        .map(|field| (field.label, field.value.clone(), field.multiline))
        .collect();
    assert_eq!(
        values,
        [
            ("File", "notes.txt".to_owned(), false),
            ("Edits", "2".to_owned(), false),
            ("Old text 1", "gamma".to_owned(), true),
            ("New text 1", "G".to_owned(), true),
            ("Old text 2", "alpha".to_owned(), true),
            ("New text 2", "A".to_owned(), true),
            ("Size", "17 -> 9 bytes".to_owned(), false),
            ("Current SHA-256", sha256(b"alpha beta gamma\n"), false),
            ("Proposed SHA-256", sha256(b"A beta G\n"), false),
        ]
    );
    assert_eq!(fs::read(&target).expect("read"), b"alpha beta gamma\n");
    assert_eq!(fixture.checkpoint_operations(), 0);
    assert_eq!(statuses_json(&fixture.approvals()), before);
    assert_eq!(fixture.audit_reads(), reads + 1, "one audited read");
}

/// After an exact review and an approval, the identical multi-edit applies:
/// the review digest is the dispatch digest.
#[test]
fn an_exactly_reviewed_multi_edit_is_the_call_that_applies() {
    let mut fixture = Fixture::new();
    let target = fixture.write_file("notes.txt", "alpha beta\n");
    let edits = [("alpha", "A"), ("beta", "B")];
    fixture.turn(vec![multi_call("multi-1", "notes.txt", &edits)]);
    let approval = fixture.pending(ApprovalOperation::FileWrite);
    let recorded = fixture.recorded();
    exact(fixture.review(&approval, &recorded));

    approve(&fixture, &approval);
    fixture.turn(vec![multi_call("multi-2", "notes.txt", &edits)]);

    assert_eq!(fs::read_to_string(&target).expect("read"), "A B\n");
    assert_eq!(fixture.state(approval.id), ApprovalState::Consumed);
    assert_eq!(fixture.checkpoint_operations(), 1);
}

#[test]
fn a_multi_edit_review_is_stale_after_the_file_changes() {
    let mut fixture = Fixture::new();
    let target = fixture.write_file("notes.txt", "one two");
    fixture.turn(vec![multi_call(
        "multi-1",
        "notes.txt",
        &[("one", "1"), ("two", "2")],
    )]);
    let approval = fixture.pending(ApprovalOperation::FileWrite);
    let recorded = fixture.recorded();
    exact(fixture.review(&approval, &recorded));

    fs::write(&target, "one two!").expect("change the file after the request");
    assert_eq!(
        fixture.review(&approval, &recorded).expect("review"),
        ApprovalReview::Stale(StaleReason::NoMatchingRequest)
    );
    // A change that breaks one match is also not a match.
    fs::write(&target, "one three").expect("remove a match");
    assert_eq!(
        fixture.review(&approval, &recorded).expect("review"),
        ApprovalReview::Stale(StaleReason::NoMatchingRequest)
    );
}

#[test]
fn a_create_approval_reviews_exact_fields_and_is_stale_once_the_file_exists() {
    let mut fixture = Fixture::new();
    fixture.turn(vec![create_call("create-1", "new.txt", "hello\nworld\n")]);
    let approval = fixture.pending(ApprovalOperation::FileWrite);
    let recorded = fixture.recorded();
    let before = statuses_json(&fixture.approvals());

    let request = exact(fixture.review(&approval, &recorded));

    assert_eq!(request.tool, CREATE_TOOL);
    let values: Vec<(&str, String, bool)> = request
        .fields
        .iter()
        .map(|field| (field.label, field.value.clone(), field.multiline))
        .collect();
    assert_eq!(
        values,
        [
            ("File", "new.txt".to_owned(), false),
            ("Size", "12 bytes".to_owned(), false),
            ("SHA-256", sha256(b"hello\nworld\n"), false),
            ("Content", "hello\nworld\n".to_owned(), true),
        ]
    );
    assert!(!fixture.workspace.join("new.txt").exists());
    assert_eq!(statuses_json(&fixture.approvals()), before);

    // Another writer creates the file: the request can no longer proceed.
    fixture.write_file("new.txt", "other");
    assert_eq!(
        fixture.review(&approval, &recorded).expect("review"),
        ApprovalReview::Stale(StaleReason::NoMatchingRequest)
    );
    assert_eq!(
        fs::read(fixture.workspace.join("new.txt")).expect("read"),
        b"other"
    );
}

/// After an exact review and an approval, the identical call creates the file.
#[test]
fn an_exactly_reviewed_create_is_the_call_that_creates() {
    let mut fixture = Fixture::new();
    fixture.turn(vec![create_call("create-1", "new.txt", "content")]);
    let approval = fixture.pending(ApprovalOperation::FileWrite);
    let recorded = fixture.recorded();
    exact(fixture.review(&approval, &recorded));

    approve(&fixture, &approval);
    fixture.turn(vec![create_call("create-2", "new.txt", "content")]);

    assert_eq!(
        fs::read_to_string(fixture.workspace.join("new.txt")).expect("read"),
        "content"
    );
    assert_eq!(fixture.state(approval.id), ApprovalState::Consumed);
    assert_eq!(fixture.checkpoint_operations(), 0);
}

/// A review matches only the tool whose digest the approval holds: an edit
/// approval is not shown as a multi-edit, and the reverse.
#[test]
fn a_review_never_matches_another_tools_candidate() {
    let mut fixture = Fixture::new();
    fixture.write_file("notes.txt", "alpha beta\n");
    fixture.turn(vec![edit_call("edit-1", "notes.txt", "beta", "gamma")]);
    let edit_approval = fixture.pending(ApprovalOperation::FileWrite);
    let edit_recorded = fixture.recorded();

    // Only an equivalent multi-edit is offered for the edit approval.
    let equivalent = vec![RecordedToolRequest {
        request_id: Uuid::new_v4(),
        call_id: "multi-1".into(),
        tool: MULTI_EDIT_TOOL.to_owned(),
        arguments:
            json!({"path": "notes.txt", "edits": [{"old_text": "beta", "new_text": "gamma"}]})
                .to_string(),
    }];
    assert_eq!(
        fixture.review(&edit_approval, &equivalent).expect("review"),
        ApprovalReview::Stale(StaleReason::NoMatchingRequest)
    );
    // The real edit candidate still matches, even with the multi-edit present.
    let mut both = equivalent;
    both.splice(0..0, edit_recorded);
    let request = exact(fixture.review(&edit_approval, &both));
    assert_eq!(request.tool, EDIT_TOOL);

    // A create candidate never matches a replacement approval either.
    let creates = vec![RecordedToolRequest {
        request_id: Uuid::new_v4(),
        call_id: "create-1".into(),
        tool: CREATE_TOOL.to_owned(),
        arguments: json!({"path": "fresh.txt", "content": "gamma"}).to_string(),
    }];
    assert_eq!(
        fixture.review(&edit_approval, &creates).expect("review"),
        ApprovalReview::Stale(StaleReason::NoMatchingRequest)
    );
}

/// The eight-candidate bound counts distinct requests across all three
/// file-write tools together, newest first.
#[test]
fn the_candidate_bound_spans_all_file_write_tools() {
    let mut fixture = Fixture::new();
    fixture.write_file("notes.txt", "alpha bcc\n");
    fixture.turn(vec![create_call("create-1", "new.txt", "x")]);
    let approval = fixture.pending(ApprovalOperation::FileWrite);
    let matching = fixture.recorded();

    let newer = |count: usize| {
        let mut all = matching.clone();
        for index in 0..count {
            let (tool, arguments) = match index % 3 {
                0 => (
                    EDIT_TOOL,
                    json!({"path": "notes.txt", "old_text": "alpha", "new_text": format!("o{index}")}),
                ),
                1 => (
                    MULTI_EDIT_TOOL,
                    json!({"path": "notes.txt", "edits": [{"old_text": "alpha", "new_text": format!("o{index}")}]}),
                ),
                _ => (
                    CREATE_TOOL,
                    json!({"path": format!("other{index}.txt"), "content": "y"}),
                ),
            };
            all.push(RecordedToolRequest {
                request_id: Uuid::new_v4(),
                call_id: format!("call-{index}"),
                tool: tool.to_owned(),
                arguments: arguments.to_string(),
            });
        }
        all
    };

    // Seven newer candidates: the matching one is the eighth and is found.
    let reads = fixture.audit_reads();
    exact(fixture.review(&approval, &newer(7)));
    assert_eq!(fixture.audit_reads() - reads, 8);

    // Eight newer candidates push it beyond the bound.
    let reads = fixture.audit_reads();
    assert_eq!(
        fixture.review(&approval, &newer(8)).expect("review"),
        ApprovalReview::Stale(StaleReason::NoMatchingRequest)
    );
    assert_eq!(fixture.audit_reads() - reads, 8);
}

#[test]
fn hostile_multi_edit_and_create_text_is_escaped() {
    let mut fixture = Fixture::new();
    let contents = "a\u{202e}b \\u{202e} c\n";
    fixture.write_file("odd.txt", contents);
    fixture.turn(vec![multi_call(
        "multi-1",
        "odd.txt",
        &[("a\u{202e}b", "x\u{2028}y\r"), ("\\u{202e}", "\\u{200b}")],
    )]);
    let approval = fixture.pending(ApprovalOperation::FileWrite);
    let recorded = fixture.recorded();
    let request = exact(fixture.review(&approval, &recorded));
    for field in &request.fields {
        assert_display_clean(&field.value);
    }
    assert_eq!(field(&request, "Old text 1").value, "a\\u{202e}b");
    assert_eq!(field(&request, "New text 1").value, "x\\u{2028}y\\u{000d}");
    assert_eq!(field(&request, "Old text 2").value, "\\u{5c}u{202e}");
    assert_eq!(field(&request, "New text 2").value, "\\u{5c}u{200b}");

    let mut fixture = Fixture::new();
    fixture.turn(vec![create_call(
        "create-1",
        "new.txt",
        "ok\u{202e}\u{feff}\r\\u{200b}\0",
    )]);
    let approval = fixture.pending(ApprovalOperation::FileWrite);
    let recorded = fixture.recorded();
    let request = exact(fixture.review(&approval, &recorded));
    for field in &request.fields {
        assert_display_clean(&field.value);
    }
    assert_eq!(
        field(&request, "Content").value,
        "ok\\u{202e}\\u{feff}\\u{000d}\\u{5c}u{200b}\\u{0000}"
    );
}
