//! ChatGPT sign-in, sign-out, and plan-backed connections for the trusted
//! front ends (ADR-0093).
//!
//! Only the CLI and the desktop `settings` window call these. The settings
//! store is never held during the browser step or a network request: a
//! sign-in reads what it sends, closes the store, runs the browser flow, and
//! reopens the store to record the result. A sign-out takes the session lock
//! first, then revokes, then opens the store, so the lock order is always the
//! session lock before the settings store.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use omgcode_core::control::{
    ControlError, ControlStore, CredentialId, CredentialStore, ProviderCredentialError,
    ProviderCredentialUseError,
};
use omgcode_core::provider::chatgpt::{
    self, AuthEndpoints, ChatGptTokens, SessionLock, SignInError, SignInRequest,
};
use omgcode_core::provider::{
    AccessTokenSource, ListedModel, list_local_models, list_subscription_models,
};
use omgcode_core::turn::CancelToken;

use crate::credential_source::{LmToken, LocalConnection};
use crate::error::{AppError, ErrorKind};

/// The token source of one signed-in profile for one turn. `Debug` names no
/// token.
#[derive(Clone)]
pub struct PlanSession(Arc<ChatGptTokens>);

impl PlanSession {
    pub(crate) fn new(tokens: ChatGptTokens) -> Self {
        Self(Arc::new(tokens))
    }

    /// The source a plan-backed model client asks for each request's token.
    #[must_use]
    pub fn tokens(&self) -> Arc<dyn AccessTokenSource> {
        Arc::clone(&self.0) as Arc<dyn AccessTokenSource>
    }
}

impl fmt::Debug for PlanSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PlanSession(<tokens>)")
    }
}

/// A recorded sign-in. Nothing here is secret.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignInOutcome {
    /// The profile's recorded name.
    pub profile: String,
    /// The account's email, for display only, when the ID token held one.
    pub account_label: Option<String>,
    /// The token store binding's identifier.
    pub credential_id: String,
    /// The settings revision after the sign-in.
    pub revision: u64,
}

/// A recorded sign-out.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignOutOutcome {
    /// Whether OpenAI confirmed the refresh token's revocation. When it did
    /// not, the operator can disconnect OMGCode in ChatGPT settings.
    pub revoked: bool,
    /// Every token slot that could not be deleted, by identifier.
    pub orphans: Vec<CredentialId>,
    /// The settings revision after the sign-out.
    pub revision: u64,
}

fn credential_error(message: String) -> AppError {
    AppError::new(ErrorKind::Credential, message)
}

fn sign_in_error(error: &SignInError) -> AppError {
    match error {
        SignInError::Cancelled => AppError::new(ErrorKind::Cancelled, error.to_string()),
        _ => credential_error(format!("{}: {error}", error.code())),
    }
}

fn control_error(error: &ControlError) -> AppError {
    credential_error(format!("settings: {error}"))
}

/// Open the existing settings store, waiting briefly for another holder.
fn open_settings(data_root: &Path) -> Result<ControlStore, AppError> {
    match ControlStore::open_existing_waiting(data_root) {
        Ok(Some(store)) => Ok(store),
        Ok(None) => Err(credential_error(
            "no settings store exists, so no provider profile can sign in".to_owned(),
        )),
        Err(error) => Err(control_error(&error)),
    }
}

fn check_revision(store: &ControlStore, expected: u64) -> Result<(), AppError> {
    if store.revision() == expected {
        Ok(())
    } else {
        Err(conflict_error(expected, store.revision()))
    }
}

fn conflict_error(expected: u64, actual: u64) -> AppError {
    AppError::new(
        ErrorKind::Control,
        format!(
            "conflict: the settings changed (revision {actual} is current, not {expected}); read them again"
        ),
    )
}

/// A settings refusal from the sign-in's checks, as before the browser step.
fn settings_refusal(error: &ControlError) -> AppError {
    match error {
        ControlError::Conflict { expected, actual } => conflict_error(*expected, *actual),
        other => AppError::new(ErrorKind::Control, format!("settings: {other}")),
    }
}

