//! Provider credential bindings in the control journal. See ADR-0042.
//!
//! The journal records a provider profile identifier, an opaque credential
//! identifier, and the native backend. It never records a secret. Secrets live
//! only in the platform credential store, and a credential is never
//! overwritten: rotation stores a new credential, records the binding, and only
//! then deletes the old item. An interrupted rotation leaves either the old or
//! the new binding valid and at most one unbound item to remove.
//!
//! Only `api_key` profiles accept a credential for now. No provider adapter
//! reads a bound credential yet.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;
use uuid::Uuid;

pub use omgcode_platform::credentials::{
    CredentialBackend, CredentialError, CredentialId, CredentialStore, CredentialStoreStatus,
    MAX_SECRET_BYTES, Secret, probe as probe_credential_store,
};

use super::{Action, ControlError, ControlSnapshot, ControlStore, ProviderAuth};

/// One recorded credential binding: an opaque identifier and its backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialBinding {
    id: CredentialId,
    backend: CredentialBackend,
}

impl CredentialBinding {
    /// The opaque credential identifier. It is not secret.
    pub const fn id(&self) -> &CredentialId {
        &self.id
    }

    /// The backend that stored the credential.
    pub const fn backend(&self) -> CredentialBackend {
        self.backend
    }
}

/// The persisted shape of a binding.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BindingRecord {
    credential_id: String,
    backend: String,
}

