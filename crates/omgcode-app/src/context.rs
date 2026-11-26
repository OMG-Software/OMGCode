//! Bounded conversation compaction and the visible context meter.
//!
//! The core owns the cutoff, journal event, and summary validation. This module
//! supplies the local-model summarizer and applies the configured secret
//! redaction before the summary is recorded or shown. Summaries are untrusted
//! evidence and grant no authority.

use std::path::Path;

use omgcode_core::context::{
    COMPACTION_INSTRUCTIONS, CompactionLimits, CompactionOutcome, ContextBudget, ContextError,
    MAX_SUMMARY_BYTES, SummaryReply, compact,
};
use omgcode_core::prompt::{self, ConversationMessage};
use omgcode_core::provider::{LocalModelClient, ProviderError};
use omgcode_core::session::SessionJournal;
use omgcode_core::turn::CancelToken;
use serde::Serialize;

use crate::error::{AppError, ErrorKind};
use crate::secrets::redact_known_secret;
use crate::tool_mode::tool_replay_allowed;

/// Most output tokens reserved for one summary request.
const MAX_SUMMARY_OUTPUT_TOKENS: u64 = 2048;

/// Output reserve for an automatic summary request: a quarter of the context,
/// at most 2048 tokens and at least one.
pub fn summary_output_tokens(context_tokens: u64) -> u32 {
    u32::try_from((context_tokens / 4).clamp(1, MAX_SUMMARY_OUTPUT_TOKENS)).unwrap_or(1)
}

/// Compact the active context with the selected model connection.
///
/// `summary_budget` sizes the summarizer chunks; its `max_output_tokens` must
/// be the summary client's output reserve. Live `model_tools` and `read`
/// settings for the session's workspace are read before the summarizer input
/// is built and again before each request that carries tool-derived content;
/// a withdrawal stops compaction with a `permission` error and records nothing.
/// Unreadable settings fail closed: no tool data reaches the summarizer.
/// `cancel` stops a summarizer request in progress and prevents the next one
/// (ADR-0051); a cancelled compaction is `cancelled` and records nothing,
/// because the journal is written only after the last summary. Other failures
/// are `context`. `token` is removed from summarizer input and output.
pub fn compact_journal(
    journal: &mut SessionJournal,
    data_root: &Path,
    client: &LocalModelClient,
    token: Option<&str>,
    summary_budget: ContextBudget,
    cancel: &CancelToken,
) -> Result<CompactionOutcome, AppError> {
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
        || tool_replay_allowed(data_root, workspace, schema_version).unwrap_or(false),
        |history: &[ConversationMessage]| {
            let redacted: Vec<ConversationMessage> = history
                .iter()
                .map(|message| ConversationMessage {
                    role: message.role.clone(),
                    content: redact_known_secret(&message.content, token),
                })
                .collect();
            let messages = prompt::assemble(COMPACTION_INSTRUCTIONS, &redacted)
                .map_err(|_| ContextError::TooLarge)?;
            let reply = client
                .complete(&messages, cancel)
                .map_err(ContextError::Provider)?;
            let usage = reply.usage.as_ref();
            Ok(SummaryReply {
                text: redact_known_secret(&reply.content, token),
                prompt_tokens: usage.and_then(|usage| usage.prompt_tokens),
                completion_tokens: usage.and_then(|usage| usage.completion_tokens),
            })
        },
    )
    .map_err(|error| match error {
        ContextError::AuthorityWithdrawn => AppError::new(ErrorKind::Permission, error.to_string()),
        // Only a set token makes this a cancellation, as in the runtime.
        ContextError::Provider(ProviderError::Cancelled) if cancel.is_cancelled() => {
            AppError::new(ErrorKind::Cancelled, COMPACTION_CANCELLED_MESSAGE)
        }
        // A ChatGPT plan failure keeps its specific code, as in a chat turn.
        ContextError::Provider(
            error @ (ProviderError::SignInRequired
            | ProviderError::AccessTokenUnavailable
            | ProviderError::ResponseFailed(_)),
        ) => AppError::provider(error),
        error => AppError::new(ErrorKind::Context, error.to_string()),
    })
}

