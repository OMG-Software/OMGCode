//! Session tree, search, transcript, lifecycle, workspace, and creation
//! commands of the desktop service, against real journals and a real index.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use omgcode_app::desktop::{
    DesktopService, SearchRoleDto, SessionStatus, SessionSummary, SessionTree, TranscriptPage,
};
use omgcode_core::policy::Access;
use omgcode_core::session::{
    CompactionRecord, MessageRole, RequestManifest, SessionJournal, ToolOutcome,
};
use omgcode_core::turn::display_safe_line;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

mod common;
use common::{Fixture, Workspace, json_of, no_token, read_tools, session_request, text_only, wire};

/// A short pause so consecutive journal events carry distinct times.
const TICK: Duration = Duration::from_millis(8);
const NO_ENDPOINT: &str = "http://127.0.0.1:1/v1";

fn service(fixture: &Fixture) -> DesktopService {
    fixture.service(NO_ENDPOINT)
}

fn tree(service: &DesktopService, view: &str, sort: &str, descending: bool) -> SessionTree {
    service
        .session_tree(wire(
            json!({"view": view, "sort": sort, "descending": descending}),
        ))
        .expect("session tree")
}

fn create(service: &DesktopService, workspace: &Workspace, title: &str) -> SessionSummary {
    thread::sleep(TICK);
    service
        .session_create(wire(json!({"workspace_id": workspace.id, "title": title})))
        .expect("create session")
}

fn titles(tree: &SessionTree) -> Vec<Vec<String>> {
    tree.groups
        .iter()
        .map(|group| group.sessions.iter().map(|s| s.title.clone()).collect())
        .collect()
}

