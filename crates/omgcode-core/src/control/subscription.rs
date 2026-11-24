//! ChatGPT subscription sign-ins in the control journal (ADR-0093).
//!
//! A sign-in records `provider_subscription_bound`: the profile, the issued
//! client, the account subject, an optional display label, the host
//! identifier, and a credential binding for the two-slot token store. No
//! token is recorded. Sign-out is the existing `provider_credential_removed`
//! event. The registration stays on the profile after sign-out, so a later
//! sign-in must return the same client and account.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::credential::{binding_write_uncertain, delete_unbound_slots, store_failure};
use super::{
    Action, ControlError, ControlSnapshot, ControlStore, CredentialBinding, CredentialError,
    CredentialId, CredentialStore, OrphanedCredential, ProviderAuth, ProviderCredentialError,
    ProviderCredentialSet, ProviderCredentialUseError, ProviderKind,
};
use crate::provider::OPENAI_ENDPOINT;
use crate::provider::chatgpt::{
    SignInRequest, SignedIn, initial_token_record, new_host_id, valid_host_id, valid_identifier,
};

/// The outcome of a recorded ChatGPT sign-out.
#[derive(Debug)]
#[must_use]
pub struct SubscriptionSignOut {
    /// The profile that was signed out.
    pub profile_id: Uuid,
    /// The removed session's binding identifier.
    pub removed: CredentialId,
    /// Every token slot that could not be deleted: none, one, or both.
    pub orphans: Vec<OrphanedCredential>,
}

/// The longest kept account label.
const MAX_ACCOUNT_LABEL_BYTES: usize = 256;

/// The registration a sign-in created. None of it is secret.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionRegistration {
    client_id: String,
    subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_label: Option<String>,
    host_id: String,
}

impl SubscriptionRegistration {
    /// The client OpenAI issued.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// The account's ID-token subject.
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// The account's email, for display only.
    pub fn account_label(&self) -> Option<&str> {
        self.account_label.as_deref()
    }

    /// The host identifier sent at sign-in.
    pub fn host_id(&self) -> &str {
        &self.host_id
    }

    fn valid(&self) -> bool {
        valid_identifier(&self.client_id)
            && valid_identifier(&self.subject)
            && valid_host_id(&self.host_id)
            && self.account_label.as_deref().is_none_or(|label| {
                !label.is_empty()
                    && label.len() <= MAX_ACCOUNT_LABEL_BYTES
                    && !label.chars().any(char::is_control)
            })
    }
}

/// A signed-in profile's session: the binding of its token slots and the
/// registration its records must match. It holds no secret.
#[derive(Clone, Debug)]
pub struct SubscriptionConnection {
    /// The token store binding.
    pub binding: CredentialBinding,
    /// The issued client.
    pub client_id: String,
    /// The account subject.
    pub subject: String,
    /// The profile's recorded endpoint.
    pub endpoint: String,
}

/// Whether a profile is a ChatGPT subscription profile with its endpoint.
fn is_chatgpt_profile(status: &super::ProviderProfileStatus) -> bool {
    let profile = status.profile();
    profile.kind() == ProviderKind::OpenAi
        && profile.auth() == ProviderAuth::Subscription
        && profile
            .endpoint()
            .is_some_and(|endpoint| endpoint.as_str() == OPENAI_ENDPOINT)
}

impl ControlStore {
    /// Check that the named profile may start a sign-in now and return what
    /// the sign-in sends. A new host identifier is made when none is
    /// recorded; it is recorded only by a successful sign-in.
    ///
    /// # Errors
    ///
    /// `UnknownProvider`, or `ProviderRefused` for a profile that is not an
    /// `openai` subscription profile with the OpenAI endpoint, or that is
    /// already signed in.
    pub fn subscription_sign_in_request(&self, name: &str) -> Result<SignInRequest, ControlError> {
        self.ensure_healthy()?;
        let status = self
            .provider_profile(name)
            .ok_or(ControlError::UnknownProvider)?;
        if !is_chatgpt_profile(status) {
            return Err(ControlError::ProviderRefused(
                "only an openai subscription profile with the endpoint https://api.openai.com/v1 can sign in with ChatGPT",
            ));
        }
        if status.credential.is_some() {
            return Err(ControlError::ProviderRefused(
                "this profile is already signed in; sign out first",
            ));
        }
        let registration = status.subscription.as_ref();
        Ok(SignInRequest {
            client_id: registration.map(|known| known.client_id.clone()),
            subject: registration.map(|known| known.subject.clone()),
            host_id: self
                .snapshot
                .subscription_host
                .clone()
                .unwrap_or_else(new_host_id),
        })
    }

