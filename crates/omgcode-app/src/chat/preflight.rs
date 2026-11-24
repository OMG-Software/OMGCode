//! The pre-turn context trigger and visible meter.

use std::path::Path;

use omgcode_core::context::{
    ContextBudget, MESSAGE_OVERHEAD, estimate_history_input_tokens, estimate_history_parts,
};
use omgcode_core::prompt::{self, BASE_PROMPT, ConversationMessage, ConversationRole, PromptError};
use omgcode_core::provider::LocalModelClient;
use omgcode_core::session::SessionJournal;
use omgcode_core::turn::replay_active;

use super::Turn;
use crate::context::{ContextMeter, compact_journal, summary_output_tokens};
use crate::error::{AppError, ErrorKind};
use crate::tool_mode::{TOOL_SCHEMA_ESTIMATE_BYTES, tool_replay_allowed};

/// Apply the context trigger before a turn and return the visible meter.
///
/// A zero threshold is a `configuration` error. Reaching the trigger without
/// auto-compaction is a `context` error that names the operator's options.
/// With auto-compaction, the summary client compacts the journal and the
/// estimate is recomputed from live settings. No control store is held across
/// the summarizer requests. The turn's cancel token stops a summarizer
/// request; a cancelled compaction is `cancelled` and records nothing.
pub(super) fn preflight_context(
    journal: &mut SessionJournal,
    turn: &Turn<'_>,
    summary_client: &LocalModelClient,
    fixed_extra_bytes: usize,
    tools_enabled: bool,
) -> Result<ContextMeter, AppError> {
    let settings = turn.settings;
    let (data_root, token) = (turn.data_root, turn.token);
    if settings.compact_threshold == 0 {
        return Err(AppError::new(
            ErrorKind::Configuration,
            "The compaction threshold must be a positive token count",
        ));
    }
    let budget = ContextBudget {
        threshold: settings.compact_threshold,
        context_tokens: settings.context_tokens,
        max_output_tokens: settings.max_output_tokens,
    };
    // Summary chunks are sized from the summary request's own output reserve.
    let summary_budget = ContextBudget {
        threshold: u64::MAX,
        context_tokens: settings.context_tokens,
        max_output_tokens: summary_output_tokens(settings.context_tokens),
    };
    let (mut estimate, mut assembly_error) =
        estimate_with_user(journal, turn, fixed_extra_bytes, tools_enabled)?;
    let mut summarizer_calls = None;
    if budget.should_compact(estimate) || assembly_error.is_some() {
        if !settings.auto_compact {
            let reason = match assembly_error {
                Some(PromptError::ToolHistoryCapacity { max_tool_calls }) => format!(
                    "Active history leaves insufficient room for a turn with {max_tool_calls} tool calls. Lower the tool-call limit to keep more history"
                ),
                Some(_) => "Active history exceeds the prompt assembly limits".to_owned(),
                None => format!(
                    "The conservative input estimate ({estimate} tokens) reaches the compaction trigger ({})",
                    budget.trigger()
                ),
            };
            return Err(AppError::new(
                ErrorKind::Context,
                format!("{reason}. Use session compact or rerun chat with --auto-compact."),
            ));
        }
        let outcome = compact_journal(
            journal,
            data_root,
            summary_client,
            token,
            summary_budget,
            turn.cancel,
        )?;
        summarizer_calls = Some(outcome.calls);
        // Settings can change during the summarizer requests, so read the
        // replay authority again for the compacted history.
        (estimate, assembly_error) =
            estimate_with_user(journal, turn, fixed_extra_bytes, tools_enabled)?;
        if let Some(PromptError::ToolHistoryCapacity { max_tool_calls }) = assembly_error {
            return Err(AppError::new(
                ErrorKind::Context,
                format!(
                    "Compaction left insufficient room for {max_tool_calls} tool calls. Lower the tool-call limit or start a new session."
                ),
            ));
        }
        if budget.should_compact(estimate) || assembly_error.is_some() {
            return Err(AppError::new(
                ErrorKind::Context,
                "Compaction did not reduce the context below the trigger and assembly limits",
            ));
        }
    }
    Ok(ContextMeter::new(
        journal,
        budget,
        settings.max_output_tokens,
        settings.auto_compact,
        estimate,
        summarizer_calls,
    ))
}

/// Whether the next request may replay saved tool results, read from live
/// settings at each call. Text-only chat never replays them; a tool turn
/// replays them only under live `model_tools` and `read`, as the runtime
/// does. Unreadable settings fail closed to the withheld form.
fn replay_allowed_now(journal: &SessionJournal, data_root: &Path, tools_enabled: bool) -> bool {
    tools_enabled
        && tool_replay_allowed(
            data_root,
            journal.snapshot().workspace_id,
            journal.schema_version(),
        )
        .unwrap_or(false)
}

