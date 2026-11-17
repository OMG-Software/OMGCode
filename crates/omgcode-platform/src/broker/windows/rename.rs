//! Windows handle-relative rename of one pinned regular file that never
//! replaces an entry (ADR-0055).
//!
//! The source is opened relative to its parent with DELETE access and with
//! sharing that denies every other writer and deleter, so the name cannot be
//! swapped or removed while the handle is held. Its bytes are read through
//! that handle. The rename is `NtSetInformationFile` with
//! `FileRenameInformationEx`, POSIX semantics, and no replace-if-exists flag,
//! with the destination parent as the root directory. An existing name fails
//! with a collision status. Afterwards the destination is reopened and must be
//! the same file. Windows directory flushes carry no power-loss claim
//! (ADR-0023).

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Seek, SeekFrom};

use windows_sys::Wdk::Storage::FileSystem::FILE_OPEN;
use windows_sys::Win32::Foundation::{
    NTSTATUS, STATUS_ACCESS_DENIED, STATUS_DELETE_PENDING, STATUS_DIRECTORY_NOT_EMPTY,
    STATUS_FILE_IS_A_DIRECTORY, STATUS_INVALID_PARAMETER, STATUS_NOT_SAME_DEVICE,
    STATUS_NOT_SUPPORTED, STATUS_OBJECT_PATH_NOT_FOUND, STATUS_SHARING_VIOLATION,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ADD_FILE, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, SYNCHRONIZE,
};

use super::{
    DIRECTORY_ACCESS, NativeStatus, NativeWorkspace, RelativePath, SHARE_READ_WRITE, invalid_data,
    is_name_not_found, last_directory, open_relative, query_identity, query_standard, read_bounded,
    rename_no_replace_onto, validate_component, validate_directory, validate_regular_file,
};
use crate::broker::RenameError;
use crate::directory_identity;
use crate::private_store::windows::{DIRECTORY_OPTIONS, FILE_OPTIONS, FILE_READ_ACCESS};

/// Points where a test can interrupt the protocol with an injected failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RenamePoint {
    /// After the last source check; a failure here renames nothing.
    BeforeRename,
    /// After the native call, before the destination is verified.
    PublishedCheck,
    /// Before the destination parent is flushed.
    TargetParentSync,
    /// Before the source parent is flushed.
    SourceParentSync,
}

/// Statuses that prove the native call renamed nothing. Any other failure is
/// reported as uncertain.
const DEFINITE_REFUSALS: [NTSTATUS; 9] = [
    STATUS_ACCESS_DENIED,
    STATUS_SHARING_VIOLATION,
    STATUS_OBJECT_PATH_NOT_FOUND,
    STATUS_NOT_SAME_DEVICE,
    STATUS_INVALID_PARAMETER,
    STATUS_NOT_SUPPORTED,
    STATUS_DELETE_PENDING,
    STATUS_FILE_IS_A_DIRECTORY,
    STATUS_DIRECTORY_NOT_EMPTY,
];

impl NativeWorkspace {
    pub(in crate::broker) fn rename_no_replace(
        &self,
        from: &RelativePath,
        to: &RelativePath,
        expected_len: u64,
        matches: &dyn Fn(&[u8]) -> bool,
    ) -> Result<(), RenameError> {
        self.rename_with_hook(from, to, expected_len, matches, &|_| Ok(()))
    }

