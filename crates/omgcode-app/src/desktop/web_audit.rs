//! The audit of web content in one session: its `web_fetch` calls and every
//! model action made after the first successful fetch (ADR-0098).
//!
//! The operator reviews what the model did once web content was in its
//! context. The service lists, newest first, the open session's records from
//! the model action journal that either are a fetch (`ActionTool::Network`) or
//! carry `after_web_content`. The journal sets the flag; this module only
//! reads it. The list is evidence for review and grants nothing.
//!
//! Sources, all trusted, read-only, and resolved by the session identifier
//! and never by a webview-supplied path:
//!
//! - the model action journal: the tool, the validated targets (a path, or
//!   for a fetch the normalised URL) or a withheld digest, the authorization,
//!   the refusal reason, and the outcome. A withheld target is shown as
//!   withheld with its byte length; its digest is never sent as if it were a
//!   name;
//! - the session journal's recorded command requests, joined as the
//!   Processes view joins them, because a `run_command` record has no
//!   target. They name the program and arguments the model chose, under the
//!   same bounds.
//!
//! Nothing here reads a fetched body or a credential, creates a file, takes a
//! settings lock, or runs anything. All text is untrusted and is escaped with
//! `display_safe_line` and bounded before it is returned.

use std::collections::HashMap;

use omgcode_core::action_journal::{
    ActionDisposition, ActionOutcome, ActionRecord, ActionTool, Authorization, MAX_RECENT_RECORDS,
    StoredTarget,
};
use serde::Serialize;
use uuid::Uuid;

use super::DesktopService;
use super::error::DesktopError;
use super::processes::{
    CommandRequest, MAX_ENTRIES, MAX_ITEM_CHARS, MAX_TOTAL_CHARS, RecordedCommand, bound_arguments,
    bounded, command_requests, pair_requests, recent_actions, refusal_code,
};
use super::wire::SessionRequest;

/// Most characters of one target. The journal stores at most 4096 bytes.
const MAX_TARGET_CHARS: usize = 4096;

/// How one listed call ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebAuditOutcome {
    /// The action completed.
    Succeeded,
    /// The action failed with no ambiguous side effect.
    Failed,
    /// A side effect may have happened.
    Uncertain,
    /// Started with no recorded finish: still running, or interrupted, for
    /// example by a crash. The outcome is unknown, not failed.
    Unfinished,
    /// Refused before anything started.
    Refused,
}

/// How an executed call was authorized.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebAuditAuthorization {
    /// The workspace preference was `allow`.
    StandingAllow,
    /// A trusted single-use approval was consumed.
    Approval,
}

/// One target of a listed call.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WebAuditTarget {
    /// A validated workspace-relative path, or for a fetch the normalised
    /// URL. Escaped to one line and bounded. Show as text, never as a link.
    Text {
        /// The path or URL.
        text: String,
    },
    /// A value that failed validation. The journal kept only a digest, which
    /// is not sent.
    Withheld {
        /// Length of the withheld value in bytes.
        bytes: u64,
    },
}

/// One listed model call. Every text field is untrusted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WebAuditEntry {
    /// The validated call identifier. Unique within one turn only.
    pub call_id: String,
    /// RFC 3339 time of the start or refusal record.
    pub time: String,
    /// The tool's wire name, such as `network` or `read_file`.
    pub tool: String,
    /// Whether the call is a `web_fetch` call.
    pub is_fetch: bool,
    /// Whether the session had already fetched web content when the call was
    /// made.
    pub after_web_content: bool,
    /// The validated targets. A command call has none; see `program`.
    pub targets: Vec<WebAuditTarget>,
    /// How the call ended.
    pub outcome: WebAuditOutcome,
    /// The refusal reason, or the finish code of an executed call.
    pub reason: Option<String>,
    /// How an executed call was authorized; `None` for a refusal.
    pub authorization: Option<WebAuditAuthorization>,
    /// For a command call, the program the model named. `None` when the
    /// session journal is unreadable or holds no match.
    pub program: Option<String>,
    /// For a command call, the model's arguments, escaped to one line each
    /// and bounded in count and length.
    pub arguments: Option<Vec<String>>,
    /// How many arguments the call had, when known.
    pub argument_count: Option<usize>,
    /// Whether any text of this entry was shortened or dropped to a bound.
    pub text_truncated: bool,
}

/// The reply to `session_web_audit`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SessionWebAudit {
    /// At most 100 calls, newest first.
    pub entries: Vec<WebAuditEntry>,
    /// Whether older calls may exist that are not listed: more than 100
    /// matched, or the 1000 newest action records, all sessions together,
    /// were full, so older records were not read.
    pub truncated: bool,
}

impl DesktopService {
    /// List the session's web fetches and the actions made after the first
    /// successful fetch, newest first, at most 100.
    ///
    /// Read-only. It creates no file, takes no settings lock, and runs
    /// nothing. The session journal is best effort: when it cannot be read,
    /// a command entry has no program or arguments and the rest is returned.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown session; `busy` when the action journal
    /// stays held; `invalid_journal` when it fails validation; `internal`.
    pub fn session_web_audit(
        &self,
        request: SessionRequest,
    ) -> Result<SessionWebAudit, DesktopError> {
        let inner = &self.inner;
        let path = inner.resolve_path(request.session_id)?;
        let records = recent_actions(&inner.data_root)?;
        let window_full = records.len() >= MAX_RECENT_RECORDS;
        // The session journal is verified as one chain when read, so it is
        // read only to name a listed command call.
        let needs_commands = records.iter().any(|record| {
            is_listed(request.session_id, record) && record.tool == ActionTool::RunCommand
        });
        let requests = if needs_commands {
            command_requests(&path)
        } else {
            Some(Vec::new())
        };
        Ok(build(request.session_id, &records, requests, window_full))
    }
}

