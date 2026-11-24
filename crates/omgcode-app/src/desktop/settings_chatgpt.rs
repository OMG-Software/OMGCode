//! ChatGPT sign-in and sign-out from the `settings` window (ADR-0093).
//!
//! Rust asks a native confirmation, opens the system browser, waits for the
//! loopback callback, and stores the tokens. The webview receives only the
//! settings overview: never the sign-in address, the callback, or a token.
//! The wait can be cancelled from the window, which only sets a cancel token.

use omgcode_core::control::{ControlError, ProviderProfileDeclaration};
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
pub(crate) type BrowserOpener = fn(&str) -> Result<(), SignInError>;

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
        let exists = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let exists = store.provider_profile(&request.name).is_some();
            if exists {
                store
                    .subscription_sign_in_request(&request.name)
                    .map_err(|error| DesktopError::from_control(&error))?;
            }
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
        let mut revision = request.revision;
        if !exists {
            let mut store = self.inner.open_or_create_control()?;
            store
                .add_provider_profile(
                    &ProviderProfileDeclaration {
                        name: request.name.clone(),
                        kind: "openai".to_owned(),
                        auth: "subscription".to_owned(),
                        endpoint: Some(OPENAI_ENDPOINT.to_owned()),
                    },
                    revision,
                )
                .map_err(|error| DesktopError::from_control(&error))?;
            revision = store.revision();
        }
        let cancel = CancelToken::new();
        let slot = SignInSlot(&self.inner.chatgpt_sign_in);
        *slot
            .0
            .lock()
            .map_err(|_| DesktopError::from_control(&ControlError::Busy))? = Some(cancel.clone());
        let opener = self.inner.open_browser;
        let mut open = |address: &str| opener(address);
        let signed_in = crate::sign_in_with_chatgpt(
            &self.inner.data_root,
            &request.name,
            revision,
            &mut open,
            &cancel,
        );
        drop(slot);
        signed_in.map_err(|error| DesktopError::from_app(&error))?;
        let store = self.inner.open_or_create_control()?;
        Ok(overview_of(&self.inner, store))
    }

    /// `settings` only. Cancel the running ChatGPT sign-in, if any. It only
    /// sets that sign-in's cancel token; the sign-in returns `cancelled`.
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
    /// revocation; the local sign-out is recorded either way.
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
        let outcome = crate::sign_out_of_chatgpt(
            &self.inner.data_root,
            &request.name,
            request.revision,
            &CancelToken::new(),
        )
        .map_err(|error| DesktopError::from_app(&error))?;
        if let Some(orphan) = outcome.orphan {
            return Err(DesktopError::removed_key_orphaned(&orphan));
        }
        let store = self.inner.open_or_create_control()?;
        Ok(ChatGptSignOutResult {
            overview: overview_of(&self.inner, store),
            revoked: outcome.revoked,
        })
    }
}

#[cfg(test)]
mod tests;
