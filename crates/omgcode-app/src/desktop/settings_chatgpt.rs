//! ChatGPT sign-in and sign-out from the `settings` window (ADR-0093).
//!
//! Rust asks a native confirmation, opens the system browser, waits for the
//! loopback callback, and stores the tokens. The webview receives only the
//! settings overview: never the sign-in address, the callback, or a token.
//! The wait can be cancelled from the window, which only sets a cancel token.

use omgcode_core::control::ProviderProfileDeclaration;
use omgcode_core::provider::OPENAI_ENDPOINT;
use omgcode_core::provider::chatgpt::SignInError;
use omgcode_core::turn::{CancelToken, display_safe_line};

use super::DesktopService;
use super::error::DesktopError;
use super::settings::overview_of;
use super::settings_changes::{ChangeGuard, Confirmation, SettingsPrompt, check_revision, confirm};
use super::wire::{ChatGptRequest, ChatGptSignOutResult, SettingsOverview};

/// The function that opens the sign-in address. Tests replace it so no test
/// can reach OpenAI.
pub(crate) type BrowserOpener = fn(&str, &CancelToken) -> Result<(), SignInError>;

/// Removes the registered cancel token when the sign-in ends.
struct SignInSlot<'a>(&'a std::sync::Mutex<Option<CancelToken>>);

impl Drop for SignInSlot<'_> {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = None;
        }
    }
}

