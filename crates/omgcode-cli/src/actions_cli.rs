//! Trusted inspection of the model action journal (ADR-0062).
#![forbid(unsafe_code)]

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use clap::Subcommand;
use omgcode_core::action_journal::{
    ActionDisposition, ActionJournal, ActionJournalError, ActionRecord,
};
use serde_json::{Value, json};

use crate::{CliError, print_json};

/// Inspect the record of every model tool call.
#[derive(Subcommand)]
pub(crate) enum ActionsCommand {
    /// List the most recent model actions, oldest first. Each entry is one
    /// call: executed, with how it was authorised and its outcome, or refused,
    /// with the reason. An executed call with no finish has an unknown outcome.
    List {
        /// How many of the most recent calls to list (1 to 1000).
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
}

/// How long a listing waits for a writer that holds the journal.
const BUSY_WAIT: Duration = Duration::from_secs(10);
const BUSY_POLL: Duration = Duration::from_millis(20);

/// Run one action journal command.
pub(crate) fn run(action: ActionsCommand, data_root: &Path) -> Result<(), CliError> {
    match action {
        ActionsCommand::List { limit } => {
            let records = recent(data_root, limit)?;
            print_json(&json!({
                "actions": records.iter().map(record_json).collect::<Vec<Value>>(),
            }))
        }
    }
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
        "disposition": disposition,
    })
}
