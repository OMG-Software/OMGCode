//! Read-only inode attributes that statx does not report, per filesystem.
//!
//! The policy comes from the open file's own filesystem magic, so no path or
//! caller-supplied type is trusted. Only ext4, btrfs, XFS, tmpfs, and ZFS are
//! accepted (ADR-0068). Every visible flag a stage could not carry is refused.

use std::{fs::File, io, mem::size_of};

use rustix::fs::{fstatfs, ioctl_getflags};
use rustix::io::Errno;
use rustix::ioctl::{self, Opcode, Updater, opcode};

// Linux UAPI FS_EXTENT_FL is an ext4 allocation representation, not a
// permission. Do not silently preserve or discard any other visible flag.
const EXTENTS: u32 = 0x0008_0000;

// Filesystem magic numbers from include/uapi/linux/magic.h and OpenZFS.
const EXT4_SUPER_MAGIC: u32 = 0xEF53;
const BTRFS_SUPER_MAGIC: u32 = 0x9123_683E;
const XFS_SUPER_MAGIC: u32 = 0x5846_5342;
const TMPFS_MAGIC: u32 = 0x0102_1994;
const ZFS_SUPER_MAGIC: u32 = 0x2FC1_2FC1;

/// A Linux filesystem this backend has functionally tested.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinuxFilesystem {
    Ext4,
    Btrfs,
    Xfs,
    Tmpfs,
    Zfs,
}

impl LinuxFilesystem {
    /// Maps a `/proc/self/mountinfo` filesystem type.
    pub(crate) fn from_mount_type(name: &[u8]) -> Option<Self> {
        match name {
            b"ext4" => Some(Self::Ext4),
            b"btrfs" => Some(Self::Btrfs),
            b"xfs" => Some(Self::Xfs),
            b"tmpfs" => Some(Self::Tmpfs),
            b"zfs" => Some(Self::Zfs),
            _ => None,
        }
    }

    fn from_magic(magic: u32) -> Option<Self> {
        match magic {
            EXT4_SUPER_MAGIC => Some(Self::Ext4),
            BTRFS_SUPER_MAGIC => Some(Self::Btrfs),
            XFS_SUPER_MAGIC => Some(Self::Xfs),
            TMPFS_MAGIC => Some(Self::Tmpfs),
            ZFS_SUPER_MAGIC => Some(Self::Zfs),
            _ => None,
        }
    }

    /// `FS_IOC_GETFLAGS` bits a new inode on this filesystem may carry.
    fn allowed_flags(self) -> u32 {
        match self {
            Self::Ext4 => EXTENTS,
            Self::Btrfs | Self::Xfs | Self::Tmpfs | Self::Zfs => 0,
        }
    }

    /// Whether a missing attribute ioctl means "no attributes". Only tmpfs
    /// before Linux 6.0 lacks them, and it has no persistent inode flags.
    /// This is about attributes, not persistence: tmpfs itself is not
    /// persistent, so its checkpoints end at unmount (ADR-0068). On every
    /// other filesystem a missing ioctl could hide flags, so it refuses.
    fn missing_ioctl_means_none(self) -> bool {
        self == Self::Tmpfs
    }
}

// Linux include/uapi/linux/fs.h, struct fsxattr. All fields have valid values
// for every bit pattern, and the reserved bytes are part of the output buffer.
#[repr(C)]
#[derive(Default)]
struct FilesystemAttributes {
    flags: u32,
    extent_size: u32,
    extent_count: u32,
    project: u32,
    cow_extent_size: u32,
    reserved: [u8; 8],
}

const _: () = assert!(size_of::<FilesystemAttributes>() == 28);
const FSGETXATTR: Opcode = opcode::read::<FilesystemAttributes>(b'X', 31);

/// Reject unpreserved inode flags, project IDs, and allocation hints on the
/// same regular file. A missing or failed metadata query prevents publication.
pub(super) fn validate_file_attributes(file: &File) -> io::Result<()> {
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a regular file",
        ));
    }
    let filesystem = file_filesystem(file)?;
    let flags = match ioctl_getflags(file) {
        Ok(flags) => flags.bits(),
        Err(error) => missing_attributes(filesystem, error)?,
    };
    validate_flags(filesystem, flags)?;
    match read_attributes(file) {
        Ok(attributes) => validate_attributes(attributes),
        Err(error) => missing_attributes(filesystem, error).map(|_| ()),
    }
}

/// Identifies the filesystem holding this open file from its magic number.
fn file_filesystem(file: &File) -> io::Result<LinuxFilesystem> {
    let status = fstatfs(file)?;
    // `f_type` is a signed word whose width varies by architecture. Magic
    // numbers are 32-bit, so keep the low 32 bits of its two's complement.
    let magic = u32::try_from(i128::from(status.f_type) & 0xFFFF_FFFF)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid filesystem magic"))?;
    LinuxFilesystem::from_magic(magic).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "replacement cannot verify inode attributes on this filesystem",
        )
    })
}

