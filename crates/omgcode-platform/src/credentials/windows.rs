//! DPAPI-protected credential files in the verified-ACL private store.
//!
//! Each credential is one `CryptProtectData` blob for the current user, with
//! UI prompts forbidden and optional entropy binding it to this store's label
//! and the credential identifier. A blob copied to another identifier's file
//! therefore fails decryption. Files are created exclusively with an explicit
//! private DACL (ADR-0023), and every read rechecks that DACL from the open
//! handle before decrypting. Windows gives no power-loss contract for these
//! files; see ADR-0042.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::Path;

use windows_sys::Win32::Foundation::{ERROR_INVALID_DATA, LocalFree, NTE_BAD_DATA};
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use zeroize::{Zeroize, Zeroizing};

use super::{CredentialError, CredentialId, Secret};
use crate::private_store::PrivateDirectory;
use crate::private_store::is_private_file;

/// The private subdirectory that holds credential files.
pub(super) const DIRECTORY: &str = "credentials-v1";
/// File name suffix after the identifier.
pub(super) const SUFFIX: &str = ".dpapi";
/// Fixed entropy prefix; the identifier bytes follow it.
const ENTROPY_LABEL: &[u8] = b"OMGCode credential store v1\0";
/// A DPAPI blob for a 16 KiB secret is far smaller than this.
const MAX_BLOB_BYTES: usize = 64 * 1024;

/// The DPAPI store, holding the trusted application data root open.
pub(crate) struct DpapiStore {
    root: PrivateDirectory,
}

impl DpapiStore {
    /// Open the existing trusted data root. Nothing is created here.
    pub(super) fn open(root: &Path) -> Result<Self, CredentialError> {
        let root = PrivateDirectory::open(root).map_err(|error| {
            CredentialError::Unavailable(format!(
                "application storage cannot hold credentials: {error}"
            ))
        })?;
        Ok(Self { root })
    }

    pub(super) fn store(&self, id: &CredentialId, secret: &Secret) -> Result<(), CredentialError> {
        let blob = protect(secret.expose(), &entropy(id))?;
        if blob.len() > MAX_BLOB_BYTES {
            return Err(CredentialError::TooLarge);
        }
        let directory = self.directory_for_store()?;
        let name = file_name(id);
        let mut file = directory.create_file(&name).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                CredentialError::AlreadyExists
            } else {
                CredentialError::Io(error)
            }
        })?;
        let written = file.write_all(&blob).and_then(|()| file.sync_all());
        drop(file);
        if let Err(error) = written {
            // Remove the partial entry this call created. Either way the
            // original failure is reported; a remaining entry decrypts as
            // corrupt, never as a valid credential.
            return Err(match directory.remove_file(&name) {
                Ok(()) => CredentialError::Io(error),
                Err(cleanup) => CredentialError::Io(io::Error::new(
                    error.kind(),
                    format!("{error}; the partial credential file could not be removed: {cleanup}"),
                )),
            });
        }
        directory.sync().map_err(CredentialError::Io)
    }

    pub(super) fn load(&self, id: &CredentialId) -> Result<Secret, CredentialError> {
        let directory = self.existing_directory()?;
        let mut file = directory
            .open_file(&file_name(id), false)
            .map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => CredentialError::NotFound,
                io::ErrorKind::InvalidData => {
                    CredentialError::Corrupt("credential file failed its native checks")
                }
                _ => CredentialError::Io(error),
            })?;
        // Recheck the exact private owner and DACL from this same handle.
        if !is_private_file(&file).map_err(CredentialError::Io)? {
            return Err(CredentialError::Corrupt("credential file is not private"));
        }
        let mut blob = Vec::new();
        let limit = u64::try_from(MAX_BLOB_BYTES).map_err(|_| CredentialError::TooLarge)?;
        (&mut file)
            .take(limit.saturating_add(1))
            .read_to_end(&mut blob)
            .map_err(CredentialError::Io)?;
        if blob.is_empty() || blob.len() > MAX_BLOB_BYTES {
            return Err(CredentialError::Corrupt("credential file size is invalid"));
        }
        let mut plain = unprotect(&blob, &entropy(id))?;
        Secret::new(std::mem::take(&mut *plain))
            .map_err(|_| CredentialError::Corrupt("decrypted credential is empty or too large"))
    }

    pub(super) fn delete(&self, id: &CredentialId) -> Result<(), CredentialError> {
        let directory = self.existing_directory()?;
        directory
            .remove_file(&file_name(id))
            .map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => CredentialError::NotFound,
                _ => CredentialError::Io(error),
            })?;
        directory.sync().map_err(CredentialError::Io)
    }

    /// Open the credential directory; absence means no credential exists.
    fn existing_directory(&self) -> Result<PrivateDirectory, CredentialError> {
        self.root
            .open_dir(DIRECTORY.as_ref())
            .map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => CredentialError::NotFound,
                _ => CredentialError::Io(error),
            })
    }

    /// Open the credential directory, creating it privately on first use.
    /// The caller serializes stores, so a lost creation race is an error.
    fn directory_for_store(&self) -> Result<PrivateDirectory, CredentialError> {
        match self.root.open_dir(DIRECTORY.as_ref()) {
            Ok(directory) => Ok(directory),
            Err(error) if error.kind() == io::ErrorKind::NotFound => self
                .root
                .create_dir(DIRECTORY.as_ref())
                .map_err(CredentialError::Io),
            Err(error) => Err(CredentialError::Io(error)),
        }
    }
}

