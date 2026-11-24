//! The per-part context estimate of a session's next turn (ADR-0087), against
//! real journals and saved settings. Nothing here starts a model server: the
//! command sends nothing and records nothing.

use std::fs;
use std::path::Path;

use omgcode_app::create_session;
use omgcode_app::desktop::{ContextBreakdown, ContextPartDto, ContextPartId, DesktopService};
use omgcode_core::control::ControlStore;
use omgcode_core::session::{CompactionRecord, MessageRole, SessionJournal};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

mod common;
use common::{Fixture, json_of, read_tools, session_request, text_of, text_only, wire};

const NO_ENDPOINT: &str = "http://127.0.0.1:1/v1";
/// The allowance for the tool definitions, in estimated tokens (8 KiB).
const TOOL_ALLOWANCE: u64 = 8 * 1024;
/// The overhead of one message in the estimate.
const MESSAGE_OVERHEAD: u64 = 256;
const SUMMARY: &str = "Earlier the user asked about parsing.";
const QUESTION: &str = "a question that must never be returned";

fn breakdown(service: &DesktopService, id: Uuid) -> ContextBreakdown {
    service
        .session_context_breakdown(wire(session_request(id)))
        .expect("breakdown")
}

fn part(breakdown: &ContextBreakdown, id: ContextPartId) -> &ContextPartDto {
    breakdown
        .parts
        .iter()
        .find(|part| part.id == id)
        .expect("part present")
}

/// A session of two finished exchanges, optionally compacted over the first.
fn session(data: &Path, workspace: &Path, compacted: bool) -> (Uuid, SessionJournal) {
    let mut journal = create_session(data, workspace, "parts").expect("create");
    for (question, answer) in [(QUESTION, "first answer"), ("second", "reply two")] {
        journal
            .record_message(MessageRole::User, question)
            .expect("user");
        let turn = journal.start_turn().expect("turn");
        journal
            .record_message(MessageRole::Assistant, answer)
            .expect("assistant");
        journal.complete_turn(turn).expect("complete");
    }
    if compacted {
        let digest: String = Sha256::digest(SUMMARY.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        journal
            .record_compaction(CompactionRecord {
                summary: SUMMARY.to_owned(),
                compacted_messages: 2,
                includes_tool_results: false,
                summary_sha256: digest,
                model: "fixture".to_owned(),
                prompt_tokens: None,
                completion_tokens: None,
            })
            .expect("compact");
    }
    (journal.snapshot().session_id, journal)
}

fn sum(breakdown: &ContextBreakdown) -> u64 {
    breakdown.parts.iter().map(|part| part.tokens).sum()
}

#[test]
fn the_parts_add_up_to_the_estimate_and_carry_no_text() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("plain", text_only());
    let (id, journal) = session(&fixture.data, &workspace.path, false);
    let path = journal.path().to_path_buf();
    drop(journal);
    let before = fs::read(&path).expect("journal bytes");
    let service = fixture.service(NO_ENDPOINT);

    let result = breakdown(&service, id);

    assert_eq!(result.session_id, id);
    assert_eq!(sum(&result), result.estimated_input_tokens);
    let ids: Vec<ContextPartId> = result.parts.iter().map(|part| part.id).collect();
    assert_eq!(
        ids,
        [
            ContextPartId::System,
            ContextPartId::GlobalInstructions,
            ContextPartId::WorkspaceInstructions,
            ContextPartId::Tools,
            ContextPartId::Summary,
            ContextPartId::History,
            ContextPartId::Skills,
            ContextPartId::Framing,
        ]
    );
    // Four retained messages: their content and one overhead each.
    let history = part(&result, ContextPartId::History);
    assert_eq!(history.messages, Some(4));
    let content =
        u64::try_from(QUESTION.len() + "first answer".len() + "second".len() + "reply two".len())
            .expect("length");
    assert_eq!(history.tokens, content + 4 * MESSAGE_OVERHEAD);
    // No instructions, no summary, no tools, and skills never.
    for (id, reason) in [
        (ContextPartId::GlobalInstructions, "none_saved"),
        (ContextPartId::WorkspaceInstructions, "none_saved"),
        (ContextPartId::Tools, "preference_disabled"),
        (ContextPartId::Summary, "no_compaction"),
        (ContextPartId::Skills, "selected_per_turn"),
    ] {
        let part = part(&result, id);
        assert!(!part.included, "{id:?}");
        assert_eq!(part.tokens, 0, "{id:?}");
        assert_eq!(part.reason, Some(reason), "{id:?}");
    }
    assert_eq!(result.estimate_scope, "pre_turn_conservative");

    // Names and numbers only, and the journal is untouched.
    let text = text_of(&result);
    assert!(!text.contains(QUESTION) && !text.contains("first answer"));
    assert_eq!(fs::read(&path).expect("journal bytes"), before);
}

