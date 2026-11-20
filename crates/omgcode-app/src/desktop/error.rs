//! The one failure shape the desktop commands return.
//!
//! A [`DesktopError`] serializes as exactly `{ "code": string, "message":
//! string }`. The code is a stable `snake_case` identifier that the interface
//! may branch on. The message is short English text. It never carries an
//! absolute path, recorded file content, or the local server token: every
//! message is fixed text, a static reason from core, or the message of an
//! [`AppError`], which the application layer already keeps free of secrets.

use std::fmt;

use omgcode_core::control::{ControlError, CredentialError, CredentialId, ProviderCredentialError};
use omgcode_core::session::SessionError;
use omgcode_core::session_index::IndexError;
use omgcode_core::turn::ReviewError;
use serde::Serialize;

use crate::error::{AppError, ErrorKind};

/// A controlled failure of one desktop command.
///
/// Codes defined by this service: `busy`, `invalid_journal`, `review_expired`,
/// `review_mismatch`, `not_found`, `invalid_request`, `archived`,
/// `turn_running`, `turn_open`, `too_many_turns`, `not_pending`,
/// `writers_stopped_required`, `unknown_workspace`, `shutting_down`, and
/// `internal`. Failures that come from the application layer keep their
/// [`ErrorKind`] code (`provider`, `session`, `control`, `context`,
/// `permission`, `turn`, and the others).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DesktopError {
    code: &'static str,
    message: String,
}

