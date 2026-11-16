//! The typed failure shared by every application service.

use std::fmt;

use omgcode_core::provider::ProviderError;
use omgcode_core::session::SessionError;

/// The stable failure category behind an [`AppError`] code.
///
/// Policy and front ends may branch on the kind. The code strings are part of
/// the trusted CLI's JSON error contract and must not change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// Invalid operator input or an unusable local configuration.
    Configuration,
    /// A session journal could not be read, written, or used in its state.
    Session,
    /// The local model provider failed or returned an unusable reply.
    Provider,
    /// The workspace settings (control store) failed.
    Control,
    /// Context accounting or compaction refused or failed.
    Context,
    /// Current workspace preferences denied a requested action.
    Permission,
    /// Prompt assembly rejected the bounded request.
    Prompt,
    /// A terminal journal event could not be made durable.
    Persistence,
    /// The model returned an invalid tool call.
    ToolCall,
    /// A bounded turn limit stopped the request.
    ToolLimit,
    /// Any other controlled turn failure, such as cancellation or timeout.
    Turn,
    /// A selected provider credential could not be used. The message names a
    /// stable cause code and never contains the secret.
    Credential,
}

impl ErrorKind {
    /// The stable machine-readable code for this category.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Session => "session",
            Self::Provider => "provider",
            Self::Control => "control",
            Self::Context => "context",
            Self::Permission => "permission",
            Self::Prompt => "prompt",
            Self::Persistence => "persistence",
            Self::ToolCall => "tool_call",
            Self::ToolLimit => "tool_limit",
            Self::Turn => "turn",
            Self::Credential => "credential",
        }
    }
}

/// A controlled application failure: a stable category and a safe message.
///
/// Messages come from fixed text or from core error `Display` output. They
/// never include the configured server token, model output, or tool text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppError {
    kind: ErrorKind,
    message: String,
}

impl AppError {
    /// Build an error. `message` must not contain secrets or untrusted text.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Map a session journal failure to the `session` category.
    pub fn session(error: SessionError) -> Self {
        Self::new(ErrorKind::Session, error.to_string())
    }

    /// Map a provider failure to the `provider` category.
    pub fn provider(error: ProviderError) -> Self {
        Self::new(ErrorKind::Provider, error.to_string())
    }

    /// The failure category.
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The stable machine-readable code for the category.
    pub const fn code(&self) -> &'static str {
        self.kind.code()
    }

    /// The human-readable message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Consume the error and return its message.
    pub fn into_message(self) -> String {
        self.message
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.message)
    }
}

impl std::error::Error for AppError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_keeps_its_published_code() {
        let codes = [
            (ErrorKind::Configuration, "configuration"),
            (ErrorKind::Session, "session"),
            (ErrorKind::Provider, "provider"),
            (ErrorKind::Control, "control"),
            (ErrorKind::Context, "context"),
            (ErrorKind::Permission, "permission"),
            (ErrorKind::Prompt, "prompt"),
            (ErrorKind::Persistence, "persistence"),
            (ErrorKind::ToolCall, "tool_call"),
            (ErrorKind::ToolLimit, "tool_limit"),
            (ErrorKind::Turn, "turn"),
            (ErrorKind::Credential, "credential"),
        ];
        for (kind, code) in codes {
            assert_eq!(AppError::new(kind, "m").code(), code);
        }
    }

    #[test]
    fn provider_and_session_mappers_use_the_core_display_text() {
        let provider = AppError::provider(ProviderError::Transport);
        assert_eq!(provider.code(), "provider");
        assert_eq!(provider.message(), "local model transport failed");
        let session = AppError::session(SessionError::LimitReached);
        assert_eq!(session.code(), "session");
        assert_eq!(session.message(), SessionError::LimitReached.to_string());
    }
}
