//! The pre-turn context trigger and visible meter.

use std::path::Path;

use omgcode_core::context::{ContextBudget, estimate_history_input_tokens};
use omgcode_core::prompt::{self, BASE_PROMPT, ConversationMessage, ConversationRole, PromptError};
use omgcode_core::provider::LocalModelClient;
use omgcode_core::session::SessionJournal;
use omgcode_core::turn::replay_active;

use super::Turn;
use crate::context::{ContextMeter, compact_journal, summary_output_tokens};
use crate::error::{AppError, ErrorKind};
use crate::tool_mode::tool_replay_allowed;

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

/// The input-only estimate and any assembly limit that compaction can relieve.
fn estimate_with_user(
    journal: &SessionJournal,
    turn: &Turn<'_>,
    fixed_extra_bytes: usize,
    tools_enabled: bool,
) -> Result<(u64, Option<PromptError>), AppError> {
    // Estimate the history this mode will send: saved tool results, and a
    // summary built from them, are withheld without live tool authority.
    let (mut history, _) = replay_active(
        journal,
        replay_allowed_now(journal, turn.data_root, tools_enabled),
    );
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
