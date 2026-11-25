use std::fs;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::*;

const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn workspace() -> WorkspaceId {
    WorkspaceId::from_uuid(Uuid::new_v4()).expect("fresh workspace ID")
}

fn started<'a>(call_id: &'a str, tool: ActionTool, targets: &'a [&'a str]) -> StartedAction<'a> {
    StartedAction {
        session_id: Uuid::new_v4(),
        workspace_id: workspace(),
        call_id,
        tool,
        targets,
        request_sha256: Some(DIGEST),
        authorization: Authorization::StandingAllow {
            settings_revision: 3,
        },
    }
}

fn refused<'a>(call_id: &'a str, reason: RefusalReason) -> RefusedAction<'a> {
    RefusedAction {
        session_id: Uuid::new_v4(),
        workspace_id: workspace(),
        call_id,
        tool: ActionTool::ReadFile,
        targets: &["notes.txt"],
        request_sha256: None,
        reason,
    }
}

fn small(max_bytes: usize, max_archives: usize) -> Limits {
    Limits {
        max_bytes,
        max_events: MAX_EVENTS,
        max_archives,
    }
}

fn active_path(directory: &Path) -> PathBuf {
    directory.join(FILE_NAME)
}

fn active_lines(directory: &Path) -> Vec<Value> {
    fs::read_to_string(active_path(directory))
        .expect("active journal")
        .lines()
        .map(|line| serde_json::from_str(line).expect("json line"))
        .collect()
}

fn rewrite_active(directory: &Path, lines: &[Value]) {
    let mut text = String::new();
    for line in lines {
        text.push_str(&line.to_string());
        text.push('\n');
    }
    fs::write(active_path(directory), text).expect("rewrite journal");
}

/// Write one started and finished action plus one refusal, then close.
fn seeded(directory: &Path) {
    let mut journal = ActionJournal::open(directory).expect("open");
    let id = journal
        .record_started(&started("call_1", ActionTool::ReadFile, &["a.txt"]))
        .expect("start");
    journal
        .record_finished(id, ActionOutcome::Succeeded, Some("ok"), None)
        .expect("finish");
    journal
        .record_refused(&refused("call_2", RefusalReason::AccessDenied))
        .expect("refuse");
}

fn archive_names(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .expect("list directory")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| parse_archive_name(name).is_some())
        .collect();
    names.sort();
    names
}

#[test]
fn every_event_round_trips_through_recent() {
    let directory = tempfile::tempdir().expect("directory");
    let approval = Uuid::new_v4();
    let operation = Uuid::new_v4();
    let mut journal = ActionJournal::open(directory.path()).expect("open");
    let mut start = started("call_a", ActionTool::RenameFile, &["old.txt", "new.txt"]);
    start.authorization = Authorization::Approval {
        approval_id: approval,
        settings_revision: 9,
    };
    let id = journal.record_started(&start).expect("start");
    journal
        .record_finished(
            id,
            ActionOutcome::Uncertain,
            Some("rename_failed_2"),
            Some(operation),
        )
        .expect("finish");
    for reason in [
        RefusalReason::AccessDenied,
        RefusalReason::ApprovalRequired {
            approval_id: approval,
        },
        RefusalReason::ApprovalDenied {
            approval_id: approval,
        },
        RefusalReason::ApprovalExpired {
            approval_id: approval,
        },
        RefusalReason::InvalidRequest,
        RefusalReason::PolicyUnavailable,
        RefusalReason::Cancelled,
    ] {
        journal
            .record_refused(&refused("call_b", reason))
            .expect("refuse");
    }
    drop(journal);

    let records = ActionJournal::recent(directory.path(), 100).expect("recent");
    assert_eq!(records.len(), 8);
    assert!(matches!(
        records[0].disposition,
        ActionDisposition::Refused {
            reason: RefusalReason::Cancelled
        }
    ));
    let last = records.last().expect("oldest");
    assert_eq!(last.sequence, 1);
    assert_eq!(last.tool, ActionTool::RenameFile);
    assert_eq!(
        last.targets,
        [
            StoredTarget::Path("old.txt".to_owned()),
            StoredTarget::Path("new.txt".to_owned())
        ]
    );
    assert_eq!(last.request_sha256.as_deref(), Some(DIGEST));
    let ActionDisposition::Executed {
        action_id,
        authorization,
        finish,
    } = &last.disposition
    else {
        panic!("expected an executed action");
    };
    assert_eq!(*action_id, id);
    assert_eq!(
        *authorization,
        Authorization::Approval {
            approval_id: approval,
            settings_revision: 9
        }
    );
    let finish = finish.as_ref().expect("finished");
    assert_eq!(finish.outcome, ActionOutcome::Uncertain);
    assert_eq!(finish.code.as_deref(), Some("rename_failed_2"));
    assert_eq!(finish.operation_id, Some(operation));
}

