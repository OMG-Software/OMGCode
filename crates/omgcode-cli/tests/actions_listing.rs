//! Real CLI tests for `actions list` and its after-web-content review filter
//! (ADR-0098). The model action journal is seeded through core, because the
//! CLI never fetches a page for a model.

mod support;

use omgcode_core::action_journal::{
    ActionJournal, ActionOutcome, ActionTool, Authorization, RefusalReason, RefusedAction,
    StartedAction,
};
use omgcode_core::policy::WorkspaceId;
use serde_json::Value;
use uuid::Uuid;

use support::workspace_fixture::Fixture;
use support::{assert_error_code, assert_success, json_output, run_cli};

const URL: &str = "https://docs.example.com/guide";

struct Seeded {
    fixture: Fixture,
    /// The session that fetched a page.
    fetching: Uuid,
    /// A session that never did.
    quiet: Uuid,
}

fn standing() -> Authorization {
    Authorization::StandingAllow {
        settings_revision: 1,
    }
}

fn start(
    journal: &mut ActionJournal,
    session: Uuid,
    workspace: WorkspaceId,
    call_id: &str,
    tool: ActionTool,
    targets: &[&str],
    after_web_content: bool,
) {
    let action = journal
        .record_started(&StartedAction {
            session_id: session,
            workspace_id: workspace,
            call_id,
            tool,
            targets,
            request_sha256: None,
            authorization: standing(),
            after_web_content,
        })
        .expect("start");
    journal
        .record_finished(action, ActionOutcome::Succeeded, None, None)
        .expect("finish");
}

fn seeded() -> Seeded {
    let fixture = Fixture::new(b"note");
    let workspace = fixture.workspace_id;
    let fetching = Uuid::new_v4();
    let quiet = Uuid::new_v4();
    let mut journal = ActionJournal::open(&fixture.data).expect("action journal");
    // Oldest first: a read, the fetch, then three later calls of one session.
    start(
        &mut journal,
        fetching,
        workspace,
        "read-before",
        ActionTool::ReadFile,
        &["note.txt"],
        false,
    );
    start(
        &mut journal,
        fetching,
        workspace,
        "fetch-1",
        ActionTool::Network,
        &[URL],
        false,
    );
    start(
        &mut journal,
        fetching,
        workspace,
        "read-after",
        ActionTool::ReadFile,
        &["note.txt"],
        true,
    );
    journal
        .record_refused(&RefusedAction {
            session_id: fetching,
            workspace_id: workspace,
            call_id: "command-after",
            tool: ActionTool::RunCommand,
            targets: &[],
            request_sha256: None,
            reason: RefusalReason::AccessDenied,
            after_web_content: true,
        })
        .expect("refusal");
    start(
        &mut journal,
        fetching,
        workspace,
        "list-after",
        ActionTool::ListDirectory,
        &[""],
        true,
    );
    // A session that never fetched, with the newest records of all.
    start(
        &mut journal,
        quiet,
        workspace,
        "quiet-read",
        ActionTool::ReadFile,
        &["note.txt"],
        false,
    );
    start(
        &mut journal,
        quiet,
        workspace,
        "quiet-list",
        ActionTool::ListDirectory,
        &[""],
        false,
    );
    drop(journal);
    Seeded {
        fixture,
        fetching,
        quiet,
    }
}

fn list(seeded: &Seeded, extra: &[&str]) -> Vec<Value> {
    let mut args = vec!["actions", "list"];
    args.extend_from_slice(extra);
    let output = run_cli(&seeded.fixture.data, &args);
    assert_success(&output);
    json_output(&output)["actions"]
        .as_array()
        .expect("actions")
        .clone()
}

/// The whole JSON reply of a listing.
fn listing(seeded: &Seeded, extra: &[&str]) -> Value {
    let mut args = vec!["actions", "list"];
    args.extend_from_slice(extra);
    let output = run_cli(&seeded.fixture.data, &args);
    assert_success(&output);
    json_output(&output)
}

fn calls(actions: &[Value]) -> Vec<&str> {
    actions
        .iter()
        .map(|action| action["call_id"].as_str().expect("call id"))
        .collect()
}

#[test]
fn every_listed_action_reports_whether_it_came_after_web_content() {
    let seeded = seeded();
    let actions = list(&seeded, &[]);
    assert_eq!(actions.len(), 7);
    for action in &actions {
        let expected = matches!(
            action["call_id"].as_str(),
            Some("read-after" | "command-after" | "list-after")
        );
        assert_eq!(action["after_web_content"], expected, "{action}");
    }
    let fetch = actions
        .iter()
        .find(|action| action["call_id"] == "fetch-1")
        .expect("the fetch");
    assert_eq!(fetch["tool"], "network");
    assert_eq!(fetch["targets"][0]["payload"], URL);
}

