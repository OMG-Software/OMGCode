//! The tool-enabled chat turn.
//!
//! Authority comes from the live settings preference, rechecked by the core
//! runtime before every tool call, and each call stays inside the selected
//! workspace. The edit and command tools also need a trusted single-use
//! approval, which the core checks; nothing here grants or widens it.

use std::cell::RefCell;

use omgcode_core::policy::WorkspaceId;
use omgcode_core::prompt::ModelMessage;
use omgcode_core::provider::{LocalModelClient, LocalToolModel, ProviderError};
use omgcode_core::session::{RequestManifest, SessionJournal};
use omgcode_core::turn::{
    MAX_TURN_DEADLINE, ModelError, ToolModel, ToolModelReply, ToolModelRequest, TurnError,
    TurnLimit, TurnLimits, TurnReport, TurnRuntime,
};
use serde_json::Value;

use super::output::{ToolTurnOutput, ToolTurnReply, ToolTurnSummary};
use super::skills::LoadedSkillSummary;
use super::stream::StreamSink;
use super::{ChatHooks, Turn};
use crate::context::ContextMeter;
use crate::error::{AppError, ErrorKind};
use crate::secrets::redact_known_secret;

/// Runtime limits for a validated chat budget, with room for the final reply.
fn chat_limits(max_tool_calls: usize) -> TurnLimits {
    TurnLimits {
        max_rounds: max_tool_calls + 1,
        max_tool_calls,
        // Several model requests share the turn budget, and each retains its
        // own shorter timeout. The core caps the cooperative deadline.
        deadline: MAX_TURN_DEADLINE,
        ..TurnLimits::default()
    }
}

/// Run one tool-enabled turn through the core runtime, which uses the turn's
/// cancel token for its checkpoints and every model request.
pub(super) fn run(
    journal: &mut SessionJournal,
    workspace_id: WorkspaceId,
    client: &LocalModelClient,
    turn: &Turn<'_>,
    manifest: RequestManifest,
    context: ContextMeter,
    hooks: &mut ChatHooks<'_>,
) -> Result<ToolTurnOutput, AppError> {
    let token = turn.token;
    let approval_wait = hooks.approval_wait.clone();
    // Streaming is on only when the front end asked for provisional text. One
    // sink serves every request of the turn; each request restarts it.
    let sink = hooks
        .on_stream
        .as_deref_mut()
        .map(|on_stream| RefCell::new(StreamSink::new(on_stream, token)));
    let mut forward = |piece: &str| {
        if let Some(sink) = &sink {
            // The sink is borrowed only for the span of one call and its
            // callback cannot reach this cell, so a failed borrow cannot
            // happen. If it did, dropping a provisional piece is harmless.
            if let Ok(mut sink) = sink.try_borrow_mut() {
                sink.text(piece);
            }
        }
    };
    let inner = if sink.is_some() {
        LocalToolModel::streaming(client, &mut forward)
    } else {
        LocalToolModel::new(client)
    };
    let mut model = RedactingModel {
        inner,
        token,
        stream: sink.as_ref(),
        secret_in_tool_call: false,
    };
    // Tool results are recorded before the next model request, so they need
    // their own redaction; the model wrapper sees them only afterwards.
    let redact_tool_text = |text: &str| redact_known_secret(text, token);
    let limits = chat_limits(turn.settings.max_tool_calls);
    let report: TurnReport = TurnRuntime::new(journal, turn.data_root, workspace_id, limits)
        .and_then(|runtime| {
            let runtime = runtime
                .with_cancel_token(turn.cancel.clone())
                .with_tool_text_filter(&redact_tool_text)
                .with_skills(turn.skills.to_vec());
            let mut runtime = match approval_wait {
                Some(slot) => runtime.with_approval_wait(slot),
                None => runtime,
            };
            runtime.run(
                &mut model,
                turn.message,
                turn.instructions,
                manifest,
            )
        })
        .map_err(|error| {
            if model.secret_in_tool_call {
                return AppError::new(
                    ErrorKind::Provider,
                    "The model returned a tool call containing the configured server token; it was not recorded or run",
                );
            }
            turn_error(error, model.inner.last_error())
        })?;
    Ok(ToolTurnOutput {
        skills: LoadedSkillSummary::from_prompts(turn.skills),
        reply: ToolTurnReply {
            content: report.reply,
            finish_reason: report.finish_reason,
        },
        model_tools: ToolTurnSummary {
            mode: "enabled",
            workspace_id: workspace_id.to_string(),
            rounds: report.rounds,
            tool_calls: report.tool_calls,
            tool_bytes: report.tool_bytes,
            withheld_tool_results: report.withheld_tool_results,
        },
        context,
    })
}

