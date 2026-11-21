//! Protected OS storage for provider credentials. See ADR-0042.
//!
//! Windows encrypts each credential with DPAPI for the current user and writes
//! it create-only as one file in a verified-ACL private directory. Linux stores
//! each credential as one freedesktop Secret Service item in the default
//! collection. Every other platform is unavailable before any I/O. Nothing
//! falls back to plaintext.
//!
//! A credential is immutable: an identifier is stored once and never
//! overwritten. These stores protect credentials at rest and from other users.
//! Any process running as the same user can read them; this is not isolation
//! from same-user code.

use std::fmt;
use std::io::{self, Read};
use std::path::Path;

pub use zeroize::Zeroizing;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

/// Largest accepted secret, in bytes.
pub const MAX_SECRET_BYTES: usize = 16 * 1024;
/// Length of a credential identifier: a random UUID in lowercase simple form.
const ID_CHARS: usize = 32;

/// An opaque credential identifier: 32 lowercase hexadecimal characters of a
/// random (version 4) UUID. Any other text is refused before storage I/O.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CredentialId(String);

impl CredentialId {
    /// Validate one identifier without touching storage.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError::Invalid`] for any text that is not the
    /// fixed lowercase simple form of a version 4, RFC 4122 variant UUID.
    pub fn parse(text: &str) -> Result<Self, CredentialError> {
        let bytes = text.as_bytes();
        let hex = bytes.len() == ID_CHARS
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte));
        // Position 12 holds the version nibble and 16 the variant nibble.
        let random = hex
            && bytes.get(12) == Some(&b'4')
            && bytes
                .get(16)
                .is_some_and(|variant| matches!(variant, b'8' | b'9' | b'a' | b'b'));
        if random {
            Ok(Self(text.to_owned()))
        } else {
            Err(CredentialError::Invalid(
                "a credential identifier is 32 lowercase hex characters of a random UUID",
            ))
        }
    }

    /// The identifier text. It is not secret.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CredentialId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// An owned, bounded, non-empty secret. Its buffer is zeroed on drop, and
/// `Debug` never shows its bytes. It is deliberately not `Clone`.
///
/// Zeroing on drop reduces, but cannot guarantee, the removal of copies that
/// libraries or the operating system make.
pub struct Secret(Zeroizing<Vec<u8>>);

impl Secret {
    /// Take ownership of secret bytes. Refused bytes are zeroed before return.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError::Invalid`] when empty and
    /// [`CredentialError::TooLarge`] above [`MAX_SECRET_BYTES`].
    pub fn new(bytes: Vec<u8>) -> Result<Self, CredentialError> {
        let bytes = Zeroizing::new(bytes);
        check_length(bytes.len())?;
        Ok(Self(bytes))
    }

    /// Read one secret from a trusted input stream, such as standard input.
    ///
    /// At most [`MAX_SECRET_BYTES`] plus a two-byte line ending are read.
    /// Exactly one trailing `\n` or `\r\n` is removed. Every buffer that held
    /// input bytes is zeroed on drop.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError::TooLarge`] for longer input,
    /// [`CredentialError::Invalid`] when nothing remains after the line
    /// ending, and [`CredentialError::Io`] when the stream fails.
    pub fn read_from(mut reader: impl Read) -> Result<Self, CredentialError> {
        let limit = MAX_SECRET_BYTES + 2;
        // One spare byte detects longer input without a probe buffer that
        // would hold secret bytes outside this zeroed allocation.
        let mut buffer = Zeroizing::new(vec![0_u8; limit + 1]);
        let mut filled = 0;
        while filled < buffer.len() {
            let Some(free) = buffer.get_mut(filled..) else {
                break;
            };
            match reader.read(free) {
                Ok(0) => break,
                Ok(count) => {
                    filled = filled
                        .checked_add(count.min(free.len()))
                        .ok_or(CredentialError::TooLarge)?;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(CredentialError::Io(error)),
            }
        }
        if filled > limit {
            return Err(CredentialError::TooLarge);
        }
        let input = buffer.get(..filled).ok_or(CredentialError::TooLarge)?;
        let text = input
            .strip_suffix(b"\r\n")
            .or_else(|| input.strip_suffix(b"\n"))
            .unwrap_or(input);
        check_length(text.len())?;
        let mut owned = Zeroizing::new(Vec::with_capacity(text.len()));
        owned.extend_from_slice(text);
        Ok(Self(owned))
    }

    /// Borrow the secret bytes. Callers must not log, display, or persist them.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(<redacted>)")
    }
}

