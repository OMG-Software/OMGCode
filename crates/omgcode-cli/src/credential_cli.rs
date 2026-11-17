//! Trusted operator commands that store, bind, and remove provider
//! credentials. See ADR-0042.
//!
//! A secret is read only from standard input, never from an argument or the
//! environment, and is never echoed, logged, or written to a journal or
//! output. Output names only the profile, the opaque credential identifier,
//! and the backend. No provider adapter reads a bound credential yet.
#![forbid(unsafe_code)]

use omgcode_core::control::{
    ControlStore, CredentialError, CredentialStore, CredentialStoreStatus, OrphanedCredential,
    PackageAuth, PackageCredentialError, ProviderCredentialError, Secret,
    delete_unbound_credential,
};
use serde_json::{Value, json};
use std::io::{self, IsTerminal};
use std::path::Path;

use crate::{CliError, print_json};

/// Store `secret` as a new credential, bind it to `name`, and delete any
/// credential it replaces. The caller reads the secret with [`read_secret`]
/// before it opens the control store.
pub(crate) fn set(
    store: &mut ControlStore,
    data_root: &Path,
    name: &str,
    secret: Secret,
    revision: u64,
) -> Result<(), CliError> {
    let credentials = CredentialStore::open(data_root).map_err(credential_error)?;
    let set = store
        .set_provider_credential(&credentials, name, &secret, revision)
        .map_err(provider_credential_error)?;
    drop(secret);
    let mut output = json!({
        "provider_credential": {
            "profile": name,
            "profile_id": set.profile_id,
            "credential_id": set.binding.id().as_str(),
            "backend": set.binding.backend().as_str(),
            "replaced_credential_id": set.replaced.as_ref().map(|id| id.as_str()),
        },
        "snapshot": store.snapshot(),
    });
    add_orphan_warning(&mut output, set.orphan.as_ref());
    print_json(&output)
}

/// Record that `name` has no credential, then delete the native item.
pub(crate) fn remove(
    store: &mut ControlStore,
    data_root: &Path,
    name: &str,
    revision: u64,
) -> Result<(), CliError> {
    // Open the native store first, so an unavailable or locked store refuses
    // the command before anything is recorded.
    let credentials = CredentialStore::open(data_root).map_err(credential_error)?;
    let removal = store
        .remove_provider_credential(&credentials, name, revision)
        .map_err(provider_credential_error)?;
    let mut output = json!({
        "removed_provider_credential": {
            "profile": name,
            "profile_id": removal.profile_id,
            "credential_id": removal.removed.as_str(),
        },
        "snapshot": store.snapshot(),
    });
    add_orphan_warning(&mut output, removal.orphan.as_ref());
    print_json(&output)
}

/// Remove a provider profile. A bound credential's binding ends in the same
/// recorded event, and its native item is deleted afterwards.
pub(crate) fn remove_profile(
    store: &mut ControlStore,
    data_root: &Path,
    name: &str,
    revision: u64,
) -> Result<Value, CliError> {
    let bound = store
        .provider_profile(name)
        .is_some_and(|status| status.credential().is_some());
    let credentials = if bound {
        Some(CredentialStore::open(data_root).map_err(credential_error)?)
    } else {
        None
    };
    let removal = store
        .remove_provider_profile(name, revision)
        .map_err(crate::control_cli::control_error)?;
    let orphan = match (&removal.credential, &credentials) {
        (Some(binding), Some(credentials)) => delete_unbound_credential(credentials, binding),
        (Some(binding), None) => Some(OrphanedCredential {
            id: binding.id().clone(),
            error: CredentialError::Unavailable("the credential store was not opened".to_owned()),
        }),
        (None, _) => None,
    };
    let mut output = json!({
        "removed_provider_profile_id": removal.profile_id,
        "removed_credential_id": removal.credential.as_ref().map(|binding| binding.id().as_str()),
        "snapshot": store.snapshot(),
    });
    add_orphan_warning(&mut output, orphan.as_ref());
    Ok(output)
}

/// Store `secret` as a new registry credential, bind it to one declared
/// origin of the package profile `name`, and delete any credential it
/// replaces. The caller reads the secret with [`read_secret`] before it opens
/// the control store. The secret is validated before anything is stored.
pub(crate) fn set_package(
    store: &mut ControlStore,
    data_root: &Path,
    name: &str,
    origin: &str,
    auth: &str,
    secret: Secret,
    revision: u64,
) -> Result<(), CliError> {
    let auth = PackageAuth::parse(auth)
        .ok_or_else(|| CliError::new("configuration", "Authentication must be bearer or basic"))?;
    let credentials = CredentialStore::open(data_root).map_err(credential_error)?;
    let set = store
        .set_package_credential(&credentials, name, origin, auth, &secret, revision)
        .map_err(package_credential_error)?;
    drop(secret);
    let mut output = json!({
        "package_credential": {
            "profile": name,
            "profile_id": set.profile_id,
            "origin": set.origin,
            "auth": set.auth.as_str(),
            "credential_id": set.binding.id().as_str(),
            "backend": set.binding.backend().as_str(),
            "replaced_credential_id": set.replaced.as_ref().map(|id| id.as_str()),
        },
        "snapshot": store.snapshot(),
    });
    add_orphan_warning(&mut output, set.orphan.as_ref());
    print_json(&output)
}