/// Remove a known local server token from text sent to and received from the model.
///
/// A tool call that contains the token is refused rather than redacted, because
/// editing its arguments could change the requested path. The runtime records
/// calls before any result filter runs, so the refusal happens here.
struct RedactingModel<'a, 's> {
    inner: LocalToolModel<'a>,
    token: Option<&'a str>,
    /// The turn's stream sink, shared with `inner`'s text callback.
    stream: Option<&'a RefCell<StreamSink<'s>>>,
    secret_in_tool_call: bool,
}

impl RedactingModel<'_, '_> {
    /// Run `step` on the stream sink, when the turn streams.
    fn with_stream(&self, step: impl for<'x> FnOnce(&mut StreamSink<'x>)) {
        // Borrowed only for this call, which no callback can re-enter.
        if let Some(Ok(mut sink)) = self.stream.map(RefCell::try_borrow_mut) {
            step(&mut sink);
        }
    }
}

impl ToolModel for RedactingModel<'_, '_> {
    fn complete(&mut self, request: ToolModelRequest<'_>) -> Result<ToolModelReply, ModelError> {
        let messages: Vec<ModelMessage> = request
            .messages
            .iter()
            .map(|message| ModelMessage {
                role: message.role,
                content: redact_known_secret(&message.content, self.token),
            })
            .collect();
        self.with_stream(|sink| sink.start_request());
        let result = self.inner.complete(ToolModelRequest {
            messages: &messages,
            tools: request.tools,
            cancel: request.cancel,
        });
        if result.is_ok() {
            self.with_stream(|sink| sink.finish_request());
        }
        let mut reply = result?;
        if let Some(token) = self.token.filter(|token| !token.is_empty())
            && reply
                .tool_calls
                .iter()
                .any(|call| contains_secret(call, token, true))
        {
            self.secret_in_tool_call = true;
            return Err(ModelError::InvalidResponse);
        }
        reply.content = redact_known_secret(&reply.content, self.token);
        Ok(reply)
    }
}