/// Maps an attribute-query failure: no attributes on tmpfs, refusal elsewhere.
fn missing_attributes(filesystem: LinuxFilesystem, error: Errno) -> io::Result<u32> {
    let missing = error == Errno::NOTTY || error == Errno::OPNOTSUPP;
    if missing && filesystem.missing_ioctl_means_none() {
        return Ok(0);
    }
    if missing {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "replacement cannot read inode attributes on this filesystem",
        ));
    }
    Err(error.into())
}

fn validate_flags(filesystem: LinuxFilesystem, flags: u32) -> io::Result<()> {
    if flags & !filesystem.allowed_flags() != 0 {
        return Err(unsupported_attributes());
    }
    Ok(())
}

/// `FS_XFLAG_HASATTR`: a read-only status bit for an attribute fork. XFS can
/// report it with no extended attribute left; those are refused separately
/// by `flistxattr`, and `FS_IOC_FSSETXATTR` cannot set or clear the bit.
const XFLAG_HAS_ATTRIBUTE_FORK: u32 = 0x8000_0000;

/// Refuse any settable `fsx_xflags` flag, a project ID, or reserved bytes.
///
/// An extent size or CoW extent size belongs to the inode only while its
/// `FS_XFLAG_EXTSIZE`, `FS_XFLAG_EXTSZINHERIT`, or `FS_XFLAG_COWEXTSIZE`
/// flag is set, and every flag is refused here. A size reported without its
/// flag is the filesystem's default, which a new stage file reports too, so
/// it is not an attribute to preserve. Some kernels report XFS's default CoW
/// extent size this way on a reflink filesystem.
fn validate_attributes(attributes: FilesystemAttributes) -> io::Result<()> {
    if attributes.flags & !XFLAG_HAS_ATTRIBUTE_FORK != 0
        || attributes.project != 0
        || attributes.reserved != [0; 8]
    {
        return Err(unsupported_attributes());
    }
    // The kernel-managed extent count, like the inode number, can change.
    Ok(())
}

// Rustix exposes safe GETFLAGS, but no safe FSGETXATTR wrapper. Keep the
// read-only fixed-layout query here; never pass an input path or raw FD onward.
#[allow(unsafe_code)]
fn read_attributes(file: &File) -> Result<FilesystemAttributes, Errno> {
    let mut attributes = FilesystemAttributes::default();
    // SAFETY: FS_IOC_FSGETXATTR is _IOR('X', 31, struct fsxattr). The C-layout
    // buffer above has exactly that size and alignment and starts initialized.
    // Updater borrows it exclusively through the synchronous ioctl. The kernel
    // only writes the fixed output buffer and retains no pointer. The borrowed
    // File is the live regular-file handle checked by validate_file_attributes.
    unsafe {
        ioctl::ioctl(file, Updater::<FSGETXATTR, _>::new(&mut attributes))?;
    }
    Ok(attributes)
}

