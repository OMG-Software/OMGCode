//! Windows volume qualification for private storage, replacement, and grants.
//!
//! Only NTFS and ReFS are accepted, and only when the volume reports every
//! capability the callers rely on: persistent ACLs, POSIX unlink and rename,
//! case-preserved Unicode names, and extended-attribute queries. The name list
//! keeps an untested filesystem that happens to report the same flags refused.
//! See ADR-0068. This is functional qualification, not a power-loss claim.

use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationByHandleW;
use windows_sys::Win32::System::SystemServices::{
    FILE_CASE_PRESERVED_NAMES, FILE_PERSISTENT_ACLS, FILE_SUPPORTS_EXTENDED_ATTRIBUTES,
    FILE_SUPPORTS_POSIX_UNLINK_RENAME, FILE_UNICODE_ON_DISK,
};

/// `MAX_PATH + 1` units, the documented filesystem-name buffer size.
const FILE_SYSTEM_NAME_UNITS: usize = 261;
/// Capabilities every accepted volume must report.
const REQUIRED_FLAGS: u32 = FILE_PERSISTENT_ACLS
    | FILE_SUPPORTS_POSIX_UNLINK_RENAME
    | FILE_CASE_PRESERVED_NAMES
    | FILE_UNICODE_ON_DISK
    | FILE_SUPPORTS_EXTENDED_ATTRIBUTES;
/// Longest filesystem name repeated in an error message.
const MAX_REPORTED_NAME: usize = 32;

/// A Windows filesystem this backend has functionally tested.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WindowsFilesystem {
    Ntfs,
    Refs,
}

/// Why a caller needs the volume, named in refusals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VolumeRole {
    /// The application data root or a store beneath it.
    PrivateStore,
    /// The directory that holds a file being replaced or removed.
    ReplacementTarget,
    /// A directory granted to a Windows AppContainer.
    ContainerGrant,
}

impl VolumeRole {
    fn subject(self) -> &'static str {
        match self {
            Self::PrivateStore => "private storage volume",
            Self::ReplacementTarget => "workspace volume",
            Self::ContainerGrant => "container grant volume",
        }
    }
}

/// Refuses a volume that is not NTFS or ReFS with every required capability.
///
/// The name and flags come from the open handle, so no path is resolved. A
/// refusal is `Unsupported` and names the role and the filesystem.
#[allow(unsafe_code)]
pub(crate) fn require_supported_filesystem(
    file: &File,
    role: VolumeRole,
) -> io::Result<WindowsFilesystem> {
    let mut name = [0u16; FILE_SYSTEM_NAME_UNITS];
    let capacity = u32::try_from(name.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "name buffer too large"))?;
    let mut flags = 0u32;
    // SAFETY: `file` is live, `flags` is writable, and `name` is writable for
    // `capacity` units. The other optional outputs are null, which the API
    // permits.
    let read = unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle() as HANDLE,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut flags,
            name.as_mut_ptr(),
            capacity,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    let length = name
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(name.len());
    classify_volume(&name[..length], flags, role)
}

/// Classifies a filesystem name and capability flags without native calls.
pub(crate) fn classify_volume(
    name: &[u16],
    flags: u32,
    role: VolumeRole,
) -> io::Result<WindowsFilesystem> {
    let filesystem = if "NTFS".encode_utf16().eq(name.iter().copied()) {
        WindowsFilesystem::Ntfs
    } else if "ReFS".encode_utf16().eq(name.iter().copied()) {
        WindowsFilesystem::Refs
    } else {
        return Err(refusal(format!(
            "{} uses {}; file writes require NTFS or ReFS",
            role.subject(),
            reported_name(name)
        )));
    };
    if flags & REQUIRED_FLAGS != REQUIRED_FLAGS {
        return Err(refusal(format!(
            "{} uses {} without ACL, POSIX rename, Unicode name, or extended-attribute support",
            role.subject(),
            reported_name(name)
        )));
    }
    Ok(filesystem)
}

/// Display text only; the comparison above uses the native units.
fn reported_name(name: &[u16]) -> String {
    if name.is_empty() {
        return "an unnamed filesystem".to_owned();
    }
    let shown = name.get(..MAX_REPORTED_NAME).unwrap_or(name);
    String::from_utf16_lossy(shown)
}

