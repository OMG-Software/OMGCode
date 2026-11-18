//! Native tests for read-only journal inspection, discovery, and resolution.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use omgcode_core::session::{
    CompactionRecord, MessageRole, MessageSource, RequestManifest, SessionError, SessionInspection,
    SessionJournal, ToolOutcome, TranscriptEntryKind, discover, locate,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use uuid::Uuid;

fn setup() -> (TempDir, PathBuf, PathBuf) {
    let temp = TempDir::new().expect("temporary directory");
    let root = temp.path().join("app-data");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace directory");
    (temp, root, workspace)
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn manifest() -> RequestManifest {
    RequestManifest {
        base_prompt: "BASE-PROMPT-TEXT".into(),
        prompt_version: "7".into(),
        prompt_sha256: "a".repeat(64),
        custom_instructions: "SECRET-CUSTOM-INSTRUCTIONS".into(),
        model: "local-model".into(),
        base_url: "http://127.0.0.1:1234/v1".into(),
        context_tokens: 32_000,
        max_output_tokens: 2_000,
        tool_schema_version: Some("1".into()),
        tools: Some(vec!["workspace_read_file".into(), "workspace_list".into()]),
        skills: Vec::new(),
    }
}

/// Create a small idle journal with `messages` user messages and release it.
fn small_journal(root: &Path, workspace: &Path, messages: usize) -> PathBuf {
    let mut journal = SessionJournal::create(root, workspace, "Small").expect("create");
    for index in 0..messages {
        journal
            .record_message(MessageRole::User, &format!("message {index}"))
            .expect("message");
    }
    journal.path().to_path_buf()
}

/// Replace the journal's text with `edit(text)`.
fn rewrite(path: &Path, edit: impl FnOnce(String) -> String) {
    let text = fs::read_to_string(path).expect("journal text");
    fs::write(path, edit(text)).expect("rewrite journal");
}

/// Create a file symlink. Returns false, after saying so, when this host does
/// not allow the calling account to create one.
fn try_symlink_file(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let result = std::os::windows::fs::symlink_file(target, link);
    report_symlink(result)
}

fn try_symlink_dir(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let result = std::os::windows::fs::symlink_dir(target, link).or_else(|_| {
        // A junction needs no privilege and is also a reparse point.
        let status = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other("mklink /J failed"))
        }
    });
    report_symlink(result)
}

fn report_symlink(result: std::io::Result<()>) -> bool {
    match result {
        Ok(()) => true,
        Err(error) => {
            eprintln!("SKIPPED link assertions: this host cannot create a symlink: {error}");
            false
        }
    }
}