#[test]
fn serialized_shape_matches_the_documented_schema() {
    let directory = tempfile::tempdir().expect("directory");
    seeded(directory.path());
    let lines = active_lines(directory.path());
    assert_eq!(lines[0]["schema_version"], 1);
    assert_eq!(lines[0]["sequence"], 1);
    assert_eq!(lines[0]["event"]["type"], "action_started");
    assert_eq!(lines[0]["event"]["payload"]["tool"], "read_file");
    assert_eq!(
        lines[0]["event"]["payload"]["authorization"],
        json!({"type": "standing_allow", "payload": {"settings_revision": 3}})
    );
    assert_eq!(lines[1]["event"]["type"], "action_finished");
    assert_eq!(lines[2]["event"]["type"], "action_refused");
    assert_eq!(
        lines[2]["event"]["payload"]["reason"],
        json!({"type": "access_denied"})
    );
    assert!(lines[0]["time"].as_str().expect("time").ends_with('Z'));
}

/// Apply `edit` to the first record's JSON and expect the journal to refuse it.
fn assert_tampered_line_is_corrupt(edit: impl FnOnce(&mut Value)) {
    let directory = tempfile::tempdir().expect("directory");
    seeded(directory.path());
    let mut lines = active_lines(directory.path());
    edit(&mut lines[0]);
    rewrite_active(directory.path(), &lines);
    let before = fs::read(active_path(directory.path())).expect("bytes");
    assert!(matches!(
        ActionJournal::open(directory.path()),
        Err(ActionJournalError::Corrupt(_))
    ));
    assert!(matches!(
        ActionJournal::recent(directory.path(), 10),
        Err(ActionJournalError::Corrupt(_))
    ));
    assert_eq!(
        fs::read(active_path(directory.path())).expect("bytes"),
        before
    );
}

#[test]
fn unknown_fields_are_refused_at_every_level() {
    assert_tampered_line_is_corrupt(|line| line["extra"] = json!(1));
    assert_tampered_line_is_corrupt(|line| line["event"]["extra"] = json!(1));
    assert_tampered_line_is_corrupt(|line| line["event"]["payload"]["extra"] = json!(1));
    assert_tampered_line_is_corrupt(|line| {
        line["event"]["payload"]["authorization"]["payload"]["extra"] = json!(1);
    });
}

#[test]
fn unsupported_schema_and_bad_envelope_values_are_refused() {
    assert_tampered_line_is_corrupt(|line| line["schema_version"] = json!(2));
    assert_tampered_line_is_corrupt(|line| line["time"] = json!("yesterday"));
    assert_tampered_line_is_corrupt(|line| line["time"] = json!("2026-01-01T00:00:00+02:00"));
    assert_tampered_line_is_corrupt(|line| line["sequence"] = json!(0));
    assert_tampered_line_is_corrupt(|line| line["event"]["type"] = json!("action_ran"));
}

#[test]
fn non_monotonic_and_duplicate_sequences_are_refused() {
    for edit in [3u64, 1, 5] {
        let directory = tempfile::tempdir().expect("directory");
        seeded(directory.path());
        let mut lines = active_lines(directory.path());
        lines[1]["sequence"] = json!(edit);
        rewrite_active(directory.path(), &lines);
        assert!(matches!(
            ActionJournal::open(directory.path()),
            Err(ActionJournalError::Corrupt("sequence is not consecutive"))
        ));
    }
}

#[test]
fn malformed_stored_values_are_refused() {
    assert_tampered_line_is_corrupt(|line| {
        line["event"]["payload"]["request_sha256"] = json!(DIGEST.to_uppercase());
    });
    assert_tampered_line_is_corrupt(|line| {
        line["event"]["payload"]["request_sha256"] = json!("abc");
    });
    assert_tampered_line_is_corrupt(|line| {
        line["event"]["payload"]["call_id"] = json!("bad\u{1b}[2Jid");
    });
    assert_tampered_line_is_corrupt(|line| {
        line["event"]["payload"]["call_id"] = json!("x".repeat(129));
    });
    assert_tampered_line_is_corrupt(|line| {
        line["event"]["payload"]["targets"] = json!([{"type": "path", "payload": "../escape"}]);
    });
    assert_tampered_line_is_corrupt(|line| {
        line["event"]["payload"]["targets"] = json!([{"type": "path", "payload": "a\u{202e}txt"}]);
    });
    assert_tampered_line_is_corrupt(|line| {
        let path = json!({"type": "path", "payload": "a"});
        line["event"]["payload"]["targets"] = json!([path, path, path]);
    });
    assert_tampered_line_is_corrupt(|line| {
        line["event"]["payload"]["targets"] = json!([path_bytes("A", 1)]);
    });
    assert_tampered_line_is_corrupt(|line| {
        line["event"]["payload"]["targets"] = json!([path_bytes(&"a".repeat(63), 1)]);
    });
    assert_tampered_line_is_corrupt(|line| {
        line["event"]["payload"]["targets"] = json!([{"type": "withheld", "payload": {
            "sha256": DIGEST, "bytes": 1, "extra": 1}}]);
    });
    assert_tampered_line_is_corrupt(|line| {
        line["event"]["payload"]["action_id"] = json!("00000000-0000-0000-0000-000000000000");
    });
    assert_tampered_line_is_corrupt(|line| {
        line["event"]["payload"]["workspace_id"] = json!(Uuid::nil());
    });
}