/// The message of a cancelled compaction, which recorded nothing.
const COMPACTION_CANCELLED_MESSAGE: &str = "Compaction was cancelled; nothing was recorded";

/// Visible, conservative context accounting for one chat turn.
#[derive(Clone, Debug, Serialize)]
pub struct ContextMeter {
    estimated_input_tokens: u64,
    context_tokens: u64,
    max_output_tokens: u32,
    effective_trigger: u64,
    auto_compaction: bool,
    estimate_scope: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    compaction: Option<CompactionMeter>,
}

impl ContextMeter {
    /// Build the meter from a finished pre-turn estimate and the active journal.
    pub(crate) fn new(
        journal: &SessionJournal,
        budget: ContextBudget,
        max_output_tokens: u32,
        auto_compaction: bool,
        estimate: u64,
        summarizer_calls: Option<usize>,
    ) -> Self {
        let compaction = journal
            .snapshot()
            .compaction
            .as_ref()
            .map(|record| CompactionMeter {
                compacted_messages: record.compacted_messages,
                retained_messages: u64::try_from(journal.snapshot().messages.len())
                    .unwrap_or(u64::MAX)
                    .saturating_sub(record.compacted_messages),
                summarizer_calls,
                summary_sha256: record.summary_sha256.clone(),
            });
        Self {
            estimated_input_tokens: estimate,
            context_tokens: budget.context_tokens,
            max_output_tokens,
            effective_trigger: budget.trigger(),
            auto_compaction,
            estimate_scope: "pre_turn_conservative",
            compaction,
        }
    }

    /// The conservative input estimate for the turn, in tokens.
    pub fn estimated_input_tokens(&self) -> u64 {
        self.estimated_input_tokens
    }

    /// The validated context capacity of the selected model.
    pub fn context_tokens(&self) -> u64 {
        self.context_tokens
    }

    /// The output reserve for the turn.
    pub fn max_output_tokens(&self) -> u32 {
        self.max_output_tokens
    }

    /// The estimate at which compaction triggers.
    pub fn effective_trigger(&self) -> u64 {
        self.effective_trigger
    }

    /// Whether compaction before a turn was enabled.
    pub fn auto_compaction(&self) -> bool {
        self.auto_compaction
    }

    /// The fixed label of what the estimate covers.
    pub fn estimate_scope(&self) -> &'static str {
        self.estimate_scope
    }

    /// The active compaction boundary, when the session has one.
    pub fn compaction(&self) -> Option<&CompactionMeter> {
        self.compaction.as_ref()
    }
}

/// The active compaction boundary and summary identity.
#[derive(Clone, Debug, Serialize)]
pub struct CompactionMeter {
    compacted_messages: u64,
    retained_messages: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    summarizer_calls: Option<usize>,
    summary_sha256: String,
}

impl CompactionMeter {
    /// Journal messages now represented by the summary.
    pub fn compacted_messages(&self) -> u64 {
        self.compacted_messages
    }

    /// Journal messages retained verbatim after the boundary.
    pub fn retained_messages(&self) -> u64 {
        self.retained_messages
    }

    /// Summarizer calls made during this turn's preflight, if it compacted.
    pub fn summarizer_calls(&self) -> Option<usize> {
        self.summarizer_calls
    }

    /// Digest of the active summary text.
    pub fn summary_sha256(&self) -> &str {
        &self.summary_sha256
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_reserve_is_a_quarter_of_the_context_within_bounds() {
        assert_eq!(summary_output_tokens(0), 1);
        assert_eq!(summary_output_tokens(3), 1);
        assert_eq!(summary_output_tokens(400), 100);
        assert_eq!(summary_output_tokens(1_000_000), 2048);
    }
}
