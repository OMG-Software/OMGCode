//! Reading an operator-selected skill folder into memory.
//!
//! The folder is opened once without following a final link and pinned by its
//! native identity. Every later read goes through the bounded native file
//! broker, which refuses links, reparse points, special files, and
//! multi-link files. Any refused or unreadable entry refuses the whole import;
//! nothing is skipped. Bytes are read once. Digests and the stored copy use
//! those same bytes, never a second read of the source.

use std::collections::HashSet;
use std::io;
use std::path::{Component, Path, PathBuf};

use omgcode_platform::broker::{self, EntryKind, RelativePath, Workspace};
use thiserror::Error;

use super::package::{SkillManifest, SkillPackageError, parse_skill_md};
use super::store::SkillStoreError;
use crate::session::sha256_hex;
use crate::workspace::{open_directory, validate_directory_metadata};

/// Maximum number of regular files in one package.
pub const MAX_PACKAGE_FILES: usize = 128;
/// Maximum number of directories below the package root.
pub const MAX_PACKAGE_DIRECTORIES: usize = 64;
/// Maximum size of one package file in bytes.
pub const MAX_PACKAGE_FILE_BYTES: usize = 1024 * 1024;
/// Maximum total size of all package files in bytes.
pub const MAX_PACKAGE_BYTES: u64 = 4 * 1024 * 1024;
/// Maximum directory depth below the package root.
pub const MAX_PACKAGE_DEPTH: usize = 4;
/// Maximum entries in one directory. This matches the private store's
/// inventory bound, so every imported directory can later be removed.
pub const MAX_DIRECTORY_ENTRIES: usize = 128;
/// Maximum length of an entry name in bytes.
const MAX_ENTRY_NAME_BYTES: usize = 64;
/// Maximum aggregate name units one listing may return.
const LISTING_NAME_UNITS: usize = 64 * 1024;
/// File name of the manifest, which must sit at the package root.
pub(crate) const SKILL_MD: &str = "SKILL.md";

