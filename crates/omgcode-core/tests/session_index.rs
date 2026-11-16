//! Native tests for the derived session index. Journals are real files made by
//! the real `SessionJournal`, and the index is a real SQLite database.

use std::fs;
use std::path::{Path, PathBuf};

use omgcode_core::session::{
    CompactionRecord, MessageRole, NativePath, RequestManifest, SessionInspection, SessionJournal,
    ToolOutcome,
};
use omgcode_core::session_index::{
    APPLICATION_ID, ArchiveView, INDEX_DIRECTORY, INDEX_FILE, IndexError, IndexedSession,
    IndexedState, SearchListing, SearchRole, SessionIndex, SessionListing, SessionQuery,
    SessionSort,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use time::OffsetDateTime;
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
        custom_instructions: "CUSTOM-INSTRUCTIONS-TEXT".into(),
        model: "local-model".into(),
        base_url: "http://127.0.0.1:1234/v1".into(),
        context_tokens: 32_000,
        max_output_tokens: 2_000,
        tool_schema_version: Some("1".into()),
        tools: Some(vec!["workspace_read_file".into()]),
    }
}

/// Create a journal with user messages, release it, and return id and path.
fn session(root: &Path, workspace: &Path, title: &str, messages: &[&str]) -> (Uuid, PathBuf) {
    let mut journal = SessionJournal::create(root, workspace, title).expect("create");
    let id = journal.snapshot().session_id;
    for message in messages {
        journal
            .record_message(MessageRole::User, message)
            .expect("message");
    }
    (id, journal.path().to_path_buf())
}

/// Move a journal file under another day directory and return its new path.
fn move_to_day(root: &Path, path: &Path, day: &str) -> PathBuf {
    let directory = root.join("sessions").join(day);
    fs::create_dir_all(&directory).expect("day directory");
    let target = directory.join(path.file_name().expect("file name"));
    fs::rename(path, &target).expect("move journal");
    target
}

fn append(path: &Path, text: &str) {
    let mut journal = SessionJournal::open(path).expect("open writer");
    journal
        .record_message(MessageRole::User, text)
        .expect("append");
}

fn database_path(root: &Path) -> PathBuf {
    root.join(INDEX_DIRECTORY).join(INDEX_FILE)
}

fn refreshed(root: &Path) -> SessionIndex {
    let mut index = SessionIndex::open(root).expect("open index");
    index.refresh(1000).expect("refresh");
    index
}

fn query(view: ArchiveView, sort: SessionSort, descending: bool, limit: usize) -> SessionQuery {
    SessionQuery {
        view,
        sort,
        descending,
        limit,
    }
}

fn list(index: &SessionIndex, view: ArchiveView) -> SessionListing {
    index
        .sessions(&query(view, SessionSort::Title, false, 5000))
        .expect("listing")
}

fn ids(listing: &SessionListing) -> Vec<Uuid> {
    listing.sessions.iter().map(|s| s.session_id).collect()
}

fn find(index: &SessionIndex, text: &str, view: ArchiveView) -> SearchListing {
    index.search(text, view, 50).expect("search")
}

fn nanos(time: OffsetDateTime) -> i128 {
    time.unix_timestamp_nanos()
}

fn raw(root: &Path) -> Connection {
    Connection::open(database_path(root)).expect("raw connection")
}

fn read_all(paths: &[PathBuf]) -> Vec<Vec<u8>> {
    paths
        .iter()
        .map(|path| fs::read(path).expect("journal bytes"))
        .collect()
}

#[test]
fn first_build_lists_every_session_with_the_inspected_metadata() {
    // Given sessions in two workspaces on different days, one archived.
    let (temp, root, workspace_a) = setup();
    let workspace_b = temp.path().join("workspace-b");
    fs::create_dir_all(&workspace_b).expect("second workspace");
    let (a1, p1) = session(&root, &workspace_a, "Alpha", &["one"]);
    let (b1, p2) = session(&root, &workspace_b, "Bravo", &["two", "three"]);
    let (a2, p3) = session(&root, &workspace_a, "Charlie", &[]);
    let p1 = move_to_day(&root, &p1, "2025/03/04");
    let p2 = move_to_day(&root, &p2, "2025/03/05");
    let _ = move_to_day(&root, &p3, "2024/12/31");
    {
        let mut journal = SessionJournal::open(&p2).expect("open");
        journal.archive().expect("archive");
    }

    // When the index is built.
    let mut index = SessionIndex::open(&root).expect("open");
    let report = index.refresh(1000).expect("refresh");

    // Then every journal was inspected and metadata matches the inspection.
    assert_eq!(report.discovered, 3);
    assert_eq!(report.inspected, 3);
    assert_eq!(
        report.unchanged + report.busy + report.invalid + report.removed,
        0
    );
    assert_eq!(report.remaining, 0);
    let active = list(&index, ArchiveView::Active);
    let archived = list(&index, ArchiveView::Archived);
    assert_eq!((active.total, archived.total), (2, 1));
    assert_eq!(ids(&archived), vec![b1]);
    let by_id = |listing: &SessionListing, id: Uuid| -> IndexedSession {
        listing
            .sessions
            .iter()
            .find(|s| s.session_id == id)
            .expect("session listed")
            .clone()
    };
    for (id, path, listing) in [(a1, &p1, &active), (b1, &p2, &archived)] {
        let inspection = SessionInspection::inspect(path).expect("inspect");
        let indexed = by_id(listing, id);
        assert_eq!(indexed.state, IndexedState::Ok);
        assert_eq!(indexed.invalid_code, None);
        assert_eq!(indexed.title.as_deref(), Some(inspection.title.as_str()));
        assert_eq!(indexed.workspace_id, Some(inspection.workspace_id));
        assert_eq!(indexed.workspace, Some(inspection.workspace.clone()));
        assert_eq!(
            indexed.folder_display,
            Some(
                inspection
                    .workspace
                    .to_path_buf()
                    .expect("native path")
                    .to_string_lossy()
                    .into_owned()
            )
        );
        assert_eq!(
            indexed.created_at.map(nanos),
            Some(nanos(inspection.created_at))
        );
        assert_eq!(
            indexed.updated_at.map(nanos),
            Some(nanos(inspection.updated_at))
        );
        assert_eq!(indexed.archived, inspection.archived);
        assert_eq!(indexed.active_turn, None);
        assert_eq!(indexed.event_count, inspection.event_count);
        assert!(!indexed.incomplete_tail);
        assert_eq!(index.session(id).expect("lookup"), Some(indexed));
    }
    assert_eq!(by_id(&active, a2).title.as_deref(), Some("Charlie"));
    assert_eq!(by_id(&active, a1).workspace, by_id(&active, a2).workspace);
    assert_ne!(
        by_id(&active, a1).workspace_id,
        by_id(&archived, b1).workspace_id
    );
    assert_eq!(index.session(Uuid::new_v4()).expect("unknown"), None);
}

