//! The two-slot token store and the session lock (ADR-0093).
//!
//! Protected-store items are never overwritten (ADR-0042), and a refresh
//! records nothing in the control journal. A sign-in's binding therefore
//! names two slots: the bound identifier and a companion derived from it.
//! Each slot holds one token record with a generation counter. A reader uses
//! the readable record with the higher generation; a writer replaces the
//! other slot, which holds the older, already rotated record. An interrupted
//! write leaves the current record in place.
//!
//! Every read, refresh, and sign-out of one session runs under an exclusive
//! file lock, so two processes never spend the same refresh token.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Arc;
use std::time::{Duration, Instant};

use omgcode_platform::credentials::{
    CredentialError, CredentialId, CredentialStore, Secret, Zeroizing,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{AuthEndpoints, SignInError, TokenSet, now_seconds, refresh, revoke};
use crate::control::CredentialBinding;
use crate::provider::{AccessTokenSource, ProviderError};
use crate::storage::{create_private_directories, sync_parent_directory};
use crate::turn::CancelToken;

/// The token record format version.
const RECORD_VERSION: u8 = 1;
/// Refresh when less than this much lifetime is left.
const REFRESH_MARGIN: u64 = 5 * 60;
/// How often a waiting holder retries the session lock.
const LOCK_POLL: Duration = Duration::from_millis(50);
/// The longest a holder waits for the session lock.
const LOCK_WAIT: Duration = Duration::from_secs(40);
/// The directory under the data root that holds session lock files.
const LOCK_DIRECTORY: &str = "subscription-locks";
/// Domain separation for the companion slot identifier.
const COMPANION_DOMAIN: &[u8] = b"omgcode-chatgpt-token-companion-v1\0";

/// The native operations the token store needs. The protected
/// [`CredentialStore`] implements it; tests use memory.
pub trait SlotStore {
    /// Load one item.
    ///
    /// # Errors
    ///
    /// [`CredentialError::NotFound`] for an absent item, others as the store
    /// reports them.
    fn load(&self, id: &CredentialId) -> Result<Secret, CredentialError>;
    /// Store one new item.
    ///
    /// # Errors
    ///
    /// As the store reports them.
    fn store(&self, id: &CredentialId, secret: &Secret) -> Result<(), CredentialError>;
    /// Delete one item.
    ///
    /// # Errors
    ///
    /// [`CredentialError::NotFound`] for an absent item, others as the store
    /// reports them.
    fn delete(&self, id: &CredentialId) -> Result<(), CredentialError>;
}

impl SlotStore for CredentialStore {
    fn load(&self, id: &CredentialId) -> Result<Secret, CredentialError> {
        Self::load(self, id)
    }

    fn store(&self, id: &CredentialId, secret: &Secret) -> Result<(), CredentialError> {
        Self::store(self, id, secret)
    }

    fn delete(&self, id: &CredentialId) -> Result<(), CredentialError> {
        Self::delete(self, id)
    }
}

/// The companion slot of a bound identifier: SHA-256 over a fixed domain and
/// the identifier, shaped as a UUIDv4 so the store accepts it.
///
/// # Panics
///
/// Never: a UUID's simple form always parses as a credential identifier.
#[must_use]
pub fn companion_credential_id(id: &CredentialId) -> CredentialId {
    let digest = Sha256::new()
        .chain_update(COMPANION_DOMAIN)
        .chain_update(id.as_str().as_bytes())
        .finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(digest.get(..16).unwrap_or(&[0; 16]));
    let uuid = uuid::Builder::from_random_bytes(bytes).into_uuid();
    CredentialId::parse(&uuid.simple().to_string())
        .expect("a version 4 UUID is a valid credential identifier")
}

/// The persisted record, borrowed for writing.
#[derive(Serialize)]
struct RecordOut<'a> {
    version: u8,
    generation: u64,
    client_id: &'a str,
    subject: &'a str,
    access_token: &'a str,
    refresh_token: &'a str,
    scopes: &'a [String],
    expires_at: u64,
}