#[test]
fn after_web_content_lists_exactly_the_marked_records_newest_first() {
    let seeded = seeded();
    let actions = list(&seeded, &["--after-web-content"]);
    assert_eq!(
        calls(&actions),
        ["list-after", "command-after", "read-after"]
    );
    assert!(
        actions
            .iter()
            .all(|action| action["after_web_content"] == true)
    );
    // The refused call and the executed ones are all there, by tool.
    assert_eq!(actions[1]["tool"], "run_command");
    assert!(actions[1]["disposition"]["refused"].is_object());
}

#[test]
fn a_session_filter_narrows_the_listing_and_combines_with_the_mark() {
    let seeded = seeded();
    let fetching = seeded.fetching.to_string();
    let quiet = seeded.quiet.to_string();

    let all_of_one = list(&seeded, &["--session", &fetching]);
    assert_eq!(all_of_one.len(), 5);
    assert!(
        all_of_one
            .iter()
            .all(|action| action["session_id"] == fetching)
    );

    let marked_of_one = list(&seeded, &["--session", &fetching, "--after-web-content"]);
    assert_eq!(
        calls(&marked_of_one),
        ["list-after", "command-after", "read-after"]
    );

    // A session that never fetched has nothing to review.
    assert!(list(&seeded, &["--session", &quiet, "--after-web-content"]).is_empty());
    assert_eq!(list(&seeded, &["--session", &quiet]).len(), 2);
    let unknown = Uuid::new_v4().to_string();
    assert!(list(&seeded, &["--session", &unknown]).is_empty());
}

#[test]
fn the_limit_applies_after_the_filters_so_newer_unmarked_records_do_not_hide_them() {
    let seeded = seeded();
    // The two newest records belong to the quiet session.
    let newest = list(&seeded, &["--limit", "2"]);
    assert_eq!(calls(&newest), ["quiet-list", "quiet-read"]);
    let marked = list(&seeded, &["--after-web-content", "--limit", "2"]);
    assert_eq!(calls(&marked), ["list-after", "command-after"]);
}

#[test]
fn a_filtered_listing_reports_whether_it_may_be_incomplete() {
    let seeded = seeded();
    assert_eq!(
        listing(&seeded, &["--after-web-content"])["truncated"],
        false
    );
    let quiet = seeded.quiet.to_string();
    let limited = listing(&seeded, &["--session", &quiet, "--limit", "1"]);
    assert_eq!(
        calls(limited["actions"].as_array().expect("actions")),
        ["quiet-list"]
    );
    assert_eq!(limited["truncated"], true, "a second call matched");
    // An unfiltered listing keeps its shape.
    assert!(listing(&seeded, &[]).get("truncated").is_none());
}

/// More than 1000 newer calls push the marked calls out of the search window,
/// and the listing says so instead of looking complete.
#[test]
fn a_filtered_listing_is_truncated_when_newer_calls_fill_the_search_window() {
    let seeded = seeded();
    let mut journal = ActionJournal::open(&seeded.fixture.data).expect("action journal");
    for index in 0..1000 {
        let call_id = format!("later-{index}");
        journal
            .record_refused(&RefusedAction {
                session_id: seeded.quiet,
                workspace_id: seeded.fixture.workspace_id,
                call_id: &call_id,
                tool: ActionTool::RunCommand,
                targets: &[],
                request_sha256: None,
                reason: RefusalReason::AccessDenied,
                after_web_content: false,
            })
            .expect("refusal");
    }
    drop(journal);
    let reply = listing(&seeded, &["--after-web-content"]);
    assert_eq!(
        reply["actions"],
        Value::Array(Vec::new()),
        "the marked calls are older than the window"
    );
    assert_eq!(reply["truncated"], true);
}

#[test]
fn a_filtered_listing_still_bounds_its_limit_and_refuses_a_bad_session() {
    let seeded = seeded();
    for limit in ["0", "1001"] {
        let refused = run_cli(
            &seeded.fixture.data,
            &["actions", "list", "--after-web-content", "--limit", limit],
        );
        assert_error_code(&refused, "configuration");
    }
    let output = run_cli(
        &seeded.fixture.data,
        &["actions", "list", "--session", "not-a-uuid"],
    );
    assert!(
        !output.status.success(),
        "a malformed session id is refused"
    );
}