#[test]
fn every_sort_key_orders_in_both_directions_with_deterministic_ties() {
    // Given sessions where three titles differ only by case.
    let (_temp, root, workspace) = setup();
    let mut known = Vec::new();
    for title in ["delta", "Same", "same", "SAME", "Éclair", "apple"] {
        let (id, path) = session(&root, &workspace, title, &["x"]);
        known.push((
            id,
            title.to_owned(),
            SessionInspection::inspect(&path).expect("inspect"),
        ));
    }
    let index = refreshed(&root);

    for sort in [
        SessionSort::Updated,
        SessionSort::Created,
        SessionSort::Title,
    ] {
        for descending in [false, true] {
            let mut expected: Vec<_> = known.iter().collect();
            expected.sort_by(|a, b| {
                let primary = match sort {
                    SessionSort::Updated => nanos(a.2.updated_at).cmp(&nanos(b.2.updated_at)),
                    SessionSort::Created => nanos(a.2.created_at).cmp(&nanos(b.2.created_at)),
                    SessionSort::Title => a.1.to_lowercase().cmp(&b.1.to_lowercase()),
                };
                let primary = if descending {
                    primary.reverse()
                } else {
                    primary
                };
                primary.then(a.0.cmp(&b.0))
            });
            let listing = index
                .sessions(&query(ArchiveView::Active, sort, descending, 5000))
                .expect("listing");
            let expected_ids: Vec<Uuid> = expected.iter().map(|k| k.0).collect();
            assert_eq!(
                ids(&listing),
                expected_ids,
                "{sort:?} descending={descending}"
            );
        }
    }
    // The tied titles come out in ascending session id whichever way titles sort.
    for descending in [false, true] {
        let listing = index
            .sessions(&query(
                ArchiveView::Active,
                SessionSort::Title,
                descending,
                5000,
            ))
            .expect("listing");
        let tied: Vec<Uuid> = listing
            .sessions
            .iter()
            .filter(|s| {
                s.title
                    .as_deref()
                    .is_some_and(|t| t.eq_ignore_ascii_case("same"))
            })
            .map(|s| s.session_id)
            .collect();
        let mut sorted = tied.clone();
        sorted.sort();
        assert_eq!(tied, sorted);
        assert_eq!(tied.len(), 3);
    }
}

#[test]
fn listing_reports_the_view_total_before_the_limit_and_clamps_the_limit() {
    let (_temp, root, workspace) = setup();
    for index in 0..5 {
        session(&root, &workspace, &format!("Session {index}"), &[]);
    }
    let index = refreshed(&root);

    let two = index
        .sessions(&query(ArchiveView::Active, SessionSort::Title, false, 2))
        .expect("two");
    assert_eq!((two.sessions.len(), two.total), (2, 5));
    let zero = index
        .sessions(&query(ArchiveView::Active, SessionSort::Title, false, 0))
        .expect("zero clamps to one");
    assert_eq!((zero.sessions.len(), zero.total), (1, 5));
    let huge = index
        .sessions(&query(
            ArchiveView::Active,
            SessionSort::Title,
            false,
            usize::MAX,
        ))
        .expect("huge clamps");
    assert_eq!((huge.sessions.len(), huge.total), (5, 5));
    let archived = list(&index, ArchiveView::Archived);
    assert_eq!((archived.sessions.len(), archived.total), (0, 0));
}

#[test]
fn a_second_refresh_inspects_nothing_and_an_append_reindexes_only_that_journal() {
    let (_temp, root, workspace) = setup();
    let mut paths = Vec::new();
    for index in 0..4 {
        paths.push(session(&root, &workspace, &format!("S{index}"), &["first"]).1);
    }
    let mut index = SessionIndex::open(&root).expect("open");
    assert_eq!(index.refresh(1000).expect("first").inspected, 4);

    let second = index.refresh(1000).expect("second");
    assert_eq!(
        (second.discovered, second.unchanged, second.inspected),
        (4, 4, 0)
    );
    assert_eq!(second.remaining, 0);

    append(&paths[2], "uniqueappended");
    let third = index.refresh(1000).expect("third");
    assert_eq!((third.inspected, third.unchanged), (1, 3));
    assert_eq!(
        find(&index, "uniqueappended", ArchiveView::Active)
            .hits
            .len(),
        1
    );
}

#[test]
fn a_deleted_journal_leaves_the_listing_and_the_search() {
    let (_temp, root, workspace) = setup();
    let (keep, _) = session(&root, &workspace, "Keep", &["keepword"]);
    let (gone, gone_path) = session(&root, &workspace, "Gone", &["goneword"]);
    let mut index = SessionIndex::open(&root).expect("open");
    index.refresh(1000).expect("first");
    assert_eq!(find(&index, "goneword", ArchiveView::Active).hits.len(), 1);

    fs::remove_file(&gone_path).expect("delete journal");
    let report = index.refresh(1000).expect("second");

    assert_eq!(
        (report.removed, report.discovered, report.unchanged),
        (1, 1, 1)
    );
    assert_eq!(ids(&list(&index, ArchiveView::Active)), vec![keep]);
    assert!(
        find(&index, "goneword", ArchiveView::Active)
            .hits
            .is_empty()
    );
    assert_eq!(find(&index, "keepword", ArchiveView::Active).hits.len(), 1);
    assert_eq!(index.session(gone).expect("lookup"), None);
    assert_eq!(index.journal_path(gone).expect("path lookup"), None);
}