    pub(super) fn rename_with_hook(
        &self,
        from: &RelativePath,
        to: &RelativePath,
        expected_len: u64,
        matches: &dyn Fn(&[u8]) -> bool,
        hook: &dyn Fn(RenamePoint) -> io::Result<()>,
    ) -> Result<(), RenameError> {
        let refuse = RenameError::Refused;
        self.ensure_current().map_err(refuse)?;
        let (from_handles, from_name) = self.open_parent(from).map_err(refuse)?;
        let (to_handles, to_name) = self.open_parent(to).map_err(refuse)?;
        validate_component(&from_name).map_err(refuse)?;
        validate_component(&to_name).map_err(refuse)?;
        let from_directory = last_directory(&from_handles).map_err(refuse)?;
        let to_directory = last_directory(&to_handles).map_err(refuse)?;
        let volume = self.workspace_identity.volume;

        // Deny every other writer and deleter for as long as this handle lives.
        let source = open_relative(
            from_directory,
            &from_name,
            FILE_READ_ACCESS | DELETE,
            FILE_OPEN,
            FILE_OPTIONS,
            FILE_SHARE_READ,
            0,
        )
        .map_err(|error| {
            if is_name_not_found(&error) {
                RenameError::SourceChanged
            } else {
                RenameError::Refused(error)
            }
        })?;
        validate_regular_file(&source, volume).map_err(refuse)?;
        let source_identity = query_identity(&source).map_err(refuse)?;
        let length = u64::try_from(query_standard(&source).map_err(refuse)?.EndOfFile)
            .map_err(|_| RenameError::Refused(invalid_data("native file size is negative")))?;
        if length != expected_len {
            return Err(RenameError::SourceChanged);
        }
        let limit = usize::try_from(expected_len)
            .map_err(|_| RenameError::Refused(invalid_data("rename size is out of range")))?;
        let mut reader = source.try_clone().map_err(refuse)?;
        reader.seek(SeekFrom::Start(0)).map_err(refuse)?;
        let bytes = read_bounded(reader, limit).map_err(refuse)?;
        if bytes.len() != limit || !matches(&bytes) {
            return Err(RenameError::SourceChanged);
        }
        let after = query_identity(&source).map_err(refuse)?;
        if after.VolumeSerialNumber != source_identity.VolumeSerialNumber
            || after.FileId.Identifier != source_identity.FileId.Identifier
        {
            return Err(RenameError::SourceChanged);
        }

        // Reopen both parents with the access that rename and flush need.
        let reopen = |directory: &File| {
            open_relative(
                directory,
                OsStr::new(""),
                DIRECTORY_ACCESS | FILE_ADD_FILE,
                FILE_OPEN,
                DIRECTORY_OPTIONS,
                SHARE_READ_WRITE,
                0,
            )
        };
        let target_parent = reopen(to_directory).map_err(refuse)?;
        let source_parent = reopen(from_directory).map_err(refuse)?;
        validate_directory(&target_parent, volume).map_err(refuse)?;
        validate_directory(&source_parent, volume).map_err(refuse)?;
        if source_identity.VolumeSerialNumber != volume {
            return Err(RenameError::Refused(io::Error::new(
                io::ErrorKind::Unsupported,
                "rename source is not on the workspace volume",
            )));
        }
        hook(RenamePoint::BeforeRename).map_err(RenameError::Refused)?;
        if let Err(error) = rename_no_replace_onto(&source, &target_parent, &to_name) {
            return Err(classify(error));
        }

        // From here the rename happened. Any failure is reported as uncertain
        // identity or durability and is never retried.
        let same_directory = directory_identity(&target_parent).map_err(RenameError::Uncertain)?
            == directory_identity(&source_parent).map_err(RenameError::Uncertain)?;
        (|| -> io::Result<()> {
            hook(RenamePoint::PublishedCheck)?;
            let renamed = open_relative(
                to_directory,
                &to_name,
                FILE_READ_ACCESS,
                FILE_OPEN,
                FILE_OPTIONS,
                FILE_SHARE_READ | FILE_SHARE_DELETE,
                0,
            )?;
            validate_regular_file(&renamed, volume)?;
            let identity = query_identity(&renamed)?;
            if identity.VolumeSerialNumber != source_identity.VolumeSerialNumber
                || identity.FileId.Identifier != source_identity.FileId.Identifier
            {
                return Err(invalid_data(
                    "renamed name does not resolve to the pinned source",
                ));
            }
            match open_relative(
                from_directory,
                &from_name,
                FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                FILE_OPEN,
                FILE_OPTIONS,
                FILE_SHARE_READ | FILE_SHARE_DELETE,
                0,
            ) {
                Err(error) if is_name_not_found(&error) => {}
                // Another entry may have taken the old name; the pinned
                // source must not still be there.
                Ok(old)
                    if query_identity(&old)?.FileId.Identifier
                        == source_identity.FileId.Identifier =>
                {
                    return Err(invalid_data("the old name still resolves to the source"));
                }
                Ok(_) => {}
                Err(error) => return Err(error),
            }
            hook(RenamePoint::TargetParentSync)?;
            target_parent.sync_all()?;
            if !same_directory {
                hook(RenamePoint::SourceParentSync)?;
                source_parent.sync_all()?;
            }
            Ok(())
        })()
        .map_err(RenameError::Uncertain)
    }
}