fn manifest() -> RequestManifest {
    RequestManifest {
        base_prompt: "BASE".into(),
        prompt_version: "7".into(),
        prompt_sha256: "a".repeat(64),
        custom_instructions: String::new(),
        model: "local-model".into(),
        base_url: "http://127.0.0.1:1234/v1".into(),
        context_tokens: 32_000,
        max_output_tokens: 2_000,
        tool_schema_version: Some("1".into()),
        tools: Some(vec!["workspace_read_file".into()]),
        skills: Vec::new(),
    }
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Create a journal directly with the core, release it, and return id and path.
fn core_session(fixture: &Fixture, workspace: &Workspace, title: &str) -> (Uuid, PathBuf) {
    let journal = SessionJournal::create(&fixture.data, &workspace.path, title).expect("create");
    (journal.snapshot().session_id, journal.path().to_path_buf())
}

fn fixture_with_workspaces() -> (Fixture, Workspace, Workspace) {
    let fixture = Fixture::new();
    let first = fixture.workspace("alpha-project", text_only());
    let second = fixture.workspace("beta-project", text_only());
    (fixture, first, second)
}

#[test]
fn the_tree_groups_by_folder_orders_each_sort_and_filters_the_view() {
    let (fixture, alpha, beta) = fixture_with_workspaces();
    let service = service(&fixture);
    let charlie = create(&service, &alpha, "Charlie");
    create(&service, &beta, "alpha");
    create(&service, &alpha, "Bravo");
    let archived = create(&service, &beta, "Zulu archived");
    service
        .session_archive(wire(session_request(archived.session_id)))
        .expect("archive");

    // Updated, newest first: Bravo, alpha, Charlie -> groups alpha-project, beta-project.
    let by_updated = tree(&service, "active", "updated", true);
    assert_eq!(by_updated.indexed_sessions, 3);
    assert_eq!(by_updated.skipped_entries, 0);
    assert_eq!(
        titles(&by_updated),
        [vec!["Bravo", "Charlie"], vec!["alpha"]]
    );
    // Oldest first: Charlie, alpha, Bravo.
    assert_eq!(
        titles(&tree(&service, "active", "updated", false)),
        [vec!["Charlie", "Bravo"], vec!["alpha"]]
    );
    // Created, ascending and descending.
    assert_eq!(
        titles(&tree(&service, "active", "created", false)),
        [vec!["Charlie", "Bravo"], vec!["alpha"]]
    );
    assert_eq!(
        titles(&tree(&service, "active", "created", true)),
        [vec!["Bravo", "Charlie"], vec!["alpha"]]
    );
    // Title, case-insensitive: alpha, Bravo, Charlie.
    assert_eq!(
        titles(&tree(&service, "active", "title", false)),
        [vec!["alpha"], vec!["Bravo", "Charlie"]]
    );
    assert_eq!(
        titles(&tree(&service, "active", "title", true)),
        [vec!["Charlie", "Bravo"], vec!["alpha"]]
    );

    // The group folders and their opaque, stable, distinct identifiers.
    let groups = &by_updated.groups;
    assert!(groups[0].folder_display.ends_with("alpha-project"));
    assert!(groups[1].folder_display.ends_with("beta-project"));
    assert_eq!(groups[0].group_id.len(), 16);
    assert_ne!(groups[0].group_id, groups[1].group_id);
    let again = tree(&service, "active", "title", false);
    let alpha_group = again
        .groups
        .iter()
        .find(|group| group.folder_display.ends_with("alpha-project"))
        .expect("alpha group");
    assert_eq!(alpha_group.group_id, groups[0].group_id);

    // The archived view holds only the archived session.
    let archived_view = tree(&service, "archived", "updated", true);
    assert_eq!(archived_view.indexed_sessions, 1);
    assert_eq!(titles(&archived_view), [vec!["Zulu archived"]]);
    assert!(archived_view.groups[0].sessions[0].archived);
    assert_eq!(charlie.status, SessionStatus::Idle);
}

#[test]
fn a_session_without_a_registered_workspace_still_groups_by_its_recorded_folder() {
    let fixture = Fixture::new();
    let loose = fixture.workspace("loose", text_only());
    let (id, _) = core_session(&fixture, &loose, "Direct");
    let service = service(&fixture);
    let tree = tree(&service, "active", "updated", true);
    assert_eq!(tree.groups.len(), 1);
    assert_eq!(tree.groups[0].sessions[0].session_id, id);
    assert!(tree.groups[0].folder_display.ends_with("loose"));
}

#[test]
fn an_open_turn_is_reported_and_interrupting_closes_it_with_the_operator_code() {
    let (fixture, alpha, _) = fixture_with_workspaces();
    let (id, path) = core_session(&fixture, &alpha, "Unfinished");
    {
        let mut journal = SessionJournal::open(&path).expect("open");
        journal
            .record_message(MessageRole::User, "question")
            .expect("message");
        journal.start_turn().expect("start turn");
    }
    let service = service(&fixture);
    let listed = tree(&service, "active", "updated", true);
    assert_eq!(listed.groups[0].sessions[0].status, SessionStatus::TurnOpen);

    // Archiving is refused while a turn is open.
    let refused = service
        .session_archive(wire(session_request(id)))
        .expect_err("an open turn blocks archiving");
    assert_eq!(refused.code(), "turn_open");

    let closed = service
        .session_interrupt(wire(session_request(id)))
        .expect("interrupt");
    assert_eq!(closed.status, SessionStatus::Idle);
    let page = transcript(&service, id, None, 50);
    let failure = json_of(page.entries.last().expect("entries"));
    assert_eq!(failure["kind"], "turn_failed");
    assert_eq!(failure["code"], "operator_interrupted");

    let again = service
        .session_interrupt(wire(session_request(id)))
        .expect_err("no turn is open");
    assert_eq!(again.code(), "invalid_request");
    assert_eq!(again.message(), "No turn is open.");
}

#[test]
fn archive_and_restore_change_the_listing_and_are_idempotent() {
    let (fixture, alpha, _) = fixture_with_workspaces();
    let service = service(&fixture);
    let session = create(&service, &alpha, "Keep or file");
    let request = || wire(session_request(session.session_id));

    let archived = service.session_archive(request()).expect("archive");
    assert!(archived.archived);
    assert_eq!(archived.event_count, session.event_count + 1);
    let twice = service.session_archive(request()).expect("archive again");
    assert!(twice.archived);
    assert_eq!(twice.event_count, archived.event_count, "no second event");
    assert_eq!(
        tree(&service, "active", "updated", true).indexed_sessions,
        0
    );
    assert_eq!(
        tree(&service, "archived", "updated", true).indexed_sessions,
        1
    );

    let restored = service.session_restore(request()).expect("restore");
    assert!(!restored.archived);
    let twice = service.session_restore(request()).expect("restore again");
    assert_eq!(twice.event_count, restored.event_count);
    assert_eq!(
        tree(&service, "active", "updated", true).indexed_sessions,
        1
    );

    let missing = service
        .session_archive(wire(session_request(Uuid::new_v4())))
        .expect_err("unknown session");
    assert_eq!(missing.code(), "not_found");
}

#[test]
fn a_corrupted_journal_is_invalid_with_a_code_and_a_placeholder_title() {
    let (fixture, alpha, _) = fixture_with_workspaces();
    let (id, path) = core_session(&fixture, &alpha, "Soon broken");
    fs::write(&path, b"this is not a journal\n").expect("corrupt the journal");
    let service = service(&fixture);
    let listed = tree(&service, "active", "updated", true);
    let broken = listed
        .groups
        .iter()
        .flat_map(|group| &group.sessions)
        .find(|session| session.session_id == id)
        .expect("the corrupt session is listed");
    assert_eq!(broken.status, SessionStatus::Invalid);
    assert_eq!(broken.title, "(unreadable session)");
    let code = broken.invalid_code.as_deref().expect("an invalid code");
    assert!(code.starts_with("invalid_journal"), "{code}");

    let error = service
        .session_transcript(wire(
            json!({"session_id": id, "after_seq": null, "limit": 10}),
        ))
        .expect_err("a corrupt journal has no transcript");
    assert_eq!(error.code(), "invalid_journal");
    let error = service
        .session_archive(wire(session_request(id)))
        .expect_err("a corrupt journal cannot be archived");
    assert_eq!(error.code(), "invalid_journal");
}

#[cfg(windows)]
#[test]
fn a_journal_held_by_another_writer_is_busy() {
    let (fixture, alpha, _) = fixture_with_workspaces();
    let writer = SessionJournal::create(&fixture.data, &alpha.path, "Held elsewhere")
        .expect("create and hold");
    let id = writer.snapshot().session_id;
    let service = service(&fixture);
    let listed = tree(&service, "active", "updated", true);
    let held = &listed.groups[0].sessions[0];
    assert_eq!(held.session_id, id);
    assert_eq!(held.status, SessionStatus::Busy);
    assert_eq!(held.title, "(session in use)");

    let error = service
        .session_transcript(wire(
            json!({"session_id": id, "after_seq": null, "limit": 10}),
        ))
        .expect_err("a held journal cannot be inspected on Windows");
    assert_eq!(error.code(), "busy");
    let error = service
        .session_archive(wire(session_request(id)))
        .expect_err("a held journal cannot be opened");
    assert_eq!(error.code(), "busy");
    drop(writer);
}

#[test]
fn hostile_titles_and_folders_come_back_escaped() {
    let fixture = Fixture::new();
    let hostile_folder = "evil\u{202e}dir";
    let workspace = fixture.workspace(hostile_folder, text_only());
    let hostile_title = "Pay \u{202e}txt.exe\u{200b} now\u{1b}[2J";
    let (id, _) = core_session(&fixture, &workspace, hostile_title);
    let service = service(&fixture);
    let listed = tree(&service, "active", "updated", true);
    let group = &listed.groups[0];
    let session = &group.sessions[0];
    assert_eq!(session.session_id, id);
    assert_eq!(session.title, display_safe_line(hostile_title));
    assert!(session.title.contains("\\u{202e}"));
    for text in [&session.title, &group.folder_display] {
        assert!(
            text.chars()
                .all(|c| !c.is_control() && !matches!(c, '\u{202e}' | '\u{200b}')),
            "hidden characters must be escaped in {text:?}"
        );
    }
    assert!(group.folder_display.contains("\\u{202e}"));

    // The transcript page and search results are escaped the same way.
    let page = transcript(&service, id, None, 10);
    assert_eq!(page.session.title, session.title);
    assert_eq!(page.folder_display, group.folder_display);
}

fn transcript(
    service: &DesktopService,
    id: Uuid,
    after: Option<u64>,
    limit: u32,
) -> TranscriptPage {
    service
        .session_transcript(wire(
            json!({"session_id": id, "after_seq": after, "limit": limit}),
        ))
        .expect("transcript")
}

#[test]
fn search_maps_roles_sequence_numbers_snippets_and_the_view() {
    let (fixture, alpha, beta) = fixture_with_workspaces();
    let (first, path) = core_session(&fixture, &alpha, "Orchard notes");
    {
        let mut journal = SessionJournal::open(&path).expect("open");
        journal
            .record_message(MessageRole::User, "tell me about the quince tree")
            .expect("user");
        journal
            .record_message(MessageRole::Assistant, "a quince is a fruit")
            .expect("assistant");
    }
    let (second, _) = core_session(&fixture, &beta, "Quince recipes");
    let service = service(&fixture);

    let search = |query: &str, view: &str, limit: u32| {
        service
            .session_search(wire(json!({"query": query, "view": view, "limit": limit})))
            .expect("search")
    };
    let result = search("quince", "active", 50);
    assert!(!result.truncated);
    let mut found: Vec<(Uuid, Option<u64>, SearchRoleDto)> = result
        .hits
        .iter()
        .map(|hit| (hit.session.session_id, hit.seq, hit.role))
        .collect();
    found.sort_by_key(|(id, seq, _)| (*id, *seq));
    let mut expected = vec![
        (first, Some(1), SearchRoleDto::User),
        (first, Some(2), SearchRoleDto::Assistant),
        (second, None, SearchRoleDto::Title),
    ];
    expected.sort_by_key(|(id, seq, _)| (*id, *seq));
    assert_eq!(found, expected);
    for hit in &result.hits {
        assert!(hit.snippet.iter().any(|part| part.matched), "{hit:?}");
        let joined: String = hit.snippet.iter().map(|part| part.text.as_str()).collect();
        assert!(joined.to_lowercase().contains("quince"));
        assert!(!hit.folder_display.is_empty());
    }

    // The limit clamps to at least one hit, and truncation is reported.
    let clamped = search("quince", "active", 0);
    assert_eq!(clamped.hits.len(), 1);
    assert!(clamped.truncated);

    // The view filter: archive the first session.
    service
        .session_archive(wire(session_request(first)))
        .expect("archive");
    assert_eq!(search("quince", "active", 50).hits.len(), 1);
    assert_eq!(search("quince", "archived", 50).hits.len(), 2);

    let error = service
        .session_search(wire(json!({"query": "   ", "view": "active", "limit": 10})))
        .expect_err("an empty query is invalid");
    assert_eq!(error.code(), "invalid_request");
}

/// A journal that records every kind of event, as the core inspection test does.
fn rich_journal(fixture: &Fixture, workspace: &Workspace) -> (Uuid, PathBuf, Uuid, Uuid) {
    let mut journal =
        SessionJournal::create(&fixture.data, &workspace.path, "Everything").expect("create");
    let id = journal.snapshot().session_id;
    let path = journal.path().to_path_buf();
    journal
        .record_message(MessageRole::User, "hello")
        .expect("user");
    let first = journal.start_turn().expect("turn one");
    journal
        .record_manifest(first, manifest())
        .expect("manifest");
    journal
        .record_tool_request(
            first,
            "call-1",
            "workspace_read_file",
            "{\"path\":\"a.txt\"}",
        )
        .expect("tool request");
    let rendered = "file contents";
    journal
        .record_tool_result(
            first,
            "call-1",
            ToolOutcome::Succeeded,
            None,
            u64::try_from(rendered.len()).expect("size"),
            &sha256_hex(rendered),
        )
        .expect("tool result");
    journal
        .record_tool_message("call-1", rendered)
        .expect("tool message");
    journal
        .record_message(MessageRole::Assistant, "done")
        .expect("assistant");
    journal.complete_turn(first).expect("complete");
    journal
        .record_message(MessageRole::User, "second")
        .expect("second user");
    let second = journal.start_turn().expect("turn two");
    journal
        .fail_turn(second, "provider_unreachable")
        .expect("fail");
    let summary = "Earlier work read a file.";
    journal
        .record_compaction(CompactionRecord {
            summary: summary.into(),
            compacted_messages: 3,
            includes_tool_results: true,
            summary_sha256: sha256_hex(summary),
            model: "fixture".into(),
            prompt_tokens: Some(10),
            completion_tokens: Some(4),
        })
        .expect("compact");
    journal.archive().expect("archive");
    journal.restore().expect("restore");
    (id, path, first, second)
}

#[test]
fn every_transcript_entry_kind_maps_to_its_wire_shape() {
    let (fixture, alpha, _) = fixture_with_workspaces();
    let (id, _, first, second) = rich_journal(&fixture, &alpha);
    let service = service(&fixture);
    let page = transcript(&service, id, None, 200);
    assert_eq!(page.next_after_seq, None);
    assert!(!page.incomplete_tail);
    assert_eq!(page.session.session_id, id);
    assert_eq!(page.session.status, SessionStatus::Idle);
    assert!(page.folder_display.ends_with("alpha-project"));

    let entries: Vec<Value> = page.entries.iter().map(json_of).collect();
    let kinds: Vec<&str> = entries
        .iter()
        .map(|entry| entry["kind"].as_str().expect("kind"))
        .collect();
    assert_eq!(
        kinds,
        [
            "message",
            "turn_started",
            "request_manifest",
            "tool_requested",
            "tool_completed",
            "message",
            "message",
            "turn_completed",
            "message",
            "turn_started",
            "turn_failed",
            "context_compacted",
            "session_archived",
            "session_restored",
        ]
    );
    let seqs: Vec<u64> = entries
        .iter()
        .map(|entry| entry["seq"].as_u64().expect("seq"))
        .collect();
    assert_eq!(seqs, (1..=14).collect::<Vec<u64>>());

    assert_eq!(entries[0]["role"], "user");
    assert_eq!(entries[0]["source"], "user_cli");
    assert_eq!(entries[0]["text"], "hello");
    assert!(entries[0]["call_id"].is_null());
    assert_eq!(entries[1]["request_id"], first.to_string());
    assert_eq!(entries[2]["model"], "local-model");
    assert_eq!(entries[2]["prompt_version"], "7");
    assert_eq!(entries[2]["tools"], json!(["workspace_read_file"]));
    assert_eq!(entries[3]["tool"], "workspace_read_file");
    assert_eq!(entries[3]["arguments"], "{\"path\":\"a.txt\"}");
    assert_eq!(entries[4]["outcome"], "succeeded");
    assert!(entries[4]["code"].is_null());
    assert_eq!(entries[4]["output_bytes"], 13);
    assert!(entries[4].get("output_sha256").is_none(), "digest omitted");
    assert_eq!(entries[5]["role"], "tool");
    assert_eq!(entries[5]["source"], "core_tool");
    assert_eq!(entries[5]["call_id"], "call-1");
    assert_eq!(entries[6]["role"], "assistant");
    assert_eq!(entries[6]["source"], "provider");
    assert_eq!(entries[10]["request_id"], second.to_string());
    assert_eq!(entries[10]["code"], "provider_unreachable");
    assert_eq!(entries[11]["summary"], "Earlier work read a file.");
    assert_eq!(entries[11]["compacted_messages"], 3);
    assert_eq!(entries[11]["model"], "fixture");
    for entry in &entries {
        let stamp = entry["timestamp"].as_str().expect("timestamp");
        assert!(stamp.ends_with('Z') && stamp.contains('T'), "{stamp}");
    }
}

#[test]
fn transcript_pages_follow_after_seq_and_limit_until_no_entries_remain() {
    let (fixture, alpha, _) = fixture_with_workspaces();
    let (id, _, _, _) = rich_journal(&fixture, &alpha);
    let service = service(&fixture);

    let mut seen = Vec::new();
    let mut after = None;
    let mut pages = 0;
    loop {
        let page = transcript(&service, id, after, 5);
        assert!(page.entries.len() <= 5);
        seen.extend(page.entries.iter().map(|entry| entry.seq));
        pages += 1;
        match page.next_after_seq {
            Some(next) => {
                assert_eq!(Some(next), page.entries.last().map(|entry| entry.seq));
                after = Some(next);
            }
            None => break,
        }
        assert!(pages < 10, "paging must terminate");
    }
    assert_eq!(seen, (1..=14).collect::<Vec<u64>>());
    assert_eq!(pages, 3);

    // An exact fit has no next page; a limit of zero clamps to one entry.
    assert_eq!(transcript(&service, id, Some(9), 5).next_after_seq, None);
    let one = transcript(&service, id, None, 0);
    assert_eq!(one.entries.len(), 1);
    assert_eq!(one.next_after_seq, Some(1));
    // Past the end there is nothing.
    let none = transcript(&service, id, Some(14), 5);
    assert!(none.entries.is_empty() && none.next_after_seq.is_none());
}

#[test]
fn a_transcript_page_holds_at_most_one_mebibyte_of_text() {
    let (fixture, alpha, _) = fixture_with_workspaces();
    let (id, path) = core_session(&fixture, &alpha, "Heavy");
    let chunk = "x".repeat(16_000);
    {
        let mut journal = SessionJournal::open(&path).expect("open");
        for _ in 0..80 {
            journal
                .record_message(MessageRole::User, &chunk)
                .expect("message");
        }
    }
    let service = service(&fixture);
    let page = transcript(&service, id, None, 200);
    // 65 entries of 16,000 bytes fit in 1 MiB; a 66th would exceed it.
    assert_eq!(page.entries.len(), 65);
    assert_eq!(page.next_after_seq, Some(65));
    let rest = transcript(&service, id, page.next_after_seq, 200);
    assert_eq!(rest.entries.len(), 15);
    assert_eq!(rest.next_after_seq, None);
}

#[test]
fn an_incomplete_tail_is_reported_and_an_unknown_session_is_not_found() {
    let (fixture, alpha, _) = fixture_with_workspaces();
    let (id, path) = core_session(&fixture, &alpha, "Mid-append");
    OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open for append")
        .write_all(b"{\"partial")
        .expect("partial record");
    let service = service(&fixture);
    let page = transcript(&service, id, None, 10);
    assert!(page.incomplete_tail);

    let missing = service
        .session_transcript(wire(
            json!({"session_id": Uuid::new_v4(), "after_seq": null, "limit": 10}),
        ))
        .expect_err("unknown session");
    assert_eq!(missing.code(), "not_found");
}

#[test]
fn a_session_the_index_has_not_seen_is_found_after_one_refresh() {
    let (fixture, alpha, _) = fixture_with_workspaces();
    let service = service(&fixture);
    // Warm the index, then add a journal behind its back.
    tree(&service, "active", "updated", true);
    let (id, _) = core_session(&fixture, &alpha, "Late arrival");
    let page = transcript(&service, id, None, 10);
    assert_eq!(page.session.title, "Late arrival");
}

#[test]
fn workspace_listing_reports_the_store_state_and_permissions() {
    let bare = Fixture::bare();
    let service = service(&bare);
    let empty = service.workspace_list().expect("workspace list");
    assert!(!empty.development_root_set);
    assert!(empty.workspaces.is_empty());
    let error = service
        .session_create(wire(json!({"workspace_id": Uuid::new_v4(), "title": "t"})))
        .expect_err("no store, no workspace");
    assert_eq!(error.code(), "unknown_workspace");

    let fixture = Fixture::new();
    let service = fixture.service(NO_ENDPOINT);
    let rooted = service.workspace_list().expect("workspace list");
    assert!(rooted.development_root_set);
    assert!(rooted.workspaces.is_empty());

    let tools = fixture.workspace("tools-on", read_tools());
    let plain = fixture.workspace("plain", text_only());
    let listed = service.workspace_list().expect("workspace list");
    assert_eq!(listed.workspaces.len(), 2);
    let find = |id: Uuid| {
        listed
            .workspaces
            .iter()
            .find(|choice| choice.workspace_id == id)
            .expect("registered workspace")
    };
    let tools_on = find(tools.id);
    assert!(tools_on.folder_display.ends_with("tools-on"));
    assert!(
        tools_on.model_tools
            && tools_on.read.permits()
            && tools_on.write == Access::Deny
            && tools_on.execute == Access::Deny
    );
    let plain_off = find(plain.id);
    assert!(
        !plain_off.model_tools
            && plain_off.read == Access::Deny
            && plain_off.write == Access::Deny
            && plain_off.execute == Access::Deny
    );
}

#[test]
fn creating_a_session_uses_a_registered_workspace_and_core_validates_the_title() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("created-here", text_only());
    let service = service(&fixture);

    let summary = service
        .session_create(wire(
            json!({"workspace_id": workspace.id, "title": "Fresh \u{202e}title"}),
        ))
        .expect("create");
    assert_eq!(summary.status, SessionStatus::Idle);
    assert!(!summary.archived);
    assert_eq!(summary.event_count, 1);
    assert_eq!(summary.title, display_safe_line("Fresh \u{202e}title"));
    assert!(summary.created_at.is_some() && summary.updated_at.is_some());

    let listed = tree(&service, "active", "updated", true);
    assert_eq!(listed.groups.len(), 1);
    assert_eq!(listed.groups[0].sessions[0].session_id, summary.session_id);
    assert!(listed.groups[0].folder_display.ends_with("created-here"));

    let unknown = service
        .session_create(wire(json!({"workspace_id": Uuid::new_v4(), "title": "t"})))
        .expect_err("unregistered workspace");
    assert_eq!(unknown.code(), "unknown_workspace");
    let empty = service
        .session_create(wire(json!({"workspace_id": workspace.id, "title": "   "})))
        .expect_err("a blank title");
    assert_eq!(empty.code(), "invalid_request");
    assert_eq!(
        tree(&service, "active", "updated", true).indexed_sessions,
        1,
        "a refused create leaves no session"
    );
}

#[test]
fn a_service_opens_on_a_new_data_root_and_lists_nothing() {
    let fixture = Fixture::bare();
    let service = DesktopService::open_with_token_source(
        omgcode_app::desktop::DesktopConfig {
            data_root: fixture.data.join("not-yet-created"),
            endpoint: NO_ENDPOINT.to_owned(),
            token_source: omgcode_app::TokenSource::Environment,
            stream: true,
        },
        no_token,
    )
    .expect("open on a missing directory");
    let empty = tree(&service, "active", "updated", true);
    assert!(empty.groups.is_empty());
    assert_eq!(empty.indexed_sessions, 0);
}