#[test]
fn a_small_budget_leaves_remaining_work_and_repeated_calls_converge() {
    let (_temp, root, workspace) = setup();
    for index in 0..5 {
        session(&root, &workspace, &format!("S{index}"), &[]);
    }
    let mut index = SessionIndex::open(&root).expect("open");

    let first = index.refresh(2).expect("first");
    assert_eq!((first.inspected, first.remaining), (2, 3));
    assert_eq!(list(&index, ArchiveView::Active).total, 2);
    let second = index.refresh(2).expect("second");
    assert_eq!(
        (second.inspected, second.unchanged, second.remaining),
        (2, 2, 1)
    );
    let third = index.refresh(2).expect("third");
    assert_eq!(
        (third.inspected, third.unchanged, third.remaining),
        (1, 4, 0)
    );
    assert_eq!(list(&index, ArchiveView::Active).total, 5);
    // A zero budget is clamped to one journal rather than doing nothing.
    session(&root, &workspace, "Late", &[]);
    assert_eq!(index.refresh(0).expect("clamped").inspected, 1);
}

#[test]
fn archive_and_restore_are_reflected_after_a_refresh() {
    let (_temp, root, workspace) = setup();
    let (id, path) = session(&root, &workspace, "Cycle", &["body"]);
    let mut index = SessionIndex::open(&root).expect("open");
    index.refresh(1000).expect("first");
    assert_eq!(ids(&list(&index, ArchiveView::Active)), vec![id]);

    SessionJournal::open(&path)
        .expect("open")
        .archive()
        .expect("archive");
    index.refresh(1000).expect("after archive");
    assert!(list(&index, ArchiveView::Active).sessions.is_empty());
    assert_eq!(ids(&list(&index, ArchiveView::Archived)), vec![id]);
    assert!(index.session(id).expect("lookup").expect("row").archived);
    assert_eq!(find(&index, "body", ArchiveView::Archived).hits.len(), 1);
    assert!(find(&index, "body", ArchiveView::Active).hits.is_empty());

    SessionJournal::open(&path)
        .expect("open")
        .restore()
        .expect("restore");
    index.refresh(1000).expect("after restore");
    assert_eq!(ids(&list(&index, ArchiveView::Active)), vec![id]);
    assert!(list(&index, ArchiveView::Archived).sessions.is_empty());
}

#[test]
fn an_unfinished_turn_is_reported_as_the_active_turn() {
    let (_temp, root, workspace) = setup();
    let mut journal = SessionJournal::create(&root, &workspace, "Busy turn").expect("create");
    let id = journal.snapshot().session_id;
    let turn = journal.start_turn().expect("turn");
    drop(journal);

    let index = refreshed(&root);

    let indexed = index.session(id).expect("lookup").expect("row");
    assert_eq!(indexed.state, IndexedState::Ok);
    assert_eq!(indexed.active_turn, Some(turn));
}

#[cfg(windows)]
#[test]
fn a_journal_held_by_a_writer_keeps_its_previous_data_and_is_retried() {
    let (_temp, root, workspace) = setup();
    let (id, path) = session(&root, &workspace, "Orchard", &["original apple"]);
    let mut index = SessionIndex::open(&root).expect("open");
    index.refresh(1000).expect("first");
    let before = index.session(id).expect("lookup").expect("row");

    let mut writer = SessionJournal::open(&path).expect("hold the writer lock");
    writer
        .record_message(MessageRole::User, "added banana")
        .expect("append");
    let held = index.refresh(1000).expect("refresh beside writer");
    assert_eq!((held.inspected, held.busy), (1, 1));
    let busy = index.session(id).expect("lookup").expect("row");
    assert_eq!(busy.state, IndexedState::Busy);
    assert_eq!(busy.title, before.title);
    assert_eq!(busy.updated_at, before.updated_at);
    assert_eq!(busy.event_count, before.event_count);
    assert_eq!(find(&index, "apple", ArchiveView::Active).hits.len(), 1);
    assert!(find(&index, "banana", ArchiveView::Active).hits.is_empty());
    // A busy row is retried on every refresh, never treated as unchanged.
    assert_eq!(index.refresh(1000).expect("again").inspected, 1);

    drop(writer);
    let released = index.refresh(1000).expect("refresh after release");
    assert_eq!((released.inspected, released.busy), (1, 0));
    let ok = index.session(id).expect("lookup").expect("row");
    assert_eq!(ok.state, IndexedState::Ok);
    assert_eq!(ok.event_count, before.event_count + 1);
    assert_eq!(find(&index, "banana", ArchiveView::Active).hits.len(), 1);
}

#[cfg(windows)]
#[test]
fn a_never_indexed_journal_held_by_a_writer_gets_a_minimal_busy_row() {
    let (_temp, root, workspace) = setup();
    let writer = SessionJournal::create(&root, &workspace, "Held").expect("create");
    let id = writer.snapshot().session_id;
    let mut index = SessionIndex::open(&root).expect("open");

    let report = index.refresh(1000).expect("refresh");

    assert_eq!((report.inspected, report.busy), (1, 1));
    let row = index.session(id).expect("lookup").expect("row");
    assert_eq!(row.state, IndexedState::Busy);
    assert_eq!(
        (row.title, row.workspace, row.created_at, row.updated_at),
        (None, None, None, None)
    );
    assert_eq!((row.archived, row.event_count), (false, 0));
    // A busy row with no data counts as active and sorts after known rows.
    assert_eq!(ids(&list(&index, ArchiveView::Active)), vec![id]);
    let refreshed_row = index
        .refresh_session(id)
        .expect("refresh one")
        .expect("row");
    assert_eq!(refreshed_row.state, IndexedState::Busy);
    drop(writer);
    index.refresh(1000).expect("after release");
    assert_eq!(
        index
            .session(id)
            .expect("lookup")
            .expect("row")
            .title
            .as_deref(),
        Some("Held")
    );
}

