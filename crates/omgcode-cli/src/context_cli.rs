//! CLI glue for bounded conversation compaction.
//!
//! The core owns the cutoff, journal event, and summary validation. This module
//! supplies the local-model summarizer and applies the configured secret
//! redaction before the summary is recorded or shown.

use omgcode_core::context::{
    COMPACTION_INSTRUCTIONS, CompactionLimits, CompactionOutcome, ContextBudget, ContextError,
    MAX_SUMMARY_BYTES, SummaryReply, compact,
};
use omgcode_core::prompt::{self, ConversationMessage};
use omgcode_core::provider::LocalModelClient;
use omgcode_core::session::SessionJournal;

use crate::{CliError, redact_known_secret};

/// Compact the active context with the selected local model.
///
/// `tool_results_allowed` must come from live settings
/// ([`crate::model_tools::tool_replay_allowed`]); without it, saved tool
/// results are withheld from the summarizer.
pub(crate) fn compact_journal(
    journal: &mut SessionJournal,
    client: &LocalModelClient,
    token: Option<&str>,
    budget: ContextBudget,
    tool_results_allowed: bool,
) -> Result<CompactionOutcome, CliError> {
    let model = client.model().to_owned();
    compact(
        journal,
        CompactionLimits {
            max_chunk_bytes: budget.summarizer_chunk_bytes(),
            max_summary_bytes: MAX_SUMMARY_BYTES,
        },
        model,
        tool_results_allowed,
        |history: &[ConversationMessage]| {
            let redacted: Vec<ConversationMessage> = history
                .iter()
                .map(|message| ConversationMessage {
                    role: message.role,
                    content: redact_known_secret(&message.content, token),
                })
                .collect();
            let messages = prompt::assemble(COMPACTION_INSTRUCTIONS, &redacted)
                .map_err(|_| ContextError::TooLarge)?;
            let reply = client.complete(&messages).map_err(ContextError::Provider)?;
            let usage = reply.usage.as_ref();
            Ok(SummaryReply {
                text: redact_known_secret(&reply.content, token),
                prompt_tokens: usage.and_then(|usage| usage.prompt_tokens),
                completion_tokens: usage.and_then(|usage| usage.completion_tokens),
            })
        },
    )
    .map_err(|error| CliError::new("context", error.to_string()))
}