/// The persisted record, owned for reading.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordIn {
    version: u8,
    generation: u64,
    client_id: String,
    subject: String,
    access_token: String,
    refresh_token: String,
    scopes: Vec<String>,
    expires_at: u64,
}

/// The current record of one session and the slot that holds it.
#[derive(Debug)]
pub struct StoredTokens {
    slot: CredentialId,
    generation: u64,
    /// The record's tokens.
    pub tokens: TokenSet,
}

impl StoredTokens {
    /// The record's generation; a sign-in stores generation 1.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// The slot that holds the record.
    #[must_use]
    pub const fn slot(&self) -> &CredentialId {
        &self.slot
    }
}

fn encode(
    generation: u64,
    client_id: &str,
    subject: &str,
    tokens: &TokenSet,
) -> Result<Secret, CredentialError> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(8 * 1024));
    serde_json::to_writer(
        &mut *bytes,
        &RecordOut {
            version: RECORD_VERSION,
            generation,
            client_id,
            subject,
            access_token: &tokens.access_token,
            refresh_token: &tokens.refresh_token,
            scopes: &tokens.scopes,
            expires_at: tokens.expires_at,
        },
    )
    .map_err(|_| {
        CredentialError::Unavailable("the token record could not be encoded".to_owned())
    })?;
    Secret::new(std::mem::take(&mut *bytes))
}

/// Decode one slot. A record for another client or account is unreadable.
fn decode(secret: &Secret, client_id: &str, subject: &str) -> Option<(u64, TokenSet)> {
    let record: RecordIn = serde_json::from_slice(secret.expose()).ok()?;
    let RecordIn {
        version,
        generation,
        client_id: record_client,
        subject: record_subject,
        access_token,
        refresh_token,
        scopes,
        expires_at,
    } = record;
    let tokens = TokenSet {
        access_token: Zeroizing::new(access_token),
        refresh_token: Zeroizing::new(refresh_token),
        scopes,
        expires_at,
    };
    (version == RECORD_VERSION
        && generation > 0
        && record_client == client_id
        && record_subject == subject
        && !tokens.access_token.is_empty()
        && !tokens.refresh_token.is_empty())
    .then_some((generation, tokens))
}

/// The first record of a sign-in, generation 1, for the bound slot.
///
/// # Errors
///
/// A record too large for the protected store.
pub fn initial_token_record(
    client_id: &str,
    subject: &str,
    tokens: &TokenSet,
) -> Result<Secret, CredentialError> {
    encode(1, client_id, subject, tokens)
}

/// Load the current record of the session bound at `binding_id`.
///
/// # Errors
///
/// [`SignInError::SignInRequired`] when neither slot holds a record, and
/// [`SignInError::TokenStore`] when a slot that exists cannot be read or
/// decoded. A newer record that cannot be read is never skipped for an older
/// one, because the older refresh token may already have been rotated.
pub fn load_current_record(
    store: &dyn SlotStore,
    binding_id: &CredentialId,
    client_id: &str,
    subject: &str,
) -> Result<StoredTokens, SignInError> {
    let mut current: Option<StoredTokens> = None;
    for slot in [binding_id.clone(), companion_credential_id(binding_id)] {
        let secret = match store.load(&slot) {
            Ok(secret) => secret,
            Err(CredentialError::NotFound) => continue,
            Err(_) => return Err(SignInError::TokenStore),
        };
        let Some((generation, tokens)) = decode(&secret, client_id, subject) else {
            return Err(SignInError::TokenStore);
        };
        if current
            .as_ref()
            .is_none_or(|known| generation > known.generation)
        {
            current = Some(StoredTokens {
                slot,
                generation,
                tokens,
            });
        }
    }
    current.ok_or(SignInError::SignInRequired)
}

