//! Handle-relative private storage. This is not workspace authorization.
//!
//! Linux requires openat2 and procfs. Windows requires NTFS or ReFS and
//! verified ACLs.
//! Other backends refuse before I/O.

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(target_os = "linux")]
pub use linux::PrivateDirectory;
#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use windows::WindowsPrivateDirectory as PrivateDirectory;

/// Native identity metadata for one already-open private regular file.
/// These values do not authorize access or prove checkpoint durability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PrivateFileIdentity {
    /// Native volume identifier.
    pub volume: u64,
    /// Native file identifier in the existing 16-byte representation.
    pub file_id: [u8; 16],
}

/// Native security that a replacement must carry over from its source.
///
/// This is captured evidence. It never authorizes access.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeSecurity {
    /// Linux ownership and ordinary permission bits.
    Unix {
        /// File owner ID.
        owner: u32,
        /// File group ID.
        group: u32,
        /// Ordinary permission bits only.
        mode: u32,
    },
    /// Windows owner, group, and DACL, plus the content-indexing opt-out.
    Windows {
        /// Canonical SDDL read from one open handle.
        descriptor: String,
        /// Whether the file is excluded from content indexing.
        not_content_indexed: bool,
    },
}

/// Metadata that a replacement stage can preserve from its source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StageMetadata {
    /// Identity of the already-open, regular, single-link file.
    pub identity: PrivateFileIdentity,
    /// Security read from that same file.
    pub security: NativeSecurity,
}

/// Inspect the same private file handle whose bytes the caller reads or hashes.
/// Linux requires a regular, single-link, current-user-owned mode0600 file.
/// Windows requires a regular, single-link file with the exact private DACL.
/// Other platforms refuse before I/O. No pathname lookup or reopen occurs.
pub fn file_identity(file: &std::fs::File) -> std::io::Result<PrivateFileIdentity> {
    #[cfg(target_os = "linux")]
    {
        linux::file_identity(file)
    }
    #[cfg(windows)]
    {
        windows::file_identity(file)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = file;
        Err(unsupported())
    }
}

/// Reports whether this open file has exactly the owner and protected DACL that
/// the Windows store gives its own files. A mismatch is `false`; a failed
/// query is an error. The handle must have READ_CONTROL.
#[cfg(windows)]
pub fn is_private_file(file: &std::fs::File) -> std::io::Result<bool> {
    windows::is_private_file(file)
}

/// A principal that a private root grants access to beyond the current user,
/// SYSTEM, and Administrators.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ExtraGrant {
    /// The principal's SID in `S-1-…` form. A repair compares only this.
    pub sid: String,
    /// `DOMAIN\name` when Windows can resolve the SID; for display only.
    pub account: Option<String>,
}

/// Whether a private root's extra grants can be removed (ADR-0073).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RootAccess {
    /// The root grants no principal beyond the trusted ones.
    Private,
    /// Removing these grants, sorted by SID, would leave only trusted ones.
    Repairable(Vec<ExtraGrant>),
    /// The root cannot be repaired here, with a fixed reason.
    NotRepairable(&'static str),
}

/// A refused or failed removal of extra grants.
#[derive(Debug)]
pub enum RootRepairError {
    /// The root's extra grants differ from the list the operator confirmed.
    /// Nothing was changed.
    Changed,
    /// The root cannot be repaired here, with a fixed reason. Nothing was
    /// changed.
    NotRepairable(&'static str),
    /// A native call failed: opening the root, reading it, writing the DACL,
    /// or the check after the write. The DACL may or may not have been
    /// replaced.
    Io(std::io::Error),
}

impl std::fmt::Display for RootRepairError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Changed => formatter
                .write_str("the folder's permissions changed after review; nothing was changed"),
            Self::NotRepairable(reason) => formatter.write_str(reason),
            // The call can fail after the DACL write, so claim neither outcome.
            Self::Io(error) => write!(
                formatter,
                "the folder's permissions may or may not have changed: {error}"
            ),
        }
    }
}

impl std::error::Error for RootRepairError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Changed | Self::NotRepairable(_) => None,
        }
    }
}

/// The fixed reason on platforms whose private roots this module never edits.
#[cfg(not(windows))]
const REPAIR_UNSUPPORTED: &str = "folder permission repair is available only on Windows";