/// Map a native rename failure: a collision proves the destination exists, a
/// known refusal status proves nothing was renamed, anything else is uncertain.
fn classify(error: io::Error) -> RenameError {
    if error.kind() == io::ErrorKind::AlreadyExists {
        return RenameError::TargetExists;
    }
    if error.kind() == io::ErrorKind::InvalidInput {
        return RenameError::Refused(error);
    }
    let refused = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<NativeStatus>())
        .is_some_and(|status| DEFINITE_REFUSALS.contains(&status.0));
    if refused {
        RenameError::Refused(error)
    } else {
        RenameError::Uncertain(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::windows::tests::create_junction_bounded;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Fixture {
        workspace: NativeWorkspace,
        directory: PathBuf,
        _root: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().expect("root");
        let directory = root.path().join("workspace");
        fs::create_dir(&directory).expect("workspace");
        let open = |path: &Path| {
            directory_identity(&super::super::open_configured_root(path).expect("directory"))
                .expect("identity")
        };
        let workspace = NativeWorkspace::open(
            root.path(),
            open(root.path()),
            &RelativePath::new(Path::new("workspace")).expect("workspace path"),
            open(&directory),
        )
        .expect("native workspace");
        Fixture {
            workspace,
            directory,
            _root: root,
        }
    }

    fn relative(path: &str) -> RelativePath {
        RelativePath::new(Path::new(path)).expect("relative path")
    }

    fn accepts(expected: &'static [u8]) -> impl Fn(&[u8]) -> bool {
        move |bytes| bytes == expected
    }

    #[test]
    fn rename_moves_a_file_across_directories_and_keeps_its_identity() {
        let fixture = fixture();
        fs::write(fixture.directory.join("a.txt"), b"alpha").expect("source");
        fs::create_dir(fixture.directory.join("sub")).expect("destination parent");
        let before = fs::metadata(fixture.directory.join("a.txt")).expect("metadata");
        fixture
            .workspace
            .rename_no_replace(
                &relative("a.txt"),
                &relative("sub/b.txt"),
                5,
                &accepts(b"alpha"),
            )
            .expect("rename");
        assert!(!fixture.directory.join("a.txt").exists());
        assert_eq!(
            fs::read(fixture.directory.join("sub/b.txt")).expect("renamed"),
            b"alpha"
        );
        let after = fs::metadata(fixture.directory.join("sub/b.txt")).expect("metadata");
        assert_eq!(before.len(), after.len());
    }

    #[test]
    fn rename_refuses_an_existing_destination_and_keeps_both_files() {
        let fixture = fixture();
        fs::write(fixture.directory.join("a.txt"), b"alpha").expect("source");
        fs::write(fixture.directory.join("b.txt"), b"beta").expect("existing");
        let error = fixture
            .workspace
            .rename_no_replace(
                &relative("a.txt"),
                &relative("b.txt"),
                5,
                &accepts(b"alpha"),
            )
            .expect_err("an existing destination is never replaced");
        assert!(matches!(error, RenameError::TargetExists), "{error:?}");
        assert_eq!(
            fs::read(fixture.directory.join("a.txt")).expect("a"),
            b"alpha"
        );
        assert_eq!(
            fs::read(fixture.directory.join("b.txt")).expect("b"),
            b"beta"
        );
    }

    #[test]
    fn rename_refuses_directories_junctions_and_hard_links() {
        let fixture = fixture();
        fs::create_dir(fixture.directory.join("dir")).expect("directory");
        fs::write(fixture.directory.join("file"), b"x").expect("file");
        fs::hard_link(
            fixture.directory.join("file"),
            fixture.directory.join("twin"),
        )
        .expect("hard link");
        let outside = fixture._root.path().join("outside");
        fs::create_dir(&outside).expect("outside");
        create_junction_bounded(&fixture.directory.join("junction"), &outside)
            .expect("junction fixture");
        for name in ["dir", "junction", "file", "twin"] {
            let error = fixture
                .workspace
                .rename_no_replace(&relative(name), &relative("moved"), 1, &accepts(b"x"))
                .expect_err("refused before any rename");
            assert!(
                matches!(error, RenameError::Refused(_)),
                "{name}: {error:?}"
            );
        }
        assert!(!fixture.directory.join("moved").exists());
        assert!(outside.exists());
    }

    #[test]
    fn rename_refuses_a_source_whose_bytes_or_length_differ() {
        let fixture = fixture();
        fs::write(fixture.directory.join("a.txt"), b"alpha").expect("source");
        for (length, expected) in [
            (5u64, b"alphx" as &'static [u8]),
            (4u64, b"alph" as &'static [u8]),
        ] {
            let error = fixture
                .workspace
                .rename_no_replace(
                    &relative("a.txt"),
                    &relative("b.txt"),
                    length,
                    &accepts(expected),
                )
                .expect_err("a different source is refused");
            assert!(matches!(error, RenameError::SourceChanged), "{error:?}");
        }
        assert!(fixture.directory.join("a.txt").exists());
        assert!(!fixture.directory.join("b.txt").exists());
    }

    #[test]
    fn a_missing_source_is_a_changed_source() {
        let fixture = fixture();
        let error = fixture
            .workspace
            .rename_no_replace(&relative("a.txt"), &relative("b.txt"), 0, &|_| true)
            .expect_err("nothing to rename");
        assert!(matches!(error, RenameError::SourceChanged), "{error:?}");
    }

    #[test]
    fn the_pinned_source_cannot_be_swapped_or_removed_before_the_call() {
        let fixture = fixture();
        let source = fixture.directory.join("a.txt");
        fs::write(&source, b"alpha").expect("source");
        let blocked = AtomicBool::new(false);
        fixture
            .workspace
            .rename_with_hook(
                &relative("a.txt"),
                &relative("b.txt"),
                5,
                &accepts(b"alpha"),
                &|point| {
                    if point == RenamePoint::BeforeRename {
                        // The retained handle denies delete sharing, so the
                        // swap that the protocol must exclude cannot happen.
                        let moved = fixture.directory.join("moved.txt");
                        blocked.store(
                            fs::rename(&source, &moved).is_err()
                                && fs::remove_file(&source).is_err(),
                            Ordering::SeqCst,
                        );
                    }
                    Ok(())
                },
            )
            .expect("rename");
        assert!(blocked.load(Ordering::SeqCst));
        assert_eq!(
            fs::read(fixture.directory.join("b.txt")).expect("renamed"),
            b"alpha"
        );
    }

    #[test]
    fn faults_before_and_after_the_call_are_refused_or_uncertain() {
        let fixture = fixture();
        fs::write(fixture.directory.join("a.txt"), b"alpha").expect("source");
        let error = fixture
            .workspace
            .rename_with_hook(
                &relative("a.txt"),
                &relative("b.txt"),
                5,
                &accepts(b"alpha"),
                &|point| {
                    if point == RenamePoint::BeforeRename {
                        Err(io::Error::other("injected"))
                    } else {
                        Ok(())
                    }
                },
            )
            .expect_err("a fault before the call renames nothing");
        assert!(matches!(error, RenameError::Refused(_)), "{error:?}");
        assert!(fixture.directory.join("a.txt").exists());

        for failing in [RenamePoint::PublishedCheck, RenamePoint::TargetParentSync] {
            let fixture = self::fixture();
            fs::write(fixture.directory.join("a.txt"), b"alpha").expect("source");
            let error = fixture
                .workspace
                .rename_with_hook(
                    &relative("a.txt"),
                    &relative("b.txt"),
                    5,
                    &accepts(b"alpha"),
                    &|point| {
                        if point == failing {
                            Err(io::Error::other("injected"))
                        } else {
                            Ok(())
                        }
                    },
                )
                .expect_err("a fault after the call is uncertain");
            assert!(matches!(error, RenameError::Uncertain(_)), "{error:?}");
            assert!(fixture.directory.join("b.txt").exists(), "{failing:?}");
            assert!(!fixture.directory.join("a.txt").exists(), "{failing:?}");
        }
    }
}
