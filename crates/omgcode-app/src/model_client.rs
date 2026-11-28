//! The model client a turn uses: the OpenAI-compatible HTTP client, or a
//! recorded Claude Code connection that runs the signed-in `claude` program
//! (ADR-0109).
//!
//! Both answer the same three questions a turn asks, one text reply, one
//! streamed text reply, and a tool model for the core runtime, so chat,
//! tool turns, and compaction need no other branch. Neither grants
//! authority: every tool call still goes through the core runtime's policy
//! checks, approvals, and journals.

use omgcode_core::prompt::ModelMessage;
use omgcode_core::provider::claude_code::{ClaudeCodeClient, ClaudeCodeModel};
use omgcode_core::provider::{
    LocalModelClient, LocalToolModel, ModelReply, ProviderError, ReasoningEffort,
};
use omgcode_core::turn::{CancelToken, ModelError, ToolModel, ToolModelReply, ToolModelRequest};

/// One validated model client.
#[derive(Debug)]
pub enum ModelClient {
    /// An OpenAI-compatible endpoint, local or hosted, or a ChatGPT plan.
    Http(LocalModelClient),
    /// A recorded `claude_code` connection.
    ClaudeCode(ClaudeCodeClient),
}

impl From<LocalModelClient> for ModelClient {
    fn from(client: LocalModelClient) -> Self {
        Self::Http(client)
    }
}

impl From<ClaudeCodeClient> for ModelClient {
    fn from(client: ClaudeCodeClient) -> Self {
        Self::ClaudeCode(client)
    }
}

impl ModelClient {
    /// The model name every request names.
    #[must_use]
    pub fn model(&self) -> &str {
        match self {
            Self::Http(client) => client.model(),
            Self::ClaudeCode(client) => client.model(),
        }
    }

    /// The reasoning effort every chat request sends, if any.
    #[must_use]
    pub const fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        match self {
            Self::Http(client) => client.reasoning_effort(),
            Self::ClaudeCode(client) => client.reasoning_effort(),
        }
    }

    /// One text reply.
    ///
    /// # Errors
    ///
    /// The client's [`ProviderError`].
    pub fn complete(
        &self,
        messages: &[ModelMessage],
        cancel: &CancelToken,
    ) -> Result<ModelReply, ProviderError> {
        match self {
            Self::Http(client) => client.complete(messages, cancel),
            Self::ClaudeCode(client) => client.complete(messages, cancel),
        }
    }

    /// One text reply whose provisional pieces go to `on_text`.
    ///
    /// # Errors
    ///
    /// The client's [`ProviderError`].
    pub fn complete_streaming(
        &self,
        messages: &[ModelMessage],
        on_text: &mut dyn FnMut(&str),
        cancel: &CancelToken,
    ) -> Result<ModelReply, ProviderError> {
        match self {
            Self::Http(client) => client.complete_streaming(messages, on_text, cancel),
            Self::ClaudeCode(client) => client.complete_streaming(messages, on_text, cancel),
        }
    }

    /// The tool model of one turn, streaming provisional text to `on_text`
    /// when given.
    pub(crate) fn tool_model<'a>(
        &'a self,
        on_text: Option<&'a mut dyn FnMut(&str)>,
    ) -> TurnModel<'a> {
        match (self, on_text) {
            (Self::Http(client), Some(on_text)) => {
                TurnModel::Http(LocalToolModel::streaming(client, on_text))
            }
            (Self::Http(client), None) => TurnModel::Http(LocalToolModel::new(client)),
            (Self::ClaudeCode(client), Some(on_text)) => {
                TurnModel::ClaudeCode(Box::new(ClaudeCodeModel::streaming(client, on_text)))
            }
            (Self::ClaudeCode(client), None) => {
                TurnModel::ClaudeCode(Box::new(ClaudeCodeModel::new(client)))
            }
        }
    }
}

/// The core runtime's model for one tool turn. The Claude Code model holds
/// a live process session, so it is boxed.
pub(crate) enum TurnModel<'a> {
    Http(LocalToolModel<'a>),
    ClaudeCode(Box<ClaudeCodeModel<'a>>),
}

impl TurnModel<'_> {
    /// The failure behind the last [`ModelError`], for its specific code.
    pub(crate) const fn last_error(&self) -> Option<ProviderError> {
        match self {
            Self::Http(model) => model.last_error(),
            Self::ClaudeCode(model) => model.last_error(),
        }
    }
}

impl ToolModel for TurnModel<'_> {
    fn complete(&mut self, request: ToolModelRequest<'_>) -> Result<ToolModelReply, ModelError> {
        match self {
            Self::Http(model) => model.complete(request),
            Self::ClaudeCode(model) => model.complete(request),
        }
    }
}