#[test]
fn duplicate_json_keys_blank_lines_and_torn_tails_are_refused() {
    for text in [
        "{\"schema_version\":1,\"schema_version\":1}\n".to_owned(),
        "\n".to_owned(),
        "{\"schema_version\":1".to_owned(),
    ] {
        let directory = tempfile::tempdir().expect("directory");
        drop(ActionJournal::open(directory.path()).expect("create"));
        fs::write(active_path(directory.path()), &text).expect("write");
        assert!(matches!(
            ActionJournal::open(directory.path()),
            Err(ActionJournalError::Corrupt(_))
        ));
        assert_eq!(
            fs::read_to_string(active_path(directory.path())).expect("read"),
            text
        );
    }
}

#[test]
fn duplicate_start_and_duplicate_finish_are_refused() {
    let directory = tempfile::tempdir().expect("directory");
    seeded(directory.path());
    let mut lines = active_lines(directory.path());
    let mut again = lines[1].clone();
    again["sequence"] = json!(4);
    lines.push(again);
    rewrite_active(directory.path(), &lines);
    assert!(matches!(
        ActionJournal::open(directory.path()),
        Err(ActionJournalError::Corrupt("duplicate action finish"))
    ));
}

#[test]
fn untrusted_values_are_rejected_not_stored() {
    let directory = tempfile::tempdir().expect("directory");
    let mut journal = ActionJournal::open(directory.path()).expect("open");
    let bad_calls = [
        "",
        "a\nb",
        "a\u{0}b",
        "a\u{7f}b",
        "a\u{202e}b",
        "a\u{200b}b",
    ];
    for call in bad_calls {
        assert!(matches!(
            journal.record_started(&started(call, ActionTool::ReadFile, &["a"])),
            Err(ActionJournalError::InvalidValue(_))
        ));
    }
    assert!(matches!(
        journal.record_started(&started(&"c".repeat(129), ActionTool::ReadFile, &["a"])),
        Err(ActionJournalError::InvalidValue(_))
    ));
    let two = ["a", "b"];
    assert!(
        journal
            .record_started(&started("c", ActionTool::ReadFile, &two))
            .is_err()
    );
    assert!(
        journal
            .record_started(&started("c", ActionTool::RenameFile, &["a"]))
            .is_err()
    );
    assert!(
        journal
            .record_started(&started("c", ActionTool::RenameFile, &[]))
            .is_err()
    );
    assert!(
        journal
            .record_started(&started("c", ActionTool::ReadFile, &["a", "b", "c"]))
            .is_err()
    );
    let mut bad_digest = started("c", ActionTool::ReadFile, &["a"]);
    let digests = [
        DIGEST.to_uppercase(),
        "zz".repeat(32),
        "ab".to_owned(),
        String::new(),
    ];
    for digest in &digests {
        bad_digest.request_sha256 = Some(digest);
        assert!(journal.record_started(&bad_digest).is_err());
    }
    let mut bad_approval = started("c", ActionTool::ReadFile, &["a"]);
    bad_approval.authorization = Authorization::Approval {
        approval_id: Uuid::nil(),
        settings_revision: 1,
    };
    assert!(journal.record_started(&bad_approval).is_err());
    assert!(
        journal
            .record_refused(&refused(
                "c",
                RefusalReason::ApprovalDenied {
                    approval_id: Uuid::nil()
                }
            ))
            .is_err()
    );

    // Nothing was stored, and the listing root is the one valid empty target.
    assert_eq!(
        fs::metadata(active_path(directory.path()))
            .expect("meta")
            .len(),
        0
    );
    journal
        .record_started(&started("c", ActionTool::ListDirectory, &[""]))
        .expect("root listing");
}

#[test]
fn finish_codes_and_ids_are_validated() {
    let directory = tempfile::tempdir().expect("directory");
    let mut journal = ActionJournal::open(directory.path()).expect("open");
    let id = journal
        .record_started(&started("c", ActionTool::RunCommand, &[]))
        .expect("start");
    for code in ["", "UPPER", "has space", "dash-ed", "é", &"a".repeat(65)] {
        assert!(matches!(
            journal.record_finished(id, ActionOutcome::Failed, Some(code), None),
            Err(ActionJournalError::InvalidValue(_))
        ));
    }
    assert!(
        journal
            .record_finished(id, ActionOutcome::Failed, None, Some(Uuid::nil()))
            .is_err()
    );
    assert!(
        journal
            .record_finished(Uuid::new_v4(), ActionOutcome::Failed, None, None)
            .is_err()
    );
    journal
        .record_finished(id, ActionOutcome::Failed, Some(&"a".repeat(64)), None)
        .expect("valid finish");
    assert!(
        journal
            .record_finished(id, ActionOutcome::Failed, None, None)
            .is_err(),
        "a second finish is refused"
    );
}

#[test]
fn unfinished_start_is_reported_as_unfinished_and_stays_so() {
    let directory = tempfile::tempdir().expect("directory");
    let mut journal = ActionJournal::open(directory.path()).expect("open");
    journal
        .record_started(&started("crash", ActionTool::DeleteFile, &["gone.txt"]))
        .expect("start");
    drop(journal);
    // Reopening recovers nothing automatically: the start stays unfinished.
    drop(ActionJournal::open(directory.path()).expect("reopen"));
    let records = ActionJournal::recent(directory.path(), 10).expect("recent");
    assert_eq!(records.len(), 1);
    assert!(matches!(
        &records[0].disposition,
        ActionDisposition::Executed { finish: None, .. }
    ));
}