fn refusal(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(name: &str) -> Vec<u16> {
        name.encode_utf16().collect()
    }

    fn refusal_text(name: &str, flags: u32, role: VolumeRole) -> String {
        let error = classify_volume(&units(name), flags, role).expect_err("refused");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        error.to_string()
    }

    #[test]
    fn given_ntfs_with_required_flags_when_classified_then_ntfs_is_accepted() {
        assert_eq!(
            classify_volume(&units("NTFS"), REQUIRED_FLAGS, VolumeRole::PrivateStore)
                .expect("ntfs"),
            WindowsFilesystem::Ntfs
        );
    }

    #[test]
    fn given_refs_with_required_flags_when_classified_then_refs_is_accepted() {
        // Flags observed on a Windows 11 build 26300 Dev Drive.
        assert_eq!(
            classify_volume(&units("ReFS"), 0x5FC6_36CF, VolumeRole::ReplacementTarget)
                .expect("refs"),
            WindowsFilesystem::Refs
        );
    }

    #[test]
    fn given_another_filesystem_when_classified_then_the_role_and_name_are_reported() {
        for name in ["exFAT", "FAT32", "FAT", "CSVFS", "ntfs", "REFS"] {
            assert_eq!(
                refusal_text(name, u32::MAX, VolumeRole::ReplacementTarget),
                format!("workspace volume uses {name}; file writes require NTFS or ReFS")
            );
        }
    }

    #[test]
    fn given_an_empty_name_when_classified_then_it_is_refused() {
        assert_eq!(
            refusal_text("", u32::MAX, VolumeRole::ContainerGrant),
            "container grant volume uses an unnamed filesystem; file writes require NTFS or ReFS"
        );
    }

    #[test]
    fn given_refs_missing_one_required_flag_when_classified_then_it_is_refused() {
        for missing in [
            FILE_PERSISTENT_ACLS,
            FILE_SUPPORTS_POSIX_UNLINK_RENAME,
            FILE_CASE_PRESERVED_NAMES,
            FILE_UNICODE_ON_DISK,
            FILE_SUPPORTS_EXTENDED_ATTRIBUTES,
        ] {
            assert_eq!(
                refusal_text("ReFS", REQUIRED_FLAGS & !missing, VolumeRole::PrivateStore),
                "private storage volume uses ReFS without ACL, POSIX rename, Unicode name, \
                 or extended-attribute support"
            );
        }
    }

    #[test]
    fn given_a_long_unknown_name_when_classified_then_the_reported_name_is_bounded() {
        let long = "X".repeat(300);
        let text = refusal_text(&long, u32::MAX, VolumeRole::PrivateStore);
        assert!(text.contains(&"X".repeat(MAX_REPORTED_NAME)));
        assert!(!text.contains(&"X".repeat(MAX_REPORTED_NAME + 1)));
    }

    /// CI sets the variable for each filesystem job, so a job whose temporary
    /// directory landed on another volume fails instead of passing silently.
    #[test]
    #[ignore = "set OMGCODE_TEST_EXPECT_FS to NTFS or ReFS for a filesystem job"]
    fn the_temporary_directory_is_on_the_expected_filesystem() {
        let expected = std::env::var("OMGCODE_TEST_EXPECT_FS")
            .expect("OMGCODE_TEST_EXPECT_FS names the expected filesystem");
        let directory = tempfile::tempdir().expect("temporary directory");
        let file = File::create(directory.path().join("probe")).expect("probe");
        let actual = match require_supported_filesystem(&file, VolumeRole::PrivateStore)
            .expect("supported temporary filesystem")
        {
            WindowsFilesystem::Ntfs => "NTFS",
            WindowsFilesystem::Refs => "ReFS",
        };
        assert_eq!(actual, expected, "temporary directory filesystem");
    }

    #[test]
    fn given_the_test_temporary_volume_when_queried_then_it_is_supported() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let file = File::create(directory.path().join("probe")).expect("probe");
        require_supported_filesystem(&file, VolumeRole::PrivateStore)
            .expect("test temporary storage must be on NTFS or ReFS");
    }
}