/// The history the next request of this mode would send: the active journal
/// messages, with saved tool results, and a summary built from them, withheld
/// without live tool authority.
pub(crate) fn active_history(
    journal: &SessionJournal,
    data_root: &Path,
    tools_enabled: bool,
) -> Vec<ConversationMessage> {
    replay_active(
        journal,
        replay_allowed_now(journal, data_root, tools_enabled),
    )
    .0
}

/// The conservative input estimate of the next turn, split into the parts the
/// desktop's context tab shows (ADR-0087).
///
/// Every field is in estimated tokens and the fields add up to the estimate
/// that [`preflight_context`] computes for the same state without the turn's
/// new user message (`total`, from the same core arithmetic). The message
/// overhead of the system and instruction messages, and the instructions
/// label, belong to no part, so they are reported as `framing`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NextTurnParts {
    /// The compiled base prompt and its message overhead.
    pub system: u64,
    /// The share of the custom instructions that came from the global text.
    pub global_instructions: u64,
    /// The rest of the custom instructions: the workspace text and the
    /// headings that separate the two.
    pub workspace_instructions: u64,
    /// The advertised tool definitions; zero when the turn offers no tools.
    pub tools: u64,
    /// The active compaction summary with its message overhead, when the
    /// session has one.
    pub summary: Option<u64>,
    /// The retained history messages after the compaction boundary.
    pub history_messages: u64,
    /// Their content and message overhead.
    pub history: u64,
    /// The instructions label and the instruction message's overhead.
    pub framing: u64,
    /// The estimate: the sum of the parts above.
    pub total: u64,
}

/// Estimate the next turn's input by part, without a new user message.
///
/// `instructions` is the composed custom instruction text a turn would send
/// and `global_bytes` the bytes of it that belong to the global part, never
/// more than `instructions.len()`. `tools_enabled` is whether the turn would
/// offer the workspace tools. Skills are loaded per turn and are not counted.
///
/// # Errors
///
/// A `context` error when a count overflows.
pub(crate) fn estimate_next_turn_parts(
    journal: &SessionJournal,
    data_root: &Path,
    instructions: &str,
    global_bytes: usize,
    tools_enabled: bool,
) -> Result<NextTurnParts, AppError> {
    let history = active_history(journal, data_root, tools_enabled);
    let extra = if tools_enabled {
        TOOL_SCHEMA_ESTIMATE_BYTES
    } else {
        0
    };
    let parts = estimate_history_parts(BASE_PROMPT.len(), instructions, &history, extra)
        .map_err(|error| AppError::new(ErrorKind::Context, error.to_string()))?;
    let total = parts
        .total()
        .map_err(|error| AppError::new(ErrorKind::Context, error.to_string()))?;
    let overhead = u64::try_from(MESSAGE_OVERHEAD).unwrap_or(u64::MAX);
    let has_summary = journal.snapshot().compaction.is_some() && !history.is_empty();
    let summary_bytes = history
        .first()
        .filter(|_| has_summary)
        .map_or(0, |message| {
            u64::try_from(message.content.len()).unwrap_or(u64::MAX)
        });
    let kept = u64::try_from(history.len())
        .unwrap_or(u64::MAX)
        .saturating_sub(u64::from(has_summary));
    let global = u64::try_from(global_bytes)
        .unwrap_or(u64::MAX)
        .min(parts.custom_instructions);
    Ok(NextTurnParts {
        system: parts.base_prompt.saturating_add(overhead),
        global_instructions: global,
        workspace_instructions: parts.custom_instructions - global,
        tools: parts.fixed_extra,
        summary: has_summary.then(|| summary_bytes.saturating_add(overhead)),
        history_messages: kept,
        history: parts
            .history_content
            .saturating_sub(summary_bytes)
            .saturating_add(kept.saturating_mul(overhead)),
        framing: parts.instructions_label.saturating_add(overhead),
        total,
    })
}

