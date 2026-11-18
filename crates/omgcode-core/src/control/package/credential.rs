//! Registry credential bindings for package-network profiles. See ADR-0056.
//!
//! A binding names one profile, one of that profile's origins, an
//! authentication scheme, and an opaque credential identifier. The journal
//! never records a secret. Secrets live only in the platform credential store,
//! and a credential is never overwritten: rotation stores a new credential,
//! records the binding, and only then deletes the old item, with the same
//! recovery rules as provider credentials (ADR-0042).
//!
//! Only the trusted `packages fetch` command reads a bound credential, and it
//! sends it to the one origin the binding names ([`ControlStore::load_package_credential`]).
//! No run, model tool, or provider adapter reads it.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::{PackageOrigin, parse_origin, refused};
use crate::control::credential::{
    CredentialBackend, CredentialBinding, CredentialError, CredentialId, CredentialStore,
    OrphanedCredential, ProviderCredentialError, Secret, binding_write_uncertain,
    delete_unbound_credential, store_failure,
};
use crate::control::{Action, ControlError, ControlSnapshot, ControlStore};

/// How a bound registry credential is sent.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageAuth {
    /// `Authorization: Bearer <secret>`.
    Bearer,
    /// `Authorization: Basic <base64(secret)>`; the secret is `user:password`.
    Basic,
}

impl PackageAuth {
    /// The scheme as the operator spells it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bearer => "bearer",
            Self::Basic => "basic",
        }
    }

    /// Parse an operator's spelling.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "bearer" => Some(Self::Bearer),
            "basic" => Some(Self::Basic),
            _ => None,
        }
    }
}

/// One credential bound to one origin of a package-network profile.
#[derive(Clone, Debug, Serialize)]
pub struct PackageCredentialStatus {
    origin: String,
    auth: PackageAuth,
    binding: CredentialBinding,
}

impl PackageCredentialStatus {
    /// The origin's canonical text ([`PackageOrigin::render`]).
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// How the credential is sent.
    pub const fn auth(&self) -> PackageAuth {
        self.auth
    }

    /// The opaque credential identifier and its backend.
    pub const fn binding(&self) -> &CredentialBinding {
        &self.binding
    }
}

/// The outcome of removing a package profile.
#[derive(Debug)]
#[must_use]
pub struct PackageProfileRemoval {
    /// The removed profile.
    pub profile_id: Uuid,
    /// The bindings the same event ended. The caller deletes each native
    /// item with [`delete_unbound_credential`].
    pub credentials: Vec<CredentialBinding>,
}

/// The outcome of storing and binding a new registry credential.
#[derive(Debug)]
#[must_use]
pub struct PackageCredentialSet {
    /// The profile that now holds the binding.
    pub profile_id: Uuid,
    /// The origin's canonical text.
    pub origin: String,
    /// How the credential is sent.
    pub auth: PackageAuth,
    /// The new binding.
    pub binding: CredentialBinding,
    /// The credential this binding replaced, if any.
    pub replaced: Option<CredentialId>,
    /// The replaced credential, when its native item could not be deleted.
    pub orphan: Option<OrphanedCredential>,
}

/// The outcome of removing a binding and its native item.
#[derive(Debug)]
#[must_use]
pub struct PackageCredentialRemoval {
    /// The profile whose binding was removed.
    pub profile_id: Uuid,
    /// The origin's canonical text.
    pub origin: String,
    /// The removed credential's identifier.
    pub removed: CredentialId,
    /// The removed credential, when its native item could not be deleted.
    pub orphan: Option<OrphanedCredential>,
}

/// A registry credential command failure. Journal and native-store
/// categories stay distinct, and no message contains secret bytes.
#[derive(Debug, Error)]
pub enum PackageCredentialError {
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
    /// The native store failed after it may have created an item for the new
    /// identifier, and that item could not be deleted or confirmed absent.
    #[error(
        "the credential was not stored ({error}), and an item for the new credential {credential_id} may remain and could not be deleted: {cleanup}"
    )]
    Unstored {
        /// Why the store failed.
        #[source]
        error: CredentialError,
        /// The identifier any remaining item has.
        credential_id: CredentialId,
        /// Why it could not be deleted.
        cleanup: CredentialError,
    },
    /// The new credential was stored, but the binding's journal write or
    /// sync failed after it began, so the binding may or may not be
    /// recorded. The new item is kept and any replaced credential was not
    /// deleted.
    #[error(
        "the binding of the new credential {credential_id} may or may not be recorded ({error}); the credential was kept. Run package-profile-show: if it shows this identifier, keep it or remove it with package-credential-remove; otherwise delete the unbound item by hand"
    )]
    BindingUncertain {
        /// Why the binding write is uncertain.
        #[source]
        error: ControlError,
        /// The kept credential's identifier.
        credential_id: CredentialId,
        /// The credential that was bound before, if any. If the new binding
        /// is recorded, it is no longer bound and was not deleted.
        replaced: Option<CredentialId>,
    },
}