fn unsupported_attributes() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "replacement cannot preserve these inode attributes",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::fs::{IFlags, ioctl_setflags};

    const BTRFS_NOCOW: u32 = 0x0080_0000;
    const COMPRESSED: u32 = 0x0000_0004;

    #[test]
    fn given_a_disposable_file_when_nodump_is_set_then_its_attributes_are_refused() {
        // NODUMP is settable by the owner on every accepted filesystem, so this
        // checks the refusal path on whichever one holds the temporary file.
        let file = tempfile::tempfile().expect("disposable file");
        validate_file_attributes(&file).expect("ordinary attributes");
        let original = ioctl_getflags(&file).expect("original inode flags");
        let changed = original | IFlags::NODUMP;
        ioctl_setflags(&file, changed).expect("set disposable file's nodump flag");
        assert_eq!(
            validate_file_attributes(&file)
                .expect_err("nodump cannot be preserved")
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(ioctl_getflags(&file).expect("retained flags"), changed);
    }

    #[test]
    fn filesystem_attributes_allow_allocation_count_but_refuse_project_metadata() {
        validate_attributes(FilesystemAttributes {
            extent_count: 2,
            ..Default::default()
        })
        .expect("allocation count can differ on the new inode");
        let attributed = FilesystemAttributes {
            project: 7,
            ..Default::default()
        };
        assert_eq!(
            validate_attributes(attributed)
                .expect_err("project identity is unsupported")
                .kind(),
            io::ErrorKind::Unsupported
        );
    }

    #[test]
    fn given_each_accepted_mount_type_when_mapped_then_its_magic_maps_to_the_same_kind() {
        for (name, magic) in [
            (&b"ext4"[..], EXT4_SUPER_MAGIC),
            (b"btrfs", BTRFS_SUPER_MAGIC),
            (b"xfs", XFS_SUPER_MAGIC),
            (b"tmpfs", TMPFS_MAGIC),
            (b"zfs", ZFS_SUPER_MAGIC),
        ] {
            let kind = LinuxFilesystem::from_mount_type(name).expect("accepted type");
            assert_eq!(LinuxFilesystem::from_magic(magic), Some(kind));
        }
    }

    #[test]
    fn given_an_unlisted_mount_type_when_mapped_then_it_is_refused() {
        for name in [
            &b"ext3"[..],
            b"nfs4",
            b"overlay",
            b"fuse.sshfs",
            b"vfat",
            b"exfat",
            b"f2fs",
            b"EXT4",
            b"",
        ] {
            assert_eq!(LinuxFilesystem::from_mount_type(name), None);
        }
        assert_eq!(LinuxFilesystem::from_magic(0x6969), None); // NFS
        assert_eq!(LinuxFilesystem::from_magic(0x794C_7630), None); // overlayfs
    }

    #[test]
    fn given_size_hints_without_their_flags_when_validated_then_they_are_defaults() {
        validate_attributes(FilesystemAttributes {
            extent_size: 64 * 1024,
            cow_extent_size: 128 * 4096,
            extent_count: 3,
            ..FilesystemAttributes::default()
        })
        .expect("unflagged sizes are filesystem defaults");
    }

    #[test]
    fn given_only_the_attribute_fork_status_bit_when_validated_then_it_is_accepted() {
        validate_attributes(FilesystemAttributes {
            flags: XFLAG_HAS_ATTRIBUTE_FORK,
            ..FilesystemAttributes::default()
        })
        .expect("the status bit is not a settable attribute");
        let with_flag = FilesystemAttributes {
            flags: XFLAG_HAS_ATTRIBUTE_FORK | 0x0000_0008, // FS_XFLAG_APPEND
            ..FilesystemAttributes::default()
        };
        assert!(validate_attributes(with_flag).is_err());
    }

    #[test]
    fn given_a_flag_project_or_reserved_byte_when_validated_then_it_is_refused() {
        // FS_XFLAG_COWEXTSIZE, FS_XFLAG_EXTSIZE, and FS_XFLAG_PROJINHERIT.
        for flags in [0x0001_0000, 0x0000_0800, 0x0000_0200] {
            let attributes = FilesystemAttributes {
                flags,
                cow_extent_size: 128 * 4096,
                ..FilesystemAttributes::default()
            };
            assert_eq!(
                validate_attributes(attributes)
                    .expect_err("a set hint flag is refused")
                    .to_string(),
                "replacement cannot preserve these inode attributes"
            );
        }
        let project = FilesystemAttributes {
            project: 7,
            ..FilesystemAttributes::default()
        };
        assert!(validate_attributes(project).is_err());
        let reserved = FilesystemAttributes {
            reserved: [1, 0, 0, 0, 0, 0, 0, 0],
            ..FilesystemAttributes::default()
        };
        assert!(validate_attributes(reserved).is_err());
    }

    #[test]
    fn given_ext4_extents_when_validated_then_only_ext4_accepts_the_flag() {
        validate_flags(LinuxFilesystem::Ext4, EXTENTS).expect("ext4 extents");
        for filesystem in [
            LinuxFilesystem::Btrfs,
            LinuxFilesystem::Xfs,
            LinuxFilesystem::Tmpfs,
            LinuxFilesystem::Zfs,
        ] {
            validate_flags(filesystem, 0).expect("no flags");
            assert_eq!(
                validate_flags(filesystem, EXTENTS)
                    .expect_err("extents flag is not expected here")
                    .kind(),
                io::ErrorKind::Unsupported
            );
        }
    }

    #[test]
    fn given_btrfs_nocow_or_compression_when_validated_then_they_are_refused() {
        for flag in [BTRFS_NOCOW, COMPRESSED] {
            assert_eq!(
                validate_flags(LinuxFilesystem::Btrfs, flag)
                    .expect_err("btrfs flag cannot be preserved")
                    .to_string(),
                "replacement cannot preserve these inode attributes"
            );
        }
    }

    #[test]
    fn given_a_missing_attribute_ioctl_when_mapped_then_only_tmpfs_reports_no_attributes() {
        assert_eq!(
            missing_attributes(LinuxFilesystem::Tmpfs, Errno::NOTTY).expect("old tmpfs"),
            0
        );
        for filesystem in [
            LinuxFilesystem::Ext4,
            LinuxFilesystem::Btrfs,
            LinuxFilesystem::Xfs,
            LinuxFilesystem::Zfs,
        ] {
            let error = missing_attributes(filesystem, Errno::NOTTY).expect_err("refused");
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            assert_eq!(
                error.to_string(),
                "replacement cannot read inode attributes on this filesystem"
            );
        }
        assert_eq!(
            missing_attributes(LinuxFilesystem::Tmpfs, Errno::IO)
                .expect_err("a real failure is not absence")
                .raw_os_error(),
            Some(libc::EIO)
        );
    }
}