#[test]
fn inspection_reports_header_and_every_recorded_event() {
    let (_temp, root, workspace) = setup();
    let mut journal =
        SessionJournal::create(&root, &workspace, "Inspect everything").expect("create");
    let path = journal.path().to_path_buf();
    let workspace_id = journal.snapshot().workspace_id;
    let session_id = journal.snapshot().session_id;
    let native_workspace = journal.snapshot().workspace.clone();

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
    let record = CompactionRecord {
        summary: summary.into(),
        compacted_messages: 3,
        includes_tool_results: true,
        summary_sha256: sha256_hex(summary),
        model: "fixture".into(),
        prompt_tokens: Some(10),
        completion_tokens: Some(4),
    };
    journal.record_compaction(record.clone()).expect("compact");
    journal.archive().expect("archive");
    journal.restore().expect("restore");
    drop(journal);

    let inspection = SessionInspection::inspect(&path).expect("inspect");

    assert_eq!(inspection.session_id, session_id);
    assert_eq!(inspection.workspace_id, workspace_id);
    assert_eq!(inspection.workspace, native_workspace);
    assert_eq!(inspection.title, "Inspect everything");
    assert!(!inspection.archived);
    assert_eq!(inspection.active_turn, None);
    assert_eq!(inspection.schema_version, 3);
    assert_eq!(inspection.event_count, 15);
    assert!(!inspection.incomplete_tail);
    assert!(inspection.created_at <= inspection.updated_at);
    let last = inspection.entries.last().expect("entries");
    assert_eq!(inspection.updated_at, last.timestamp);

    let seqs: Vec<u64> = inspection.entries.iter().map(|entry| entry.seq).collect();
    assert_eq!(seqs, (1..=14).collect::<Vec<u64>>());
    let kinds: Vec<&TranscriptEntryKind> =
        inspection.entries.iter().map(|entry| &entry.kind).collect();
    let expected = vec![
        TranscriptEntryKind::Message {
            role: MessageRole::User,
            source: MessageSource::UserCli,
            text: "hello".into(),
            call_id: None,
        },
        TranscriptEntryKind::TurnStarted { request_id: first },
        TranscriptEntryKind::RequestManifest {
            request_id: first,
            model: "local-model".into(),
            prompt_version: "7".into(),
            tools: vec!["workspace_read_file".into(), "workspace_list".into()],
        },
        TranscriptEntryKind::ToolRequested {
            request_id: first,
            call_id: "call-1".into(),
            tool: "workspace_read_file".into(),
            arguments: "{\"path\":\"a.txt\"}".into(),
        },
        TranscriptEntryKind::ToolCompleted {
            request_id: first,
            call_id: "call-1".into(),
            outcome: ToolOutcome::Succeeded,
            code: None,
            output_bytes: 13,
            output_sha256: sha256_hex(rendered),
        },
        TranscriptEntryKind::Message {
            role: MessageRole::Tool,
            source: MessageSource::CoreTool,
            text: rendered.into(),
            call_id: Some("call-1".into()),
        },
        TranscriptEntryKind::Message {
            role: MessageRole::Assistant,
            source: MessageSource::Provider,
            text: "done".into(),
            call_id: None,
        },
        TranscriptEntryKind::TurnCompleted { request_id: first },
        TranscriptEntryKind::Message {
            role: MessageRole::User,
            source: MessageSource::UserCli,
            text: "second".into(),
            call_id: None,
        },
        TranscriptEntryKind::TurnStarted { request_id: second },
        TranscriptEntryKind::TurnFailed {
            request_id: second,
            code: "provider_unreachable".into(),
        },
        TranscriptEntryKind::ContextCompacted { record },
        TranscriptEntryKind::SessionArchived,
        TranscriptEntryKind::SessionRestored,
    ];
    assert_eq!(kinds, expected.iter().collect::<Vec<_>>());

    // The serialized form tags each entry, uses RFC 3339 times, and leaves the
    // base prompt and custom instructions out of the manifest entry.
    let json = serde_json::to_value(&inspection).expect("serialize");
    assert_eq!(json["entries"][0]["kind"], "message");
    assert_eq!(json["entries"][2]["kind"], "request_manifest");
    assert_eq!(json["entries"][10]["kind"], "turn_failed");
    assert!(
        json["created_at"]
            .as_str()
            .is_some_and(|text| text.ends_with('Z'))
    );
    let text = json.to_string();
    assert!(!text.contains("BASE-PROMPT-TEXT"));
    assert!(!text.contains("SECRET-CUSTOM-INSTRUCTIONS"));
}

#[test]
fn inspection_reports_an_active_turn_and_an_archived_session() {
    let (_temp, root, workspace) = setup();
    let mut journal = SessionJournal::create(&root, &workspace, "States").expect("create");
    let path = journal.path().to_path_buf();
    let request = journal.start_turn().expect("turn");
    drop(journal);
    let active = SessionInspection::inspect(&path).expect("inspect active");
    assert_eq!(active.active_turn, Some(request));
    assert!(!active.archived);

    let mut journal = SessionJournal::open(&path).expect("reopen");
    journal.complete_turn(request).expect("complete");
    journal.archive().expect("archive");
    drop(journal);
    let archived = SessionInspection::inspect(&path).expect("inspect archived");
    assert!(archived.archived);
    assert_eq!(archived.active_turn, None);
}

#[cfg(windows)]
#[test]
fn inspection_of_a_live_writer_is_busy_and_disturbs_nothing() {
    let (_temp, root, workspace) = setup();
    let mut journal = SessionJournal::create(&root, &workspace, "Live").expect("create");
    let path = journal.path().to_path_buf();
    for _ in 0..5 {
        assert!(matches!(
            SessionInspection::inspect(&path),
            Err(SessionError::Busy)
        ));
    }
    // The writer is unaffected, and the observer did not take the lock.
    journal
        .record_message(MessageRole::User, "still appending")
        .expect("append after refused inspections");
    drop(journal);
    let inspection = SessionInspection::inspect(&path).expect("inspect after release");
    assert_eq!(inspection.event_count, 2);
}

#[cfg(unix)]
#[test]
fn inspection_of_a_live_writer_succeeds_and_disturbs_nothing() {
    let (_temp, root, workspace) = setup();
    let mut journal = SessionJournal::create(&root, &workspace, "Live").expect("create");
    let path = journal.path().to_path_buf();
    let inspection = SessionInspection::inspect(&path).expect("inspect beside a writer");
    assert_eq!(inspection.event_count, 1);
    journal
        .record_message(MessageRole::User, "still appending")
        .expect("append after inspection");
    assert_eq!(
        SessionInspection::inspect(&path)
            .expect("again")
            .event_count,
        2
    );
}