impl From<ProviderCredentialError> for PackageCredentialError {
    fn from(error: ProviderCredentialError) -> Self {
        match error {
            ProviderCredentialError::Control(error) => Self::Control(error),
            ProviderCredentialError::Credential(error) => Self::Credential(error),
            ProviderCredentialError::Unrecorded {
                error,
                credential_id,
                cleanup,
            } => Self::Unrecorded {
                error,
                credential_id,
                cleanup,
            },
            ProviderCredentialError::Unstored {
                error,
                credential_id,
                cleanup,
            } => Self::Unstored {
                error,
                credential_id,
                cleanup,
            },
            ProviderCredentialError::BindingUncertain {
                error,
                credential_id,
                replaced,
            } => Self::BindingUncertain {
                error,
                credential_id,
                replaced,
            },
        }
    }
}

/// A refusal or failure to read a bound registry credential for a fetch.
///
/// No message contains secret bytes; an opaque credential identifier may
/// appear. A native failure keeps its category.
#[derive(Debug, Error)]
pub enum PackageCredentialUseError {
    /// The settings store cannot be used.
    #[error("the settings store cannot be used: {0}")]
    Settings(#[source] ControlError),
    /// The profile is no longer recorded.
    #[error("the package profile is not recorded")]
    UnknownProfile,
    /// The bound credential was stored by a different backend than the one
    /// this host opened.
    #[error(
        "credential {credential_id} was stored by {}, not {}",
        .stored.as_str(),
        .current.as_str()
    )]
    OtherBackend {
        /// The bound credential's identifier.
        credential_id: CredentialId,
        /// The backend recorded in the binding.
        stored: CredentialBackend,
        /// The backend of the open credential store.
        current: CredentialBackend,
    },
    /// The native store could not return the bound credential.
    #[error("credential {credential_id} could not be read: {error}")]
    Store {
        /// The bound credential's identifier.
        credential_id: CredentialId,
        /// The native failure, with its category.
        #[source]
        error: CredentialError,
    },
}

/// A registry credential read for one fetch. The secret zeroes itself on drop
/// and `Debug` redacts it.
pub struct LoadedPackageCredential {
    auth: PackageAuth,
    credential_id: CredentialId,
    secret: Secret,
}

impl LoadedPackageCredential {
    /// How the credential is sent.
    pub const fn auth(&self) -> PackageAuth {
        self.auth
    }

    /// The credential's opaque identifier.
    pub const fn credential_id(&self) -> &CredentialId {
        &self.credential_id
    }

    /// The secret. It must not be logged, displayed, or persisted.
    pub const fn secret(&self) -> &Secret {
        &self.secret
    }
}