impl DesktopError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// The stable machine-readable code.
    pub const fn code(&self) -> &'static str {
        self.code
    }

    /// The short human-readable message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// A journal, the settings store, or the index is held elsewhere. Retry later.
    pub(crate) fn busy() -> Self {
        Self::new(
            "busy",
            "The data is in use by another operation. Try again shortly.",
        )
    }

    /// The session's journal failed validation.
    pub(crate) fn invalid_journal() -> Self {
        Self::new("invalid_journal", "The session journal failed validation.")
    }

    /// The review token is missing, used, or older than its lifetime.
    pub(crate) fn review_expired() -> Self {
        Self::new(
            "review_expired",
            "The review has expired. Open the approval again.",
        )
    }

    /// The review token does not belong to this approval.
    pub(crate) fn review_mismatch() -> Self {
        Self::new(
            "review_mismatch",
            "The review does not match this approval. Open the approval again.",
        )
    }

    /// No such session or approval.
    pub(crate) fn not_found(what: &'static str) -> Self {
        Self::new("not_found", format!("The {what} was not found."))
    }

    /// The request is not acceptable as sent.
    pub(crate) fn invalid_request(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message)
    }

    /// The session is archived.
    pub(crate) fn archived() -> Self {
        Self::new("archived", "Restore the archived session before chatting.")
    }

    /// A turn for the session is running in this application.
    pub(crate) fn turn_running() -> Self {
        Self::new(
            "turn_running",
            "A chat turn is already running for this session.",
        )
    }

    /// The journal records a turn that never finished.
    pub(crate) fn turn_open() -> Self {
        Self::new(
            "turn_open",
            "The session has an unfinished turn. Interrupt it first.",
        )
    }

    /// The running-turn limit is reached.
    pub(crate) fn too_many_turns() -> Self {
        Self::new(
            "too_many_turns",
            "Too many chat turns are running. Wait for one to finish.",
        )
    }

    /// The approval already has a decision, or it expired.
    pub(crate) fn not_pending() -> Self {
        Self::new("not_pending", "The approval is no longer pending.")
    }

    /// A file write needs the writers-stopped acknowledgement.
    pub(crate) fn writers_stopped_required() -> Self {
        Self::new(
            "writers_stopped_required",
            "Confirm that other writers to the file are stopped to approve a file write.",
        )
    }

    /// The workspace is not registered.
    pub(crate) fn unknown_workspace() -> Self {
        Self::new("unknown_workspace", "The workspace is not registered.")
    }

    /// The saved chat preferences could not be written.
    pub(crate) fn preferences() -> Self {
        Self::new("preferences", "The chat preferences could not be saved.")
    }

    /// The per-session connection and model choices could not be saved.
    pub(crate) fn session_choices() -> Self {
        Self::new(
            "session_choices",
            "The choice for this session could not be saved.",
        )
    }

    /// The session's recorded connection can no longer be used as recorded
    /// (ADR-0070). Nothing was sent.
    pub(crate) fn session_connection_stale() -> Self {
        Self::new(
            "session_connection_stale",
            "The connection chosen for this session changed or was removed. Choose the provider again, then send.",
        )
    }

    /// The operator closed a folder dialog or declined a confirmation. Nothing
    /// changed.
    pub(crate) fn cancelled() -> Self {
        Self::new("cancelled", "No change was made.")
    }

    /// The caller's settings revision is not the current one.
    pub(crate) fn stale_revision() -> Self {
        Self::new(
            "stale_revision",
            "The settings changed since this window read them. Review the current values and try again.",
        )
    }

    /// Another settings change from this application is waiting for the
    /// operator.
    pub(crate) fn settings_change_running() -> Self {
        Self::new(
            "settings_change_running",
            "Another settings change is waiting for a decision.",
        )
    }

    /// The change was recorded, but runs that lost access were not observed
    /// to stop, so the revocation is not complete.
    pub(crate) fn runs_not_stopped(count: usize) -> Self {
        Self::new(
            "runs_not_stopped",
            format!(
                "The settings change was recorded, but {count} run(s) that lost access were not observed to stop. Inspect them with the trusted CLI `runs audit`."
            ),
        )
    }

    /// The change was recorded, but the run journal could not be checked for
    /// runs that lost access. The cause is not shown because it can name a
    /// path.
    pub(crate) fn runs_unchecked() -> Self {
        Self::new(
            "runs_not_stopped",
            "The settings change was recorded, but runs that may have lost access could not be checked. Inspect them with the trusted CLI `runs audit`.",
        )
    }

    /// The active model connection changed in `settings` since the window read
    /// it (ADR-0064). Nothing was sent.
    pub(crate) fn connection_changed() -> Self {
        Self::new(
            "connection_changed",
            "The model connection changed in settings. Check the model name, then send again.",
        )
    }

    /// The model connection was fixed by a launch argument, so it cannot be
    /// chosen in `settings`.
    pub(crate) fn connection_fixed() -> Self {
        Self::new(
            "connection_fixed",
            "The model connection was set when the application started. Restart without --endpoint or --provider-profile to choose it here.",
        )
    }

    /// The API key is empty, too long, or has a control character. The message
    /// is fixed and never repeats the key or its length.
    pub(crate) fn malformed_key_input() -> Self {
        Self::new(
            "invalid_request",
            "The API key is empty, longer than 16 KiB, or contains a control character.",
        )
    }

    /// The native credential store refused. Each message is fixed text, so
    /// neither a secret nor an operating-system detail is repeated.
    pub(crate) fn from_credential(error: &CredentialError) -> Self {
        let (code, message) = match error {
            CredentialError::Unavailable(_) => (
                "credential_unavailable",
                "No protected credential store is available on this computer.",
            ),
            CredentialError::Locked => (
                "credential_locked",
                "The credential store is locked. Unlock it outside OMGCode and try again.",
            ),
            CredentialError::TooLarge | CredentialError::Invalid(_) => {
                return Self::malformed_key_input();
            }
            CredentialError::PromptRequired => (
                "credential_prompt_required",
                "The credential store asked for interactive confirmation, which OMGCode never shows.",
            ),
            CredentialError::NotFound
            | CredentialError::AlreadyExists
            | CredentialError::Corrupt(_)
            | CredentialError::Io(_) => (
                "credential_io",
                "The credential store could not store or delete the key.",
            ),
        };
        Self::new(code, message)
    }

    /// Map a failure of core's store-and-bind call.
    pub(crate) fn from_provider_credential(error: &ProviderCredentialError) -> Self {
        match error {
            ProviderCredentialError::Control(error) => Self::from_control(error),
            ProviderCredentialError::Credential(error) => Self::from_credential(error),
            ProviderCredentialError::Unrecorded { credential_id, .. }
            | ProviderCredentialError::Unstored { credential_id, .. } => {
                Self::credential_item_orphaned(credential_id)
            }
            ProviderCredentialError::BindingUncertain {
                credential_id,
                replaced,
                ..
            } => Self::credential_binding_uncertain(credential_id, replaced.as_ref()),
        }
    }

    /// A new stored item that no profile names could not be deleted. Its
    /// opaque identifier is reported so the operator can find it; the key and
    /// the native error text are not.
    fn credential_item_orphaned(credential_id: &CredentialId) -> Self {
        Self::new(
            "credential_orphaned",
            format!(
                "The stored API key {credential_id} could not be cleaned up and may remain in the operating system's credential store with no connection naming it. Delete it there."
            ),
        )
    }

    /// The new key was stored, but whether its binding was recorded is
    /// unknown, so the key was kept, never deleted (security coding rules).
    /// The opaque identifier, and the one it may have replaced, let the
    /// operator check with the trusted CLI. The key itself is never shown.
    pub(crate) fn credential_binding_uncertain(
        credential_id: &CredentialId,
        replaced: Option<&CredentialId>,
    ) -> Self {
        let replaced = replaced.map_or_else(String::new, |old| {
            format!(
                " If it is bound, the key it replaced, {old}, is no longer bound and was not deleted; delete it by hand."
            )
        });
        Self::new(
            "credential_binding_uncertain",
            format!(
                "The API key was stored as {credential_id}, but whether the connection names it is uncertain, so it was kept. Run `provider-profile-show` in the trusted CLI: if it shows this identifier, keep it or remove it with `provider-credential-remove`; otherwise delete the item by hand.{replaced}"
            ),
        )
    }

    /// A stored item may remain that no profile names, or the settings change
    /// was recorded but its stored key was not deleted. The operator removes
    /// it with the operating system's credential manager.
    pub(crate) fn credential_orphaned() -> Self {
        Self::new(
            "credential_orphaned",
            "A stored API key could not be cleaned up and may remain in the operating system's credential store. Delete it there, or check the connection in the trusted CLI with `provider-profile-show`.",
        )
    }

    /// The application is shutting down.
    pub(crate) fn shutting_down() -> Self {
        Self::new("shutting_down", "The application is shutting down.")
    }

    /// Service state or a lower layer is unavailable.
    pub(crate) fn internal(message: &'static str) -> Self {
        Self::new("internal", message)
    }

    /// A poisoned service lock. State whose invariants are uncertain is not used.
    pub(crate) fn poisoned() -> Self {
        Self::internal("The desktop service state is unavailable.")
    }

    /// Keep the application layer's code and message.
    pub(crate) fn from_app(error: &AppError) -> Self {
        Self::new(error.code(), error.message())
    }

    /// Remove a known secret from the message.
    pub(crate) fn redacted(mut self, secret: Option<&str>) -> Self {
        self.message = crate::redact_known_secret(&self.message, secret);
        self
    }

    /// Map a title-validation failure from session creation to `invalid_request`.
    pub(crate) fn from_create(error: &AppError) -> Self {
        if error.kind() == ErrorKind::Session
            && error.message().starts_with("invalid session value")
        {
            Self::invalid_request(error.message())
        } else {
            Self::from_app(error)
        }
    }

    /// Map a journal failure.
    pub(crate) fn from_session(error: &SessionError) -> Self {
        match error {
            SessionError::Busy => Self::busy(),
            SessionError::InvalidJournal(_) | SessionError::LimitReached => Self::invalid_journal(),
            SessionError::ActiveTurn => Self::turn_open(),
            SessionError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
                Self::not_found("session")
            }
            SessionError::Io(_) => Self::new("session", "The session journal could not be read."),
            SessionError::InvalidValue(reason) => Self::invalid_request(*reason),
            SessionError::WrongTurn | SessionError::Poisoned => {
                Self::new("session", error.to_string())
            }
        }
    }

    /// Map an index failure. Index text and stored values never reach the caller.
    pub(crate) fn from_index(error: &IndexError) -> Self {
        match error {
            IndexError::Busy => Self::busy(),
            IndexError::InvalidQuery(reason) => Self::invalid_request(*reason),
            IndexError::InvalidIndexEntry(_) => Self::invalid_journal(),
            IndexError::UnsafeLocation(_)
            | IndexError::Session(_)
            | IndexError::Io(_)
            | IndexError::Sqlite(_) => Self::internal("The session index is unavailable."),
        }
    }

    /// Map a settings-store failure.
    pub(crate) fn from_control(error: &ControlError) -> Self {
        match error {
            ControlError::Busy => Self::busy(),
            ControlError::UnknownWorkspace => Self::unknown_workspace(),
            ControlError::UnknownApproval => Self::not_found("approval"),
            ControlError::Conflict { .. } => Self::stale_revision(),
            ControlError::NoRoot => Self::new(
                "no_development_root",
                "Choose a development root before registering a workspace.",
            ),
            ControlError::Io(_)
            | ControlError::Container(_)
            | ControlError::ProfileRemoval(_)
            | ControlError::WorkspacePath(_) => {
                Self::new("control", "The settings store could not be used.")
            }
            other => Self::new("control", other.to_string()),
        }
    }

    /// Map an approval-review failure.
    pub(crate) fn from_review(error: &ReviewError) -> Self {
        match error {
            ReviewError::Busy => Self::busy(),
            other => Self::new("control", other.to_string()),
        }
    }
}