#[test]
fn a_writer_opens_immediately_after_many_inspections() {
    let (_temp, root, workspace) = setup();
    let path = small_journal(&root, &workspace, 2);
    for _ in 0..200 {
        SessionInspection::inspect(&path).expect("inspect idle journal");
        // An inspection that returned must leave no lock behind.
        drop(SessionJournal::open(&path).expect("writer opens right after an inspection"));
    }
    for _ in 0..200 {
        SessionInspection::inspect(&path).expect("inspect idle journal");
    }
    let mut writer = SessionJournal::open(&path).expect("writer opens after the loop");
    writer
        .record_message(MessageRole::Assistant, "ok")
        .expect("append");
}

#[test]
fn an_incomplete_tail_is_ignored_without_changing_the_file() {
    let (_temp, root, workspace) = setup();
    let path = small_journal(&root, &workspace, 2);
    let mut file = OpenOptions::new().append(true).open(&path).expect("append");
    file.write_all(b"{\"schema_version\":3,\"session_id\":\"partial")
        .expect("partial record");
    drop(file);
    let before = fs::read(&path).expect("bytes");

    let inspection = SessionInspection::inspect(&path).expect("inspect with tail");
    assert!(inspection.incomplete_tail);
    assert_eq!(inspection.event_count, 3);
    assert_eq!(inspection.entries.len(), 2);
    assert_eq!(fs::read(&path).expect("bytes after"), before);

    // The writer path is unchanged: it still refuses the incomplete tail.
    assert!(matches!(
        SessionJournal::open(&path),
        Err(SessionError::InvalidJournal(_))
    ));
    assert_eq!(fs::read(&path).expect("bytes after open"), before);
    // And that refused open released its lock.
    assert!(SessionInspection::inspect(&path).is_ok());
}

#[test]
fn a_journal_with_no_complete_record_is_invalid() {
    let (_temp, root, workspace) = setup();
    let path = small_journal(&root, &workspace, 0);
    let file = OpenOptions::new().write(true).open(&path).expect("open");
    file.set_len(20)
        .expect("truncate to a partial first record");
    drop(file);
    assert!(matches!(
        SessionInspection::inspect(&path),
        Err(SessionError::InvalidJournal(_))
    ));
    fs::write(&path, b"").expect("empty file");
    assert!(matches!(
        SessionInspection::inspect(&path),
        Err(SessionError::InvalidJournal(_))
    ));
}

#[test]
fn interior_corruption_is_rejected() {
    let (_temp, root, workspace) = setup();
    let path = small_journal(&root, &workspace, 3);
    rewrite(&path, |text| {
        let mut lines: Vec<&str> = text.lines().collect();
        lines[1] = "{not json}";
        lines.join("\n") + "\n"
    });
    assert!(matches!(
        SessionInspection::inspect(&path),
        Err(SessionError::InvalidJournal(_))
    ));
}

#[test]
fn duplicate_keys_are_rejected() {
    let (_temp, root, workspace) = setup();
    let path = small_journal(&root, &workspace, 2);
    rewrite(&path, |text| {
        text.replacen("\"seq\":1,", "\"seq\":1,\"seq\":1,", 1)
    });
    assert!(matches!(
        SessionInspection::inspect(&path),
        Err(SessionError::InvalidJournal(_))
    ));
}

#[test]
fn a_session_id_that_differs_from_the_filename_is_rejected() {
    let (_temp, root, workspace) = setup();
    let path = small_journal(&root, &workspace, 1);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("name");
    let (stamp, _) = name.rsplit_once("Z-").expect("stamp");
    let renamed = path.with_file_name(format!("{stamp}Z-{}.jsonl", Uuid::new_v4()));
    fs::rename(&path, &renamed).expect("rename");
    assert!(matches!(
        SessionInspection::inspect(&renamed),
        Err(SessionError::InvalidJournal(_))
    ));
}

#[test]
fn a_non_contiguous_sequence_is_rejected() {
    let (_temp, root, workspace) = setup();
    let path = small_journal(&root, &workspace, 3);
    rewrite(&path, |text| text.replacen("\"seq\":2,", "\"seq\":5,", 1));
    assert!(matches!(
        SessionInspection::inspect(&path),
        Err(SessionError::InvalidJournal(_))
    ));
}

