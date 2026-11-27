//! The typed failure shared by every application service.

use std::fmt;

use omgcode_core::provider::{ProviderError, ResponseFailure};
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
    /// Any other controlled turn failure, such as a timeout.
    Turn,
    /// The operator cancelled the turn or the compaction. A turn that had
    /// started records the failure reason `cancelled` and no assistant
    /// message; a turn or compaction cancelled before it was recorded records
    /// nothing (ADR-0050, ADR-0051).
    Cancelled,
    /// A selected provider credential could not be used. The message names a
    /// stable cause code and never contains the secret.
    Credential,
    /// A selected skill could not be loaded or was refused. The error's own
    /// code names the cause, such as `skill_unavailable` or `package_tampered`.
    Skill,
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
            Self::Cancelled => "cancelled",
            Self::Credential => "credential",
            Self::Skill => "skill",
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
    /// A more specific stable code than the category's, when one exists.
    detail: Option<&'static str>,
    message: String,
}

impl AppError {
    /// Build an error. `message` must not contain secrets or untrusted text.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            detail: None,
            message: message.into(),
        }
    }

    /// Build a `skill` category error whose stable code is `code`, such as
    /// `skill_unavailable`. `message` must not contain skill text.
    pub fn skill(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Skill,
            detail: Some(code),
            message: message.into(),
        }
    }

    /// Map a session journal failure to the `session` category.
    pub fn session(error: SessionError) -> Self {
        Self::new(ErrorKind::Session, error.to_string())
    }

    /// Map a provider failure to the `provider` category. A ChatGPT plan
    /// failure keeps a specific stable code, such as `signin_required` or
    /// `usage_limit_exceeded`, so a front end can say what to do (ADR-0093).
    pub fn provider(error: ProviderError) -> Self {
        Self {
            kind: ErrorKind::Provider,
            detail: provider_failure_code(error),
            message: error.to_string(),
        }
    }

    /// The failure category.
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The stable machine-readable code: the specific code when the error has
    /// one, otherwise the category's.
    pub const fn code(&self) -> &'static str {
        match self.detail {
            Some(code) => code,
            None => self.kind.code(),
        }
    }

    /// Add `note` to the message in brackets, keeping the category and code.
    /// `note` must not contain secrets or untrusted text.
    #[must_use]
    pub fn with_note(mut self, note: &str) -> Self {
        self.message = format!("{} ({note})", self.message);
        self
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

/// The specific code of a ChatGPT plan failure (ADR-0093) or of a refused
/// reasoning effort (ADR-0108), which the runtime's model error categories
/// do not carry. Other failures have none.
const fn provider_failure_code(error: ProviderError) -> Option<&'static str> {
    match error {
        ProviderError::EffortRefused(_) => Some("effort_refused"),
        ProviderError::SignInRequired => Some("signin_required"),
        ProviderError::AccessTokenUnavailable => Some("access_token_unavailable"),
        ProviderError::ResponseFailed(ResponseFailure::Failed) => Some("plan_request_failed"),
        ProviderError::ResponseFailed(ResponseFailure::Incomplete) => {
            Some("plan_response_incomplete")
        }
        ProviderError::ResponseFailed(failure) => Some(failure.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use omgcode_core::provider::ReasoningEffort;

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
            (ErrorKind::Cancelled, "cancelled"),
            (ErrorKind::Credential, "credential"),
            (ErrorKind::Skill, "skill"),
        ];
        for (kind, code) in codes {
            assert_eq!(AppError::new(kind, "m").code(), code);
        }
    }

    #[test]
    fn a_skill_error_reports_its_specific_code_and_the_skill_category() {
        let error = AppError::skill("skill_unavailable", "m");
        assert_eq!(error.kind(), ErrorKind::Skill);
        assert_eq!(error.code(), "skill_unavailable");
    }

    #[test]
    fn provider_and_session_mappers_use_the_core_display_text() {
        let provider = AppError::provider(ProviderError::Transport);
        assert_eq!(provider.code(), "provider");
        assert_eq!(provider.message(), "local model transport failed");
        let plan = AppError::provider(ProviderError::SignInRequired);
        assert_eq!(plan.kind(), ErrorKind::Provider);
        assert_eq!(plan.code(), "signin_required");
        assert!(plan.message().contains("sign in again"));
        for (failure, code) in [
            (ResponseFailure::UsageLimitExceeded, "usage_limit_exceeded"),
            (ResponseFailure::NotAuthorized, "not_authorized"),
            (ResponseFailure::Failed, "plan_request_failed"),
            (ResponseFailure::Incomplete, "plan_response_incomplete"),
        ] {
            assert_eq!(
                AppError::provider(ProviderError::ResponseFailed(failure)).code(),
                code
            );
        }
        assert_eq!(
            AppError::provider(ProviderError::AccessTokenUnavailable).code(),
            "access_token_unavailable"
        );
        let effort = AppError::provider(ProviderError::EffortRefused(ReasoningEffort::Max));
        assert_eq!(effort.kind(), ErrorKind::Provider);
        assert_eq!(effort.code(), "effort_refused");
        assert!(effort.message().contains("reasoning effort `max`"));
        assert_eq!(
            AppError::provider(ProviderError::HttpStatus(400)).code(),
            "provider"
        );
        let session = AppError::session(SessionError::LimitReached);
        assert_eq!(session.code(), "session");
        assert_eq!(session.message(), SessionError::LimitReached.to_string());
    }
}