impl Serialize for CredentialBinding {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        BindingRecord {
            credential_id: self.id.as_str().to_owned(),
            backend: self.backend.as_str().to_owned(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CredentialBinding {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let record = BindingRecord::deserialize(deserializer)?;
        Ok(Self {
            id: CredentialId::parse(&record.credential_id).map_err(serde::de::Error::custom)?,
            backend: CredentialBackend::parse(&record.backend)
                .ok_or_else(|| serde::de::Error::custom("unknown credential backend"))?,
        })
    }
}

/// Serde helpers for a recorded credential identifier.
pub(super) mod id_text {
    use super::CredentialId;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(crate) fn serialize<S: Serializer>(
        id: &CredentialId,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(id.as_str())
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<CredentialId, D::Error> {
        let text = String::deserialize(deserializer)?;
        CredentialId::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// Serde helpers for an optional recorded credential identifier.
pub(super) mod optional_id_text {
    use super::CredentialId;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(crate) fn serialize<S: Serializer>(
        id: &Option<CredentialId>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match id {
            Some(id) => serializer.serialize_some(id.as_str()),
            None => serializer.serialize_none(),
        }
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<CredentialId>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|text| CredentialId::parse(&text).map_err(serde::de::Error::custom))
            .transpose()
    }
}

/// Operator-facing binding state: `{"state":"none"}` or
/// `{"state":"bound","id":...,"backend":...}`.
pub(super) fn serialize_state<S: Serializer>(
    binding: &Option<CredentialBinding>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    #[derive(Serialize)]
    #[serde(tag = "state", rename_all = "snake_case")]
    enum State<'a> {
        None,
        Bound { id: &'a str, backend: &'static str },
    }
    match binding {
        None => State::None,
        Some(binding) => State::Bound {
            id: binding.id.as_str(),
            backend: binding.backend.as_str(),
        },
    }
    .serialize(serializer)
}

/// A native item that is no longer bound but could not be deleted. The
/// operator removes it outside OMGCode; its identifier is not secret.
#[derive(Debug)]
pub struct OrphanedCredential {
    /// The unbound credential's identifier.
    pub id: CredentialId,
    /// Why it was not deleted.
    pub error: CredentialError,
}

/// The outcome of storing and binding a new credential.
#[derive(Debug)]
#[must_use]
pub struct ProviderCredentialSet {
    /// The profile that now holds the binding.
    pub profile_id: Uuid,
    /// The new binding.
    pub binding: CredentialBinding,
    /// The binding this one replaced, if any.
    pub replaced: Option<CredentialId>,
    /// The replaced credential, when its native item could not be deleted.
    pub orphan: Option<OrphanedCredential>,
}

/// The outcome of removing a binding and its native item.
#[derive(Debug)]
#[must_use]
pub struct ProviderCredentialRemoval {
    /// The profile whose binding was removed.
    pub profile_id: Uuid,
    /// The removed credential's identifier.
    pub removed: CredentialId,
    /// The removed credential, when its native item could not be deleted.
    pub orphan: Option<OrphanedCredential>,
}

/// The outcome of removing a provider profile.
#[derive(Debug)]
#[must_use]
pub struct ProviderProfileRemoval {
    /// The removed profile.
    pub profile_id: Uuid,
    /// The credential whose binding the same event removed, if any. The
    /// caller deletes its native item with [`delete_unbound_credential`].
    pub credential: Option<CredentialBinding>,
}

/// A credential command failure. Journal and native-store categories stay
/// distinct, and no message contains secret bytes.
#[derive(Debug, Error)]
pub enum ProviderCredentialError {
    /// The settings journal refused or failed the change.
    #[error(transparent)]
    Control(#[from] ControlError),
    /// The native store refused or failed. Nothing was recorded.
    #[error(transparent)]
    Credential(#[from] CredentialError),
    /// The new credential was stored, its binding was not recorded, and the
    /// new native item could not be deleted. It is an unbound orphan.
    #[error(
        "the binding was not recorded ({error}), and the new credential {credential_id} could not be deleted: {cleanup}"
    )]
    Unrecorded {
        /// Why the binding was not recorded.
        #[source]
        error: ControlError,
        /// The orphaned credential's identifier.
        credential_id: CredentialId,
        /// Why it could not be deleted.
        cleanup: CredentialError,
    },
}

impl ControlStore {
    /// Store `secret` as a new credential and bind it to the named `api_key`
    /// profile, then delete any credential it replaces.
    ///
    /// The order keeps an interrupted run recoverable: the native store, then
    /// the journal binding, then the old item's deletion. A refused or failed
    /// binding deletes the new item; an uncertain journal write can therefore
    /// leave a recorded binding whose item is absent, which loading reports
    /// as not found. A failed old-item deletion is returned as an orphan,
    /// never as an error that hides the new binding.
    ///
    /// # Errors
    ///
    /// Revision, profile, and authentication-mode checks run before any
    /// native I/O and return [`ProviderCredentialError::Control`].
    pub fn set_provider_credential(
        &mut self,
        credentials: &CredentialStore,
        name: &str,
        secret: &Secret,
        expected_revision: u64,
    ) -> Result<ProviderCredentialSet, ProviderCredentialError> {
        let profile_id = self.credential_profile(name, expected_revision)?;
        let id = CredentialId::parse(&Uuid::new_v4().simple().to_string())?;
        credentials.store(&id, secret)?;
        let binding = CredentialBinding {
            id: id.clone(),
            backend: credentials.backend(),
        };
        let replaced = match self.bind_credential(profile_id, binding.clone()) {
            Ok(replaced) => replaced,
            Err(error) => {
                return Err(match credentials.delete(&id) {
                    Ok(()) => ProviderCredentialError::Control(error),
                    Err(cleanup) => ProviderCredentialError::Unrecorded {
                        error,
                        credential_id: id,
                        cleanup,
                    },
                });
            }
        };
        let orphan = replaced
            .as_ref()
            .and_then(|old| delete_unbound_credential(credentials, old));
        Ok(ProviderCredentialSet {
            profile_id,
            binding,
            replaced: replaced.map(|old| old.id),
            orphan,
        })
    }

    /// Record that the named profile no longer has a credential, then delete
    /// the native item. A failed deletion is returned as an orphan.
    ///
    /// # Errors
    ///
    /// Refuses a stale revision, an unknown profile, or a profile without a
    /// binding before any native I/O.
    pub fn remove_provider_credential(
        &mut self,
        credentials: &CredentialStore,
        name: &str,
        expected_revision: u64,
    ) -> Result<ProviderCredentialRemoval, ProviderCredentialError> {
        let (profile_id, binding) = self.unbind_credential(name, expected_revision)?;
        let orphan = delete_unbound_credential(credentials, &binding);
        Ok(ProviderCredentialRemoval {
            profile_id,
            removed: binding.id,
            orphan,
        })
    }

    /// Check the revision and that the profile accepts a credential.
    fn credential_profile(&self, name: &str, expected_revision: u64) -> Result<Uuid, ControlError> {
        self.check_revision(expected_revision)?;
        let status = self
            .provider_profile(name)
            .ok_or(ControlError::UnknownProvider)?;
        require_api_key(status.profile().auth())?;
        Ok(status.profile().id())
    }