#[test]
fn second_writer_is_busy_and_does_not_disturb_the_first() {
    let directory = tempfile::tempdir().expect("directory");
    let mut first = ActionJournal::open(directory.path()).expect("first");
    assert!(matches!(
        ActionJournal::open(directory.path()),
        Err(ActionJournalError::Busy)
    ));
    first
        .record_refused(&refused("c", RefusalReason::Cancelled))
        .expect("first still writes");
    drop(first);
    ActionJournal::open(directory.path()).expect("lock released on drop");
}

#[test]
fn reading_does_not_modify_any_file() {
    let directory = tempfile::tempdir().expect("directory");
    {
        let mut journal = ActionJournal::open_with(directory.path(), small(3000, 4)).expect("open");
        for _ in 0..12 {
            journal
                .record_refused(&refused("c", RefusalReason::InvalidRequest))
                .expect("refuse");
        }
    }
    let snapshot = |directory: &Path| {
        let mut files: Vec<(String, Vec<u8>)> = fs::read_dir(directory)
            .expect("list")
            .map(|entry| {
                let entry = entry.expect("entry");
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    fs::read(entry.path()).expect("read"),
                )
            })
            .collect();
        files.sort();
        files
    };
    let before = snapshot(directory.path());
    assert!(before.len() > 1, "rotation produced archives");
    ActionJournal::recent(directory.path(), 1000).expect("recent");
    assert_eq!(snapshot(directory.path()), before);
}

#[test]
fn recent_of_a_missing_journal_is_empty_and_limits_are_checked() {
    let directory = tempfile::tempdir().expect("directory");
    assert!(
        ActionJournal::recent(directory.path(), 5)
            .expect("empty")
            .is_empty()
    );
    assert!(
        ActionJournal::recent(&directory.path().join("absent"), 5)
            .expect("absent")
            .is_empty()
    );
    assert!(!directory.path().join("absent").exists());
    for limit in [0, 1001] {
        assert!(matches!(
            ActionJournal::recent(directory.path(), limit),
            Err(ActionJournalError::InvalidValue(_))
        ));
    }
}

#[test]
fn rotation_keeps_sequences_monotonic_and_recent_spans_archives_newest_first() {
    let directory = tempfile::tempdir().expect("directory");
    let total = 20;
    {
        let mut journal =
            ActionJournal::open_with(directory.path(), small(3000, 100)).expect("open");
        for index in 0..total {
            let call = format!("call_{index}");
            journal
                .record_refused(&refused(&call, RefusalReason::AccessDenied))
                .expect("refuse");
        }
    }
    let archives = archive_names(directory.path());
    assert!(
        archives.len() >= 2,
        "several rotations happened: {archives:?}"
    );
    assert!(
        fs::metadata(active_path(directory.path()))
            .expect("active")
            .len()
            <= 3000,
        "active file respects the byte bound"
    );
    let records = ActionJournal::recent(directory.path(), 1000).expect("recent");
    assert_eq!(records.len(), total);
    let sequences: Vec<u64> = records.iter().map(|record| record.sequence).collect();
    let expected: Vec<u64> = (1..=u64::try_from(total).expect("count")).rev().collect();
    assert_eq!(sequences, expected);
    assert_eq!(records[0].call_id, "call_19");
    let limited = ActionJournal::recent(directory.path(), 3).expect("limited");
    assert_eq!(limited.len(), 3);
    assert_eq!(limited[2].call_id, "call_17");

    // A reopened writer continues the sequence across rotations.
    let mut journal = ActionJournal::open_with(directory.path(), small(3000, 100)).expect("reopen");
    journal
        .record_refused(&refused("after", RefusalReason::Cancelled))
        .expect("refuse");
    drop(journal);
    let newest = ActionJournal::recent(directory.path(), 1).expect("newest");
    assert_eq!(newest[0].sequence, 21);
}

#[test]
fn event_bound_also_rotates() {
    let directory = tempfile::tempdir().expect("directory");
    {
        let limits = Limits {
            max_bytes: MAX_BYTES,
            max_events: 3,
            max_archives: 10,
        };
        let mut journal = ActionJournal::open_with(directory.path(), limits).expect("open");
        for _ in 0..7 {
            journal
                .record_refused(&refused("c", RefusalReason::Cancelled))
                .expect("refuse");
        }
    }
    assert_eq!(archive_names(directory.path()).len(), 2);
    assert_eq!(active_lines(directory.path()).len(), 1);
    assert_eq!(
        ActionJournal::recent(directory.path(), 100)
            .expect("recent")
            .len(),
        7
    );
}