#[test]
fn a_compaction_summary_is_its_own_part_and_the_history_starts_after_the_boundary() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("compacted", text_only());
    let (id, journal) = session(&fixture.data, &workspace.path, true);
    drop(journal);
    let service = fixture.service(NO_ENDPOINT);

    let result = breakdown(&service, id);

    let summary = part(&result, ContextPartId::Summary);
    assert!(summary.included);
    assert_eq!(summary.reason, None);
    assert!(
        summary.tokens >= u64::try_from(SUMMARY.len()).expect("length") + MESSAGE_OVERHEAD,
        "the summary, its label, and its overhead; got {}",
        summary.tokens
    );
    let history = part(&result, ContextPartId::History);
    assert_eq!(history.messages, Some(2));
    let content = u64::try_from("second".len() + "reply two".len()).expect("length");
    assert_eq!(history.tokens, content + 2 * MESSAGE_OVERHEAD);
    assert_eq!(sum(&result), result.estimated_input_tokens);
}

#[test]
fn tools_are_counted_only_when_live_settings_would_offer_them() {
    let fixture = Fixture::new();
    let off = fixture.workspace("tools-off", text_only());
    let on = fixture.workspace("tools-on", read_tools());
    let (off_id, off_journal) = session(&fixture.data, &off.path, false);
    let (on_id, on_journal) = session(&fixture.data, &on.path, false);
    drop((off_journal, on_journal));
    let service = fixture.service(NO_ENDPOINT);

    let without = breakdown(&service, off_id);
    let with = breakdown(&service, on_id);

    let tools_off = part(&without, ContextPartId::Tools);
    assert!(!tools_off.included);
    assert_eq!(tools_off.reason, Some("preference_disabled"));
    if omgcode_app::MODEL_TOOLS_SUPPORTED {
        let tools_on = part(&with, ContextPartId::Tools);
        assert!(tools_on.included);
        assert_eq!(tools_on.tokens, TOOL_ALLOWANCE);
        assert_eq!(tools_on.reason, None);
        assert_eq!(
            with.estimated_input_tokens,
            without.estimated_input_tokens + TOOL_ALLOWANCE
        );
        assert_eq!(sum(&with), with.estimated_input_tokens);
    }
}

#[test]
fn global_and_workspace_instructions_are_split_and_add_up() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("instructed", text_only());
    let (id, journal) = session(&fixture.data, &workspace.path, false);
    drop(journal);
    let service = fixture.service(NO_ENDPOINT);
    let plain = breakdown(&service, id);

    service
        .settings_save_chat_defaults(wire(json!({"defaults": {
            "context_tokens": 32_768,
            "max_output_tokens": 2048,
            "max_tool_calls": 8,
            "turn_timeout_seconds": 60,
            "instructions": "Be brief."
        }})))
        .expect("global instructions");
    let global_only = breakdown(&service, id);
    assert_eq!(
        part(&global_only, ContextPartId::GlobalInstructions).tokens,
        u64::try_from("Be brief.".len()).expect("length")
    );
    assert!(!part(&global_only, ContextPartId::WorkspaceInstructions).included);

    service
        .settings_set_workspace_instructions(wire(json!({
            "workspace_id": workspace.id,
            "instructions": "Use tabs."
        })))
        .expect("workspace instructions");
    let both = breakdown(&service, id);
    let global = part(&both, ContextPartId::GlobalInstructions);
    let local = part(&both, ContextPartId::WorkspaceInstructions);
    assert!(global.included && local.included);
    assert_eq!(
        global.tokens,
        u64::try_from("Global instructions:\nBe brief.".len()).expect("length")
    );
    assert_eq!(
        local.tokens,
        u64::try_from("\n\nWorkspace instructions:\nUse tabs.".len()).expect("length")
    );
    assert_eq!(sum(&both), both.estimated_input_tokens);
    assert_eq!(
        both.estimated_input_tokens,
        plain.estimated_input_tokens + global.tokens + local.tokens
    );
}