/// Whether the audit lists `record`: the session's own fetch, or its action
/// made after web content entered the session.
fn is_listed(session_id: Uuid, record: &ActionRecord) -> bool {
    record.session_id == session_id
        && (record.tool == ActionTool::Network || record.after_web_content)
}

/// Join the sources into the reply. Pure, so tests need no journals.
///
/// `records` is newest first, as the action journal returns it. `requests` is
/// the session's recorded command requests, oldest first.
fn build(
    session_id: Uuid,
    records: &[ActionRecord],
    requests: Option<Vec<CommandRequest>>,
    window_full: bool,
) -> SessionWebAudit {
    // Pair requests over every command record of the session, not only the
    // listed ones: a call identifier repeats across turns, and the pairing
    // is aligned at the newest end.
    let commands: Vec<&ActionRecord> = records
        .iter()
        .rev()
        .filter(|record| record.session_id == session_id && record.tool == ActionTool::RunCommand)
        .collect();
    let paired = pair_requests(&commands, requests.as_deref());
    let by_sequence: HashMap<u64, &CommandRequest> = commands
        .iter()
        .zip(paired)
        .filter_map(|(record, request)| Some((record.sequence, request?)))
        .collect();

    let listed: Vec<&ActionRecord> = records
        .iter()
        .filter(|record| is_listed(session_id, record))
        .collect();
    let truncated = window_full || listed.len() > MAX_ENTRIES;
    let entries = listed
        .into_iter()
        .take(MAX_ENTRIES)
        .map(|record| entry_of(record, by_sequence.get(&record.sequence).copied()))
        .collect();
    SessionWebAudit { entries, truncated }
}

/// One listed call.
fn entry_of(record: &ActionRecord, request: Option<&CommandRequest>) -> WebAuditEntry {
    let mut text_truncated = false;
    let targets = record
        .targets
        .iter()
        .map(|target| match target {
            StoredTarget::Path(text) => {
                let (text, cut) = bounded(text, MAX_TARGET_CHARS);
                text_truncated |= cut;
                WebAuditTarget::Text { text }
            }
            StoredTarget::Withheld(withheld) => WebAuditTarget::Withheld {
                bytes: withheld.bytes,
            },
        })
        .collect();

    let (outcome, reason, authorization) = match &record.disposition {
        ActionDisposition::Refused { reason } => (
            WebAuditOutcome::Refused,
            Some(refusal_code(*reason).to_owned()),
            None,
        ),
        ActionDisposition::Executed {
            authorization,
            finish,
            ..
        } => {
            let authorization = Some(match authorization {
                Authorization::StandingAllow { .. } => WebAuditAuthorization::StandingAllow,
                Authorization::Approval { .. } => WebAuditAuthorization::Approval,
            });
            match finish {
                Some(finish) => (
                    match finish.outcome {
                        ActionOutcome::Succeeded => WebAuditOutcome::Succeeded,
                        ActionOutcome::Failed => WebAuditOutcome::Failed,
                        ActionOutcome::Uncertain => WebAuditOutcome::Uncertain,
                    },
                    finish.code.clone(),
                    authorization,
                ),
                None => (WebAuditOutcome::Unfinished, None, authorization),
            }
        }
    };

    let mut entry = WebAuditEntry {
        call_id: record.call_id.clone(),
        time: record.time.clone(),
        tool: tool_name(record.tool).to_owned(),
        is_fetch: record.tool == ActionTool::Network,
        after_web_content: record.after_web_content,
        targets,
        outcome,
        reason,
        authorization,
        program: None,
        arguments: None,
        argument_count: None,
        text_truncated,
    };
    if record.tool == ActionTool::RunCommand
        && let Some(command) = request.and_then(RecordedCommand::parse)
    {
        fill_command(&mut entry, &command);
    }
    entry
}

/// Set the program and arguments from the model's recorded request under the
/// Processes view's bounds: the program is charged to the per-call text
/// budget first, then the arguments.
fn fill_command(entry: &mut WebAuditEntry, command: &RecordedCommand) {
    let mut budget = MAX_TOTAL_CHARS;
    if let Some(program) = &command.program {
        let (text, cut) = bounded(program, MAX_ITEM_CHARS.min(budget));
        budget = budget.saturating_sub(text.chars().count());
        entry.text_truncated |= cut;
        entry.program = Some(text);
    }
    if let Some(arguments) = &command.arguments {
        entry.argument_count = Some(arguments.len());
        entry.arguments = Some(bound_arguments(
            arguments,
            &mut budget,
            &mut entry.text_truncated,
        ));
    }
}

/// The tool's recorded wire name.
const fn tool_name(tool: ActionTool) -> &'static str {
    match tool {
        ActionTool::ReadFile => "read_file",
        ActionTool::ListDirectory => "list_directory",
        ActionTool::EditFile => "edit_file",
        ActionTool::MultiEditFile => "multi_edit_file",
        ActionTool::CreateFile => "create_file",
        ActionTool::CreateDirectory => "create_directory",
        ActionTool::RenameFile => "rename_file",
        ActionTool::DeleteFile => "delete_file",
        ActionTool::RunCommand => "run_command",
        ActionTool::Network => "network",
        ActionTool::Unrecognized => "unrecognized",
    }
}

#[cfg(test)]
mod tests;