#[test]
fn oldest_archive_is_dropped_only_after_the_new_archive_is_durable() {
    let directory = tempfile::tempdir().expect("directory");
    let mut journal = ActionJournal::open_with(directory.path(), small(2500, 2)).expect("open");
    while archive_names(directory.path()).len() < 2 {
        journal
            .record_refused(&refused("c", RefusalReason::Cancelled))
            .expect("refuse");
    }
    let before = archive_names(directory.path());
    // Windows blocks other handles from reading the locked active file.
    let state_before = (journal.events, journal.bytes);

    // Fail the next rotation before the archive is in place.
    journal.fail_next_archive_for_test();
    let mut failed = false;
    for _ in 0..20 {
        if journal
            .record_refused(&refused("c", RefusalReason::Cancelled))
            .is_err()
        {
            failed = true;
            break;
        }
    }
    assert!(failed, "the injected archive failure surfaced");
    assert_eq!(
        archive_names(directory.path()),
        before,
        "no archive dropped or added"
    );
    assert!(!directory.path().join(TEMP_NAME).exists());
    assert!(
        journal.events >= state_before.0 && journal.bytes >= state_before.1,
        "failed rotation lost no active record"
    );

    // The writer is still usable; the next rotation succeeds and prunes the oldest.
    for _ in 0..20 {
        journal
            .record_refused(&refused("c", RefusalReason::Cancelled))
            .expect("recovered");
    }
    let after = archive_names(directory.path());
    assert_eq!(after.len(), 2);
    assert!(!after.contains(&before[0]), "oldest archive was pruned");
    drop(journal);
    ActionJournal::recent(directory.path(), 1000).expect("journal still valid");
}

#[test]
fn finish_always_fits_after_a_start_even_at_the_bound() {
    let directory = tempfile::tempdir().expect("directory");
    let mut journal = ActionJournal::open_with(directory.path(), small(5000, 3)).expect("open");
    for round in 0..30 {
        let call = format!("call_{round}");
        let id = journal
            .record_started(&started(&call, ActionTool::EditFile, &["src/dir/file.rs"]))
            .expect("start");
        let before = journal.bytes;
        journal
            .record_finished(
                id,
                ActionOutcome::Succeeded,
                Some("done"),
                Some(Uuid::new_v4()),
            )
            .expect("finish fits");
        assert!(
            journal.bytes > before || journal.events == 1,
            "finish landed or rotated"
        );
    }
    drop(journal);
    let records = ActionJournal::recent(directory.path(), 1000).expect("recent");
    assert!(!records.is_empty());
    assert!(records.iter().all(|record| matches!(
        &record.disposition,
        ActionDisposition::Executed {
            finish: Some(_),
            ..
        }
    )));
}

#[test]
fn worst_case_finish_record_fits_its_reservation() {
    let line = JournalLine {
        schema_version: SCHEMA_VERSION,
        sequence: u64::MAX,
        time: "9999-12-31T23:59:59.999999999Z".to_owned(),
        event: JournalEvent::Finished(FinishedPayload {
            action_id: Uuid::new_v4(),
            outcome: ActionOutcome::Uncertain,
            code: Some("a".repeat(MAX_CODE_BYTES)),
            operation_id: Some(Uuid::new_v4()),
        }),
    };
    assert!(encode_line(&line).expect("encode").len() <= FINISH_RESERVE_BYTES);
}

#[test]
fn worst_case_start_record_fits_the_event_bound() {
    let quotes = "\"".repeat(MAX_TARGET_BYTES);
    let line = JournalLine {
        schema_version: SCHEMA_VERSION,
        sequence: u64::MAX,
        time: "9999-12-31T23:59:59.999999999Z".to_owned(),
        event: JournalEvent::Started(StartedPayload {
            action_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            workspace_id: workspace(),
            call_id: "\\".repeat(MAX_CALL_ID_BYTES),
            tool: ActionTool::RenameFile,
            targets: vec![
                StoredTarget::Path(quotes.clone()),
                StoredTarget::Path(quotes),
            ],
            request_sha256: Some(DIGEST.to_owned()),
            authorization: Authorization::Approval {
                approval_id: Uuid::new_v4(),
                settings_revision: u64::MAX,
            },
        }),
    };
    assert!(encode_line(&line).expect("encode").len() <= MAX_EVENT_BYTES);
}

#[test]
fn write_failure_poisons_the_writer_and_reopen_fails_closed_on_a_torn_line() {
    let directory = tempfile::tempdir().expect("directory");
    let mut journal = ActionJournal::open(directory.path()).expect("open");
    journal
        .record_refused(&refused("c", RefusalReason::Cancelled))
        .expect("refuse");
    journal.fail_next_write_for_test();
    assert!(matches!(
        journal.record_refused(&refused("c", RefusalReason::Cancelled)),
        Err(ActionJournalError::Io(_))
    ));
    assert!(matches!(
        journal.record_refused(&refused("c", RefusalReason::Cancelled)),
        Err(ActionJournalError::Poisoned)
    ));
    assert!(matches!(
        journal.record_started(&started("c", ActionTool::ReadFile, &["a"])),
        Err(ActionJournalError::Poisoned)
    ));
    drop(journal);
    // The injected failure wrote nothing, so the journal is still intact.
    assert_eq!(
        ActionJournal::recent(directory.path(), 10)
            .expect("recent")
            .len(),
        1
    );

    let mut bytes = fs::read(active_path(directory.path())).expect("bytes");
    bytes.extend_from_slice(b"{\"schema_version\":1,\"seq");
    fs::write(active_path(directory.path()), &bytes).expect("tear");
    assert!(matches!(
        ActionJournal::open(directory.path()),
        Err(ActionJournalError::Corrupt("incomplete final line"))
    ));
    assert_eq!(
        fs::read(active_path(directory.path())).expect("bytes"),
        bytes
    );
}