#[test]
fn an_oversized_journal_is_rejected_without_reading_it_all() {
    let (_temp, root, workspace) = setup();
    let path = small_journal(&root, &workspace, 1);
    let mut file = OpenOptions::new().append(true).open(&path).expect("append");
    file.write_all(&vec![b'a'; 9 * 1024 * 1024])
        .expect("padding");
    drop(file);
    assert!(matches!(
        SessionInspection::inspect(&path),
        Err(SessionError::InvalidJournal(_))
    ));
}

#[test]
fn a_symlinked_journal_is_rejected() {
    let (temp, root, workspace) = setup();
    let path = small_journal(&root, &workspace, 1);
    let name = path.file_name().expect("name");
    let alias_directory = temp.path().join("alias");
    fs::create_dir(&alias_directory).expect("alias directory");
    let alias = alias_directory.join(name);
    if !try_symlink_file(&path, &alias) {
        return;
    }
    assert!(matches!(
        SessionInspection::inspect(&alias),
        Err(SessionError::InvalidJournal(_))
    ));
}

#[test]
fn an_unnamed_or_missing_journal_is_a_controlled_failure() {
    let (temp, _root, _workspace) = setup();
    let bad_name = temp.path().join("not-a-journal.jsonl");
    fs::write(&bad_name, b"").expect("file");
    assert!(matches!(
        SessionInspection::inspect(&bad_name),
        Err(SessionError::InvalidJournal(_))
    ));
    let missing = temp
        .path()
        .join(format!("omgcode-00-00-00-000Z-{}.jsonl", Uuid::new_v4()));
    assert!(matches!(
        SessionInspection::inspect(&missing),
        Err(SessionError::Io(_))
    ));
}

fn journal_name(id: Uuid) -> String {
    format!("omgcode-01-02-03-004Z-{id}.jsonl")
}

fn place(root: &Path, day: &str, name: &str, bytes: &[u8]) -> PathBuf {
    let directory = root.join("sessions").join(day);
    fs::create_dir_all(&directory).expect("directory");
    let path = directory.join(name);
    fs::write(&path, bytes).expect("file");
    path
}

#[test]
fn discovery_lists_a_nested_layout_in_relative_path_order() {
    let (temp, _root, _workspace) = setup();
    let root = temp.path().join("data");
    let ids: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
    place(&root, "2026/01/06", &journal_name(ids[0]), b"abc");
    place(&root, "2025/12/31", &journal_name(ids[1]), b"");
    place(&root, "2026/01/05", &journal_name(ids[2]), b"hello");
    place(&root, "2026/01/05", &journal_name(ids[3]), b"x");

    let found = discover(&root).expect("discover");
    assert_eq!(found.skipped_links, 0);
    assert_eq!(found.unrecognized, 0);
    let relative: Vec<PathBuf> = found
        .journals
        .iter()
        .map(|item| item.relative_path.clone())
        .collect();
    let mut sorted = relative.clone();
    sorted.sort();
    assert_eq!(relative, sorted, "sorted by relative path");
    assert_eq!(found.journals.len(), 4);
    assert_eq!(found.journals[0].session_id, ids[1]);
    assert_eq!(found.journals[0].size, 0);
    assert_eq!(
        found.journals[0].relative_path,
        Path::new("sessions")
            .join("2025")
            .join("12")
            .join("31")
            .join(journal_name(ids[1]))
    );
    for item in &found.journals {
        assert!(item.path.is_absolute());
        assert_eq!(item.path, root.join(&item.relative_path));
        assert!(item.path.is_file());
        assert_eq!(item.size, fs::metadata(&item.path).expect("metadata").len());
    }
    let again = discover(&root).expect("discover again");
    assert_eq!(again, found, "deterministic");
}

#[test]
fn discovery_counts_and_ignores_names_outside_the_layout() {
    let (temp, _root, _workspace) = setup();
    let root = temp.path().join("data");
    let id = Uuid::new_v4();
    let good = place(&root, "2026/01/05", &journal_name(id), b"");
    // Each of these is one unrecognized entry.
    place(&root, "2026/01/05", "notes.txt", b"");
    place(
        &root,
        "2026/01/05",
        &format!("{}.bak", journal_name(Uuid::new_v4())),
        b"",
    );
    place(
        &root,
        "2026/01/05",
        &journal_name(Uuid::new_v4()).to_uppercase(),
        b"",
    );
    place(
        &root,
        "2026/01/05",
        "omgcode-25-00-00-000Z-00000000-0000-4000-8000-000000000000.jsonl",
        b"",
    );
    // A journal-named file one level too high.
    place(&root, "2026/01", &journal_name(Uuid::new_v4()), b"");
    // Wrong-length directories, with a valid journal inside each.
    place(&root, "26/01/05", &journal_name(Uuid::new_v4()), b"");
    place(&root, "2026/1/05", &journal_name(Uuid::new_v4()), b"");
    place(&root, "2026/01/5", &journal_name(Uuid::new_v4()), b"");
    place(&root, "abcd/01/05", &journal_name(Uuid::new_v4()), b"");
    // A plain file where a year directory belongs.
    fs::write(root.join("sessions").join("2027"), b"").expect("file");

    let found = discover(&root).expect("discover");
    assert_eq!(found.journals.len(), 1);
    assert_eq!(found.journals[0].session_id, id);
    assert_eq!(found.journals[0].path, good);
    // 4 bad files, 1 high file, 4 bad directories, 1 plain file.
    assert_eq!(found.unrecognized, 10);
    assert_eq!(found.skipped_links, 0);
}