/// Store `tokens` as the next generation in the slot `current` does not
/// occupy, deleting that slot's older record first.
///
/// # Errors
///
/// The store's error. The current record is left in place.
pub fn store_next_record(
    store: &dyn SlotStore,
    binding_id: &CredentialId,
    current: &StoredTokens,
    client_id: &str,
    subject: &str,
    tokens: &TokenSet,
) -> Result<(), CredentialError> {
    let other = if current.slot == *binding_id {
        companion_credential_id(binding_id)
    } else {
        binding_id.clone()
    };
    let secret = encode(
        current.generation.saturating_add(1),
        client_id,
        subject,
        tokens,
    )?;
    match store.delete(&other) {
        Ok(()) | Err(CredentialError::NotFound) => {}
        Err(error) => return Err(error),
    }
    store.store(&other, &secret)
}

/// Revoke the current refresh token of a session once. The caller holds the
/// session lock.
///
/// # Errors
///
/// Any failure to read the record or to have the revocation confirmed.
pub fn revoke_current(
    store: &dyn SlotStore,
    endpoints: &AuthEndpoints,
    binding_id: &CredentialId,
    client_id: &str,
    subject: &str,
    cancel: &CancelToken,
) -> Result<(), SignInError> {
    let current = load_current_record(store, binding_id, client_id, subject)?;
    revoke(endpoints, client_id, &current.tokens.refresh_token, cancel)
}

/// An exclusive lock on one session, held until dropped.
///
/// It also guards the session's sign-out marker. A recorded sign-out can
/// leave a token slot it could not delete, so a token source already running
/// for the session would still find a record there. The sign-out writes the
/// marker under this lock before it records anything, and every token
/// request checks for it under the same lock before reading a slot, so a
/// request after a sign-out returns no token and writes no new record. A
/// binding identifier is new for every sign-in, so a marker is never
/// cleared.
#[derive(Debug)]
pub struct SessionLock {
    _file: File,
    directory: PathBuf,
    signed_out: PathBuf,
}

impl SessionLock {
    /// Take the lock for the session bound at `binding_id`, retrying every
    /// 50 ms for at most 40 seconds or until `cancel` is set. The lock file
    /// is never deleted.
    ///
    /// # Errors
    ///
    /// `Cancelled`, or `TokenStore` for a busy or unusable lock.
    pub fn acquire(
        storage_root: &Path,
        binding_id: &CredentialId,
        cancel: &CancelToken,
    ) -> Result<Self, SignInError> {
        let directory = storage_root.join(LOCK_DIRECTORY);
        create_private_directories(&directory).map_err(|_| SignInError::TokenStore)?;
        let path = directory.join(format!("{}.lock", binding_id.as_str()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|_| SignInError::TokenStore)?;
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            // A cancelled caller never takes the lock, even a free one.
            if cancel.is_cancelled() {
                return Err(SignInError::Cancelled);
            }
            match file.try_lock() {
                Ok(()) => {
                    return Ok(Self {
                        _file: file,
                        signed_out: directory.join(format!("{}.signed-out", binding_id.as_str())),
                        directory,
                    });
                }
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(_)) => return Err(SignInError::TokenStore),
            }
            if Instant::now() >= deadline {
                return Err(SignInError::TokenStore);
            }
            std::thread::sleep(LOCK_POLL);
        }
    }

    /// Record durably that this session was signed out, before the sign-out
    /// itself is recorded. A marker an earlier sign-out left is kept as it is.
    ///
    /// # Errors
    ///
    /// `TokenStore` when the marker cannot be written and synced. A marker
    /// this call created is then removed again, so a failure leaves the
    /// session as it was, unless that removal also fails.
    pub fn mark_signed_out(&self) -> Result<(), SignInError> {
        if self.is_signed_out()? {
            return Ok(());
        }
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.signed_out)
            .and_then(|file| file.sync_all())
            .map_err(|_| SignInError::TokenStore)
            .and_then(|()| {
                sync_parent_directory(&self.directory).map_err(|_| SignInError::TokenStore)
            });
        if written.is_err() {
            let _ = std::fs::remove_file(&self.signed_out);
        }
        written
    }

    /// Whether this session was signed out. Any entry at the marker's path
    /// counts, so nothing placed there can make a session usable again.
    ///
    /// # Errors
    ///
    /// `TokenStore` when the marker's path cannot be checked.
    pub fn is_signed_out(&self) -> Result<bool, SignInError> {
        match std::fs::symlink_metadata(&self.signed_out) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(SignInError::TokenStore),
        }
    }
}

