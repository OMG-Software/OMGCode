//! The text-only chat turn.
//!
//! Text-only chat has no model file-read authority. The turn is one provider
//! request, which watches the turn's cancel token and can be interrupted
//! whether or not it streams (ADR-0050, ADR-0051).

use omgcode_core::prompt::{self, ConversationMessage, ConversationRole};
use omgcode_core::provider::{LocalModelClient, ProviderError};
use omgcode_core::session::{MessageRole, RequestManifest, SessionError, SessionJournal};
use omgcode_core::turn::replay_active;

use super::output::TextTurnOutput;
use super::stream::StreamSink;
use super::tools::CANCELLED_MESSAGE;
use super::{ChatHooks, Turn};
use crate::context::ContextMeter;
use crate::error::{AppError, ErrorKind};
use crate::secrets::redact_known_secret;

/// Largest assistant text kept resumable within the journal and prompt limits.
const MAX_REPLY_BYTES: usize = 16 * 1024;

/// Run one text-only turn. Deterministic failures return before the turn starts;
/// uncertain storage state after a failed write is preserved, not overwritten.
pub(super) fn run(
    journal: &mut SessionJournal,
    client: &LocalModelClient,
    turn: &Turn<'_>,
    reason: &'static str,
    manifest: RequestManifest,
    context: ContextMeter,
    hooks: &mut ChatHooks<'_>,
) -> Result<TextTurnOutput, AppError> {
    let token = turn.token;
    // Text-only chat has no model file-read authority, so saved tool results,
    // and a summary built from them, are withheld rather than sent again.
    let (mut history, withheld_tool_results) = replay_active(journal, false);
    for message in &mut history {
        message.content = redact_known_secret(&message.content, token);
    }
    history.push(ConversationMessage {
        role: ConversationRole::User,
        content: turn.message.to_owned(),
    });
    let messages = prompt::assemble(turn.instructions, &history)
        .map_err(|error| AppError::new(ErrorKind::Prompt, error.to_string()))?;
    journal
        .preflight_request(turn.message, &manifest)
        .map_err(AppError::session)?;
    let cancel = turn.cancel;
    // As in the runtime, a turn cancelled before it starts records nothing.
    if cancel.is_cancelled() {
        return Err(AppError::new(ErrorKind::Cancelled, CANCELLED_MESSAGE));
    }
    let request_id = journal.start_turn().map_err(AppError::session)?;
    journal
        .record_message(MessageRole::User, turn.message)
        .map_err(AppError::session)?;
    journal
        .record_manifest(request_id, manifest)
        .map_err(AppError::session)?;
    // A streamed request shows provisional redacted text. The journal is
    // computed below from the whole reply, exactly as without streaming.
    let mut sink = hooks
        .on_stream
        .as_deref_mut()
        .map(|on_stream| StreamSink::new(on_stream, token));
    let mut result = match sink.as_mut() {
        Some(sink) => {
            sink.start_request();
            client.complete_streaming(&messages, &mut |piece: &str| sink.text(piece), cancel)
        }
        None => client.complete(&messages, cancel),
    };
    // As in the runtime, a cancellation seen once the reply has arrived still
    // ends the turn as cancelled, recording no reply.
    if result.is_ok() && cancel.is_cancelled() {
        result = Err(ProviderError::Cancelled);
    }
    if let (Ok(_), Some(sink)) = (&result, sink.as_mut()) {
        sink.finish_request();
    }
    let mut reply = match result {
        Ok(reply) => reply,
        Err(ProviderError::Cancelled) => {
            journal.fail_turn(request_id, "cancelled").map_err(|_| {
                AppError::new(
                    ErrorKind::Persistence,
                    "Turn was cancelled and the cancellation could not be recorded",
                )
            })?;
            return Err(AppError::new(ErrorKind::Cancelled, CANCELLED_MESSAGE));
        }
        Err(error) => {
            journal
                .fail_turn(request_id, "provider_error")
                .map_err(|_| {
                    AppError::new(
                        ErrorKind::Persistence,
                        "Provider failed and failure could not be recorded",
                    )
                })?;
            return Err(AppError::provider(error));
        }
    };
    reply.content = redact_known_secret(&reply.content, token);
    // Keep every accepted reply resumable within the journal and prompt limits.
    // The wire response limit also covers protocol metadata, so it is larger.
    if reply.content.len() > MAX_REPLY_BYTES {
        journal
            .fail_turn(request_id, "response_too_large")
            .map_err(AppError::session)?;
        return Err(AppError::new(
            ErrorKind::Provider,
            "Assistant text exceeds the 16 KiB session message limit",
        ));
    }
    if let Err(error) = journal.record_message(MessageRole::Assistant, &reply.content) {
        if matches!(
            error,
            SessionError::LimitReached | SessionError::InvalidValue(_)
        ) {
            journal
                .fail_turn(request_id, "response_too_large")
                .map_err(AppError::session)?;
        }
        // Failed writes/syncs can have changed storage. Do not overwrite that
        // uncertain state with a fabricated terminal event or an automatic retry.
        return Err(AppError::session(error));
    }
    journal
        .complete_turn(request_id)
        .map_err(AppError::session)?;
    Ok(TextTurnOutput {
        reply,
        reason,
        withheld_tool_results,
        context,
    })
}
