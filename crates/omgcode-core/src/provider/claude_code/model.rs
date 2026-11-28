//! The Claude Code connection as an ordinary [`ToolModel`] (ADR-0109).
//!
//! Claude Code runs its own model loop. The adapter maps that loop onto the
//! turn runtime's rounds: each `tools/call` the relay forwards is returned
//! as a tool call, and its MCP request stays open while the runtime checks
//! the workspace policy, waits for any approval, dispatches, and journals
//! the call. The next round answers each held request with the result text
//! the runtime recorded, matched by call identifier. The process's final
//! `result` ends the turn. No tool runs outside the runtime's dispatcher.

use serde_json::{Value, json};

use super::ClaudeCodeFailure;
use super::launch::{ClaudeCodeClient, Progress, Session, failure};
use crate::prompt::{ModelMessage, ModelRole};
use crate::provider::ProviderError;
use crate::turn::{CancelToken, ModelError, ToolModel, ToolModelReply, ToolModelRequest};

/// One turn's Claude Code process, started by the first round.
pub struct ClaudeCodeModel<'a> {
    client: &'a ClaudeCodeClient,
    on_text: Option<&'a mut dyn FnMut(&str)>,
    session: Option<Session>,
    last_error: Option<ProviderError>,
    generated_ids: u64,
}

impl<'a> ClaudeCodeModel<'a> {
    /// A model whose provisional text is not forwarded.
    pub const fn new(client: &'a ClaudeCodeClient) -> Self {
        Self {
            client,
            on_text: None,
            session: None,
            last_error: None,
            generated_ids: 0,
        }
    }

    /// A model that forwards the main agent's text deltas to `on_text`.
    pub fn streaming(client: &'a ClaudeCodeClient, on_text: &'a mut dyn FnMut(&str)) -> Self {
        Self {
            on_text: Some(on_text),
            ..Self::new(client)
        }
    }

    /// The failure behind the last [`ModelError`], for its specific code.
    pub const fn last_error(&self) -> Option<ProviderError> {
        self.last_error
    }

    fn step(&mut self, request: &ToolModelRequest<'_>) -> Result<ToolModelReply, ProviderError> {
        // Neither a process nor a held answer may start once the turn is
        // cancelled; the caller drops any running session on this error.
        if request.cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        match self.session.as_mut() {
            None => {
                self.session = Some(self.client.start(request.messages, request.tools)?);
            }
            Some(session) => answer_held(session, request.messages, request.cancel)?,
        }
        let on_text = self
            .on_text
            .as_mut()
            .map(|on_text| &mut **on_text as &mut dyn FnMut(&str));
        let session = self
            .session
            .as_mut()
            .ok_or(failure(ClaudeCodeFailure::Failed))?;
        match session.run(request.cancel, on_text)? {
            Progress::Finished(text) => {
                let finished = session.finish();
                self.session = None;
                finished?;
                Ok(ToolModelReply {
                    content: text,
                    tool_calls: Vec::new(),
                    finish_reason: "stop".to_owned(),
                })
            }
            Progress::Calls(calls) => {
                let content = session.take_text();
                let supplied: Vec<String> = calls
                    .iter()
                    .filter_map(|call| call.tool_use_id.clone())
                    .collect();
                let mut held: Vec<(String, _)> = Vec::with_capacity(calls.len());
                let mut tool_calls = Vec::with_capacity(calls.len());
                for call in calls {
                    // A generated identifier skips every identifier the
                    // process supplied in this round and every one already
                    // given, so it never makes the round's calls ambiguous.
                    let id = match &call.tool_use_id {
                        Some(id) => id.clone(),
                        None => loop {
                            self.generated_ids = self.generated_ids.saturating_add(1);
                            let candidate = format!("omgcode_call_{}", self.generated_ids);
                            if !supplied.contains(&candidate)
                                && !held.iter().any(|(given, _)| *given == candidate)
                            {
                                break candidate;
                            }
                        },
                    };
                    tool_calls.push(tool_call(&id, &call.name, &call.arguments_json));
                    held.push((id, call));
                }
                session.hold(held);
                Ok(ToolModelReply {
                    content,
                    tool_calls,
                    finish_reason: "tool_calls".to_owned(),
                })
            }
        }
    }
}

impl ToolModel for ClaudeCodeModel<'_> {
    fn complete(&mut self, request: ToolModelRequest<'_>) -> Result<ToolModelReply, ModelError> {
        self.step(&request).map_err(|error| {
            self.last_error = Some(error);
            // A failed round leaves nothing to resume: end the process now.
            self.session = None;
            crate::provider::model_error(error)
        })
    }
}

/// Answer every call the last round held with the matching result among the
/// messages after the last assistant or user message.
fn answer_held(
    session: &mut Session,
    messages: &[ModelMessage],
    cancel: &CancelToken,
) -> Result<(), ProviderError> {
    let trailing = messages
        .iter()
        .rposition(|message| !matches!(message.role, ModelRole::ToolResult(_)))
        .map_or(messages, |index| {
            messages.get(index + 1..).unwrap_or_default()
        });
    let held = session.held().to_vec();
    if held.is_empty() {
        return Err(failure(ClaudeCodeFailure::Protocol));
    }
    for (id, call) in &held {
        let text = trailing
            .iter()
            .find_map(|message| match &message.role {
                ModelRole::ToolResult(answered) if answered.call_id() == id => {
                    Some(message.content.as_str())
                }
                _ => None,
            })
            .ok_or(failure(ClaudeCodeFailure::Protocol))?;
        session.answer(call, text, cancel)?;
    }
    session.hold(Vec::new());
    Ok(())
}

fn tool_call(id: &str, name: &str, arguments: &str) -> Value {
    json!({
        "id": id,
        "type": "function",
        "function": { "name": name, "arguments": arguments },
    })
}