/// Record that one origin of the package profile `name` has no credential,
/// then delete the native item.
pub(crate) fn remove_package(
    store: &mut ControlStore,
    data_root: &Path,
    name: &str,
    origin: &str,
    revision: u64,
) -> Result<(), CliError> {
    // Open the native store first, so an unavailable or locked store refuses
    // the command before anything is recorded.
    let credentials = CredentialStore::open(data_root).map_err(credential_error)?;
    let removal = store
        .remove_package_credential(&credentials, name, origin, revision)
        .map_err(package_credential_error)?;
    let mut output = json!({
        "removed_package_credential": {
            "profile": name,
            "profile_id": removal.profile_id,
            "origin": removal.origin,
            "credential_id": removal.removed.as_str(),
        },
        "snapshot": store.snapshot(),
    });
    add_orphan_warning(&mut output, removal.orphan.as_ref());
    print_json(&output)
}

/// Remove a package profile. Every credential bound to its origins ends in
/// the same recorded event, and their native items are deleted afterwards.
pub(crate) fn remove_package_profile(
    store: &mut ControlStore,
    data_root: &Path,
    name: &str,
    revision: u64,
) -> Result<Value, CliError> {
    let bound = store
        .package_profile(name)
        .is_some_and(|status| !status.credentials().is_empty());
    let credentials = if bound {
        Some(CredentialStore::open(data_root).map_err(credential_error)?)
    } else {
        None
    };
    let removal = store
        .remove_package_profile(name, revision)
        .map_err(crate::control_cli::control_error)?;
    let orphans: Vec<OrphanedCredential> = removal
        .credentials
        .iter()
        .filter_map(|binding| match &credentials {
            Some(credentials) => delete_unbound_credential(credentials, binding),
            None => Some(OrphanedCredential {
                id: binding.id().clone(),
                error: CredentialError::Unavailable(
                    "the credential store was not opened".to_owned(),
                ),
            }),
        })
        .collect();
    let mut output = json!({
        "removed_package_profile_id": removal.profile_id,
        "removed_credential_ids": removal
            .credentials
            .iter()
            .map(|binding| binding.id().as_str())
            .collect::<Vec<_>>(),
        "snapshot": store.snapshot(),
    });
    if let (Some(first), Some(object)) = (orphans.first(), output.as_object_mut()) {
        object.insert(
            "warning".to_owned(),
            json!({
                "code": "credential_orphaned",
                "credential_ids": orphans.iter().map(|orphan| orphan.id.as_str()).collect::<Vec<_>>(),
                "message": format!(
                    "{} credential(s) are no longer bound, but their native items could not be deleted (first: {}). Remove them manually.",
                    orphans.len(),
                    first.error
                ),
            }),
        );
    }
    Ok(output)
}

/// The `credential_store` capability: a backend name, or `unavailable` with
/// its reason. Probing creates, unlocks, and reads nothing.
pub(crate) fn capability() -> Value {
    match omgcode_core::control::probe_credential_store() {
        CredentialStoreStatus::Available(backend) => {
            json!({"backend": backend.as_str(), "state": "available"})
        }
        CredentialStoreStatus::Locked(backend) => json!({
            "backend": backend.as_str(),
            "state": "locked",
            "reason": "the default collection is locked; unlock it outside OMGCode",
        }),
        CredentialStoreStatus::Unavailable(reason) => {
            json!({"backend": "unavailable", "state": "unavailable", "reason": reason})
        }
    }
}

/// Read the secret from piped standard input. A terminal is refused so the
/// secret is not echoed while typed. Call it before opening the control
/// store, so no lock is held while input is awaited.
pub(crate) fn read_secret() -> Result<Secret, CliError> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        return Err(CliError::new(
            "credential_input",
            "Pipe the secret on standard input; terminal input would echo it",
        ));
    }
    Secret::read_from(stdin.lock()).map_err(|error| match error {
        CredentialError::Io(_) => CliError::new(
            "credential_input",
            "Could not read the secret from standard input",
        ),
        error => CliError::new("credential_input", error.to_string()),
    })
}