#[cfg(unix)]
#[test]
fn inspection_beside_a_live_writer_indexes_the_journal() {
    let (_temp, root, workspace) = setup();
    let mut writer = SessionJournal::create(&root, &workspace, "Live").expect("create");
    let id = writer.snapshot().session_id;
    writer
        .record_message(MessageRole::User, "written while held")
        .expect("message");
    let mut index = SessionIndex::open(&root).expect("open");

    let report = index.refresh(1000).expect("refresh");

    assert_eq!((report.inspected, report.busy), (1, 0));
    assert_eq!(
        index.session(id).expect("lookup").expect("row").state,
        IndexedState::Ok
    );
    assert_eq!(find(&index, "written", ArchiveView::Active).hits.len(), 1);
}

#[test]
fn a_corrupt_journal_is_invalid_without_text_and_not_reinspected_until_it_changes() {
    let (_temp, root, workspace) = setup();
    let (id, path) = session(&root, &workspace, "Fragile", &["valuableword"]);
    let valid = fs::read(&path).expect("valid bytes");
    let mut index = SessionIndex::open(&root).expect("open");
    index.refresh(1000).expect("first");
    assert_eq!(
        find(&index, "valuableword", ArchiveView::Active).hits.len(),
        1
    );

    fs::write(&path, b"this is not a journal\n").expect("corrupt");
    let corrupt = index.refresh(1000).expect("second");
    assert_eq!((corrupt.inspected, corrupt.invalid), (1, 1));
    let row = index.session(id).expect("lookup").expect("row");
    assert_eq!(row.state, IndexedState::Invalid);
    let code = row.invalid_code.expect("code");
    assert!(code.starts_with("invalid_journal"), "{code}");
    assert_eq!(
        (row.title, row.created_at, row.workspace),
        (None, None, None)
    );
    assert!(
        find(&index, "valuableword", ArchiveView::Active)
            .hits
            .is_empty()
    );
    assert!(find(&index, "Fragile", ArchiveView::Active).hits.is_empty());

    let settled = index.refresh(1000).expect("third");
    assert_eq!(
        (settled.inspected, settled.unchanged, settled.invalid),
        (0, 1, 0)
    );

    fs::write(&path, &valid).expect("replace with a valid file");
    let fixed = index.refresh(1000).expect("fourth");
    assert_eq!((fixed.inspected, fixed.invalid), (1, 0));
    let row = index.session(id).expect("lookup").expect("row");
    assert_eq!(row.state, IndexedState::Ok);
    assert_eq!(row.invalid_code, None);
    assert_eq!(
        find(&index, "valuableword", ArchiveView::Active).hits.len(),
        1
    );
}

#[test]
fn a_journal_whose_events_name_another_session_is_invalid() {
    let (_temp, root, workspace) = setup();
    let (id, path) = session(&root, &workspace, "Mismatch", &["text"]);
    let other = Uuid::new_v4();
    let text = fs::read_to_string(&path)
        .expect("journal text")
        .replace(&id.to_string(), &other.to_string());
    // The file name still carries `id`; every event now carries `other`.
    fs::write(&path, text).expect("rewrite");

    let index = refreshed(&root);

    let row = index.session(id).expect("lookup").expect("row");
    assert_eq!(row.state, IndexedState::Invalid);
    assert!(row.invalid_code.is_some());
    assert!(find(&index, "text", ArchiveView::Active).hits.is_empty());
}

#[test]
fn two_files_with_one_session_id_are_invalid_and_neither_is_indexed() {
    let (_temp, root, workspace) = setup();
    let (id, path) = session(&root, &workspace, "Twin", &["twinword"]);
    let copy_directory = root.join("sessions/2020/01/01");
    fs::create_dir_all(&copy_directory).expect("directory");
    let copy = copy_directory.join(path.file_name().expect("name"));
    fs::copy(&path, &copy).expect("duplicate file");
    let mut index = SessionIndex::open(&root).expect("open");

    let report = index.refresh(1000).expect("refresh");

    assert_eq!(
        (report.discovered, report.invalid, report.inspected),
        (2, 1, 0)
    );
    let row = index.session(id).expect("lookup").expect("row");
    assert_eq!(row.state, IndexedState::Invalid);
    assert_eq!(row.invalid_code.as_deref(), Some("duplicate_session_id"));
    assert!(
        find(&index, "twinword", ArchiveView::Active)
            .hits
            .is_empty()
    );
    assert!(matches!(
        index.journal_path(id),
        Err(IndexError::InvalidIndexEntry(_))
    ));
    let settled = index.refresh(1000).expect("again");
    assert_eq!(
        (settled.unchanged, settled.invalid, settled.inspected),
        (1, 0, 0)
    );

    fs::remove_file(&copy).expect("remove the duplicate");
    let resolved = index.refresh(1000).expect("resolved");
    assert_eq!((resolved.inspected, resolved.invalid), (1, 0));
    assert_eq!(
        index.session(id).expect("lookup").expect("row").state,
        IndexedState::Ok
    );
    assert_eq!(find(&index, "twinword", ArchiveView::Active).hits.len(), 1);
}