/// The input-only estimate and any assembly limit that compaction can relieve.
fn estimate_with_user(
    journal: &SessionJournal,
    turn: &Turn<'_>,
    fixed_extra_bytes: usize,
    tools_enabled: bool,
) -> Result<(u64, Option<PromptError>), AppError> {
    let mut history = active_history(journal, turn.data_root, tools_enabled);
    history.push(ConversationMessage {
        role: ConversationRole::User,
        content: turn.message.to_owned(),
    });
    let estimate = estimate_history_input_tokens(
        BASE_PROMPT.len(),
        turn.instructions,
        &history,
        fixed_extra_bytes,
    )
    .map_err(|error| AppError::new(ErrorKind::Context, error.to_string()))?;
    let assembly_error =
        match prompt::assemble_with_skills(turn.instructions, turn.skills, &history) {
            Ok(_) if tools_enabled => prompt::check_tool_turn_history_capacity(
                history.len(),
                turn.settings.max_tool_calls,
            )
            .err(),
            Err(
                error
                @ (PromptError::TooManyHistoryMessages | PromptError::AssembledPromptTooLarge),
            ) => Some(error),
            _ => None,
        };
    Ok((estimate, assembly_error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChatSettings;
    use omgcode_core::session::{CompactionRecord, MessageRole};
    use omgcode_core::turn::CancelToken;
    use sha2::{Digest, Sha256};

    const SUMMARY: &str = "Earlier the user asked about parsing.";

    /// A journal with two finished exchanges and, when `compacted`, a
    /// compaction that covers the first one.
    fn journal(data_root: &Path, compacted: bool) -> SessionJournal {
        journal_with(data_root, compacted, false)
    }

    /// As [`journal`], with a compaction summary that records whether it was
    /// built over saved tool results.
    fn journal_with(
        data_root: &Path,
        compacted: bool,
        includes_tool_results: bool,
    ) -> SessionJournal {
        let workspace = data_root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let mut journal = SessionJournal::create(data_root, &workspace, "parts").expect("create");
        for (question, answer) in [("first question", "first answer"), ("second", "reply two")] {
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
                    includes_tool_results,
                    summary_sha256: digest,
                    model: "fixture".to_owned(),
                    prompt_tokens: None,
                    completion_tokens: None,
                })
                .expect("compact");
        }
        journal
    }

    /// The parts add up to the trigger's own estimate, once the new message
    /// the trigger counts is added (its content and one message overhead).
    fn assert_parts_match_preflight(compacted: bool, tools_enabled: bool) {
        let directory = tempfile::tempdir().expect("data root");
        let journal = journal(directory.path(), compacted);
        let instructions = "Global instructions:\nbe brief\n\nWorkspace instructions:\nuse tabs";
        let message = "what next?";
        let settings = ChatSettings::new("http://127.0.0.1:1/v1", "model", 8192);
        let cancel = CancelToken::default();
        let turn = Turn {
            message,
            instructions,
            settings: &settings,
            token: None,
            data_root: directory.path(),
            cancel: &cancel,
            skills: &[],
        };
        let extra = if tools_enabled {
            TOOL_SCHEMA_ESTIMATE_BYTES
        } else {
            0
        };
        let (preflight, _) =
            estimate_with_user(&journal, &turn, extra, tools_enabled).expect("preflight");

        let global = "Global instructions:\nbe brief".len();
        let parts = estimate_next_turn_parts(
            &journal,
            directory.path(),
            instructions,
            global,
            tools_enabled,
        )
        .expect("parts");

        let summed = parts.system
            + parts.global_instructions
            + parts.workspace_instructions
            + parts.tools
            + parts.summary.unwrap_or(0)
            + parts.history
            + parts.framing;
        assert_eq!(summed, parts.total, "the parts sum to their own total");
        let new_message = u64::try_from(message.len()).expect("length")
            + u64::try_from(MESSAGE_OVERHEAD).expect("overhead");
        assert_eq!(parts.total + new_message, preflight);
        assert_eq!(parts.summary.is_some(), compacted);
        assert_eq!(parts.history_messages, if compacted { 2 } else { 4 });
        assert_eq!(parts.tools > 0, tools_enabled);
        assert_eq!(
            parts.global_instructions + parts.workspace_instructions,
            u64::try_from(instructions.len()).expect("length")
        );
    }

    #[test]
    fn the_parts_sum_to_the_preflight_estimate_without_a_compaction() {
        assert_parts_match_preflight(false, false);
    }

    #[test]
    fn the_parts_sum_to_the_preflight_estimate_with_a_compaction_summary() {
        assert_parts_match_preflight(true, false);
    }

    #[test]
    fn the_tool_definitions_are_a_part_only_when_tools_are_offered() {
        assert_parts_match_preflight(false, true);
        assert_parts_match_preflight(true, true);
    }

    /// Without live tool authority a summary built over tool results is sent
    /// as the withheld notice, which stays the first history message. The
    /// Summary part is the notice, and every retained message stays in
    /// History.
    #[test]
    fn a_withheld_summary_is_counted_as_its_notice_and_keeps_every_retained_message() {
        let directory = tempfile::tempdir().expect("data root");
        let journal = journal_with(directory.path(), true, true);
        let parts =
            estimate_next_turn_parts(&journal, directory.path(), "", 0, false).expect("parts");
        let overhead = u64::try_from(MESSAGE_OVERHEAD).expect("overhead");
        let notice =
            u64::try_from(omgcode_core::turn::WITHHELD_COMPACTION_SUMMARY.len()).expect("length");
        assert_eq!(parts.summary, Some(notice + overhead));
        assert_eq!(parts.history_messages, 2);
        let retained = u64::try_from("second".len() + "reply two".len()).expect("length");
        assert_eq!(parts.history, retained + 2 * overhead);
    }
}