/// Report a native item that is no longer bound but was not deleted. The
/// command still succeeds, because the new state is recorded.
fn add_orphan_warning(output: &mut Value, orphan: Option<&OrphanedCredential>) {
    if let (Some(orphan), Some(object)) = (orphan, output.as_object_mut()) {
        object.insert(
            "warning".to_owned(),
            json!({
                "code": "credential_orphaned",
                "credential_id": orphan.id.as_str(),
                "message": format!(
                    "The credential is no longer bound, but its native item could not be deleted: {}. Remove it manually.",
                    orphan.error
                ),
            }),
        );
    }
}

/// Stable error codes for each native-store category.
pub(crate) fn credential_error(error: CredentialError) -> CliError {
    let code = match &error {
        CredentialError::Unavailable(_) => "credential_unavailable",
        CredentialError::Locked => "credential_locked",
        CredentialError::NotFound => "credential_not_found",
        CredentialError::AlreadyExists => "credential_exists",
        CredentialError::Corrupt(_) => "credential_corrupt",
        CredentialError::TooLarge | CredentialError::Invalid(_) => "credential_input",
        CredentialError::Io(_) => "credential_io",
        CredentialError::PromptRequired => "credential_prompt_required",
    };
    CliError::new(code, error.to_string())
}

fn provider_credential_error(error: ProviderCredentialError) -> CliError {
    match error {
        ProviderCredentialError::Control(error) => crate::control_cli::control_error(error),
        ProviderCredentialError::Credential(error) => credential_error(error),
        error @ (ProviderCredentialError::Unrecorded { .. }
        | ProviderCredentialError::Unstored { .. }) => {
            CliError::new("credential_orphaned", error.to_string())
        }
        error @ ProviderCredentialError::BindingUncertain { .. } => {
            let replaced = match &error {
                ProviderCredentialError::BindingUncertain {
                    replaced: Some(old),
                    ..
                } => format!(
                    " If the new binding is recorded, the replaced credential {old} is no longer bound and was not deleted; delete it by hand."
                ),
                _ => String::new(),
            };
            CliError::new(
                "credential_binding_uncertain",
                format!("{error}.{replaced}"),
            )
        }
    }
}

fn package_credential_error(error: PackageCredentialError) -> CliError {
    match error {
        PackageCredentialError::Control(error) => crate::control_cli::control_error(error),
        PackageCredentialError::Credential(error) => credential_error(error),
        error @ (PackageCredentialError::Unrecorded { .. }
        | PackageCredentialError::Unstored { .. }) => {
            CliError::new("credential_orphaned", error.to_string())
        }
        error @ PackageCredentialError::BindingUncertain { .. } => {
            let replaced = match &error {
                PackageCredentialError::BindingUncertain {
                    replaced: Some(old),
                    ..
                } => format!(
                    " If the new binding is recorded, the replaced credential {old} is no longer bound and was not deleted; delete it by hand."
                ),
                _ => String::new(),
            };
            CliError::new(
                "credential_binding_uncertain",
                format!("{error}.{replaced}"),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omgcode_core::control::{ControlError, CredentialId};

    const NEW: &str = "0123456789ab4def8123456789abcdef";
    const OLD: &str = "fedcba9876544321a123456789abcdef";

    fn id(text: &str) -> CredentialId {
        CredentialId::parse(text).expect("credential id")
    }

    /// Failures that leave an item behind name its identifier under a stable
    /// code, and a refused prompt has its own code.
    #[test]
    fn credential_failures_have_stable_codes_and_name_the_identifier() {
        let error = provider_credential_error(ProviderCredentialError::Unstored {
            error: CredentialError::Io(io::Error::other("directory sync failed")),
            credential_id: id(NEW),
            cleanup: CredentialError::Locked,
        });
        assert_eq!(error.code, "credential_orphaned");
        assert!(error.message.contains(NEW), "{}", error.message);

        let error = provider_credential_error(ProviderCredentialError::BindingUncertain {
            error: ControlError::Io(io::Error::other("sync failed")),
            credential_id: id(NEW),
            replaced: Some(id(OLD)),
        });
        assert_eq!(error.code, "credential_binding_uncertain");
        for expected in [
            NEW,
            OLD,
            "provider-profile-show",
            "provider-credential-remove",
        ] {
            assert!(error.message.contains(expected), "{}", error.message);
        }

        let error = provider_credential_error(ProviderCredentialError::BindingUncertain {
            error: ControlError::Io(io::Error::other("sync failed")),
            credential_id: id(NEW),
            replaced: None,
        });
        assert_eq!(error.code, "credential_binding_uncertain");
        assert!(
            !error.message.contains("replaced credential"),
            "{}",
            error.message
        );

        let error = credential_error(CredentialError::PromptRequired);
        assert_eq!(error.code, "credential_prompt_required");
        assert!(error.message.contains("never prompts"), "{}", error.message);
    }
}