/// Sign the named `openai` subscription profile in with ChatGPT.
///
/// `open_browser` receives the authorization address. The CLI prints it and
/// opens it; the desktop shell only opens it.
///
/// # Errors
///
/// A `credential` error naming the sign-in's stable code, `cancelled` when
/// the operator cancelled, and `control` for a stale revision or a profile
/// that cannot sign in (not a ChatGPT connection, or already signed in).
pub fn sign_in_with_chatgpt(
    data_root: &Path,
    name: &str,
    expected_revision: u64,
    open_browser: &mut dyn FnMut(&str) -> Result<(), SignInError>,
    cancel: &CancelToken,
) -> Result<SignInOutcome, AppError> {
    let new_host = chatgpt::new_host_id().map_err(|error| sign_in_error(&error))?;
    let request: SignInRequest = {
        let store = open_settings(data_root)?;
        check_revision(&store, expected_revision)?;
        // A profile that cannot sign in is a settings refusal, as in the
        // desktop service and the other profile commands.
        store
            .subscription_sign_in_request(name, &new_host)
            .map_err(|error| settings_refusal(&error))?
    };
    // Fail before the browser step when no protected store can hold tokens.
    // The opened store is released at once.
    CredentialStore::open(data_root)
        .map(|_store| ())
        .map_err(|error| {
            credential_error(format!(
                "{}: {error}",
                ProviderCredentialUseError::native_code(&error)
            ))
        })?;
    let endpoints = AuthEndpoints::openai();
    let signed_in = chatgpt::sign_in(&endpoints, &request, open_browser, cancel)
        .map_err(|error| sign_in_error(&error))?;
    // A cancel that arrived after the browser step but before the tokens are
    // recorded is honoured: the unused session is ended, best effort, and
    // nothing is recorded. Recording is one local write, after which the
    // sign-in completes.
    if cancel.is_cancelled() {
        let _ = chatgpt::revoke(
            &endpoints,
            &signed_in.client_id,
            &signed_in.tokens.refresh_token,
            &CancelToken::new(),
        );
        return Err(sign_in_error(&SignInError::Cancelled));
    }
    // Set when the binding write's outcome is unknown: the profile may then be
    // signed in with these tokens, so they are kept and not revoked.
    let uncertain = std::cell::Cell::new(false);
    let recorded = (|| {
        let mut store = open_settings(data_root)?;
        let credentials = CredentialStore::open(data_root).map_err(|error| {
            credential_error(format!(
                "{}: {error}",
                ProviderCredentialUseError::native_code(&error)
            ))
        })?;
        let set = store
            .record_provider_subscription(
                &credentials,
                name,
                &request,
                &signed_in,
                expected_revision,
            )
            .map_err(|error| match error {
                // The API-key advice names provider-credential-remove, which
                // refuses a subscription profile; a session ends with sign-out.
                ProviderCredentialError::BindingUncertain { credential_id, .. } => {
                    uncertain.set(true);
                    credential_error(format!(
                        "signin_binding_uncertain: the sign-in's tokens were kept as {credential_id}, but whether the profile names them is uncertain. Run provider-profile-show: if it shows this identifier, the profile is signed in; to end the session, run provider-signout. Otherwise delete the item by hand"
                    ))
                }
                // Only these two can leave a new token item behind.
                orphan @ (ProviderCredentialError::Unrecorded { .. }
                | ProviderCredentialError::Unstored { .. }) => {
                    credential_error(format!("signin_not_recorded: {orphan}"))
                }
                // A stale revision, a refusal from the re-run checks, or a
                // refused binding whose new item was deleted: no item remains.
                ProviderCredentialError::Control(error) => settings_refusal(&error),
                ProviderCredentialError::Credential(error) => credential_error(format!(
                    "{}: {error}",
                    ProviderCredentialUseError::native_code(&error)
                )),
            })?;
        let profile = store.provider_profile(name).map_or_else(
            || name.to_owned(),
            |status| status.profile().name().to_owned(),
        );
        Ok(SignInOutcome {
            profile,
            account_label: signed_in.email.clone(),
            credential_id: set.binding.id().as_str().to_owned(),
            revision: store.revision(),
        })
    })();
    if recorded.is_err() && !uncertain.get() {
        // The tokens were not recorded, so ask OpenAI to end them. Best
        // effort. An uncertain write keeps them, as `set_provider_credential`
        // keeps a key, because a recorded binding must name a live session.
        let _ = chatgpt::revoke(
            &endpoints,
            &signed_in.client_id,
            &signed_in.tokens.refresh_token,
            &CancelToken::new(),
        );
    }
    recorded
}

