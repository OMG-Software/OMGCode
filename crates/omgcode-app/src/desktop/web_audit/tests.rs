use omgcode_core::action_journal::{ActionFinish, RefusalReason, WithheldTarget};
use omgcode_core::policy::WorkspaceId;
use serde_json::json;

use super::*;

fn workspace() -> WorkspaceId {
    Uuid::new_v4().to_string().parse().expect("workspace id")
}

fn record(session: Uuid, call_id: &str, sequence: u64, tool: ActionTool) -> ActionRecord {
    ActionRecord {
        sequence,
        time: format!("2026-01-01T00:00:{:02}Z", sequence % 60),
        session_id: session,
        workspace_id: workspace(),
        call_id: call_id.to_owned(),
        tool,
        targets: Vec::new(),
        request_sha256: None,
        after_web_content: false,
        disposition: ActionDisposition::Executed {
            action_id: Uuid::new_v4(),
            authorization: Authorization::StandingAllow {
                settings_revision: 1,
            },
            finish: Some(ActionFinish {
                time: "2026-01-01T00:01:00Z".to_owned(),
                outcome: ActionOutcome::Succeeded,
                code: None,
                operation_id: None,
            }),
        },
    }
}

fn request(call_id: &str, program: &str, arguments: &[&str]) -> CommandRequest {
    CommandRequest {
        call_id: call_id.to_owned(),
        arguments: json!({"profile": "rust", "program": program, "arguments": arguments})
            .to_string(),
    }
}

#[test]
fn a_record_that_is_neither_a_fetch_nor_after_web_content_is_not_listed() {
    let session = Uuid::new_v4();
    let records = [record(session, "plain", 1, ActionTool::ReadFile)];

    let audit = build(session, &records, Some(Vec::new()), false);

    assert!(audit.entries.is_empty());
    assert!(!audit.truncated);
}

#[test]
fn a_fetch_is_listed_as_a_fetch_with_its_url_as_text() {
    let session = Uuid::new_v4();
    let mut fetch = record(session, "f", 1, ActionTool::Network);
    fetch.targets = vec![StoredTarget::Path("https://example.com/a?q=1".to_owned())];

    let audit = build(session, &[fetch], Some(Vec::new()), false);

    assert_eq!(audit.entries.len(), 1);
    let entry = &audit.entries[0];
    assert!(entry.is_fetch);
    assert_eq!(entry.tool, "network");
    assert_eq!(
        entry.targets,
        [WebAuditTarget::Text {
            text: "https://example.com/a?q=1".to_owned()
        }]
    );
    assert_eq!(entry.outcome, WebAuditOutcome::Succeeded);
    assert_eq!(
        entry.authorization,
        Some(WebAuditAuthorization::StandingAllow)
    );
}

#[test]
fn a_withheld_target_sends_its_length_and_never_its_digest() {
    let session = Uuid::new_v4();
    let mut marked = record(session, "w", 1, ActionTool::ReadFile);
    marked.after_web_content = true;
    marked.targets = vec![StoredTarget::Withheld(WithheldTarget {
        sha256: "ab".repeat(32),
        bytes: 77,
    })];

    let audit = build(session, &[marked], Some(Vec::new()), false);

    assert_eq!(
        audit.entries[0].targets,
        [WebAuditTarget::Withheld { bytes: 77 }]
    );
    let wire = serde_json::to_string(&audit).expect("json");
    assert!(!wire.contains(&"ab".repeat(32)));
}

#[test]
fn a_refusal_has_its_reason_and_no_authorization() {
    let session = Uuid::new_v4();
    let mut refused = record(session, "r", 1, ActionTool::Network);
    refused.disposition = ActionDisposition::Refused {
        reason: RefusalReason::AccessDenied,
    };

    let audit = build(session, &[refused], Some(Vec::new()), false);

    let entry = &audit.entries[0];
    assert_eq!(entry.outcome, WebAuditOutcome::Refused);
    assert_eq!(entry.reason.as_deref(), Some("access_denied"));
    assert_eq!(entry.authorization, None);
}

#[test]
fn an_executed_call_with_no_finish_is_unfinished() {
    let session = Uuid::new_v4();
    let mut open = record(session, "o", 1, ActionTool::Network);
    open.disposition = ActionDisposition::Executed {
        action_id: Uuid::new_v4(),
        authorization: Authorization::Approval {
            approval_id: Uuid::new_v4(),
            settings_revision: 1,
        },
        finish: None,
    };

    let audit = build(session, &[open], Some(Vec::new()), false);

    assert_eq!(audit.entries[0].outcome, WebAuditOutcome::Unfinished);
    assert_eq!(
        audit.entries[0].authorization,
        Some(WebAuditAuthorization::Approval)
    );
}

#[test]
fn a_command_after_a_fetch_pairs_with_its_request_even_when_an_unlisted_call_shares_its_identifier()
{
    let session = Uuid::new_v4();
    // Newest first: the listed command, then an earlier unlisted command with
    // the same identifier from a previous turn.
    let mut later = record(session, "call_1", 2, ActionTool::RunCommand);
    later.after_web_content = true;
    let earlier = record(session, "call_1", 1, ActionTool::RunCommand);
    let requests = vec![
        request("call_1", "echo", &["old"]),
        request("call_1", "curl", &["-s", "http://x"]),
    ];

    let audit = build(session, &[later, earlier], Some(requests), false);

    assert_eq!(audit.entries.len(), 1);
    assert_eq!(audit.entries[0].program.as_deref(), Some("curl"));
    assert_eq!(
        audit.entries[0].arguments.as_deref(),
        Some(&["-s".to_owned(), "http://x".to_owned()][..])
    );
    assert_eq!(audit.entries[0].argument_count, Some(2));
}

#[test]
fn a_command_without_a_readable_session_journal_has_no_text_but_is_still_listed() {
    let session = Uuid::new_v4();
    let mut command = record(session, "c", 1, ActionTool::RunCommand);
    command.after_web_content = true;

    let audit = build(session, &[command], None, false);

    assert_eq!(audit.entries.len(), 1);
    assert_eq!(audit.entries[0].program, None);
    assert_eq!(audit.entries[0].arguments, None);
}

#[test]
fn a_hostile_argument_is_escaped_to_one_line() {
    let session = Uuid::new_v4();
    let mut command = record(session, "c", 1, ActionTool::RunCommand);
    command.after_web_content = true;

    let audit = build(
        session,
        &[command],
        Some(vec![request("c", "sh", &["a\nb\u{202e}c"])]),
        false,
    );

    let argument = &audit.entries[0].arguments.as_ref().expect("arguments")[0];
    assert!(!argument.contains('\n'));
    assert!(!argument.contains('\u{202e}'));
}

#[test]
fn more_than_one_hundred_matches_list_the_newest_hundred_and_say_truncated() {
    let session = Uuid::new_v4();
    // Newest first, sequence 120 down to 1.
    let records: Vec<ActionRecord> = (1..=120)
        .rev()
        .map(|sequence| record(session, "f", sequence, ActionTool::Network))
        .collect();

    let audit = build(session, &records, Some(Vec::new()), false);

    assert_eq!(audit.entries.len(), 100);
    assert!(audit.truncated);
    assert_eq!(audit.entries[0].time, "2026-01-01T00:00:00Z");
}

#[test]
fn a_full_record_window_says_truncated_even_with_few_matches() {
    let session = Uuid::new_v4();
    let records = [record(session, "f", 1, ActionTool::Network)];

    let audit = build(session, &records, Some(Vec::new()), true);

    assert_eq!(audit.entries.len(), 1);
    assert!(audit.truncated);
}