impl std::fmt::Debug for LoadedPackageCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoadedPackageCredential")
            .field("auth", &self.auth)
            .field("credential_id", &self.credential_id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// Check a secret before anything is stored: visible ASCII only, no control
/// characters or line breaks, no leading or trailing whitespace. A bearer
/// token has no space at all; a basic secret is `user:password` with a
/// nonempty user name before the first colon.
///
/// # Errors
///
/// Returns [`ControlError::PackageProfileRefused`] with a fixed reason that
/// never contains the secret.
pub fn validate_package_secret(auth: PackageAuth, secret: &Secret) -> Result<(), ControlError> {
    let bytes = secret.expose();
    let (Some(first), Some(last)) = (bytes.first(), bytes.last()) else {
        return Err(refused("a registry credential must not be empty"));
    };
    if bytes.len() > crate::control::MAX_SECRET_BYTES {
        return Err(refused("a registry credential is too long"));
    }
    if !bytes.iter().all(|byte| (0x20..=0x7e).contains(byte)) {
        return Err(refused(
            "a registry credential must be visible ASCII without control characters or line breaks",
        ));
    }
    if *first == b' ' || *last == b' ' {
        return Err(refused(
            "a registry credential must not start or end with whitespace",
        ));
    }
    match auth {
        PackageAuth::Bearer => {
            if bytes.contains(&b' ') {
                return Err(refused("a bearer token must not contain whitespace"));
            }
        }
        PackageAuth::Basic => {
            let user = bytes.split(|byte| *byte == b':').next().unwrap_or_default();
            if !bytes.contains(&b':') || user.is_empty() {
                return Err(refused(
                    "a basic credential must be user:password with a nonempty user",
                ));
            }
        }
    }
    Ok(())
}

impl ControlStore {
    /// Store `secret` as a new credential and bind it to `origin` of the named
    /// package profile, then delete any credential it replaces.
    ///
    /// The order and the recovery rules are those of
    /// [`ControlStore::set_provider_credential`]: the native store, then the
    /// journal binding, then the old item's deletion. A binding refused before
    /// any journal write deletes the new item; a binding whose journal write
    /// may have happened keeps it and reports
    /// [`PackageCredentialError::BindingUncertain`].
    ///
    /// # Errors
    ///
    /// The revision, profile, origin, and secret checks run before any native
    /// I/O and return [`PackageCredentialError::Control`].
    pub fn set_package_credential(
        &mut self,
        credentials: &CredentialStore,
        name: &str,
        origin: &str,
        auth: PackageAuth,
        secret: &Secret,
        expected_revision: u64,
    ) -> Result<PackageCredentialSet, PackageCredentialError> {
        self.check_revision(expected_revision)?;
        let (profile_id, origin) = self.credential_target(name, origin)?;
        validate_package_secret(auth, secret)?;
        let replaced = self.bound_package_credential(profile_id, &origin);
        let id = CredentialId::parse(&Uuid::new_v4().simple().to_string())?;
        if let Err(error) = credentials.store(&id, secret) {
            return Err(store_failure(credentials, id, error).into());
        }
        let binding = CredentialBinding::new(id.clone(), credentials.backend());
        let appended = self.next_revision().and_then(|revision| {
            self.append(
                Action::PackageCredentialBound {
                    profile_id,
                    origin: origin.clone(),
                    auth,
                    binding: binding.clone(),
                },
                revision,
            )
        });
        if let Err(error) = appended {
            if binding_write_uncertain(&error) {
                return Err(PackageCredentialError::BindingUncertain {
                    error,
                    credential_id: id,
                    replaced: replaced.map(|old| old.id().clone()),
                });
            }
            return Err(match credentials.delete(&id) {
                Ok(()) | Err(CredentialError::NotFound) => PackageCredentialError::Control(error),
                Err(cleanup) => PackageCredentialError::Unrecorded {
                    error,
                    credential_id: id,
                    cleanup,
                },
            });
        }
        let orphan = replaced
            .as_ref()
            .and_then(|old| delete_unbound_credential(credentials, old));
        Ok(PackageCredentialSet {
            profile_id,
            origin,
            auth,
            binding,
            replaced: replaced.map(|old| old.id().clone()),
            orphan,
        })
    }

    /// Record that `origin` of the named profile no longer has a credential,
    /// then delete the native item. A failed deletion is returned as an
    /// orphan.
    ///
    /// # Errors
    ///
    /// Refuses a stale revision, an unknown profile or origin, or an origin
    /// without a binding before any native I/O.
    pub fn remove_package_credential(
        &mut self,
        credentials: &CredentialStore,
        name: &str,
        origin: &str,
        expected_revision: u64,
    ) -> Result<PackageCredentialRemoval, PackageCredentialError> {
        self.check_revision(expected_revision)?;
        let (profile_id, origin) = self.credential_target(name, origin)?;
        let binding = self
            .bound_package_credential(profile_id, &origin)
            .ok_or_else(|| refused("no credential is bound to this registry origin"))?;
        self.append(
            Action::PackageCredentialRemoved {
                profile_id,
                origin: origin.clone(),
                credential_id: binding.id().clone(),
            },
            self.next_revision()?,
        )?;
        let orphan = delete_unbound_credential(credentials, &binding);
        Ok(PackageCredentialRemoval {
            profile_id,
            origin,
            removed: binding.id().clone(),
            orphan,
        })
    }

    /// Read the credential bound to `origin` of profile `profile_id` for one
    /// fetch, or `None` when the origin has no binding. This takes the origin
    /// the fetch matched, so a credential is never paired with another
    /// destination.
    ///
    /// The caller must keep this `ControlStore` open across the call, so a
    /// rotation (which needs `&mut self` and the journal lock) cannot delete
    /// the item between the lookup and the load, and must drop it before any
    /// network I/O. A read records nothing.
    ///
    /// # Errors
    ///
    /// See [`PackageCredentialUseError`]. A binding that cannot be read is an
    /// error, never `None`: the caller must not send an unauthenticated
    /// request in its place.
    pub fn load_package_credential(
        &self,
        credentials: &CredentialStore,
        profile_id: Uuid,
        origin: &PackageOrigin,
    ) -> Result<Option<LoadedPackageCredential>, PackageCredentialUseError> {
        self.ensure_healthy()
            .map_err(PackageCredentialUseError::Settings)?;
        let status = self
            .package_profiles()
            .iter()
            .find(|status| status.profile().id() == profile_id)
            .ok_or(PackageCredentialUseError::UnknownProfile)?;
        let Some(bound) = status.credential_for(origin) else {
            return Ok(None);
        };
        let binding = bound.binding();
        if binding.backend() != credentials.backend() {
            return Err(PackageCredentialUseError::OtherBackend {
                credential_id: binding.id().clone(),
                stored: binding.backend(),
                current: credentials.backend(),
            });
        }
        let secret =
            credentials
                .load(binding.id())
                .map_err(|error| PackageCredentialUseError::Store {
                    credential_id: binding.id().clone(),
                    error,
                })?;
        Ok(Some(LoadedPackageCredential {
            auth: bound.auth(),
            credential_id: binding.id().clone(),
            secret,
        }))
    }

    /// Resolve the profile and one of its origins, by the origin's canonical
    /// text. The origin must be one the profile declares.
    fn credential_target(&self, name: &str, origin: &str) -> Result<(Uuid, String), ControlError> {
        let status = self
            .package_profile(name)
            .ok_or(ControlError::UnknownPackageProfile)?;
        let text = parse_origin(origin)?.render();
        if !status
            .profile()
            .origins()
            .iter()
            .any(|declared| declared.render() == text)
        {
            return Err(refused(
                "the origin is not one declared by this package-network profile",
            ));
        }
        Ok((status.profile().id(), text))
    }

    /// The binding an origin holds now, which a new binding would replace.
    fn bound_package_credential(
        &self,
        profile_id: Uuid,
        origin: &str,
    ) -> Option<CredentialBinding> {
        self.package_profiles()
            .iter()
            .find(|status| status.profile().id() == profile_id)?
            .credentials
            .iter()
            .find(|credential| credential.origin == origin)
            .map(|credential| credential.binding.clone())
    }
}

/// Validate and apply one package-credential action during replay or append
/// preflight.
pub(super) fn apply(state: &mut ControlSnapshot, action: &Action) -> Result<(), ControlError> {
    match action {
        Action::PackageCredentialBound {
            profile_id,
            origin,
            auth,
            binding,
        } => {
            let bound_elsewhere = state
                .package_profiles
                .iter()
                .any(|status| status.binds_credential(binding.id()))
                || state.provider_profiles.iter().any(|status| {
                    status
                        .credential()
                        .is_some_and(|other| other.id() == binding.id())
                });
            if bound_elsewhere {
                return Err(ControlError::Invalid(
                    "a credential is already bound to a package origin or provider profile",
                ));
            }
            let status = state
                .package_profiles
                .iter_mut()
                .find(|status| status.profile().id() == *profile_id)
                .ok_or(ControlError::Invalid(
                    "credential binding for an unknown package profile",
                ))?;
            if !status
                .profile()
                .origins()
                .iter()
                .any(|declared| declared.render() == *origin)
            {
                return Err(ControlError::Invalid(
                    "credential binding for an origin the profile does not declare",
                ));
            }
            let entry = PackageCredentialStatus {
                origin: origin.clone(),
                auth: *auth,
                binding: binding.clone(),
            };
            match status
                .credentials
                .iter_mut()
                .find(|current| current.origin == *origin)
            {
                Some(current) => *current = entry,
                None => status.credentials.push(entry),
            }
        }
        Action::PackageCredentialRemoved {
            profile_id,
            origin,
            credential_id,
        } => {
            let status = state
                .package_profiles
                .iter_mut()
                .find(|status| status.profile().id() == *profile_id)
                .ok_or(ControlError::Invalid(
                    "credential removal for an unknown package profile",
                ))?;
            let index = status
                .credentials
                .iter()
                .position(|current| {
                    current.origin == *origin && current.binding.id() == credential_id
                })
                .ok_or(ControlError::Invalid(
                    "credential removal does not match the recorded binding",
                ))?;
            status.credentials.remove(index);
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests;