/// Sign the named profile out: revoke its refresh token once, record the
/// sign-out, and delete both token slots, all under the session lock.
///
/// # Errors
///
/// A `credential` error for a profile that is not signed in or a store that
/// cannot be used, and `control` for a revision that is stale before the
/// revocation. A settings change made during the revocation does not stop
/// the local sign-out. A failed revocation is not an error:
/// [`SignOutOutcome::revoked`] reports it.
pub fn sign_out_of_chatgpt(
    data_root: &Path,
    name: &str,
    expected_revision: u64,
    cancel: &CancelToken,
) -> Result<SignOutOutcome, AppError> {
    let connection = {
        let store = open_settings(data_root)?;
        check_revision(&store, expected_revision)?;
        store
            .provider_subscription(name)
            .map_err(|error| credential_error(format!("{}: {error}", error.code())))?
    };
    let _lock = SessionLock::acquire(data_root, connection.binding.id(), cancel)
        .map_err(|error| sign_in_error(&error))?;
    // A refresh may have held the lock meanwhile, so the revision and the
    // binding are checked again before the revocation, the one remote effect.
    // The settings store is released before the network call.
    {
        let store = open_settings(data_root)?;
        check_revision(&store, expected_revision)?;
        match store.provider_subscription(name) {
            Ok(current) if current.binding == connection.binding => {}
            _ => {
                return Err(AppError::new(
                    ErrorKind::Control,
                    "conflict: the profile's session changed; read the settings again",
                ));
            }
        }
    }
    let credentials = CredentialStore::open(data_root).map_err(|error| {
        credential_error(format!(
            "{}: {error}",
            ProviderCredentialUseError::native_code(&error)
        ))
    })?;
    let revoked = chatgpt::revoke_current(
        &credentials,
        &AuthEndpoints::openai(),
        connection.binding.id(),
        &connection.client_id,
        &connection.subject,
        cancel,
    )
    .is_ok();
    // The operator's revision was checked before the revocation. Once OpenAI
    // may have revoked the session, a settings change made meanwhile must not
    // leave the dead tokens bound, so the sign-out is recorded against the
    // current revision. Deleting the tokens widens no access. It is recorded
    // only while the profile still names the session this call revoked; the
    // lock keeps any other sign-out of that session out, so a different
    // binding means it was already signed out.
    let mut store = open_settings(data_root)?;
    match store.provider_subscription(name) {
        Ok(current) if current.binding == connection.binding => {}
        _ => {
            return Ok(SignOutOutcome {
                revoked,
                orphans: Vec::new(),
                revision: store.revision(),
            });
        }
    }
    let current_revision = store.revision();
    let removal = store
        .sign_out_provider_subscription(&credentials, name, current_revision)
        .map_err(|error| credential_error(format!("signout_not_recorded: {error}")))?;
    Ok(SignOutOutcome {
        revoked,
        orphans: removal
            .orphans
            .into_iter()
            .map(|orphan| orphan.id)
            .collect(),
        revision: store.revision(),
    })
}

/// Build the token source of a signed-in profile from an open settings store.
pub(crate) fn plan_session(
    control: &ControlStore,
    data_root: &Path,
    name: &str,
) -> Result<(String, PlanSession), AppError> {
    let connection = control
        .provider_subscription(name)
        .map_err(|error| credential_error(format!("{}: {error}", error.code())))?;
    let tokens = ChatGptTokens::new(
        data_root.to_path_buf(),
        connection.binding,
        connection.client_id,
        connection.subject,
    );
    Ok((connection.endpoint, PlanSession::new(tokens)))
}

