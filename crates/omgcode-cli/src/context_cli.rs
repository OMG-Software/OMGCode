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
use std::path::Path;

use crate::{CliError, model_tools, redact_known_secret};

/// Most output tokens reserved for one summary request.
const MAX_SUMMARY_OUTPUT_TOKENS: u64 = 2048;

/// Output reserve for an automatic summary request: a quarter of the context,
/// at most 2048 tokens and at least one.
pub(crate) fn summary_output_tokens(context_tokens: u64) -> u32 {
    u32::try_from((context_tokens / 4).clamp(1, MAX_SUMMARY_OUTPUT_TOKENS)).unwrap_or(1)
}

/// Compact the active context with the selected local model.
///
/// `summary_budget` sizes the summarizer chunks; its `max_output_tokens` must
/// be the summary client's output reserve. Live `model_tools` and `read`
/// settings for the session's workspace are read before the summarizer input
/// is built and again before each request that carries tool-derived content;
/// a withdrawal stops compaction with a `permission` error and records nothing.
pub(crate) fn compact_journal(
    journal: &mut SessionJournal,
    data_root: &Path,
    client: &LocalModelClient,
    token: Option<&str>,
    summary_budget: ContextBudget,
) -> Result<CompactionOutcome, CliError> {
    let model = client.model().to_owned();
    let workspace = journal.snapshot().workspace_id;
    let schema_version = journal.schema_version();
    compact(
        journal,
        CompactionLimits {
            max_chunk_bytes: summary_budget.summarizer_chunk_bytes(),
            max_summary_bytes: MAX_SUMMARY_BYTES,
        },
        model,
        // Unreadable settings fail closed: no tool data reaches the summarizer.
        || model_tools::tool_replay_allowed(data_root, workspace, schema_version).unwrap_or(false),
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
    .map_err(|error| match error {
        ContextError::AuthorityWithdrawn => CliError::new("permission", error.to_string()),
        error => CliError::new("context", error.to_string()),
    })
}
