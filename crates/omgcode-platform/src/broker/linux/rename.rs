//! Linux handle-relative rename of one pinned regular file that never replaces
//! an entry (ADR-0055).
//!
//! The kernel call is `renameat2` with `RENAME_NOREPLACE` between two parent
//! directory descriptors that were opened beneath the workspace without links
//! or mount crossings. The source is pinned and read through the pinned
//! object first. The name is compared with the pinned inode immediately before
//! the call and the destination name is compared with it afterwards.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use rustix::fs::{Mode, OFlags, RenameFlags, ResolveFlags, fsync, openat2, renameat_with};
use rustix::io::Errno;

use super::{
    NativeWorkspace, RelativePath, invalid_data, invalid_input, regular_single_link_snapshot,
    reopen_pinned_file,
};
use crate::broker::RenameError;

/// Points where a test can interrupt the protocol with an injected failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RenamePoint {
    /// After the last source check; a failure here renames nothing.
    BeforeRename,
    /// After the kernel call, before the destination is verified.
    PublishedCheck,
    /// Before the destination parent is flushed.
    TargetParentSync,
    /// Before the source parent is flushed.
    SourceParentSync,
}

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
        let workspace = self.workspace_handle().map_err(RenameError::Refused)?;
        let (from_parent, from_name) = self
            .parent_and_name(&workspace, from)
            .map_err(RenameError::Refused)?;
        let (to_parent, to_name) = self
            .parent_and_name(&workspace, to)
            .map_err(RenameError::Refused)?;
        let pinned = open_name(&from_parent, &from_name).map_err(source_error)?;
        let snapshot = regular_single_link_snapshot(&pinned).map_err(RenameError::Refused)?;
        if snapshot.owner != rustix::process::geteuid().as_raw() {
            return Err(RenameError::Refused(io::Error::new(
                io::ErrorKind::Unsupported,
                "rename requires a source owned by the effective user",
            )));
        }
        if snapshot.length != expected_len {
            return Err(RenameError::SourceChanged);
        }
        let from_directory = from_parent.metadata().map_err(RenameError::Refused)?;
        let to_directory = to_parent.metadata().map_err(RenameError::Refused)?;
        if from_directory.dev() != to_directory.dev() || snapshot.device != to_directory.dev() {
            return Err(RenameError::Refused(io::Error::new(
                io::ErrorKind::Unsupported,
                "rename source and destination are on different filesystems",
            )));
        }
        let mut readable = reopen_pinned_file(&pinned).map_err(RenameError::Refused)?;
        let reopened = regular_single_link_snapshot(&readable).map_err(RenameError::Refused)?;
        if !snapshot.same_object_and_version(reopened) {
            return Err(RenameError::SourceChanged);
        }
        let limit = expected_len
            .checked_add(1)
            .ok_or_else(|| RenameError::Refused(invalid_input("rename size bound overflows")))?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut readable)
            .take(limit)
            .read_to_end(&mut bytes)
            .map_err(RenameError::Refused)?;
        let read_length = u64::try_from(bytes.len())
            .map_err(|_| RenameError::Refused(invalid_data("rename source length overflows")))?;
        if read_length != expected_len || !matches(&bytes) {
            return Err(RenameError::SourceChanged);
        }
        let after = regular_single_link_snapshot(&readable).map_err(RenameError::Refused)?;
        if !snapshot.same_object_and_version(after) {
            return Err(RenameError::SourceChanged);
        }
        // The name must still be the pinned inode immediately before the call.
        let current = open_name(&from_parent, &from_name).map_err(source_error)?;
        let current = current.metadata().map_err(RenameError::Refused)?;
        if current.dev() != snapshot.device || current.ino() != snapshot.inode {
            return Err(RenameError::SourceChanged);
        }
        hook(RenamePoint::BeforeRename).map_err(RenameError::Refused)?;
        match renameat_with(
            &from_parent,
            &from_name,
            &to_parent,
            &to_name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {}
            Err(Errno::EXIST) => return Err(RenameError::TargetExists),
            Err(error) if is_definite_refusal(error) => {
                return Err(RenameError::Refused(error.into()));
            }
            Err(error) => return Err(RenameError::Uncertain(error.into())),
        }
        // From here the rename happened. Any failure is reported as uncertain
        // durability or identity and is never retried.
        let same_directory = from_directory.dev() == to_directory.dev()
            && from_directory.ino() == to_directory.ino();
        (|| -> io::Result<()> {
            hook(RenamePoint::PublishedCheck)?;
            let renamed = open_name(&to_parent, &to_name)?.metadata()?;
            if renamed.dev() != snapshot.device || renamed.ino() != snapshot.inode {
                return Err(invalid_data(
                    "renamed name does not resolve to the pinned source",
                ));
            }
            hook(RenamePoint::TargetParentSync)?;
            fsync(&to_parent)?;
            if !same_directory {
                hook(RenamePoint::SourceParentSync)?;
                fsync(&from_parent)?;
            }
            Ok(())
        })()
        .map_err(RenameError::Uncertain)
    }
}