/// List the grants on an existing private root that the private-root rule
/// refuses. The root is opened without following a final reparse point, and
/// nothing is changed. Linux and other platforms report `NotRepairable`.
///
/// # Errors
///
/// Returns an error when the root cannot be opened or its descriptor read.
pub fn inspect_root_access(path: &std::path::Path) -> std::io::Result<RootAccess> {
    #[cfg(windows)]
    {
        windows::root_access::inspect(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(RootAccess::NotRepairable(REPAIR_UNSUPPORTED))
    }
}

/// Remove the extra grants from a private root after the operator confirmed
/// exactly `confirmed` (ADR-0073). The DACL is rewritten as protected: every
/// trusted and deny entry is kept and made explicit, and the owner is
/// unchanged. The caller must obtain the operator's confirmation of the list
/// that [`inspect_root_access`] returned; this function never runs on its own.
///
/// # Errors
///
/// [`RootRepairError::Changed`] when the grants differ from `confirmed`,
/// [`RootRepairError::NotRepairable`] when the root cannot be repaired, and
/// [`RootRepairError::Io`] when a native call fails.
pub fn remove_extra_root_grants(
    path: &std::path::Path,
    confirmed: &[ExtraGrant],
) -> Result<(), RootRepairError> {
    #[cfg(windows)]
    {
        windows::root_access::remove(path, confirmed)
    }
    #[cfg(not(windows))]
    {
        let _ = (path, confirmed);
        Err(RootRepairError::NotRepairable(REPAIR_UNSUPPORTED))
    }
}

/// Inspect supported replacement-stage metadata from the same open file handle.
/// Linux rejects xattrs, special mode bits, and file attrs. Windows rejects
/// alternate streams, extended attributes, and unsupported file attributes.
pub fn stage_metadata(file: &std::fs::File) -> std::io::Result<StageMetadata> {
    #[cfg(target_os = "linux")]
    {
        linux::stage_metadata(file)
    }
    #[cfg(windows)]
    {
        windows::stage_metadata(file)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = file;
        Err(unsupported())
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
use crate::DirectoryIdentity;
#[cfg(not(any(target_os = "linux", windows)))]
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    path::Path,
    time::SystemTime,
};

/// An unavailable native private-directory backend.
#[cfg(not(any(target_os = "linux", windows)))]
pub struct PrivateDirectory;

#[cfg(not(any(target_os = "linux", windows)))]
impl PrivateDirectory {
    /// Refuse unsupported private storage before opening or creating anything.
    pub fn open(_path: &Path) -> io::Result<Self> {
        Err(unsupported())
    }
    /// Refuse unsupported private root creation before any I/O.
    pub fn create_in(
        _parent: &Path,
        _parent_identity: DirectoryIdentity,
        _name: &OsStr,
    ) -> io::Result<Self> {
        Err(unsupported())
    }
    /// Refuse an unsupported owner-only check.
    pub fn require_owner_only(&self) -> io::Result<()> {
        Err(unsupported())
    }
    /// Refuse unsupported native identity lookup.
    pub fn identity(&self) -> io::Result<DirectoryIdentity> {
        Err(unsupported())
    }
    /// Refuse unsupported native creation-time lookup.
    pub fn created(&self) -> io::Result<SystemTime> {
        Err(unsupported())
    }
    /// Refuse unsupported relative directory opening.
    pub fn open_dir(&self, _name: &OsStr) -> io::Result<Self> {
        Err(unsupported())
    }
    /// Refuse unsupported directory creation.
    pub fn create_dir(&self, _name: &OsStr) -> io::Result<Self> {
        Err(unsupported())
    }
    /// Refuse unsupported private file opening.
    pub fn open_file(&self, _name: &OsStr, _writable: bool) -> io::Result<File> {
        Err(unsupported())
    }
    /// Refuse unsupported stage inspection before file access.
    pub fn open_stage(&self) -> io::Result<File> {
        Err(unsupported())
    }
    /// Refuse unsupported private file creation.
    pub fn create_file(&self, _name: &OsStr) -> io::Result<File> {
        Err(unsupported())
    }
    /// Refuse unsupported existing-entry probing.
    pub fn exists(&self, _name: &OsStr) -> io::Result<bool> {
        Err(unsupported())
    }
    /// Refuse unsupported private-directory enumeration.
    pub fn names(&self, _limit: usize) -> io::Result<Vec<OsString>> {
        Err(unsupported())
    }
    /// Refuse unsupported directory synchronization, rather than a no-op.
    pub fn sync(&self) -> io::Result<()> {
        Err(unsupported())
    }
    /// Refuse unsupported private file removal.
    pub fn remove_file(&self, _name: &OsStr) -> io::Result<()> {
        Err(unsupported())
    }
    /// Refuse unsupported private directory removal.
    pub fn remove_dir(&self, _name: &OsStr) -> io::Result<()> {
        Err(unsupported())
    }
    /// Refuse unsupported private file replacement.
    pub fn replace_file(&self, _source: &OsStr, _target: &OsStr) -> io::Result<()> {
        Err(unsupported())
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "native private storage is not qualified on this platform",
    )
}

#[cfg(test)]
mod repair_error_tests {
    use super::RootRepairError;

    /// A failed native call can come after the DACL write, so its text must
    /// not claim that nothing changed; the refusals happen before any write.
    #[test]
    fn a_repair_error_states_only_what_is_known_about_the_folder() {
        let failed = RootRepairError::Io(std::io::Error::other("native failure")).to_string();
        assert!(failed.contains("may or may not have changed"), "{failed}");
        assert!(!failed.contains("were not changed"), "{failed}");
        assert!(failed.contains("native failure"), "{failed}");
        let changed = RootRepairError::Changed.to_string();
        assert!(changed.contains("nothing was changed"), "{changed}");
    }
}

#[cfg(all(test, not(any(target_os = "linux", windows))))]
mod tests {
    use super::*;
    #[test]
    fn unsupported_backend_does_not_create_storage() {
        let root = tempfile::tempdir().expect("fixture");
        let missing = root.path().join("never-created");
        assert_eq!(
            PrivateDirectory::open(&missing)
                .err()
                .expect("unsupported")
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert!(!missing.exists());
    }
}