#[test]
fn interrupted_rotation_is_completed_when_the_archive_is_identical() {
    let directory = tempfile::tempdir().expect("directory");
    seeded(directory.path());
    let bytes = fs::read(active_path(directory.path())).expect("bytes");
    // Simulate a crash after the archive rename and before truncation.
    fs::write(archive_path(directory.path(), 1), &bytes).expect("archive");
    fs::write(directory.path().join(TEMP_NAME), b"partial").expect("stale temporary");
    assert_eq!(
        ActionJournal::recent(directory.path(), 10)
            .expect("reader tolerates it")
            .len(),
        2
    );
    let mut journal = ActionJournal::open(directory.path()).expect("open completes rotation");
    assert_eq!(
        fs::metadata(active_path(directory.path()))
            .expect("meta")
            .len(),
        0
    );
    assert!(!directory.path().join(TEMP_NAME).exists());
    journal
        .record_refused(&refused("next", RefusalReason::Cancelled))
        .expect("refuse");
    drop(journal);
    let records = ActionJournal::recent(directory.path(), 10).expect("recent");
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].sequence, 4);
}

#[test]
fn overlapping_but_different_archive_and_misnamed_archive_are_corrupt() {
    let directory = tempfile::tempdir().expect("directory");
    seeded(directory.path());
    let mut bytes = fs::read(active_path(directory.path())).expect("bytes");
    let mut other = bytes.clone();
    other.truncate(other.iter().position(|b| *b == b'\n').expect("first line") + 1);
    fs::write(archive_path(directory.path(), 1), &other).expect("archive");
    assert!(matches!(
        ActionJournal::open(directory.path()),
        Err(ActionJournalError::Corrupt(
            "active file overlaps an archive but differs"
        ))
    ));
    fs::remove_file(archive_path(directory.path(), 1)).expect("remove");
    bytes.clear();
    let directory = tempfile::tempdir().expect("directory");
    seeded(directory.path());
    let text = fs::read(active_path(directory.path())).expect("bytes");
    fs::write(archive_path(directory.path(), 7), &text).expect("misnamed");
    assert!(matches!(
        ActionJournal::open(directory.path()),
        Err(ActionJournalError::Corrupt(_))
    ));
}

#[test]
fn a_gap_between_archive_and_active_file_is_corrupt() {
    let directory = tempfile::tempdir().expect("directory");
    {
        let mut journal =
            ActionJournal::open_with(directory.path(), small(2500, 10)).expect("open");
        for _ in 0..30 {
            journal
                .record_refused(&refused("c", RefusalReason::Cancelled))
                .expect("refuse");
        }
    }
    let names = archive_names(directory.path());
    assert!(names.len() >= 2);
    fs::remove_file(directory.path().join(&names[1])).expect("remove middle archive");
    assert!(matches!(
        ActionJournal::open(directory.path()),
        Err(ActionJournalError::Corrupt("sequence is not consecutive"))
    ));
}