fn check_length(length: usize) -> Result<(), CredentialError> {
    if length == 0 {
        Err(CredentialError::Invalid(
            "a credential secret must not be empty",
        ))
    } else if length > MAX_SECRET_BYTES {
        Err(CredentialError::TooLarge)
    } else {
        Ok(())
    }
}

/// The native store that holds a credential.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CredentialBackend {
    /// DPAPI-protected files in the private application store (Windows).
    WindowsDpapi,
    /// One item per credential in the Secret Service default collection (Linux).
    LinuxSecretService,
}

impl CredentialBackend {
    /// The stable recorded spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WindowsDpapi => "windows_dpapi",
            Self::LinuxSecretService => "linux_secret_service",
        }
    }

    /// Parse a recorded spelling exactly.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "windows_dpapi" => Some(Self::WindowsDpapi),
            "linux_secret_service" => Some(Self::LinuxSecretService),
            _ => None,
        }
    }

    /// The backend this build would use, if this platform has one.
    pub const fn native() -> Option<Self> {
        if cfg!(windows) {
            Some(Self::WindowsDpapi)
        } else if cfg!(target_os = "linux") {
            Some(Self::LinuxSecretService)
        } else {
            None
        }
    }
}

/// A credential storage failure. Categories stay distinct so callers can
/// report them; messages never contain secret bytes.
#[derive(Debug)]
pub enum CredentialError {
    /// No protected store is usable here. Nothing falls back to plaintext.
    Unavailable(String),
    /// The Windows application data folder failed its private-root checks:
    /// its owner or a grant names a principal other than the current user,
    /// SYSTEM, and Administrators, or it is not an ordinary directory.
    /// Nothing was created or repaired; the operator must fix the folder.
    StorageNotPrivate(String),
    /// The store exists but is locked. OMGCode never prompts to unlock it.
    Locked,
    /// No credential has this identifier.
    NotFound,
    /// A credential with this identifier already exists; it is never replaced.
    AlreadyExists,
    /// Stored data failed its native checks, decryption, or bounds, or more
    /// than one item matches. It is never reported as absent.
    Corrupt(&'static str),
    /// A secret exceeds [`MAX_SECRET_BYTES`].
    TooLarge,
    /// An identifier or secret was refused before any storage I/O.
    Invalid(&'static str),
    /// The Secret Service asked for interactive confirmation. OMGCode never
    /// runs a prompt; the request was refused without one.
    PromptRequired,
    /// An operating-system or service failure without a more specific category.
    Io(io::Error),
}

impl fmt::Display for CredentialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(reason) => {
                write!(formatter, "credential store is unavailable: {reason}")
            }
            Self::StorageNotPrivate(reason) => {
                write!(formatter, "application storage is not private: {reason}")
            }
            Self::Locked => formatter
                .write_str("credential store is locked; unlock it outside OMGCode and try again"),
            Self::NotFound => formatter.write_str("credential is not stored"),
            Self::AlreadyExists => formatter.write_str("credential identifier already exists"),
            Self::Corrupt(reason) => write!(formatter, "stored credential is corrupt: {reason}"),
            Self::TooLarge => write!(
                formatter,
                "credential secret exceeds {MAX_SECRET_BYTES} bytes"
            ),
            Self::Invalid(reason) => write!(formatter, "credential refused: {reason}"),
            Self::PromptRequired => formatter.write_str(
                "the Secret Service asked for interactive confirmation; OMGCode never prompts",
            ),
            Self::Io(error) => write!(formatter, "credential store I/O failed: {error}"),
        }
    }
}

impl std::error::Error for CredentialError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

/// Whether this platform's protected store can be used now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CredentialStoreStatus {
    /// The store answered and is usable.
    Available(CredentialBackend),
    /// The store answered, but its default collection is locked.
    Locked(CredentialBackend),
    /// No protected store is usable, with the reason.
    Unavailable(String),
}