    /// Record a binding under the next revision. The caller has already
    /// checked the revision with [`Self::credential_profile`] while holding
    /// this store's lock.
    fn bind_credential(
        &mut self,
        profile_id: Uuid,
        binding: CredentialBinding,
    ) -> Result<Option<CredentialBinding>, ControlError> {
        let replaced = self
            .snapshot
            .provider_profiles
            .iter()
            .find(|status| status.profile().id() == profile_id)
            .ok_or(ControlError::UnknownProvider)?
            .credential
            .clone();
        self.append(
            Action::ProviderCredentialBound {
                profile_id,
                binding,
            },
            self.next_revision()?,
        )?;
        Ok(replaced)
    }

    /// Record that the named profile's binding is removed.
    fn unbind_credential(
        &mut self,
        name: &str,
        expected_revision: u64,
    ) -> Result<(Uuid, CredentialBinding), ControlError> {
        self.check_revision(expected_revision)?;
        let status = self
            .provider_profile(name)
            .ok_or(ControlError::UnknownProvider)?;
        let profile_id = status.profile().id();
        let binding = status
            .credential
            .clone()
            .ok_or(ControlError::ProviderRefused(
                "no credential is bound to this provider profile",
            ))?;
        self.append(
            Action::ProviderCredentialRemoved {
                profile_id,
                credential_id: binding.id.clone(),
            },
            self.next_revision()?,
        )?;
        Ok((profile_id, binding))
    }
}

/// Delete an unbound credential's native item. An already absent item is not
/// an orphan. An item recorded by another backend cannot be reached here and
/// is returned as an orphan.
pub fn delete_unbound_credential(
    credentials: &CredentialStore,
    binding: &CredentialBinding,
) -> Option<OrphanedCredential> {
    if binding.backend != credentials.backend() {
        return Some(OrphanedCredential {
            id: binding.id.clone(),
            error: CredentialError::Unavailable(format!(
                "the credential was stored by {}, not {}",
                binding.backend.as_str(),
                credentials.backend().as_str()
            )),
        });
    }
    match credentials.delete(&binding.id) {
        Ok(()) | Err(CredentialError::NotFound) => None,
        Err(error) => Some(OrphanedCredential {
            id: binding.id.clone(),
            error,
        }),
    }
}

/// Only API-key profiles take a stored credential in this increment.
fn require_api_key(auth: ProviderAuth) -> Result<(), ControlError> {
    match auth {
        ProviderAuth::ApiKey => Ok(()),
        ProviderAuth::Subscription => Err(ControlError::ProviderRefused(
            "subscription credentials are not supported yet; only api_key profiles accept a stored credential",
        )),
        ProviderAuth::None => Err(ControlError::ProviderRefused(
            "this provider profile uses no credential; only api_key profiles accept a stored credential",
        )),
    }
}

/// Validate and apply one credential action during replay or append preflight.
pub(super) fn apply(state: &mut ControlSnapshot, action: &Action) -> Result<(), ControlError> {
    match action {
        Action::ProviderCredentialBound {
            profile_id,
            binding,
        } => {
            if state.provider_profiles.iter().any(|status| {
                status.profile().id() != *profile_id
                    && status
                        .credential
                        .as_ref()
                        .is_some_and(|other| other.id == binding.id)
            }) {
                return Err(ControlError::Invalid(
                    "a credential is bound to more than one provider profile",
                ));
            }
            let status = state
                .provider_profiles
                .iter_mut()
                .find(|status| status.profile().id() == *profile_id)
                .ok_or(ControlError::Invalid(
                    "credential binding for an unknown provider profile",
                ))?;
            if status.profile().auth() != ProviderAuth::ApiKey
                || status
                    .credential
                    .as_ref()
                    .is_some_and(|current| current.id == binding.id)
            {
                return Err(ControlError::Invalid("invalid provider credential binding"));
            }
            status.credential = Some(binding.clone());
        }
        Action::ProviderCredentialRemoved {
            profile_id,
            credential_id,
        } => {
            let status = state
                .provider_profiles
                .iter_mut()
                .find(|status| status.profile().id() == *profile_id)
                .ok_or(ControlError::Invalid(
                    "credential removal for an unknown provider profile",
                ))?;
            if status.credential.as_ref().map(CredentialBinding::id) != Some(credential_id) {
                return Err(ControlError::Invalid(
                    "credential removal does not match the recorded binding",
                ));
            }
            status.credential = None;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests;