    /// Store a completed sign-in's tokens and record it, with the ordering
    /// and failure reporting of [`Self::set_provider_credential`].
    ///
    /// `request` must be the one [`Self::subscription_sign_in_request`]
    /// returned for this attempt. The checks run again here, because the
    /// journal may have changed during the browser step.
    ///
    /// # Errors
    ///
    /// Revision, profile, and registration checks run before any native
    /// I/O and return [`ProviderCredentialError::Control`].
    pub fn record_provider_subscription(
        &mut self,
        credentials: &CredentialStore,
        name: &str,
        request: &SignInRequest,
        signed_in: &SignedIn,
        expected_revision: u64,
    ) -> Result<ProviderCredentialSet, ProviderCredentialError> {
        self.check_revision(expected_revision)?;
        let current = self.subscription_sign_in_request(name)?;
        if current.client_id != request.client_id
            || current.subject != request.subject
            || self
                .snapshot
                .subscription_host
                .as_ref()
                .is_some_and(|host| *host != request.host_id)
        {
            return Err(ControlError::ProviderRefused(
                "the profile's registration changed during the sign-in; sign in again",
            )
            .into());
        }
        let registration = SubscriptionRegistration {
            client_id: signed_in.client_id.clone(),
            subject: signed_in.subject.clone(),
            account_label: signed_in.email.clone(),
            host_id: request.host_id.clone(),
        };
        if !registration.valid() {
            return Err(ControlError::ProviderRefused(
                "the sign-in returned an invalid registration",
            )
            .into());
        }
        let profile_id = self
            .provider_profile(name)
            .ok_or(ControlError::UnknownProvider)?
            .profile()
            .id();
        let secret =
            initial_token_record(&signed_in.client_id, &signed_in.subject, &signed_in.tokens)
                .map_err(ProviderCredentialError::Credential)?;
        let id = CredentialId::parse(&Uuid::new_v4().simple().to_string())?;
        if let Err(error) = credentials.store(&id, &secret) {
            return Err(store_failure(credentials, id, error));
        }
        let binding = CredentialBinding::new(id.clone(), credentials.backend());
        let appended = self.next_revision().and_then(|revision| {
            self.append(
                Action::ProviderSubscriptionBound {
                    profile_id,
                    binding: binding.clone(),
                    registration,
                },
                revision,
            )
        });
        if let Err(error) = appended {
            if binding_write_uncertain(&error) {
                return Err(ProviderCredentialError::BindingUncertain {
                    error,
                    credential_id: id,
                    replaced: None,
                });
            }
            return Err(match credentials.delete(&id) {
                Ok(()) | Err(CredentialError::NotFound) => ProviderCredentialError::Control(error),
                Err(cleanup) => ProviderCredentialError::Unrecorded {
                    error,
                    credential_id: id,
                    cleanup,
                },
            });
        }
        Ok(ProviderCredentialSet {
            profile_id,
            binding,
            replaced: None,
            orphan: None,
        })
    }

    /// Record that the named ChatGPT profile is signed out, then delete both
    /// token slots. The caller holds the session lock and has already tried
    /// to revoke the refresh token. Every slot that could not be deleted is
    /// returned as an orphan.
    ///
    /// # Errors
    ///
    /// Refuses a stale revision, an unknown profile, a profile that is not a
    /// ChatGPT subscription profile, or one that is not signed in, before any
    /// native I/O.
    pub fn sign_out_provider_subscription(
        &mut self,
        credentials: &CredentialStore,
        name: &str,
        expected_revision: u64,
    ) -> Result<SubscriptionSignOut, ProviderCredentialError> {
        let status = self
            .provider_profile(name)
            .ok_or(ControlError::UnknownProvider)?;
        if !is_chatgpt_profile(status) {
            return Err(ControlError::ProviderRefused(
                "only a ChatGPT subscription profile signs out",
            )
            .into());
        }
        let (profile_id, binding) = self.unbind_credential(name, expected_revision)?;
        let orphans = delete_unbound_slots(credentials, &binding);
        Ok(SubscriptionSignOut {
            profile_id,
            removed: binding.id().clone(),
            orphans,
        })
    }

