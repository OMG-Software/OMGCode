//! The error type returned to the webview.

use std::borrow::Cow;

use omgcode_app::desktop::DesktopError;
use serde::Serialize;

/// An error that crosses the IPC boundary.
///
/// The webview is untrusted, so a message is never built from a lower-level error
/// here. It is either a fixed string chosen in this crate or the message of a
/// [`DesktopError`], which the service keeps free of absolute paths, recorded
/// content, and the local server token. The `code` is a stable machine-readable
/// token the frontend may branch on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandError {
    code: &'static str,
    message: Cow<'static, str>,
}

impl CommandError {
    /// The calling webview is not the window this command belongs to.
    pub(crate) const fn forbidden_window() -> Self {
        Self {
            code: "forbidden_window",
            message: Cow::Borrowed("This window may not use this command."),
        }
    }

    /// A window the command needs is missing or could not be controlled.
    pub(crate) const fn window_unavailable() -> Self {
        Self {
            code: "window_unavailable",
            message: Cow::Borrowed("The requested window is not available."),
        }
    }

    /// A blocking task failed or panicked before it produced a result.
    #[must_use]
    pub const fn internal() -> Self {
        Self {
            code: "internal",
            message: Cow::Borrowed("The command could not be completed."),
        }
    }

    /// The stable error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    /// The message shown to the user.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<DesktopError> for CommandError {
    /// Keeps the service's `code` and `message` exactly.
    fn from(error: DesktopError) -> Self {
        Self {
            code: error.code(),
            message: Cow::Owned(error.message().to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_as_code_and_message_only() {
        // Given a forbidden-window error.
        let error = CommandError::forbidden_window();

        // When it is serialized for the webview.
        let value = serde_json::to_value(&error).expect("serialize");

        // Then it has exactly the code and message fields.
        assert_eq!(
            value,
            serde_json::json!({
                "code": "forbidden_window",
                "message": "This window may not use this command."
            })
        );
    }

    #[test]
    fn messages_hold_no_path_separators() {
        for error in [
            CommandError::forbidden_window(),
            CommandError::window_unavailable(),
        ] {
            assert!(!error.message.contains(['/', '\\', ':']));
            assert!(!error.code().is_empty());
        }
    }
}
