//! The tool-enabled chat turn.
//!
//! Authority comes from the live settings preference, rechecked by the core
//! runtime before every tool call, and each call stays inside the selected
//! workspace. The edit and command tools also need a trusted single-use
//! approval, which the core checks; nothing here grants or widens it.

use std::time::Duration;

use omgcode_core::policy::WorkspaceId;
use omgcode_core::prompt::ModelMessage;
use omgcode_core::provider::{LocalModelClient, LocalToolModel, ProviderError};
use omgcode_core::session::{RequestManifest, SessionJournal};
use omgcode_core::turn::{
    ModelError, ToolModel, ToolModelReply, ToolModelRequest, TurnError, TurnLimits, TurnReport,
    TurnRuntime,
};
use serde_json::Value;

use super::output::{ToolTurnOutput, ToolTurnReply, ToolTurnSummary};
use super::{ChatHooks, Turn};
use crate::context::ContextMeter;
use crate::error::{AppError, ErrorKind};
use crate::secrets::redact_known_secret;

/// Cooperative deadline for one tool-enabled chat turn.
///
/// Each local model request may take up to 30 seconds, and a turn makes at most
/// four requests, so the 30-second library default would stop ordinary turns.
const CHAT_TURN_DEADLINE: Duration = Duration::from_secs(120);

/// Runtime limits for tool-enabled chat: library maxima with a chat deadline.
fn chat_limits() -> TurnLimits {
    TurnLimits {
        deadline: CHAT_TURN_DEADLINE,
        ..TurnLimits::default()
    }
}

/// Run one tool-enabled turn through the core runtime.
///
/// `hooks.on_cancel_token` is called once, right after the runtime is built
/// and before the first model request.
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
    let mut model = RedactingModel {
        inner: LocalToolModel::new(client),
        token,
        secret_in_tool_call: false,
    };
    // Tool results are recorded before the next model request, so they need
    // their own redaction; the model wrapper sees them only afterwards.
    let redact_tool_text = |text: &str| redact_known_secret(text, token);
    let report: TurnReport = TurnRuntime::new(journal, turn.data_root, workspace_id, chat_limits())
        .and_then(|runtime| {
            if let Some(hook) = hooks.on_cancel_token.as_deref_mut() {
                hook(runtime.cancel_token());
            }
            runtime.with_tool_text_filter(&redact_tool_text).run(
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
struct RedactingModel<'a> {
    inner: LocalToolModel<'a>,
    token: Option<&'a str>,
    secret_in_tool_call: bool,
}

impl ToolModel for RedactingModel<'_> {
    fn complete(&mut self, request: ToolModelRequest<'_>) -> Result<ToolModelReply, ModelError> {
        let messages: Vec<ModelMessage> = request
            .messages
            .iter()
            .map(|message| ModelMessage {
                role: message.role,
                content: redact_known_secret(&message.content, self.token),
            })
            .collect();
        let mut reply = self.inner.complete(ToolModelRequest {
            messages: &messages,
            tools: request.tools,
        })?;
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
        other => AppError::new(ErrorKind::Turn, other.to_string()),
    }
}

/// The message for a session whose previous turn never reached a terminal event.
pub(super) const ACTIVE_TURN_MESSAGE: &str = "The previous turn is incomplete; inspect it, then use session interrupt to mark it failed without retrying";

#[cfg(test)]
mod tests {
    use omgcode_core::session::SessionError;
    use omgcode_core::turn::TurnLimit;

    use super::*;

    fn pair(error: AppError) -> (&'static str, String) {
        (error.code(), error.into_message())
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
            pair(turn_error(TurnError::Cancelled, None)),
            ("turn", "turn was cancelled".to_owned())
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