/// Report whether any key or string in a raw tool-call value contains `token`.
///
/// Tool-call arguments arrive as a JSON-encoded string, so a string that parses
/// as JSON is searched once more after decoding. Decoding is not repeated
/// inside that nested value.
fn contains_secret(value: &Value, token: &str, decode_nested: bool) -> bool {
    match value {
        Value::String(text) => {
            text.contains(token)
                || (decode_nested
                    && serde_json::from_str::<Value>(text)
                        .is_ok_and(|nested| contains_secret(&nested, token, false)))
        }
        Value::Array(items) => items
            .iter()
            .any(|item| contains_secret(item, token, decode_nested)),
        Value::Object(fields) => fields
            .iter()
            .any(|(key, item)| key.contains(token) || contains_secret(item, token, decode_nested)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

/// Map a runtime failure to a stable application error without echoing model text.
fn turn_error(error: TurnError, provider: Option<ProviderError>) -> AppError {
    match error {
        TurnError::Model(_) => provider.map_or_else(
            || AppError::new(ErrorKind::Provider, "Local model request failed"),
            AppError::provider,
        ),
        TurnError::ToolDenied(_) => AppError::new(
            ErrorKind::Permission,
            "A model tool call was denied by the current workspace preferences",
        ),
        TurnError::InvalidToolCall(error) => AppError::new(
            ErrorKind::ToolCall,
            format!("The model returned an invalid tool call: {error}"),
        ),
        TurnError::Limit(limit @ TurnLimit::ToolCalls { .. }) => AppError::new(
            ErrorKind::ToolLimit,
            format!("{limit}. Start a new turn or increase the limit."),
        ),
        TurnError::Limit(limit) => AppError::new(
            ErrorKind::ToolLimit,
            format!("Turn limit exceeded: {limit}"),
        ),
        TurnError::Session(error) => AppError::session(error),
        TurnError::Prompt(error) => AppError::new(ErrorKind::Prompt, error.to_string()),
        TurnError::Archived => AppError::new(
            ErrorKind::Session,
            "Restore the archived session before chatting",
        ),
        TurnError::ActiveTurn => AppError::new(ErrorKind::Session, ACTIVE_TURN_MESSAGE),
        TurnError::Cancelled => AppError::new(ErrorKind::Cancelled, CANCELLED_MESSAGE),
        TurnError::SkillWithdrawn => AppError::skill(
            "skill_withdrawn",
            "A selected skill was disabled, removed, or changed during the turn; the turn ended",
        ),
        other => AppError::new(ErrorKind::Turn, other.to_string()),
    }
}

/// The message of a cancelled turn, on the text-only and the tool-enabled path.
pub(super) const CANCELLED_MESSAGE: &str = "turn was cancelled";

/// The message for a session whose previous turn never reached a terminal event.
pub(super) const ACTIVE_TURN_MESSAGE: &str = "The previous turn is incomplete; inspect it, then use session interrupt to mark it failed without retrying";

#[cfg(test)]
mod tests {
    use omgcode_core::session::SessionError;

    use super::*;

    fn pair(error: AppError) -> (&'static str, String) {
        (error.code(), error.into_message())
    }

    #[test]
    fn chat_reserves_a_final_request_for_each_valid_tool_call_budget() {
        use crate::chat::{ChatClients, ChatSettings};

        let mut settings = ChatSettings::new("http://127.0.0.1:1234/v1", "model", 8192);
        assert_eq!(settings.max_tool_calls, 128);
        for limit in [1, 128, 256] {
            settings.max_tool_calls = limit;
            assert!(ChatClients::new(&settings, None).is_ok());
            let limits = chat_limits(limit);
            assert_eq!(limits.max_tool_calls, limit);
            assert_eq!(limits.max_rounds, limit + 1);
        }
        for limit in [0, 257] {
            settings.max_tool_calls = limit;
            let error = ChatClients::new(&settings, None)
                .err()
                .expect("invalid budget");
            assert_eq!(
                pair(error),
                (
                    "configuration",
                    "Tool calls per turn must be between 1 and 256.".to_owned()
                )
            );
        }
    }

    #[test]
    fn model_failures_keep_the_provider_message_or_the_generic_one() {
        assert_eq!(
            pair(turn_error(
                TurnError::Model(ModelError::InvalidResponse),
                Some(ProviderError::Transport)
            )),
            ("provider", "local model transport failed".to_owned())
        );
        assert_eq!(
            pair(turn_error(
                TurnError::Model(ModelError::InvalidResponse),
                None
            )),
            ("provider", "Local model request failed".to_owned())
        );
    }

    #[test]
    fn runtime_failures_map_to_the_published_codes_and_messages() {
        assert_eq!(
            pair(turn_error(TurnError::Archived, None)),
            (
                "session",
                "Restore the archived session before chatting".to_owned()
            )
        );
        assert_eq!(
            pair(turn_error(TurnError::ActiveTurn, None)),
            ("session", ACTIVE_TURN_MESSAGE.to_owned())
        );
        assert_eq!(
            pair(turn_error(TurnError::Limit(TurnLimit::Rounds), None)),
            (
                "tool_limit",
                "Turn limit exceeded: model round limit".to_owned()
            )
        );
        assert_eq!(
            pair(turn_error(TurnError::Limit(TurnLimit::ToolCalls {
                limit: 128, used: 127, requested: 2,
            }), None)),
            ("tool_limit", "The latest response would exceed the per-turn limit of 128 tool calls (127 used, 2 requested). Skipped all calls in that response. Start a new turn or increase the limit.".to_owned())
        );
        assert_eq!(
            pair(turn_error(TurnError::Cancelled, None)),
            ("cancelled", "turn was cancelled".to_owned())
        );
        assert_eq!(
            pair(turn_error(
                TurnError::Session(SessionError::LimitReached),
                None
            )),
            ("session", SessionError::LimitReached.to_string())
        );
    }

    #[test]
    fn secrets_are_found_in_keys_values_and_nested_arguments() {
        let call = serde_json::json!({
            "id": "c1",
            "function": {"name": "read", "arguments": "{\"path\":\"has-\\u0073ecret-9\"}"}
        });
        assert!(contains_secret(&call, "secret-9", true));
        assert!(!contains_secret(&call, "secret-9", false));
        assert!(!contains_secret(&call, "other", true));
    }
}