    /// The session of a signed-in ChatGPT profile. It reads no native
    /// storage and records nothing.
    ///
    /// # Errors
    ///
    /// The `ProviderCredentialUseError` codes: an unknown profile, another
    /// kind, an API-key profile (`not_subscription`), a subscription profile
    /// without OpenAI's endpoint, or one that is not signed in.
    pub fn provider_subscription(
        &self,
        name: &str,
    ) -> Result<SubscriptionConnection, ProviderCredentialUseError> {
        self.ensure_healthy()
            .map_err(ProviderCredentialUseError::Settings)?;
        let status = self.provider_profile(name).ok_or_else(|| {
            ProviderCredentialUseError::UnknownProfile {
                profile: name.to_owned(),
            }
        })?;
        let profile = status.profile();
        if profile.kind() != ProviderKind::OpenAi {
            return Err(ProviderCredentialUseError::WrongProvider {
                profile: profile.name().to_owned(),
                expected: ProviderKind::OpenAi,
                actual: profile.kind(),
            });
        }
        if profile.auth() != ProviderAuth::Subscription {
            return Err(ProviderCredentialUseError::NotSubscription {
                profile: profile.name().to_owned(),
            });
        }
        // An `openai` subscription profile is a ChatGPT connection only with
        // OpenAI's fixed endpoint recorded.
        if !is_chatgpt_profile(status) {
            return Err(ProviderCredentialUseError::EndpointNotRecorded {
                profile: profile.name().to_owned(),
            });
        }
        let (Some(binding), Some(registration)) = (&status.credential, &status.subscription) else {
            return Err(ProviderCredentialUseError::NotBound {
                profile: profile.name().to_owned(),
            });
        };
        Ok(SubscriptionConnection {
            binding: binding.clone(),
            client_id: registration.client_id.clone(),
            subject: registration.subject.clone(),
            endpoint: OPENAI_ENDPOINT.to_owned(),
        })
    }
}

/// Validate and apply `provider_subscription_bound` during replay or append
/// preflight.
pub(super) fn apply(state: &mut ControlSnapshot, action: &Action) -> Result<(), ControlError> {
    let Action::ProviderSubscriptionBound {
        profile_id,
        binding,
        registration,
    } = action
    else {
        return Ok(());
    };
    if !registration.valid() {
        return Err(ControlError::Invalid("invalid subscription registration"));
    }
    if state
        .subscription_host
        .as_ref()
        .is_some_and(|host| *host != registration.host_id)
    {
        return Err(ControlError::Invalid(
            "a subscription sign-in names another host identifier",
        ));
    }
    if state.provider_profiles.iter().any(|status| {
        status
            .credential
            .as_ref()
            .is_some_and(|other| other.id() == binding.id())
    }) || state
        .package_profiles
        .iter()
        .any(|status| status.binds_credential(binding.id()))
    {
        return Err(ControlError::Invalid(
            "a subscription binding reuses a recorded credential",
        ));
    }
    let status = state
        .provider_profiles
        .iter_mut()
        .find(|status| status.profile().id() == *profile_id)
        .ok_or(ControlError::Invalid(
            "a subscription sign-in for an unknown provider profile",
        ))?;
    if !is_chatgpt_profile(status) || status.credential.is_some() {
        return Err(ControlError::Invalid("invalid subscription sign-in"));
    }
    if let Some(known) = &status.subscription
        && (known.client_id != registration.client_id || known.subject != registration.subject)
    {
        return Err(ControlError::Invalid(
            "a subscription sign-in changes the profile's registration",
        ));
    }
    status.credential = Some(binding.clone());
    status.subscription = Some(registration.clone());
    state.subscription_host = Some(registration.host_id.clone());
    Ok(())
}

#[cfg(test)]
mod tests;