#[test]
fn refresh_session_reinspects_one_session_regardless_of_size_and_time() {
    let (_temp, root, workspace) = setup();
    let (id, _) = session(&root, &workspace, "Genuine title", &["body"]);
    let mut index = SessionIndex::open(&root).expect("open");
    index.refresh(1000).expect("first");
    raw(&root)
        .execute("UPDATE journal SET title = 'Tampered title'", [])
        .expect("tamper");
    index.refresh(1000).expect("cache hit");
    assert_eq!(
        index
            .session(id)
            .expect("lookup")
            .expect("row")
            .title
            .as_deref(),
        Some("Tampered title")
    );

    let row = index
        .refresh_session(id)
        .expect("refresh one")
        .expect("row");

    assert_eq!(row.title.as_deref(), Some("Genuine title"));
    assert_eq!(
        index.refresh_session(Uuid::new_v4()).expect("unknown"),
        None
    );
}

#[test]
fn refresh_session_forgets_a_vanished_journal() {
    let (_temp, root, workspace) = setup();
    let (id, path) = session(&root, &workspace, "Vanishing", &["vanishword"]);
    let mut index = refreshed(&root);
    fs::remove_file(path).expect("delete");

    assert_eq!(index.refresh_session(id).expect("refresh one"), None);

    assert_eq!(index.session(id).expect("lookup"), None);
    assert!(
        find(&index, "vanishword", ArchiveView::Active)
            .hits
            .is_empty()
    );
}

fn sample_index() -> (TempDir, PathBuf, SessionIndex) {
    let (temp, root, workspace) = setup();
    session(
        &root,
        &workspace,
        "Zebra planning",
        &[
            "The quick brown fox jumps over the lazy dog",
            "Caf\u{e9} r\u{e9}sum\u{e9} and na\u{ef}ve fa\u{e7}ade",
        ],
    );
    session(&root, &workspace, "Other", &["a only b", "text only"]);
    let index = refreshed(&root);
    (temp, root, index)
}

#[test]
fn search_requires_every_term_and_matches_a_prefix_on_the_last_term_only() {
    let (_temp, _root, index) = sample_index();
    let hits = |text: &str| find(&index, text, ArchiveView::Active).hits.len();

    assert_eq!(hits("quick"), 1);
    assert_eq!(hits("quick dog"), 1);
    assert_eq!(hits("quick cat"), 0);
    assert_eq!(hits("dog quick"), 1);
    // The last term is a prefix; an earlier term must match a whole token.
    assert_eq!(hits("qui"), 1);
    assert_eq!(hits("brown fo"), 1);
    assert_eq!(hits("qui brown"), 0);
    // Case and diacritics do not matter.
    assert_eq!(hits("FOX"), 1);
    assert_eq!(hits("cafe"), 1);
    assert_eq!(hits("RESUME"), 1);
    assert_eq!(hits("naive facade"), 1);
    assert_eq!(hits("caf\u{e9}"), 1);
}

#[test]
fn title_hits_have_no_sequence_and_message_hits_carry_their_role_and_sequence() {
    let (_temp, root, workspace) = setup();
    let mut journal = SessionJournal::create(&root, &workspace, "Zebra planning").expect("create");
    journal
        .record_message(MessageRole::User, "usermarker line")
        .expect("user");
    let turn = journal.start_turn().expect("turn");
    journal.record_manifest(turn, manifest()).expect("manifest");
    journal
        .record_tool_request(
            turn,
            "call-1",
            "workspace_read_file",
            "{\"path\":\"a.txt\"}",
        )
        .expect("request");
    let rendered = "toolmarker output";
    journal
        .record_tool_result(
            turn,
            "call-1",
            ToolOutcome::Succeeded,
            None,
            u64::try_from(rendered.len()).expect("size"),
            &sha256_hex(rendered),
        )
        .expect("result");
    journal
        .record_tool_message("call-1", rendered)
        .expect("tool message");
    journal
        .record_message(MessageRole::Assistant, "assistantmarker reply")
        .expect("assistant");
    journal.complete_turn(turn).expect("complete");
    journal
        .record_message(MessageRole::User, "after the turn")
        .expect("later user");
    let summary = "summarymarker of earlier work";
    journal
        .record_compaction(CompactionRecord {
            summary: summary.into(),
            compacted_messages: 3,
            includes_tool_results: true,
            summary_sha256: sha256_hex(summary),
            model: "fixture".into(),
            prompt_tokens: None,
            completion_tokens: None,
        })
        .expect("compaction");
    let path = journal.path().to_path_buf();
    drop(journal);
    let inspection = SessionInspection::inspect(&path).expect("inspect");
    let index = refreshed(&root);

    let title = &find(&index, "zebra", ArchiveView::Active).hits;
    assert_eq!(
        (title.len(), title[0].seq, title[0].role),
        (1, None, SearchRole::Title)
    );
    for (term, role) in [
        ("usermarker", SearchRole::User),
        ("toolmarker", SearchRole::Tool),
        ("assistantmarker", SearchRole::Assistant),
        ("summarymarker", SearchRole::Summary),
    ] {
        let hits = find(&index, term, ArchiveView::Active).hits;
        assert_eq!(hits.len(), 1, "{term}");
        assert_eq!(hits[0].role, role, "{term}");
        let seq = hits[0].seq.expect("message hits have a sequence");
        assert!(
            inspection.entries.iter().any(|entry| entry.seq == seq),
            "{term}"
        );
    }
    // Tool-call arguments and manifests are not indexed.
    for unindexed in [
        "workspace_read_file",
        "a.txt",
        "CUSTOM-INSTRUCTIONS-TEXT",
        "local-model",
    ] {
        assert!(
            find(&index, unindexed, ArchiveView::Active).hits.is_empty(),
            "{unindexed}"
        );
    }
}