/// Probe the native store without creating, unlocking, or reading anything.
///
/// Windows reports DPAPI as available; its application storage is checked
/// when a store is opened. Linux connects to the Secret Service and checks the
/// default collection's lock state without prompting.
pub fn probe() -> CredentialStoreStatus {
    #[cfg(windows)]
    {
        CredentialStoreStatus::Available(CredentialBackend::WindowsDpapi)
    }
    #[cfg(target_os = "linux")]
    {
        linux::probe()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        CredentialStoreStatus::Unavailable(UNSUPPORTED.to_owned())
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
const UNSUPPORTED: &str = "no protected credential store is supported on this platform";

#[cfg(windows)]
type Backend = windows::DpapiStore;
#[cfg(target_os = "linux")]
type Backend = linux::SecretServiceStore;
/// No value exists on unsupported platforms, so no operation can run.
#[cfg(not(any(windows, target_os = "linux")))]
enum Backend {}

/// The native credential store for the current user.
///
/// The caller must serialize changes, for example under the control-journal
/// lock, and must keep secrets out of logs, journals, arguments, and output.
pub struct CredentialStore {
    backend: Backend,
}

impl CredentialStore {
    /// Open the protected store. On Windows, `app_storage_root` is the
    /// existing trusted application data directory that holds the
    /// `credentials-v1` subdirectory; on Linux it is not used.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError::Unavailable`] when no protected store can
    /// be used, [`CredentialError::StorageNotPrivate`] when the Windows data
    /// folder fails its private-root checks, and [`CredentialError::Locked`]
    /// when the Linux default collection is locked. Nothing is created by
    /// opening.
    pub fn open(app_storage_root: &Path) -> Result<Self, CredentialError> {
        #[cfg(windows)]
        {
            Ok(Self {
                backend: windows::DpapiStore::open(app_storage_root)?,
            })
        }
        #[cfg(target_os = "linux")]
        {
            let _ = app_storage_root;
            Ok(Self {
                backend: linux::SecretServiceStore::open()?,
            })
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = app_storage_root;
            Err(CredentialError::Unavailable(UNSUPPORTED.to_owned()))
        }
    }

    /// The backend that holds this store's credentials.
    pub fn backend(&self) -> CredentialBackend {
        #[cfg(windows)]
        {
            CredentialBackend::WindowsDpapi
        }
        #[cfg(target_os = "linux")]
        {
            CredentialBackend::LinuxSecretService
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            match self.backend {}
        }
    }

    /// Store a new credential. An existing identifier is refused, never
    /// replaced.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError::AlreadyExists`] for an existing identifier,
    /// [`CredentialError::Locked`] for a locked store, or another category.
    /// [`CredentialError::PromptRequired`] when the Linux service asks for
    /// confirmation. A failure after creation may leave an entry under `id`:
    /// a partial or complete Windows file, or a Linux placeholder that `load`
    /// reports as corrupt. Both backends try to remove it first; when that
    /// fails, the caller should try [`Self::delete`] and keep the identifier.
    pub fn store(&self, id: &CredentialId, secret: &Secret) -> Result<(), CredentialError> {
        #[cfg(any(windows, target_os = "linux"))]
        {
            self.backend.store(id, secret)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = (id, secret);
            match self.backend {}
        }
    }

    /// Load one credential.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError::NotFound`] when absent and
    /// [`CredentialError::Corrupt`] when stored data fails its checks.
    pub fn load(&self, id: &CredentialId) -> Result<Secret, CredentialError> {
        #[cfg(any(windows, target_os = "linux"))]
        {
            self.backend.load(id)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = id;
            match self.backend {}
        }
    }

    /// Delete one credential.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError::NotFound`] when absent. Nothing is retried.
    pub fn delete(&self, id: &CredentialId) -> Result<(), CredentialError> {
        #[cfg(any(windows, target_os = "linux"))]
        {
            self.backend.delete(id)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = id;
            match self.backend {}
        }
    }
}

#[cfg(test)]
mod tests;