/// Why an import was refused or failed. Each variant has a stable
/// [`code`](Self::code).
#[derive(Debug, Error)]
pub enum SkillImportError {
    #[error("skill source is not usable: {0}")]
    SourceInvalid(&'static str),
    #[error("skill source cannot be read at `{path}`: {source}")]
    SourceUnreadable {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("skill source entry `{0}` is a link, special file, or multi-link file")]
    EntryRefused(String),
    #[error("skill source entry name `{0}` is not allowed")]
    EntryNameInvalid(String),
    #[error("skill source entries `{0}` differ only in letter case")]
    EntryNameCollision(String),
    #[error("skill source has more than {MAX_PACKAGE_FILES} files")]
    TooManyFiles,
    #[error("skill source has more than {MAX_PACKAGE_DIRECTORIES} directories")]
    TooManyDirectories,
    #[error("skill source directory `{0}` has more than {MAX_DIRECTORY_ENTRIES} entries")]
    TooManyEntries(String),
    #[error("skill source is deeper than {MAX_PACKAGE_DEPTH} directories")]
    TooDeep,
    #[error("skill source file `{0}` is larger than {MAX_PACKAGE_FILE_BYTES} bytes")]
    FileTooLarge(String),
    #[error("skill source is larger than {MAX_PACKAGE_BYTES} bytes in total")]
    PackageTooLarge,
    #[error("skill source has no SKILL.md at its root")]
    MissingSkillMd,
    #[error("SKILL.md is invalid: {0}")]
    Package(#[from] SkillPackageError),
    #[error("skill name `{name}` does not match its directory name `{directory}`")]
    NameDirectoryMismatch { name: String, directory: String },
    #[error("skill store failed: {0}")]
    Store(#[from] SkillStoreError),
    #[error("import failed and left package `{skill_id}` behind: {cause}")]
    Orphaned {
        skill_id: uuid::Uuid,
        cause: Box<SkillImportError>,
    },
}

impl SkillImportError {
    /// Stable machine-readable code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::SourceInvalid(_) => "source_invalid",
            Self::SourceUnreadable { .. } => "source_unreadable",
            Self::EntryRefused(_) => "entry_refused",
            Self::EntryNameInvalid(_) => "entry_name_invalid",
            Self::EntryNameCollision(_) => "entry_name_collision",
            Self::TooManyFiles => "package_too_many_files",
            Self::TooManyDirectories => "package_too_many_directories",
            Self::TooManyEntries(_) => "package_too_many_entries",
            Self::TooDeep => "package_too_deep",
            Self::FileTooLarge(_) => "package_file_too_large",
            Self::PackageTooLarge => "package_too_large",
            Self::MissingSkillMd => "skill_md_missing",
            Self::Package(error) => error.code(),
            Self::NameDirectoryMismatch { .. } => "skill_name_directory_mismatch",
            Self::Store(error) => error.code(),
            Self::Orphaned { .. } => "package_orphaned",
        }
    }
}

/// One regular file read from the source, with its digest.
#[derive(Clone, Debug)]
pub(crate) struct SourceFile {
    /// `/`-joined path below the package root.
    pub(crate) path: String,
    pub(crate) bytes: Vec<u8>,
    pub(crate) sha256: String,
}

/// A fully read and validated source package.
#[derive(Debug)]
pub(crate) struct SourcePackage {
    pub(crate) manifest: SkillManifest,
    /// Sorted by path in byte order. Includes `SKILL.md`.
    pub(crate) files: Vec<SourceFile>,
    pub(crate) total_bytes: u64,
}

/// Read, bound, and validate the folder at `source`.
///
/// `source` must be an absolute path chosen by the trusted operator. It is
/// opened without following a final link; the broker then reads below that
/// pinned directory only.
pub(crate) fn read_source(source: &Path) -> Result<SourcePackage, SkillImportError> {
    let directory_name = directory_name(source)?;
    let handle = open_directory(source).map_err(|error| unreadable(source, error))?;
    let metadata = handle
        .metadata()
        .map_err(|error| unreadable(source, error))?;
    validate_directory_metadata(&metadata)
        .map_err(|_| SkillImportError::SourceInvalid("source is not an ordinary directory"))?;
    let identity =
        omgcode_platform::directory_identity(&handle).map_err(|error| unreadable(source, error))?;
    let root = RelativePath::new(Path::new("")).map_err(|error| unreadable(source, error))?;
    let workspace = Workspace::open(source, identity, &root, identity)
        .map_err(|error| unreadable(source, error))?;

    let mut files = Vec::new();
    let mut total_bytes: u64 = 0;
    let mut directories: usize = 0;
    // Each pending directory is (relative path, depth below the root).
    let mut pending: Vec<(String, usize)> = vec![(String::new(), 0)];
    while let Some((directory, depth)) = pending.pop() {
        let listing = workspace
            .list(
                &relative(&directory)?,
                MAX_DIRECTORY_ENTRIES.saturating_add(1),
                LISTING_NAME_UNITS,
            )
            .map_err(|error| unreadable_path(&directory, error))?;
        if listing.truncated || listing.entries.len() > MAX_DIRECTORY_ENTRIES {
            return Err(SkillImportError::TooManyEntries(display_path(&directory)));
        }
        let mut seen = HashSet::new();
        let mut entries = Vec::with_capacity(listing.entries.len());
        for entry in listing.entries {
            let Some(name) = entry.name.to_str().map(str::to_owned) else {
                return Err(SkillImportError::EntryNameInvalid(
                    entry.name.to_string_lossy().into_owned(),
                ));
            };
            let path = join(&directory, &name);
            if !valid_entry_name(&name) {
                return Err(SkillImportError::EntryNameInvalid(display_path(&path)));
            }
            if !seen.insert(name.to_ascii_lowercase()) {
                return Err(SkillImportError::EntryNameCollision(display_path(&path)));
            }
            entries.push((name, path, entry.kind));
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        for (_, path, kind) in entries {
            match kind {
                EntryKind::Blocked => return Err(SkillImportError::EntryRefused(path)),
                EntryKind::Directory => {
                    let child_depth = depth.saturating_add(1);
                    if child_depth > MAX_PACKAGE_DEPTH {
                        return Err(SkillImportError::TooDeep);
                    }
                    directories = directories.saturating_add(1);
                    if directories > MAX_PACKAGE_DIRECTORIES {
                        return Err(SkillImportError::TooManyDirectories);
                    }
                    pending.push((path, child_depth));
                }
                EntryKind::File => {
                    if files.len() >= MAX_PACKAGE_FILES {
                        return Err(SkillImportError::TooManyFiles);
                    }
                    let bytes = workspace
                        .read(&relative(&path)?, MAX_PACKAGE_FILE_BYTES)
                        .map_err(|error| read_error(&path, error))?;
                    total_bytes = total_bytes
                        .checked_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
                        .filter(|total| *total <= MAX_PACKAGE_BYTES)
                        .ok_or(SkillImportError::PackageTooLarge)?;
                    files.push(SourceFile {
                        sha256: sha256_hex(&bytes),
                        path,
                        bytes,
                    });
                }
            }
        }
    }
    files.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    let skill_md = files
        .iter()
        .find(|file| file.path == SKILL_MD)
        .ok_or(SkillImportError::MissingSkillMd)?;
    let parsed = parse_skill_md(&skill_md.bytes)?;
    if parsed.manifest.name.as_str() != directory_name {
        return Err(SkillImportError::NameDirectoryMismatch {
            name: parsed.manifest.name.as_str().to_owned(),
            directory: directory_name,
        });
    }
    Ok(SourcePackage {
        manifest: parsed.manifest,
        files,
        total_bytes,
    })
}

/// The final component of the source path, which must equal the skill name.
fn directory_name(source: &Path) -> Result<String, SkillImportError> {
    if !source.is_absolute() {
        return Err(SkillImportError::SourceInvalid(
            "source path is not absolute",
        ));
    }
    if source
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(SkillImportError::SourceInvalid(
            "source path contains a dot component",
        ));
    }
    source
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or(SkillImportError::SourceInvalid(
            "source path has no UTF-8 final component",
        ))
}

/// The canonical manifest and its digest: one line per file, sorted by path.
pub(crate) fn canonical_package_digest(files: &[SourceFile]) -> String {
    let mut manifest = String::new();
    for file in files {
        manifest.push_str(&format!(
            "{} {} {}\n",
            file.sha256,
            file.bytes.len(),
            file.path
        ));
    }
    sha256_hex(manifest.as_bytes())
}

/// Entry-name rule: `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`, no trailing dot, and no
/// Windows reserved device name. The trailing-dot rule is stricter than the
/// pattern because Windows would normalize the name.
fn valid_entry_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let Some((first, rest)) = bytes.split_first() else {
        return false;
    };
    if bytes.len() > MAX_ENTRY_NAME_BYTES
        || !first.is_ascii_alphanumeric()
        || !rest
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        || name.ends_with('.')
    {
        return false;
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix)
                .is_some_and(|digit| matches!(digit.as_bytes(), [b'1'..=b'9']))
        });
    !device
}