#[test]
fn search_reports_truncation_honors_the_view_and_clamps_the_limit() {
    let (_temp, root, workspace) = setup();
    for number in 0..5 {
        session(
            &root,
            &workspace,
            &format!("S{number}"),
            &["commonword here"],
        );
    }
    let (_, archived_path) = session(&root, &workspace, "Old", &["commonword archived"]);
    SessionJournal::open(&archived_path)
        .expect("open")
        .archive()
        .expect("archive");
    let many: Vec<String> = (0..205).map(|n| format!("needleword {n}")).collect();
    let many_refs: Vec<&str> = many.iter().map(String::as_str).collect();
    session(&root, &workspace, "Many", &many_refs);
    let index = refreshed(&root);

    let three = index
        .search("commonword", ArchiveView::Active, 3)
        .expect("three");
    assert_eq!((three.hits.len(), three.truncated), (3, true));
    let all = index
        .search("commonword", ArchiveView::Active, 5)
        .expect("five");
    assert_eq!((all.hits.len(), all.truncated), (5, false));
    let zero = index
        .search("commonword", ArchiveView::Active, 0)
        .expect("zero");
    assert_eq!((zero.hits.len(), zero.truncated), (1, true));
    let archived = index
        .search("commonword", ArchiveView::Archived, 10)
        .expect("archived");
    assert_eq!(archived.hits.len(), 1);
    assert!(archived.hits[0].session.archived);
    let clamped = index
        .search("needleword", ArchiveView::Active, usize::MAX)
        .expect("clamp");
    assert_eq!((clamped.hits.len(), clamped.truncated), (200, true));
}

#[test]
fn unusable_search_text_is_rejected_with_a_typed_error() {
    let (_temp, _root, index) = sample_index();
    for text in ["", "   ", "\t\n", &"x".repeat(257), &"a ".repeat(17)] {
        assert!(
            matches!(
                index.search(text, ArchiveView::Active, 10),
                Err(IndexError::InvalidQuery(_))
            ),
            "{text:?}"
        );
    }
    assert!(
        index
            .search(&"x".repeat(256), ArchiveView::Active, 10)
            .is_ok()
    );
}

#[test]
fn hostile_search_text_never_errors_and_is_matched_literally() {
    // Given one session that contains the operator text and a control that does not.
    let (_temp, root, workspace) = setup();
    session(
        &root,
        &workspace,
        "Literal",
        &[
            "x a OR b y",
            "x a AND b y",
            "x NEAR(a b) y",
            "x text:a y",
            "x (a) OR (b y",
        ],
    );
    session(&root, &workspace, "Control", &["a only b", "text only"]);
    let index = refreshed(&root);
    let control_title = |hits: &SearchListing| {
        hits.hits
            .iter()
            .any(|hit| hit.session.title.as_deref() == Some("Control"))
    };

    // Every hostile string is accepted and never reaches FTS5 as syntax.
    let hostile = [
        "\"",
        "\"\"",
        "a\"b",
        "NEAR(a b)",
        "a OR b",
        "a AND b",
        "-a",
        "^a",
        "a*",
        "text:a",
        "(",
        ")",
        "a) OR (b",
        "a NOT b",
        "{text}: a",
        "a + b",
        "*",
        "NEAR/2",
        &"z".repeat(256),
    ];
    for text in hostile {
        let result = index.search(text, ArchiveView::Active, 50);
        assert!(result.is_ok(), "{text:?}: {result:?}");
    }
    // Operator words are plain terms: all must be present, so the control
    // session (which has no `or`, `and`, `near`, or `text:` run) cannot match.
    for text in ["a OR b", "a AND b", "NEAR(a b)", "text:a", "a) OR (b"] {
        let hits = find(&index, text, ArchiveView::Active);
        assert!(!control_title(&hits), "{text}");
        assert!(
            !hits.hits.is_empty(),
            "{text} should match its literal text"
        );
    }
    assert!(find(&index, "zzzz", ArchiveView::Active).hits.is_empty());
}

#[test]
fn snippets_mark_the_matched_terms_and_reconstruct_a_substring_of_the_source() {
    let (_temp, root, workspace) = setup();
    let source = "The quick brown fox jumps over the lazy dog";
    let long: String =
        (0..300).map(|n| format!("filler{n} ")).collect::<String>() + "needle " + "tail";
    session(&root, &workspace, "Snippets", &[source, &long]);
    let index = refreshed(&root);

    let hits = find(&index, "brown fox", ArchiveView::Active).hits;
    assert_eq!(hits.len(), 1);
    let parts = &hits[0].snippet;
    let joined: String = parts.iter().map(|p| p.text.as_str()).collect();
    assert!(source.contains(&joined), "{joined:?}");
    let matched: Vec<&str> = parts
        .iter()
        .filter(|p| p.matched)
        .map(|p| p.text.as_str())
        .collect();
    assert_eq!(matched, ["brown", "fox"]);
    assert!(
        parts
            .iter()
            .all(|p| !p.text.contains(['\u{e000}', '\u{e001}']))
    );

    let hits = find(&index, "needle", ArchiveView::Active).hits;
    let parts = &hits[0].snippet;
    let joined: String = parts.iter().map(|p| p.text.as_str()).collect();
    assert!(long.contains(&joined));
    assert!(
        joined.len() < long.len() / 4,
        "the snippet is a window, not the whole text"
    );
    assert_eq!(
        parts
            .iter()
            .filter(|p| p.matched)
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>(),
        ["needle"]
    );
}

#[test]
fn transcript_text_cannot_forge_a_snippet_match() {
    let (_temp, root, workspace) = setup();
    let hostile = "before \u{e000}zebra\u{e001} after marker \u{e001}\u{e000} end";
    session(&root, &workspace, "Spoof", &[hostile]);
    let index = refreshed(&root);

    let hits = find(&index, "marker", ArchiveView::Active).hits;

    assert_eq!(hits.len(), 1);
    let parts = &hits[0].snippet;
    assert!(
        parts
            .iter()
            .all(|p| !p.text.contains(['\u{e000}', '\u{e001}']))
    );
    let matched: Vec<&str> = parts
        .iter()
        .filter(|p| p.matched)
        .map(|p| p.text.as_str())
        .collect();
    assert_eq!(matched, ["marker"]);
    // The spoofed word is plain text, not a highlight.
    assert!(parts.iter().any(|p| !p.matched && p.text.contains("zebra")));
    // The markers were replaced, so they cannot be searched for either.
    assert!(
        find(&index, "\u{e000}zebra", ArchiveView::Active)
            .hits
            .len()
            <= 1
    );
}