impl DesktopService {
    /// `settings` only. After a native confirmation that names the browser
    /// step and what goes to OpenAI, sign the named connection in with
    /// ChatGPT. A name that no profile has yet first records an `openai`
    /// subscription profile with OpenAI's endpoint. The call blocks for at
    /// most five minutes while the browser step runs; see
    /// [`settings_cancel_chatgpt_sign_in`](Self::settings_cancel_chatgpt_sign_in).
    ///
    /// # Errors
    ///
    /// `cancelled`, `stale_revision`, `settings_change_running`, `control`
    /// for a profile that cannot sign in, and the `credential` codes for a
    /// failed sign-in, each naming the sign-in's stable code.
    pub fn settings_sign_in_chatgpt(
        &self,
        request: ChatGptRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        // The cancel token is registered before the native confirmation, so a
        // cancel sent while the confirmation is open is kept and honoured
        // before anything is recorded or the browser opens.
        let cancel = CancelToken::new();
        let slot = SignInSlot(&self.inner.chatgpt_sign_in);
        *slot.0.lock().map_err(|_| DesktopError::poisoned())? = Some(cancel.clone());
        let declaration = ProviderProfileDeclaration {
            name: request.name.clone(),
            kind: "openai".to_owned(),
            auth: "subscription".to_owned(),
            endpoint: Some(OPENAI_ENDPOINT.to_owned()),
        };
        // Every check runs before the confirmation, so the operator is never
        // asked to consent for a name or profile that would be refused.
        let exists = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let exists = store.provider_profile(&request.name).is_some();
            if exists {
                store.check_subscription_sign_in(&request.name)
            } else {
                store.check_provider_profile(&declaration)
            }
            .map_err(|error| DesktopError::from_control(&error))?;
            exists
        };
        confirm(
            prompt,
            &Confirmation {
                title: "Sign in with ChatGPT",
                message: format!(
                    "Open OpenAI's sign-in page in your browser to connect {} to your ChatGPT plan?\n\nOpenAI receives the sign-in request and this computer's OMGCode host identifier. After you sign in, chat turns of sessions that use this connection, including any workspace text in them, go to OpenAI under your ChatGPT plan. The tokens are kept only in this computer's protected credential store.\n\nA completed sign-in does not show that the plan will accept OMGCode's requests.",
                    display_safe_line(&request.name)
                ),
            },
        )?;
        if cancel.is_cancelled() {
            return Err(DesktopError::cancelled());
        }
        let mut revision = request.revision;
        if !exists {
            let mut store = self.inner.open_or_create_control()?;
            store
                .add_provider_profile(&declaration, revision)
                .map_err(|error| DesktopError::from_control(&error))?;
            revision = store.revision();
        }
        let opener = self.inner.open_browser;
        let mut open = |address: &str| opener(address, &cancel);
        let signed_in = crate::sign_in_with_chatgpt(
            &self.inner.data_root,
            &request.name,
            revision,
            &mut open,
            &cancel,
        );
        drop(slot);
        signed_in.map_err(|error| DesktopError::from_app(&error))?;
        // The sign-in is recorded; a failed read must not report it as failed.
        let store = self
            .inner
            .open_or_create_control()
            .map_err(|_| DesktopError::signed_in_unread())?;
        Ok(overview_of(&self.inner, store))
    }

    /// `settings` only. Cancel the running ChatGPT sign-in or sign-out, if
    /// any. It only sets that change's cancel token. A sign-in returns
    /// `cancelled`. A sign-out returns `cancelled` until it holds the
    /// session lock; a later cancel stops the wait for OpenAI, and the
    /// sign-out is still recorded here with `revoked` false. Each
    /// registers its token before its native confirmation, so a cancel sent
    /// during the confirmation is kept. With neither running it does nothing.
    pub fn settings_cancel_chatgpt_sign_in(&self) {
        if let Ok(slot) = self.inner.chatgpt_sign_in.lock()
            && let Some(cancel) = slot.as_ref()
        {
            cancel.cancel();
        }
    }

    /// `settings` only. After a native confirmation, revoke the named
    /// connection's ChatGPT session once, record the sign-out, and delete its
    /// tokens. `revoked` is `false` when OpenAI did not confirm the
    /// revocation; the local sign-out is recorded either way. A slot that
    /// could not be deleted makes this `credential_orphaned`, whose message
    /// also says whether OpenAI confirmed the revocation.
    ///
    /// # Errors
    ///
    /// `cancelled`, `stale_revision`, `settings_change_running`, and the
    /// `credential` codes for a profile that is not signed in or a store
    /// that cannot be used.
    pub fn settings_sign_out_chatgpt(
        &self,
        request: ChatGptRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<ChatGptSignOutResult, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        // Registered as for the sign-in, so the window's Cancel can end the
        // wait for the session lock or for OpenAI.
        let cancel = CancelToken::new();
        let slot = SignInSlot(&self.inner.chatgpt_sign_in);
        *slot.0.lock().map_err(|_| DesktopError::poisoned())? = Some(cancel.clone());
        {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            store
                .provider_subscription(&request.name)
                .map_err(|error| {
                    DesktopError::from_app(&crate::AppError::new(
                        crate::ErrorKind::Credential,
                        format!("{}: {error}", error.code()),
                    ))
                })?;
        }
        confirm(
            prompt,
            &Confirmation {
                title: "Sign out of ChatGPT",
                message: format!(
                    "Sign {} out of ChatGPT?\n\nOMGCode asks OpenAI once to end the session and deletes its tokens from this computer. Sessions that use this connection stop working until you sign in again.",
                    display_safe_line(&request.name)
                ),
            },
        )?;
        if cancel.is_cancelled() {
            return Err(DesktopError::cancelled());
        }
        let outcome = crate::sign_out_of_chatgpt(
            &self.inner.data_root,
            &request.name,
            request.revision,
            &cancel,
        )
        .map_err(|error| DesktopError::from_app(&error))?;
        drop(slot);
        if !outcome.orphans.is_empty() {
            return Err(DesktopError::signed_out_tokens_orphaned(
                &outcome.orphans,
                outcome.revoked,
            ));
        }
        // The sign-out is recorded; a failed read must not report it as failed.
        let store = self
            .inner
            .open_or_create_control()
            .map_err(|_| DesktopError::signed_out_unread(outcome.revoked))?;
        Ok(ChatGptSignOutResult {
            overview: overview_of(&self.inner, store),
            revoked: outcome.revoked,
        })
    }
}

#[cfg(test)]
mod tests;
