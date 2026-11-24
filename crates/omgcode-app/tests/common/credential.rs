//! A provider profile and its stored credential in the real settings store and
//! the real native credential store, for the stored-token tests.
//!
//! The native store is shared with the user's account, so a test deletes only
//! the items it created: [`Vault`] remembers each identifier it bound and
//! deletes exactly those when it drops, including after a failed assertion.

use std::path::{Path, PathBuf};

use super::legacy_journal::forget_recorded_endpoint;
use omgcode_core::control::{
    ControlStore, CredentialId, CredentialStore, ProviderProfileDeclaration, Secret,
};

/// The profiles and native items one test created.
pub struct Vault {
    data: PathBuf,
    created: Vec<CredentialId>,
}

impl Vault {
    /// A vault over the settings store in `data`. The data root's settings
    /// store must already exist.
    pub fn new(data: &Path) -> Self {
        Self {
            data: data.to_owned(),
            created: Vec::new(),
        }
    }

    /// Record a provider profile with no endpoint, as one recorded before
    /// endpoints existed. A selected profile needs an endpoint, so most tests
    /// use [`Vault::add_profile_at`].
    pub fn add_profile(&self, name: &str, kind: &str, auth: &str) {
        self.add(name, kind, auth, None);
        forget_recorded_endpoint(&self.data, name);
    }

    /// Record a provider profile that names its endpoint.
    pub fn add_profile_at(&self, name: &str, kind: &str, auth: &str, endpoint: &str) {
        self.add(name, kind, auth, Some(endpoint));
    }

    fn add(&self, name: &str, kind: &str, auth: &str, endpoint: Option<&str>) {
        let mut control = ControlStore::open(&self.data).expect("settings store");
        let revision = control.revision();
        control
            .add_provider_profile(
                &ProviderProfileDeclaration {
                    name: name.into(),
                    kind: kind.into(),
                    auth: auth.into(),
                    endpoint: endpoint.map(str::to_owned),
                },
                revision,
            )
            .expect("provider profile");
    }

    /// Store `bytes` as the profile's credential. A second call rotates it.
    pub fn bind(&mut self, name: &str, bytes: &[u8]) -> CredentialId {
        let credentials = CredentialStore::open(&self.data).expect("native credential store");
        let mut control = ControlStore::open(&self.data).expect("settings store");
        let revision = control.revision();
        let set = control
            .set_provider_credential(
                &credentials,
                name,
                &Secret::new(bytes.to_vec()).expect("secret"),
                revision,
            )
            .expect("bind credential");
        let id = set.binding.id().clone();
        self.created.push(id.clone());
        id
    }

    /// Delete this item when the vault drops, for a credential the service
    /// under test bound rather than [`Vault::bind`].
    pub fn track(&mut self, id: CredentialId) {
        self.created.push(id);
    }

    /// Remove a profile and its credential, as `provider-profile-remove` does.
    pub fn remove_profile(&self, name: &str) {
        let mut control = ControlStore::open(&self.data).expect("settings store");
        let revision = control.revision();
        let removal = control
            .remove_provider_profile(name, revision)
            .expect("remove profile");
        if let Some(binding) = removal.credential {
            let credentials = CredentialStore::open(&self.data).expect("native credential store");
            credentials
                .delete(binding.id())
                .expect("delete the item this test created");
        }
    }

    /// Delete a native item while its binding stays recorded.
    pub fn delete_native(&self, id: &CredentialId) {
        CredentialStore::open(&self.data)
            .expect("native credential store")
            .delete(id)
            .expect("delete the item this test created");
    }

    /// The settings revision now.
    pub fn revision(&self) -> u64 {
        ControlStore::open(&self.data)
            .expect("settings store")
            .revision()
    }
}

impl Drop for Vault {
    fn drop(&mut self) {
        // Best effort, and only for items this vault created. An item a
        // rotation already deleted reports `NotFound`, which is fine.
        if let Ok(credentials) = CredentialStore::open(&self.data) {
            for id in &self.created {
                credentials.delete(id).ok();
            }
        }
    }
}