/// List the models of a resolved connection: the subscription list for a
/// plan session, the `/v1/models` list otherwise.
///
/// # Errors
///
/// `provider` for a refused list. A plan session whose access token cannot be
/// had is also `provider`, with the stable code a chat turn would report
/// (`signin_required` or `access_token_unavailable`).
pub fn list_connection_models(connection: &LocalConnection) -> Result<Vec<ListedModel>, AppError> {
    if let Some(plan) = &connection.plan {
        let token = plan
            .tokens()
            .access_token(&CancelToken::new())
            .map_err(AppError::provider)?;
        return list_subscription_models(&token).map_err(AppError::provider);
    }
    list_local_models(
        &connection.endpoint,
        connection.token.as_ref().map(LmToken::expose),
    )
    .map_err(AppError::provider)
}

/// Open the sign-in address in the system browser without a shell. Only an
/// OpenAI sign-in address is opened.
///
/// # Errors
///
/// [`SignInError::Browser`] when the address is not OpenAI's or no browser
/// could be started.
pub fn open_system_browser(address: &str) -> Result<(), SignInError> {
    if !address.starts_with("https://auth.openai.com/api/accounts/authorize?") {
        return Err(SignInError::Browser);
    }
    launch_browser(address)
}

#[cfg(windows)]
fn launch_browser(address: &str) -> Result<(), SignInError> {
    let root = std::env::var_os("SystemRoot").ok_or(SignInError::Browser)?;
    std::process::Command::new(std::path::PathBuf::from(root).join("explorer.exe"))
        .arg(address)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|_| SignInError::Browser)
}

#[cfg(not(windows))]
fn launch_browser(address: &str) -> Result<(), SignInError> {
    let mut child = std::process::Command::new("/usr/bin/xdg-open")
        .arg(address)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| SignInError::Browser)?;
    // Reap the launcher so it does not linger as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_openai_sign_in_address_is_opened() {
        for address in [
            "http://auth.openai.com/api/accounts/authorize?x=1",
            "https://auth.openai.com.evil.test/api/accounts/authorize?x=1",
            "https://evil.test/?https://auth.openai.com/api/accounts/authorize?",
            "file:///etc/passwd",
        ] {
            assert_eq!(open_system_browser(address), Err(SignInError::Browser));
        }
    }

    #[test]
    fn a_plan_session_debug_names_no_token() {
        let tokens = ChatGptTokens::new(
            std::path::PathBuf::from("."),
            serde_json::from_value(serde_json::json!({
                "credential_id": "0123456789ab4def8123456789abcdef",
                "backend": "windows_dpapi",
            }))
            .expect("binding"),
            "client".to_owned(),
            "subject".to_owned(),
        );
        assert_eq!(
            format!("{:?}", PlanSession::new(tokens)),
            "PlanSession(<tokens>)"
        );
    }

    #[test]
    fn errors_map_to_stable_codes() {
        assert_eq!(
            sign_in_error(&SignInError::Cancelled).kind(),
            ErrorKind::Cancelled
        );
        let error = sign_in_error(&SignInError::PlanUsageNotGranted);
        assert_eq!(error.kind(), ErrorKind::Credential);
        assert!(error.to_string().contains("plan_usage_not_granted"));
        let error = sign_in_error(&SignInError::Random);
        assert_eq!(error.kind(), ErrorKind::Credential);
        assert!(error.to_string().contains("random_unavailable"));
    }

    #[test]
    fn a_settings_refusal_after_the_browser_step_is_not_an_orphan() {
        let conflict = settings_refusal(&ControlError::Conflict {
            expected: 4,
            actual: 5,
        });
        assert_eq!(conflict.kind(), ErrorKind::Control);
        assert!(
            conflict
                .message()
                .starts_with("conflict: the settings changed")
        );
        let refused = settings_refusal(&ControlError::ProviderRefused("already signed in"));
        assert_eq!(refused.kind(), ErrorKind::Control);
        for error in [conflict, refused] {
            assert!(!error.to_string().contains("signin_not_recorded"));
        }
    }
}