/// Where a token source finds its slots.
enum Slots {
    /// The protected store, opened for each request.
    Native,
    /// A shared store, for tests.
    #[cfg(test)]
    Shared(Arc<dyn SlotStore + Send + Sync>),
}

/// The access-token source of one signed-in subscription profile. Each
/// request takes the session lock, refuses a signed-out session, reads the
/// current record, and refreshes it when fewer than five minutes are left.
pub struct ChatGptTokens {
    storage_root: PathBuf,
    binding: CredentialBinding,
    client_id: String,
    subject: String,
    endpoints: AuthEndpoints,
    slots: Slots,
}

impl std::fmt::Debug for ChatGptTokens {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatGptTokens")
            .field("binding", self.binding.id())
            .finish_non_exhaustive()
    }
}

impl ChatGptTokens {
    /// A source for the session bound by `binding`, whose registration is
    /// `client_id` and `subject`, reading the protected store under
    /// `storage_root`.
    #[must_use]
    pub fn new(
        storage_root: PathBuf,
        binding: CredentialBinding,
        client_id: String,
        subject: String,
    ) -> Self {
        Self {
            storage_root,
            binding,
            client_id,
            subject,
            endpoints: AuthEndpoints::openai(),
            slots: Slots::Native,
        }
    }

    /// A source over a shared test store and test endpoints.
    #[cfg(test)]
    pub(crate) fn for_test(
        storage_root: PathBuf,
        binding: CredentialBinding,
        client_id: String,
        subject: String,
        endpoints: AuthEndpoints,
        store: Arc<dyn SlotStore + Send + Sync>,
    ) -> Self {
        Self {
            storage_root,
            binding,
            client_id,
            subject,
            endpoints,
            slots: Slots::Shared(store),
        }
    }

    fn with_store<T>(
        &self,
        use_store: impl FnOnce(&dyn SlotStore) -> Result<T, SignInError>,
    ) -> Result<T, SignInError> {
        match &self.slots {
            Slots::Native => {
                let store = CredentialStore::open(&self.storage_root)
                    .map_err(|_| SignInError::TokenStore)?;
                if store.backend() != self.binding.backend() {
                    return Err(SignInError::TokenStore);
                }
                use_store(&store)
            }
            #[cfg(test)]
            Slots::Shared(store) => use_store(store.as_ref()),
        }
    }

    /// A valid access token, refreshed when needed.
    ///
    /// # Errors
    ///
    /// See [`SignInError`].
    pub fn current_access_token(
        &self,
        cancel: &CancelToken,
    ) -> Result<Zeroizing<String>, SignInError> {
        let lock = SessionLock::acquire(&self.storage_root, self.binding.id(), cancel)?;
        if lock.is_signed_out()? {
            return Err(SignInError::SignInRequired);
        }
        self.with_store(|store| {
            let current =
                load_current_record(store, self.binding.id(), &self.client_id, &self.subject)?;
            if current.tokens.expires_at > now_seconds().saturating_add(REFRESH_MARGIN) {
                return Ok(current.tokens.access_token.clone());
            }
            let fresh = refresh(
                &self.endpoints,
                &self.client_id,
                &current.tokens.refresh_token,
                cancel,
            )?;
            store_next_record(
                store,
                self.binding.id(),
                &current,
                &self.client_id,
                &self.subject,
                &fresh,
            )
            .map_err(|_| SignInError::TokenStore)?;
            Ok(fresh.access_token)
        })
    }
}

impl AccessTokenSource for ChatGptTokens {
    fn access_token(&self, cancel: &CancelToken) -> Result<Zeroizing<String>, ProviderError> {
        self.current_access_token(cancel)
            .map_err(ProviderError::from)
    }
}