#[test]
fn sql_metacharacters_in_titles_and_messages_are_stored_and_found_literally() {
    let (_temp, root, workspace) = setup();
    let hostile = "'; DROP TABLE journal;--";
    let (id, _) = session(
        &root,
        &workspace,
        hostile,
        &[hostile, "x'); DELETE FROM entry_text;--"],
    );
    session(&root, &workspace, "Another", &["still here"]);
    let index = refreshed(&root);

    let row = index.session(id).expect("lookup").expect("row");
    assert_eq!(row.title.as_deref(), Some(hostile));
    assert_eq!(list(&index, ArchiveView::Active).total, 2);
    let hits = find(&index, "DROP TABLE journal", ArchiveView::Active).hits;
    assert_eq!(hits.len(), 2);
    assert_eq!(
        find(&index, "still here", ArchiveView::Active).hits.len(),
        1
    );
    assert_eq!(
        find(&index, "DELETE FROM entry_text", ArchiveView::Active)
            .hits
            .len(),
        1
    );
}

#[test]
fn deleting_the_index_directory_rebuilds_identical_listings_and_leaves_journals_alone() {
    let (_temp, root, workspace) = setup();
    let mut paths = Vec::new();
    for (title, body) in [
        ("One", "firstbody"),
        ("Two", "secondbody"),
        ("Three", "thirdbody"),
    ] {
        paths.push(session(&root, &workspace, title, &[body]).1);
    }
    let bytes_before = read_all(&paths);
    let index = refreshed(&root);
    let listing_before = list(&index, ArchiveView::Active);
    let hits_before = find(&index, "secondbody", ArchiveView::Active);
    drop(index);

    fs::remove_dir_all(root.join(INDEX_DIRECTORY)).expect("delete the whole index");
    let mut rebuilt = SessionIndex::open(&root).expect("reopen");
    assert_eq!(list(&rebuilt, ArchiveView::Active).total, 0);
    let report = rebuilt.refresh(1000).expect("refresh");

    assert_eq!(report.inspected, 3);
    assert_eq!(list(&rebuilt, ArchiveView::Active), listing_before);
    assert_eq!(
        find(&rebuilt, "secondbody", ArchiveView::Active),
        hits_before
    );
    assert_eq!(read_all(&paths), bytes_before);
}

#[test]
fn a_garbage_database_is_replaced_without_touching_anything_else() {
    let (_temp, root, workspace) = setup();
    let (_, path) = session(&root, &workspace, "Survivor", &["survivorword"]);
    let bytes_before = fs::read(&path).expect("bytes");
    drop(refreshed(&root));
    let sibling = root.join(INDEX_DIRECTORY).join("keep.txt");
    fs::write(&sibling, "not an index file").expect("sibling");
    fs::write(database_path(&root), vec![b'x'; 4096]).expect("overwrite with garbage");

    let mut index = SessionIndex::open(&root).expect("open recovers");
    assert_eq!(list(&index, ArchiveView::Active).total, 0);
    assert_eq!(index.refresh(1000).expect("refresh").inspected, 1);

    assert_eq!(
        find(&index, "survivorword", ArchiveView::Active).hits.len(),
        1
    );
    assert_eq!(
        fs::read(&sibling).expect("sibling kept"),
        b"not an index file"
    );
    assert_eq!(fs::read(&path).expect("journal"), bytes_before);
}

#[test]
fn a_different_user_version_or_application_id_rebuilds_the_index() {
    let (_temp, root, workspace) = setup();
    session(&root, &workspace, "Versioned", &["versionword"]);
    for (pragma, value) in [
        ("user_version", 2_i64),
        ("user_version", 0),
        ("user_version", 99),
        ("application_id", 12_345),
        ("application_id", 0),
    ] {
        drop(refreshed(&root));
        raw(&root)
            .pragma_update(None, pragma, value)
            .expect("change the header");

        let mut index = SessionIndex::open(&root).expect("open recovers");
        assert_eq!(
            list(&index, ArchiveView::Active).total,
            0,
            "{pragma}={value}"
        );
        assert_eq!(
            index.refresh(1000).expect("refresh").inspected,
            1,
            "{pragma}={value}"
        );
        assert_eq!(
            find(&index, "versionword", ArchiveView::Active).hits.len(),
            1
        );
        drop(index);
        let connection = raw(&root);
        let id: i32 = connection
            .pragma_query_value(None, "application_id", |row| row.get(0))
            .expect("application_id");
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("user_version");
        assert_eq!((id, version), (APPLICATION_ID, 1));
    }
}

#[test]
fn a_database_with_the_right_header_but_no_tables_is_rebuilt() {
    let (_temp, root, workspace) = setup();
    session(&root, &workspace, "Hollow", &["hollowword"]);
    drop(refreshed(&root));
    {
        let connection = raw(&root);
        connection
            .execute_batch("DROP TABLE entry_text; DROP TABLE journal;")
            .expect("drop tables");
    }

    let mut index = SessionIndex::open(&root).expect("open recovers");
    index.refresh(1000).expect("refresh");

    assert_eq!(
        find(&index, "hollowword", ArchiveView::Active).hits.len(),
        1
    );
}

#[test]
fn journal_path_resolves_a_real_session_to_a_path_a_writer_can_open() {
    let (_temp, root, workspace) = setup();
    let (id, path) = session(&root, &workspace, "Resolve", &["body"]);
    let path = move_to_day(&root, &path, "2025/03/04");
    let index = refreshed(&root);

    let resolved = index.journal_path(id).expect("resolve").expect("known");

    assert_eq!(
        resolved,
        root.join("sessions/2025/03/04")
            .join(path.file_name().expect("name"))
    );
    let writer = SessionJournal::open(&resolved).expect("writer opens the resolved path");
    assert_eq!(writer.snapshot().session_id, id);
    assert_eq!(index.journal_path(Uuid::new_v4()).expect("unknown"), None);
}

