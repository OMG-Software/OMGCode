//! Credentials as freedesktop Secret Service items in the default collection.
//!
//! Each credential is one item whose attributes name this application and the
//! credential identifier. Secrets cross the session bus in a Diffie-Hellman
//! session with AES (the `secret-service` crate's Rust crypto). OMGCode never
//! unlocks a collection or answers a prompt: a locked collection is reported
//! as locked before any call that could prompt. Exactly one item must match an
//! identifier; more than one is corrupt. Any process of the same user can
//! read these items. See ADR-0042.

use std::collections::HashMap;

use secret_service::blocking::{Collection, SecretService};
use secret_service::{EncryptionType, Error};

use super::{CredentialBackend, CredentialError, CredentialId, CredentialStoreStatus, Secret};

/// The application attribute and its value.
pub(super) const APPLICATION_ATTRIBUTE: &str = "application";
pub(super) const APPLICATION: &str = "omgcode";
/// The attribute holding the credential identifier.
pub(super) const ID_ATTRIBUTE: &str = "omgcode_credential";
/// The stored secret's content type.
pub(super) const CONTENT_TYPE: &str = "application/octet-stream";

/// A Secret Service store. Each operation opens its own encrypted session.
pub(crate) struct SecretServiceStore {
    _private: (),
}

impl SecretServiceStore {
    /// Check that the service answers and its default collection is unlocked.
    pub(super) fn open() -> Result<Self, CredentialError> {
        let service = connect()?;
        let collection = default_collection(&service)?;
        require_unlocked(&collection)?;
        Ok(Self { _private: () })
    }

    pub(super) fn store(&self, id: &CredentialId, secret: &Secret) -> Result<(), CredentialError> {
        let service = connect()?;
        let collection = default_collection(&service)?;
        // Creating an item in a locked collection would start an unlock
        // prompt inside the crate, so refuse first.
        require_unlocked(&collection)?;
        if !collection
            .search_items(attributes(id))
            .map_err(service_error)?
            .is_empty()
        {
            return Err(CredentialError::AlreadyExists);
        }
        collection
            .create_item(
                &label(id),
                attributes(id),
                secret.expose(),
                false,
                CONTENT_TYPE,
            )
            .map_err(service_error)?;
        Ok(())
    }

    pub(super) fn load(&self, id: &CredentialId) -> Result<Secret, CredentialError> {
        let service = connect()?;
        let collection = default_collection(&service)?;
        require_unlocked(&collection)?;
        let items = collection
            .search_items(attributes(id))
            .map_err(service_error)?;
        let item = match items.as_slice() {
            [] => return Err(CredentialError::NotFound),
            [item] => item,
            _ => {
                return Err(CredentialError::Corrupt(
                    "more than one item has this identifier",
                ));
            }
        };
        if item.is_locked().map_err(service_error)? {
            return Err(CredentialError::Locked);
        }
        let bytes = item.get_secret().map_err(service_error)?;
        Secret::new(bytes)
            .map_err(|_| CredentialError::Corrupt("stored credential is empty or too large"))
    }

    pub(super) fn delete(&self, id: &CredentialId) -> Result<(), CredentialError> {
        let service = connect()?;
        let collection = default_collection(&service)?;
        require_unlocked(&collection)?;
        let items = collection
            .search_items(attributes(id))
            .map_err(service_error)?;
        let item = match items.as_slice() {
            [] => return Err(CredentialError::NotFound),
            [item] => item,
            _ => {
                return Err(CredentialError::Corrupt(
                    "more than one item has this identifier",
                ));
            }
        };
        // `delete` refuses a locked item without prompting.
        item.delete().map_err(service_error)
    }
}

/// Report availability without creating, unlocking, or reading anything.
pub(super) fn probe() -> CredentialStoreStatus {
    let result = connect().and_then(|service| {
        let collection = default_collection(&service)?;
        collection.is_locked().map_err(service_error)
    });
    match result {
        Ok(false) => CredentialStoreStatus::Available(CredentialBackend::LinuxSecretService),
        Ok(true) | Err(CredentialError::Locked) => {
            CredentialStoreStatus::Locked(CredentialBackend::LinuxSecretService)
        }
        Err(CredentialError::Unavailable(reason)) => CredentialStoreStatus::Unavailable(reason),
        Err(error) => CredentialStoreStatus::Unavailable(error.to_string()),
    }
}

/// The exact attributes of one credential's item.
pub(super) fn attributes(id: &CredentialId) -> HashMap<&str, &str> {
    HashMap::from([
        (APPLICATION_ATTRIBUTE, APPLICATION),
        (ID_ATTRIBUTE, id.as_str()),
    ])
}

pub(super) fn label(id: &CredentialId) -> String {
    format!("OMGCode credential {id}")
}

/// Open an encrypted session on the user's session bus.
fn connect<'a>() -> Result<SecretService<'a>, CredentialError> {
    SecretService::connect(EncryptionType::Dh).map_err(|error| {
        CredentialError::Unavailable(format!(
            "the Secret Service on the session bus did not answer: {error}"
        ))
    })
}

/// The default collection. A missing alias is unavailable, never created.
fn default_collection<'a>(
    service: &'a SecretService<'a>,
) -> Result<Collection<'a>, CredentialError> {
    service
        .get_default_collection()
        .map_err(|error| match error {
            Error::NoResult => CredentialError::Unavailable(
                "the Secret Service has no default collection".to_owned(),
            ),
            error => service_error(error),
        })
}

fn require_unlocked(collection: &Collection<'_>) -> Result<(), CredentialError> {
    if collection.is_locked().map_err(service_error)? {
        Err(CredentialError::Locked)
    } else {
        Ok(())
    }
}

/// Keep lock and availability categories distinct from other service errors.
fn service_error(error: Error) -> CredentialError {
    match error {
        Error::Locked => CredentialError::Locked,
        Error::Unavailable => CredentialError::Unavailable(
            "the Secret Service is not available on the session bus".to_owned(),
        ),
        Error::Prompt | Error::PromptDisconnected => CredentialError::Locked,
        Error::Crypto(reason) => CredentialError::Corrupt(reason),
        error => CredentialError::Io(std::io::Error::other(error.to_string())),
    }
}

#[cfg(test)]
mod tests;
