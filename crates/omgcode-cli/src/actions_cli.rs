//! Trusted inspection of the model action journal (ADR-0063).
#![forbid(unsafe_code)]

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use clap::Subcommand;
use omgcode_core::action_journal::{
    ActionDisposition, ActionJournal, ActionJournalError, ActionRecord, MAX_RECENT_RECORDS,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{CliError, print_json};

/// Inspect the record of every model tool call.
#[derive(Subcommand)]
pub(crate) enum ActionsCommand {
    /// List the most recent model actions, newest first. Each entry is one
    /// call: executed, with how it was authorised and its outcome, or refused,
    /// with the reason. An executed call with no finish has an unknown outcome.
    /// `after_web_content` is true for every call a session made after its
    /// first successful `web_fetch`, whatever the tool or outcome. A
    /// `run_command` entry names the program, not the files it touched.
    List {
        /// How many of the most recent matching calls to list (1 to 1000).
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// List only the calls of this session.
        #[arg(long, value_name = "SESSION_ID")]
        session: Option<Uuid>,
        /// List only the calls made after the session fetched web content.
        #[arg(long)]
        after_web_content: bool,
    },
}

/// How long a listing waits for a writer that holds the journal.
const BUSY_WAIT: Duration = Duration::from_secs(10);
const BUSY_POLL: Duration = Duration::from_millis(20);

/// Run one action journal command.
pub(crate) fn run(action: ActionsCommand, data_root: &Path) -> Result<(), CliError> {
    match action {
        ActionsCommand::List {
            limit,
            session,
            after_web_content,
        } => {
            let records = if session.is_some() || after_web_content {
                filtered(data_root, limit, session, after_web_content)?
            } else {
                recent(data_root, limit)?
            };
            print_json(&json!({
                "actions": records.iter().map(record_json).collect::<Vec<Value>>(),
            }))
        }
    }
}

/// The newest `limit` calls that match every filter.
///
/// The filters apply before the limit, so a filtered listing is not emptied by
/// newer calls that do not match. The journal keeps and lists at most
/// [`MAX_RECENT_RECORDS`] calls at a time, which bounds the search.
fn filtered(
    data_root: &Path,
    limit: usize,
    session: Option<Uuid>,
    after_web_content: bool,
) -> Result<Vec<ActionRecord>, CliError> {
    if !(1..=MAX_RECENT_RECORDS).contains(&limit) {
        return Err(CliError::new(
            "configuration",
            "limit must be between 1 and 1000".to_owned(),
        ));
    }
    let mut records = recent(data_root, MAX_RECENT_RECORDS)?;
    records.retain(|record| {
        session.is_none_or(|session| record.session_id == session)
            && (!after_web_content || record.after_web_content)
    });
    records.truncate(limit);
    Ok(records)
}

/// Read the journal, waiting while a writer holds its lock.
fn recent(data_root: &Path, limit: usize) -> Result<Vec<ActionRecord>, CliError> {
    let started = Instant::now();
    loop {
        match ActionJournal::recent(data_root, limit) {
            Err(ActionJournalError::Busy) if started.elapsed() < BUSY_WAIT => {
                thread::sleep(BUSY_POLL);
            }
            Ok(records) => return Ok(records),
            Err(error) => return Err(journal_error(&error)),
        }
    }
}

fn journal_error(error: &ActionJournalError) -> CliError {
    let code = match error {
        ActionJournalError::Busy => "busy",
        ActionJournalError::InvalidValue(_) => "configuration",
        ActionJournalError::Corrupt(_) => "invalid_journal",
        _ => "storage",
    };
    CliError::new(code, error.to_string())
}

fn record_json(record: &ActionRecord) -> Value {
    let disposition = match &record.disposition {
        ActionDisposition::Executed {
            action_id,
            authorization,
            finish,
        } => json!({
            "executed": {
                "action_id": action_id,
                "authorization": authorization,
                "finish": finish.as_ref().map(|finish| json!({
                    "time": finish.time,
                    "outcome": finish.outcome,
                    "code": finish.code,
                    "operation_id": finish.operation_id,
                })),
            }
        }),
        ActionDisposition::Refused { reason } => json!({ "refused": { "reason": reason } }),
    };
    json!({
        "sequence": record.sequence,
        "time": record.time,
        "session_id": record.session_id,
        "workspace_id": record.workspace_id.as_uuid(),
        "call_id": record.call_id,
        "tool": record.tool,
        "targets": record.targets,
        "request_sha256": record.request_sha256,
        "after_web_content": record.after_web_content,
        "disposition": disposition,
    })
}