fn path_bytes(sha256: &str, bytes: u64) -> Value {
    json!({"type": "withheld", "payload": {"sha256": sha256, "bytes": bytes}})
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn hostile_targets_are_recorded_as_digests_and_never_as_raw_text() {
    let long_path = "p".repeat(MAX_TARGET_BYTES + 1);
    let hostile: Vec<(&str, &str)> = vec![
        ("parent", "a/../SECRETPARENT"),
        ("parent_backslash", "a\\..\\SECRETBACK"),
        ("absolute", "/etc/SECRETABS"),
        ("unc", "\\\\server\\SECRETUNC"),
        ("drive", "C:\\SECRETDRIVE"),
        ("control", "SECRET\u{1b}[31mCTRL"),
        ("tab", "SECRET\tTAB"),
        ("bidi", "SECRET\u{202e}BIDI"),
        ("invisible", "SECRET\u{200b}ZW"),
        ("empty_for_read", ""),
        ("oversize", long_path.as_str()),
    ];
    for (name, target) in hostile {
        let directory = tempfile::tempdir().expect("directory");
        let mut journal = ActionJournal::open(directory.path()).expect("open");
        journal
            .record_started(&started("c", ActionTool::ReadFile, &[target]))
            .unwrap_or_else(|error| panic!("{name}: start must not fail: {error}"));
        let mut refusal = refused("c", RefusalReason::InvalidRequest);
        let targets = [target, "ok.txt"];
        refusal.targets = &targets;
        journal.record_refused(&refusal).expect("refusal");
        drop(journal);

        let text = String::from_utf8(fs::read(active_path(directory.path())).expect("bytes"))
            .expect("utf8");
        assert!(!text.contains("SECRET"), "{name}: raw hostile text stored");
        assert!(!text.contains("/etc/"), "{name}: raw path stored");
        assert!(
            !text.contains('\u{1b}') && !text.contains("\\u001b"),
            "{name}"
        );
        assert!(
            !text.contains('\u{202e}') && !text.contains("\\u202e"),
            "{name}"
        );
        assert!(!text.contains(&long_path), "{name}");
        assert!(text.contains(&sha256_hex(target)), "{name}: digest missing");

        let records = ActionJournal::recent(directory.path(), 10).expect("recent");
        let expected = StoredTarget::Withheld(WithheldTarget {
            sha256: sha256_hex(target),
            bytes: u64::try_from(target.len()).expect("length"),
        });
        assert_eq!(
            records[1].targets,
            std::slice::from_ref(&expected),
            "{name}"
        );
        assert_eq!(
            records[0].targets,
            [expected, StoredTarget::Path("ok.txt".to_owned())],
            "{name}"
        );
    }
}

#[test]
fn valid_paths_stay_paths_and_root_listing_stays_valid() {
    let directory = tempfile::tempdir().expect("directory");
    let mut journal = ActionJournal::open(directory.path()).expect("open");
    journal
        .record_started(&started("c", ActionTool::ListDirectory, &[""]))
        .expect("root");
    journal
        .record_started(&started("c", ActionTool::ReadFile, &["src\\dir/file é.rs"]))
        .expect("path");
    drop(journal);
    let records = ActionJournal::recent(directory.path(), 10).expect("recent");
    assert_eq!(records[1].targets, [StoredTarget::Path(String::new())]);
    assert_eq!(
        records[0].targets,
        [StoredTarget::Path("src\\dir/file é.rs".to_owned())]
    );
}

#[test]
fn refusals_accept_zero_to_two_targets_but_started_actions_keep_arity() {
    let directory = tempfile::tempdir().expect("directory");
    let mut journal = ActionJournal::open(directory.path()).expect("open");
    for targets in [&[][..], &["a"][..], &["a", "b"][..]] {
        let mut refusal = refused("c", RefusalReason::InvalidRequest);
        refusal.tool = ActionTool::RenameFile;
        refusal.targets = targets;
        journal.record_refused(&refusal).expect("refusal");
        refusal.tool = ActionTool::ReadFile;
        journal.record_refused(&refusal).expect("refusal");
    }
    let mut refusal = refused("c", RefusalReason::InvalidRequest);
    refusal.targets = &["a", "b", "c"];
    assert!(journal.record_refused(&refusal).is_err());
    assert!(
        journal
            .record_started(&started("c", ActionTool::ReadFile, &["a", "b"]))
            .is_err()
    );
    // An invalid call identifier is still rejected.
    assert!(
        journal
            .record_refused(&refused("bad\nid", RefusalReason::Cancelled))
            .is_err()
    );
}

#[test]
fn start_drop_reopen_finish_works_and_is_listed_finished() {
    let directory = tempfile::tempdir().expect("directory");
    let mut journal = ActionJournal::open(directory.path()).expect("open");
    let id = journal
        .record_started(&started("c", ActionTool::RunCommand, &[]))
        .expect("start");
    drop(journal);
    let mut journal = ActionJournal::open(directory.path()).expect("reopen");
    journal
        .record_finished(id, ActionOutcome::Succeeded, Some("exit_0"), None)
        .expect("finish by a later writer");
    drop(journal);
    let records = ActionJournal::recent(directory.path(), 10).expect("recent");
    assert!(matches!(
        &records[0].disposition,
        ActionDisposition::Executed {
            finish: Some(_),
            ..
        }
    ));
}

#[test]
fn start_rotate_reopen_finish_works_from_the_newest_archive() {
    let directory = tempfile::tempdir().expect("directory");
    let limits = small(2500, 4);
    let mut journal = ActionJournal::open_with(directory.path(), limits).expect("open");
    let id = journal
        .record_started(&started("c", ActionTool::EditFile, &["a.txt"]))
        .expect("start");
    while archive_names(directory.path()).is_empty() {
        journal
            .record_refused(&refused("r", RefusalReason::Cancelled))
            .expect("refuse");
    }
    drop(journal);
    assert_eq!(archive_names(directory.path()).len(), 1);
    let mut journal = ActionJournal::open_with(directory.path(), limits).expect("reopen");
    journal
        .record_finished(id, ActionOutcome::Failed, Some("boom"), None)
        .expect("finish with the start in the newest archive");
    assert!(
        journal
            .record_finished(id, ActionOutcome::Failed, None, None)
            .is_err()
    );
    drop(journal);

    // The finish is in the active file and the start in an archive: recent joins them.
    let records = ActionJournal::recent(directory.path(), 1000).expect("recent");
    let start = records
        .iter()
        .find(|record| record.tool == ActionTool::EditFile)
        .expect("start listed");
    assert!(matches!(
        &start.disposition,
        ActionDisposition::Executed {
            finish: Some(ActionFinish {
                outcome: ActionOutcome::Failed,
                ..
            }),
            ..
        }
    ));
}

#[test]
fn double_finish_across_writers_and_unknown_ids_are_refused() {
    let directory = tempfile::tempdir().expect("directory");
    let mut journal = ActionJournal::open(directory.path()).expect("open");
    let id = journal
        .record_started(&started("c", ActionTool::RunCommand, &[]))
        .expect("start");
    drop(journal);
    let mut journal = ActionJournal::open(directory.path()).expect("second writer");
    journal
        .record_finished(id, ActionOutcome::Succeeded, None, None)
        .expect("finish");
    drop(journal);
    let mut journal = ActionJournal::open(directory.path()).expect("third writer");
    assert!(
        journal
            .record_finished(id, ActionOutcome::Failed, None, None)
            .is_err(),
        "a second finish from another writer is refused"
    );
    assert!(
        journal
            .record_finished(Uuid::new_v4(), ActionOutcome::Failed, None, None)
            .is_err()
    );
}

#[test]
fn crash_leftovers_never_block_new_starts_and_stop_being_finishable() {
    let directory = tempfile::tempdir().expect("directory");
    let limits = small(8000, 4);
    let mut leftovers = Vec::new();
    {
        let mut journal = ActionJournal::open_with(directory.path(), limits).expect("open");
        for _ in 0..40 {
            leftovers.push(
                journal
                    .record_started(&started("crash", ActionTool::RunCommand, &[]))
                    .expect("start leftover"),
            );
        }
    }
    let mut journal = ActionJournal::open_with(directory.path(), limits).expect("reopen");
    assert!(
        journal
            .record_finished(leftovers[0], ActionOutcome::Uncertain, None, None)
            .is_err(),
        "the oldest leftover rotated beyond the newest archive"
    );
    let newest = *leftovers.last().expect("leftovers");
    journal
        .record_finished(newest, ActionOutcome::Uncertain, None, None)
        .expect("a leftover still in the active file or newest archive is finishable");
    for round in 0..60 {
        let call = format!("new_{round}");
        let id = journal
            .record_started(&started(&call, ActionTool::ReadFile, &["a"]))
            .expect("new starts are never blocked");
        journal
            .record_finished(id, ActionOutcome::Succeeded, None, None)
            .expect("new finishes fit");
    }
    assert!(
        journal
            .record_finished(leftovers[1], ActionOutcome::Failed, None, None)
            .is_err(),
        "a start rotated past the newest archive is no longer finishable"
    );
    assert!(journal.active_open.is_empty() && journal.archive_open.is_empty());
    drop(journal);
    let records = ActionJournal::recent(directory.path(), 1000).expect("recent");
    assert_eq!(records[0].call_id, "new_59", "newest call is listed first");
}

#[test]
fn unrelated_files_and_directory_look_alikes_are_not_archives() {
    let directory = tempfile::tempdir().expect("directory");
    seeded(directory.path());
    fs::write(directory.path().join("model-actions-v1.1.jsonl"), b"junk").expect("junk");
    fs::write(directory.path().join("notes.txt"), b"junk").expect("junk");
    assert_eq!(
        ActionJournal::recent(directory.path(), 10)
            .expect("recent")
            .len(),
        2
    );
    ActionJournal::open(directory.path()).expect("open");
}

/// A rejected call (ADR-0099) is refused with its own reason and the digest of
/// the received arguments. A call to a tool the runtime does not have is
/// recorded as `unrecognized`, with no target and no name.
#[test]
fn rejected_calls_are_recorded_with_their_reasons_and_an_unrecognized_tool() {
    let directory = tempfile::tempdir().expect("directory");
    let mut journal = ActionJournal::open(directory.path()).expect("open");
    let cases = [
        (ActionTool::CreateFile, RefusalReason::ArgumentsTooLarge),
        (ActionTool::ReadFile, RefusalReason::InvalidArguments),
        (ActionTool::ReadFile, RefusalReason::InvalidPath),
        (ActionTool::Unrecognized, RefusalReason::UnsupportedTool),
    ];
    for (tool, reason) in cases {
        let mut action = refused("rejected_1", reason);
        action.tool = tool;
        action.targets = &[];
        action.request_sha256 = Some(DIGEST);
        journal.record_refused(&action).expect("refuse");
    }
    drop(journal);

    let lines = active_lines(directory.path());
    let stored: Vec<(String, String)> = lines
        .iter()
        .map(|line| {
            (
                line["event"]["payload"]["tool"]
                    .as_str()
                    .expect("tool")
                    .to_owned(),
                line["event"]["payload"]["reason"]["type"]
                    .as_str()
                    .expect("reason")
                    .to_owned(),
            )
        })
        .collect();
    assert_eq!(
        stored,
        [
            ("create_file".to_owned(), "arguments_too_large".to_owned()),
            ("read_file".to_owned(), "invalid_arguments".to_owned()),
            ("read_file".to_owned(), "invalid_path".to_owned()),
            ("unrecognized".to_owned(), "unsupported_tool".to_owned()),
        ]
    );
    let records = ActionJournal::recent(directory.path(), 10).expect("recent");
    assert_eq!(records.len(), 4);
    assert_eq!(records[0].tool, ActionTool::Unrecognized);
    assert!(records[0].targets.is_empty());
    assert_eq!(records[0].request_sha256.as_deref(), Some(DIGEST));
    assert!(matches!(
        records[0].disposition,
        ActionDisposition::Refused {
            reason: RefusalReason::UnsupportedTool
        }
    ));
}