/// Open a name as an `O_PATH` handle without following links or leaving the
/// mount, the same way replacement pins its target.
fn open_name(parent: &File, name: &OsString) -> io::Result<File> {
    Ok(File::from(openat2(
        parent,
        Path::new(name),
        OFlags::PATH | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH
            | ResolveFlags::NO_SYMLINKS
            | ResolveFlags::NO_MAGICLINKS
            | ResolveFlags::NO_XDEV,
    )?))
}

/// A missing source name means it changed since the caller's read.
fn source_error(error: io::Error) -> RenameError {
    if error.kind() == io::ErrorKind::NotFound {
        RenameError::SourceChanged
    } else {
        RenameError::Refused(error)
    }
}

/// Kernel failures that prove `renameat2` renamed nothing. Any other failure
/// is reported as uncertain.
fn is_definite_refusal(error: Errno) -> bool {
    [
        Errno::NOENT,
        Errno::ACCESS,
        Errno::PERM,
        Errno::ROFS,
        Errno::XDEV,
        Errno::INVAL,
        Errno::NOSYS,
        Errno::NOTDIR,
        Errno::ISDIR,
        Errno::LOOP,
        Errno::NAMETOOLONG,
        Errno::NOSPC,
        Errno::DQUOT,
        Errno::BUSY,
        Errno::MLINK,
    ]
    .contains(&error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::linux::tests::{relative, workspace};
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The workspace directory inside the fixture's temporary root.
    fn ws(directory: &tempfile::TempDir) -> std::path::PathBuf {
        directory.path().join("root").join("workspace")
    }

    fn digest_matches(expected: &'static [u8]) -> impl Fn(&[u8]) -> bool {
        move |bytes| bytes == expected
    }

    #[test]
    fn rename_moves_one_file_and_keeps_its_bytes() {
        let (directory, native) = workspace();
        fs::write(ws(&directory).join("a.txt"), b"alpha").expect("source");
        fs::create_dir(ws(&directory).join("sub")).expect("destination parent");
        native
            .rename_no_replace(
                &relative("a.txt"),
                &relative("sub/b.txt"),
                5,
                &digest_matches(b"alpha"),
            )
            .expect("rename");
        assert!(!ws(&directory).join("a.txt").exists());
        assert_eq!(
            fs::read(ws(&directory).join("sub/b.txt")).expect("renamed"),
            b"alpha"
        );
    }

    #[test]
    fn rename_refuses_an_existing_destination_and_keeps_both_files() {
        let (directory, native) = workspace();
        fs::write(ws(&directory).join("a.txt"), b"alpha").expect("source");
        fs::write(ws(&directory).join("b.txt"), b"beta").expect("existing");
        let error = native
            .rename_no_replace(
                &relative("a.txt"),
                &relative("b.txt"),
                5,
                &digest_matches(b"alpha"),
            )
            .expect_err("an existing destination is never replaced");
        assert!(matches!(error, RenameError::TargetExists), "{error:?}");
        assert_eq!(fs::read(ws(&directory).join("a.txt")).expect("a"), b"alpha");
        assert_eq!(fs::read(ws(&directory).join("b.txt")).expect("b"), b"beta");
    }

    #[test]
    fn rename_refuses_directories_links_and_hard_links() {
        let (directory, native) = workspace();
        fs::create_dir(ws(&directory).join("dir")).expect("directory");
        fs::write(ws(&directory).join("file"), b"x").expect("file");
        std::os::unix::fs::symlink("file", ws(&directory).join("link")).expect("link");
        fs::hard_link(ws(&directory).join("file"), ws(&directory).join("twin")).expect("hard link");
        for name in ["dir", "link", "file", "twin"] {
            let error = native
                .rename_no_replace(
                    &relative(name),
                    &relative("moved"),
                    1,
                    &digest_matches(b"x"),
                )
                .expect_err("refused before any rename");
            assert!(
                matches!(error, RenameError::Refused(_)),
                "{name}: {error:?}"
            );
        }
        assert!(!ws(&directory).join("moved").exists());
    }

    #[test]
    fn rename_refuses_a_source_whose_bytes_or_length_differ() {
        let (directory, native) = workspace();
        fs::write(ws(&directory).join("a.txt"), b"alpha").expect("source");
        for (length, expected) in [
            (5u64, b"alphx" as &'static [u8]),
            (4u64, b"alph" as &'static [u8]),
        ] {
            let error = native
                .rename_no_replace(
                    &relative("a.txt"),
                    &relative("b.txt"),
                    length,
                    &digest_matches(expected),
                )
                .expect_err("a different source is refused");
            assert!(matches!(error, RenameError::SourceChanged), "{error:?}");
        }
        assert!(ws(&directory).join("a.txt").exists());
        assert!(!ws(&directory).join("b.txt").exists());
    }

    #[test]
    fn a_source_swapped_before_the_call_is_refused() {
        let (directory, native) = workspace();
        let source = ws(&directory).join("a.txt");
        fs::write(&source, b"alpha").expect("source");
        let swap = source.clone();
        let error = native
            .rename_with_hook(
                &relative("a.txt"),
                &relative("b.txt"),
                5,
                &digest_matches(b"alpha"),
                &move |point| {
                    // The hook runs after the last check, so a swap here is the
                    // race that the post-call identity check must expose.
                    if point == RenamePoint::BeforeRename {
                        fs::remove_file(&swap)?;
                        fs::write(&swap, b"other")?;
                    }
                    Ok(())
                },
            )
            .expect_err("the destination no longer holds the pinned inode");
        assert!(matches!(error, RenameError::Uncertain(_)), "{error:?}");
    }

    #[test]
    fn faults_before_and_after_the_call_are_refused_or_uncertain() {
        let (directory, native) = workspace();
        fs::write(ws(&directory).join("a.txt"), b"alpha").expect("source");
        let error = native
            .rename_with_hook(
                &relative("a.txt"),
                &relative("b.txt"),
                5,
                &digest_matches(b"alpha"),
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
        assert!(ws(&directory).join("a.txt").exists());

        let calls = AtomicUsize::new(0);
        let error = native
            .rename_with_hook(
                &relative("a.txt"),
                &relative("b.txt"),
                5,
                &digest_matches(b"alpha"),
                &|point| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    if point == RenamePoint::TargetParentSync {
                        Err(io::Error::other("injected"))
                    } else {
                        Ok(())
                    }
                },
            )
            .expect_err("a fault after the call is uncertain");
        assert!(matches!(error, RenameError::Uncertain(_)), "{error:?}");
        assert!(ws(&directory).join("b.txt").exists(), "the rename happened");
        assert!(!ws(&directory).join("a.txt").exists());
    }
}