#[test]
fn the_totals_are_those_of_the_composer_meter() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("totals", text_only());
    let (id, journal) = session(&fixture.data, &workspace.path, false);
    drop(journal);
    let service = fixture.service(NO_ENDPOINT);

    // No saved budgets: the capacity is unknown, the threshold alone triggers.
    let unknown = breakdown(&service, id);
    assert_eq!(unknown.context_tokens, None);
    assert!(!unknown.auto_compaction);
    assert_eq!(
        unknown.effective_trigger,
        service.chat_defaults().settings.compact_threshold
    );

    service
        .settings_save_chat_defaults(wire(json!({"defaults": {
            "context_tokens": 4096,
            "max_output_tokens": 1024,
            "max_tool_calls": 8,
            "turn_timeout_seconds": 60,
            "instructions": ""
        }})))
        .expect("budgets");
    service
        .chat_preferences_save(wire(json!({"preferences": {
            "compact_threshold": 300_000,
            "auto_compact": true,
            "enter_to_send": true
        }})))
        .expect("composer");
    let known = breakdown(&service, id);
    assert_eq!(known.context_tokens, Some(4096));
    assert_eq!(known.max_output_tokens, 1024);
    assert!(known.auto_compaction);
    // The safe input budget: capacity less the reserve less template headroom.
    assert_eq!(known.effective_trigger, 4096 - 1024 - 512);
    let json = json_of(&known);
    assert_eq!(json["context_tokens"], 4096);
    assert_eq!(json["parts"][6]["id"], "skills");
}

#[test]
fn a_session_whose_workspace_is_unregistered_still_gets_an_estimate_without_tools() {
    let fixture = Fixture::new();
    let loose = fixture.development.join("loose");
    fs::create_dir_all(&loose).expect("loose directory");
    let mut journal = SessionJournal::create(&fixture.data, &loose, "loose").expect("create");
    journal
        .record_message(MessageRole::User, "hello")
        .expect("user");
    let id = journal.snapshot().session_id;
    drop(journal);
    let service = fixture.service(NO_ENDPOINT);

    let result = breakdown(&service, id);

    let tools = part(&result, ContextPartId::Tools);
    assert!(!tools.included);
    assert_eq!(tools.reason, Some("workspace_unregistered"));
    assert_eq!(sum(&result), result.estimated_input_tokens);
    assert_eq!(part(&result, ContextPartId::History).messages, Some(1));
}

#[test]
fn an_unregistered_workspace_sends_no_instructions_even_when_its_file_remains() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("removed", text_only());
    let (id, journal) = session(&fixture.data, &workspace.path, false);
    drop(journal);
    let service = fixture.service(NO_ENDPOINT);
    service
        .settings_set_workspace_instructions(wire(json!({
            "workspace_id": workspace.id,
            "instructions": "Use tabs."
        })))
        .expect("workspace instructions");
    assert!(
        part(
            &breakdown(&service, id),
            ContextPartId::WorkspaceInstructions
        )
        .included
    );

    // Unregister through core alone, as a CLI whose file deletion failed
    // would: the saved instructions file remains (ADR-0081).
    let mut control = ControlStore::open(&fixture.data).expect("control store");
    let registered = control
        .registered_workspace(workspace.id)
        .expect("registered")
        .id();
    control
        .unregister_workspace(registered, control.revision())
        .expect("unregistered");
    drop(control);

    let result = breakdown(&service, id);
    let local = part(&result, ContextPartId::WorkspaceInstructions);
    assert!(!local.included);
    assert_eq!(local.tokens, 0);
    assert_eq!(local.reason, Some("workspace_unregistered"));
    assert_eq!(
        part(&result, ContextPartId::Tools).reason,
        Some("workspace_unregistered")
    );
    assert_eq!(sum(&result), result.estimated_input_tokens);
}

#[test]
fn an_unknown_session_is_a_controlled_error() {
    let fixture = Fixture::new();
    let service = fixture.service(NO_ENDPOINT);
    let error = service
        .session_context_breakdown(wire(session_request(Uuid::new_v4())))
        .expect_err("unknown session");
    assert_eq!(error.code(), "not_found");
}