fn join(directory: &str, name: &str) -> String {
    if directory.is_empty() {
        name.to_owned()
    } else {
        format!("{directory}/{name}")
    }
}

fn relative(path: &str) -> Result<RelativePath, SkillImportError> {
    let native: PathBuf = path.split('/').filter(|part| !part.is_empty()).collect();
    RelativePath::new(&native).map_err(|error| unreadable_path(path, error))
}

fn display_path(path: &str) -> String {
    if path.is_empty() {
        ".".to_owned()
    } else {
        path.to_owned()
    }
}

fn unreadable(source: &Path, error: io::Error) -> SkillImportError {
    SkillImportError::SourceUnreadable {
        path: source.to_string_lossy().into_owned(),
        source: error,
    }
}

fn unreadable_path(path: &str, error: io::Error) -> SkillImportError {
    SkillImportError::SourceUnreadable {
        path: display_path(path),
        source: error,
    }
}

/// A refused read is a link, special file, or multi-link file unless the file
/// was only too large or unreadable.
fn read_error(path: &str, error: io::Error) -> SkillImportError {
    if broker::is_oversize_entry(&error) {
        SkillImportError::FileTooLarge(path.to_owned())
    } else if error.kind() == io::ErrorKind::InvalidInput
        || error.kind() == io::ErrorKind::InvalidData
    {
        SkillImportError::EntryRefused(path.to_owned())
    } else {
        unreadable_path(path, error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_entry_names_when_validated_then_the_rule_is_applied() {
        for good in [
            "a",
            "SKILL.md",
            "a-b_c.d",
            "A1",
            &"a".repeat(64),
            "COM10",
            "con-x",
        ] {
            assert!(valid_entry_name(good), "{good}");
        }
        for bad in [
            "",
            ".git",
            ".hidden",
            "-a",
            "_a",
            "a b",
            "caf\u{e9}",
            "a/b",
            "a\\b",
            "a:b",
            "a.",
            &"a".repeat(65),
            "CON",
            "con.txt",
            "Nul.md",
            "COM1",
            "lpt9.x",
            "aux",
            "PRN.tar.gz",
        ] {
            assert!(!valid_entry_name(bad), "{bad}");
        }
    }

    #[test]
    fn given_the_same_files_when_digested_then_the_canonical_manifest_is_sorted_text() {
        let file = |path: &str, bytes: &[u8]| SourceFile {
            path: path.to_owned(),
            bytes: bytes.to_vec(),
            sha256: sha256_hex(bytes),
        };
        let files = vec![file("SKILL.md", b"abc"), file("a/b.txt", b"")];
        let expected = format!(
            "{} 3 SKILL.md\n{} 0 a/b.txt\n",
            sha256_hex(b"abc"),
            sha256_hex(b"")
        );
        assert_eq!(
            canonical_package_digest(&files),
            sha256_hex(expected.as_bytes())
        );
    }
}