impl fmt::Display for DesktopError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for DesktopError {}

#[cfg(test)]
mod tests {
    use std::io;

    use omgcode_core::control::{
        ControlError, CredentialError, CredentialId, ProviderCredentialError,
    };

    use super::DesktopError;

    const KEPT: &str = "0123456789ab4def8123456789abcdef";
    const REPLACED: &str = "fedcba98765443218765fedcba987654";

    fn id(text: &str) -> CredentialId {
        CredentialId::parse(text).expect("a valid identifier")
    }

    fn io_error() -> ControlError {
        // The native text must never reach the window, so it is made recognisable here.
        ControlError::Io(io::Error::other("NATIVE-DETAIL"))
    }

    /// An uncertain binding keeps the new item, so the window must name it,
    /// and the key it may have replaced, as the CLI does. No native text.
    #[test]
    fn an_uncertain_binding_reports_the_kept_and_replaced_identifiers() {
        let error =
            DesktopError::from_provider_credential(&ProviderCredentialError::BindingUncertain {
                error: io_error(),
                credential_id: id(KEPT),
                replaced: Some(id(REPLACED)),
            });
        assert_eq!(error.code(), "credential_binding_uncertain");
        assert!(error.message().contains(KEPT), "{}", error.message());
        assert!(error.message().contains(REPLACED), "{}", error.message());
        assert!(error.message().contains("was kept"), "{}", error.message());
        assert!(!error.message().contains("NATIVE-DETAIL"));

        let first =
            DesktopError::from_provider_credential(&ProviderCredentialError::BindingUncertain {
                error: io_error(),
                credential_id: id(KEPT),
                replaced: None,
            });
        assert!(first.message().contains(KEPT));
        assert!(!first.message().contains("replaced"), "{}", first.message());
    }

    /// An orphaned item that could not be deleted is named by its identifier.
    #[test]
    fn an_orphaned_item_is_named_by_its_identifier() {
        for error in [
            ProviderCredentialError::Unrecorded {
                error: io_error(),
                credential_id: id(KEPT),
                cleanup: CredentialError::Io(io::Error::other("NATIVE-DETAIL")),
            },
            ProviderCredentialError::Unstored {
                error: CredentialError::Io(io::Error::other("NATIVE-DETAIL")),
                credential_id: id(KEPT),
                cleanup: CredentialError::Io(io::Error::other("NATIVE-DETAIL")),
            },
        ] {
            let shown = DesktopError::from_provider_credential(&error);
            assert_eq!(shown.code(), "credential_orphaned");
            assert!(shown.message().contains(KEPT), "{}", shown.message());
            assert!(!shown.message().contains("NATIVE-DETAIL"));
        }
    }
}