pub(super) fn file_name(id: &CredentialId) -> OsString {
    OsString::from(format!("{id}{SUFFIX}"))
}

/// Bind a blob to this store and to its own identifier.
fn entropy(id: &CredentialId) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(ENTROPY_LABEL.len() + id.as_str().len());
    bytes.extend_from_slice(ENTROPY_LABEL);
    bytes.extend_from_slice(id.as_str().as_bytes());
    bytes
}

/// Borrow a byte slice as a DPAPI input blob. DPAPI does not write inputs.
fn input_blob(bytes: &[u8]) -> Result<CRYPT_INTEGER_BLOB, CredentialError> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len()).map_err(|_| CredentialError::TooLarge)?,
        pbData: bytes.as_ptr().cast_mut(),
    })
}

/// A DPAPI output buffer, released exactly once with `LocalFree`. A
/// plaintext buffer is zeroed before release.
struct LocalBlob {
    blob: CRYPT_INTEGER_BLOB,
    plaintext: bool,
}

impl LocalBlob {
    #[allow(unsafe_code)]
    fn as_slice(&self) -> &[u8] {
        if self.blob.pbData.is_null() || self.blob.cbData == 0 {
            return &[];
        }
        let Ok(length) = usize::try_from(self.blob.cbData) else {
            return &[];
        };
        // SAFETY: a successful DPAPI call returned `pbData` pointing to
        // `cbData` initialized bytes that this value owns until drop. The
        // returned borrow cannot outlive `self`, and nothing else aliases it.
        unsafe { std::slice::from_raw_parts(self.blob.pbData, length) }
    }

    #[allow(unsafe_code)]
    fn zero(&mut self) {
        if self.blob.pbData.is_null() || self.blob.cbData == 0 {
            return;
        }
        let Ok(length) = usize::try_from(self.blob.cbData) else {
            return;
        };
        // SAFETY: the buffer is owned by this value, holds `cbData` bytes,
        // and no shared borrow is live while `&mut self` is held.
        let bytes = unsafe { std::slice::from_raw_parts_mut(self.blob.pbData, length) };
        bytes.zeroize();
    }
}

impl Drop for LocalBlob {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        if self.plaintext {
            self.zero();
        }
        if !self.blob.pbData.is_null() {
            // SAFETY: DPAPI allocated `pbData` with `LocalAlloc` and this
            // value is its only owner, so it is freed exactly once. A failed
            // free cannot be reported from `drop`; it leaks the buffer, whose
            // plaintext was already zeroed above.
            let _leaked_on_failure = unsafe { LocalFree(self.blob.pbData.cast()) };
        }
    }
}

#[allow(unsafe_code)]
fn protect(plain: &[u8], entropy: &[u8]) -> Result<Vec<u8>, CredentialError> {
    let input = input_blob(plain)?;
    let entropy = input_blob(entropy)?;
    let mut output = LocalBlob {
        blob: CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        },
        plaintext: false,
    };
    // SAFETY: `input` and `entropy` borrow live slices for this synchronous
    // call, and DPAPI only reads them. A null description, reserved pointer,
    // and prompt are allowed; prompts are also forbidden by the flag. On
    // success DPAPI fills `output` with a `LocalAlloc` buffer that
    // `LocalBlob` frees; on failure it leaves the output null.
    let protected = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            &entropy,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output.blob,
        )
    };
    if protected == 0 {
        return Err(CredentialError::Io(io::Error::last_os_error()));
    }
    Ok(output.as_slice().to_vec())
}

#[allow(unsafe_code)]
fn unprotect(blob: &[u8], entropy: &[u8]) -> Result<Zeroizing<Vec<u8>>, CredentialError> {
    let input = input_blob(blob)?;
    let entropy = input_blob(entropy)?;
    let mut output = LocalBlob {
        blob: CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        },
        plaintext: true,
    };
    // SAFETY: as in `protect`. A null description pointer asks DPAPI not to
    // return the stored description. The plaintext output is owned by
    // `LocalBlob`, which zeroes it before `LocalFree`.
    let unprotected = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            &entropy,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output.blob,
        )
    };
    if unprotected == 0 {
        let error = io::Error::last_os_error();
        return Err(match error.raw_os_error() {
            Some(code) if is_bad_data(code) => {
                CredentialError::Corrupt("credential failed DPAPI authentication")
            }
            _ => CredentialError::Io(error),
        });
    }
    let mut plain = Zeroizing::new(Vec::with_capacity(output.as_slice().len()));
    plain.extend_from_slice(output.as_slice());
    Ok(plain)
}

/// DPAPI reports an edited, truncated, or wrongly bound blob as invalid data.
fn is_bad_data(code: i32) -> bool {
    u32::try_from(code).is_ok_and(|code| code == ERROR_INVALID_DATA) || code == NTE_BAD_DATA
}

#[cfg(test)]
mod tests;