#[test]
fn discovery_skips_a_directory_link_that_points_outside() {
    let (temp, _root, _workspace) = setup();
    let root = temp.path().join("data");
    let outside = temp.path().join("outside");
    let outside_id = Uuid::new_v4();
    place(&outside, "x/01/05", &journal_name(outside_id), b"");
    let inner = outside.join("sessions").join("x");
    fs::create_dir_all(root.join("sessions")).expect("sessions");
    // The link takes a valid year name and points at a tree holding a journal.
    if !try_symlink_dir(&inner, &root.join("sessions").join("2030")) {
        return;
    }
    let real = Uuid::new_v4();
    place(&root, "2026/01/05", &journal_name(real), b"");

    let found = discover(&root).expect("discover");
    assert_eq!(found.skipped_links, 1);
    assert_eq!(found.journals.len(), 1);
    assert_eq!(found.journals[0].session_id, real);
    assert_eq!(locate(&root, outside_id).expect("locate"), None);
}

#[test]
fn discovery_skips_a_sessions_directory_that_is_a_link() {
    let (temp, _root, _workspace) = setup();
    let root = temp.path().join("data");
    let outside = temp.path().join("outside");
    place(&outside, "2026/01/05", &journal_name(Uuid::new_v4()), b"");
    fs::create_dir_all(&root).expect("root");
    if !try_symlink_dir(&outside.join("sessions"), &root.join("sessions")) {
        return;
    }
    let found = discover(&root).expect("discover");
    assert!(found.journals.is_empty());
    assert_eq!(found.skipped_links, 1);
}

#[test]
fn a_missing_sessions_directory_is_an_empty_result() {
    let (temp, _root, _workspace) = setup();
    let found = discover(&temp.path().join("never-created")).expect("empty");
    assert!(found.journals.is_empty());
    assert_eq!(found.skipped_links, 0);
    assert_eq!(found.unrecognized, 0);
    fs::create_dir_all(temp.path().join("data")).expect("root without sessions");
    assert!(
        discover(&temp.path().join("data"))
            .expect("empty")
            .journals
            .is_empty()
    );
}

#[test]
fn locate_resolves_real_journals_by_uuid_only() {
    let (_temp, root, workspace) = setup();
    let first = SessionJournal::create(&root, &workspace, "One").expect("one");
    let second = SessionJournal::create(&root, &workspace, "Two").expect("two");
    let (first_id, first_path) = (first.snapshot().session_id, first.path().to_path_buf());
    let second_id = second.snapshot().session_id;
    drop((first, second));

    assert_eq!(locate(&root, first_id).expect("locate"), Some(first_path));
    assert_ne!(
        locate(&root, first_id).expect("locate"),
        locate(&root, second_id).expect("locate")
    );
    assert_eq!(locate(&root, Uuid::new_v4()).expect("unknown"), None);
    let inspected =
        SessionInspection::inspect(&locate(&root, second_id).expect("locate").expect("found"))
            .expect("inspect located journal");
    assert_eq!(inspected.title, "Two");
}

#[test]
fn locate_refuses_a_duplicate_uuid_instead_of_picking_one() {
    let (temp, _root, _workspace) = setup();
    let root = temp.path().join("data");
    let id = Uuid::new_v4();
    place(&root, "2026/01/05", &journal_name(id), b"");
    place(&root, "2026/01/06", &journal_name(id), b"");
    assert_eq!(discover(&root).expect("discover").journals.len(), 2);
    assert!(matches!(
        locate(&root, id),
        Err(SessionError::InvalidJournal(_))
    ));
    assert_eq!(locate(&root, Uuid::new_v4()).expect("other id"), None);
}