#[test]
fn a_tampered_stored_path_is_rejected_with_a_typed_error() {
    let (_temp, root, workspace) = setup();
    let (id, path) = session(&root, &workspace, "Tamper", &["body"]);
    let file = path
        .file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned();
    let other = Uuid::new_v4();
    let index = refreshed(&root);
    let connection = raw(&root);
    let tampered = [
        "../../evil.jsonl".to_owned(),
        format!("{}/evil.jsonl", root.display()),
        "/etc/passwd".to_owned(),
        "C:/Windows/evil.jsonl".to_owned(),
        format!("sessions\\2026\\01\\02\\{file}"),
        format!("sessions/2026/01/02/omgcode-01-02-03-004Z-{other}.jsonl"),
        format!("sessions/2026/01/02/extra/{file}"),
        format!("sessions/2026/01/../02/{file}"),
        format!("sessions/2026/01/{file}"),
        format!("sessions/2026/01/02/{file}/"),
        String::new(),
    ];
    for value in tampered {
        connection
            .execute(
                "UPDATE journal SET relative_path = ?1 WHERE session_id = ?2",
                rusqlite::params![value, id.to_string()],
            )
            .expect("tamper");
        assert!(
            matches!(
                index.journal_path(id),
                Err(IndexError::InvalidIndexEntry(_))
            ),
            "{value:?}"
        );
    }
}

#[test]
fn a_refresh_repairs_a_tampered_stored_path() {
    let (_temp, root, workspace) = setup();
    let (id, path) = session(&root, &workspace, "Repair", &["body"]);
    let mut index = refreshed(&root);
    raw(&root)
        .execute("UPDATE journal SET relative_path = '../../evil.jsonl'", [])
        .expect("tamper");
    assert!(index.journal_path(id).is_err());

    let report = index.refresh(1000).expect("refresh");

    assert_eq!(report.inspected, 1);
    assert_eq!(index.journal_path(id).expect("resolve"), Some(path));
}

#[test]
fn an_index_directory_that_is_a_link_is_refused() {
    let (temp, root, _workspace) = setup();
    fs::create_dir_all(&root).expect("root");
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).expect("target");
    if !try_symlink_dir(&elsewhere, &root.join(INDEX_DIRECTORY)) {
        return;
    }

    let result = SessionIndex::open(&root);

    assert!(
        matches!(result, Err(IndexError::UnsafeLocation(_))),
        "{result:?}"
    );
    assert_eq!(fs::read_dir(&elsewhere).expect("target").count(), 0);
}

#[test]
fn a_database_file_that_is_a_link_is_refused() {
    let (temp, root, _workspace) = setup();
    let directory = root.join(INDEX_DIRECTORY);
    fs::create_dir_all(&directory).expect("index directory");
    let target = temp.path().join("target.sqlite3");
    fs::write(&target, b"untouched").expect("target");
    if !try_symlink_file(&target, &directory.join(INDEX_FILE)) {
        return;
    }

    let result = SessionIndex::open(&root);

    assert!(
        matches!(result, Err(IndexError::UnsafeLocation(_))),
        "{result:?}"
    );
    assert_eq!(fs::read(&target).expect("target"), b"untouched");
}

#[test]
fn contention_past_the_busy_timeout_is_a_distinct_busy_error() {
    let (_temp, root, workspace) = setup();
    session(&root, &workspace, "Locked", &[]);
    let index = refreshed(&root);
    let connection = raw(&root);
    connection
        .execute_batch("BEGIN EXCLUSIVE")
        .expect("exclusive lock");

    let result = index.sessions(&query(ArchiveView::Active, SessionSort::Title, false, 10));

    assert!(matches!(result, Err(IndexError::Busy)), "{result:?}");
    connection.execute_batch("ROLLBACK").expect("release");
    assert_eq!(list(&index, ArchiveView::Active).total, 1);
}

#[test]
fn the_workspace_path_round_trips_losslessly_through_the_stored_units() {
    let (_temp, root, workspace) = setup();
    let (id, path) = session(&root, &workspace, "Units", &[]);
    let inspection = SessionInspection::inspect(&path).expect("inspect");
    let index = refreshed(&root);

    let row = index.session(id).expect("lookup").expect("row");

    assert_eq!(row.workspace, Some(inspection.workspace.clone()));
    match row.workspace.expect("workspace") {
        NativePath::WindowsUtf16(units) => assert!(!units.is_empty()),
        NativePath::UnixBytes(bytes) => assert!(!bytes.is_empty()),
    }
}

#[test]
fn the_bundled_sqlite_has_fts5_and_refuses_sql_extension_loading() {
    let connection = Connection::open_in_memory().expect("in-memory database");
    connection
        .execute_batch(
            "CREATE VIRTUAL TABLE probe USING fts5(text, tokenize = 'unicode61 remove_diacritics 2');
             INSERT INTO probe (text) VALUES ('Caf\u{e9} au lait');",
        )
        .expect("FTS5 is compiled in");
    let hits: i64 = connection
        .query_row(
            "SELECT count(*) FROM probe WHERE probe MATCH 'cafe'",
            [],
            |row| row.get(0),
        )
        .expect("match");
    assert_eq!(hits, 1);
    let version: String = connection
        .query_row("SELECT sqlite_version()", [], |row| row.get(0))
        .expect("version");
    eprintln!("bundled SQLite version: {version}");
    assert!(
        connection
            .query_row("SELECT load_extension('no-such-extension')", [], |_| Ok(()))
            .is_err()
    );
}

// Link helpers, as in `tests/session_inspection.rs`.

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
