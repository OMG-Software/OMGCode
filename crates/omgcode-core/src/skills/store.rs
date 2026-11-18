//! Private, exclusive-create storage for imported skill packages.
//!
//! Layout: `<data_root>/skills-v1/packages/<skill_id>/...`. Every directory and
//! file is created exclusively inside an owner-only private directory, written,
//! and synced. Nothing is replaced or renamed. A package directory has no
//! authority: only a control-journal registry record can cause a load, so a
//! directory left by an interrupted import is inert. See ADR-0057.

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::path::Path;

use omgcode_platform::private_store::PrivateDirectory;
use thiserror::Error;
use uuid::Uuid;

use super::import::{
    MAX_DIRECTORY_ENTRIES, MAX_PACKAGE_DEPTH, MAX_PACKAGE_DIRECTORIES, MAX_PACKAGE_FILES, SKILL_MD,
    SkillImportError, SourceFile, canonical_package_digest, read_source,
};
use super::package::{MAX_SKILL_MD_BYTES, SkillManifest, SkillPackageError, parse_skill_md};
use crate::session::sha256_hex;
use crate::workspace::DirectoryRecord;

const STORE_DIRECTORY: &str = "skills-v1";
const PACKAGES_DIRECTORY: &str = "packages";

/// Why a store operation failed. Each variant has a stable
/// [`code`](Self::code).
#[derive(Debug, Error)]
pub enum SkillStoreError {
    #[error("the data directory cannot be opened as private storage: {0}")]
    Root(#[source] io::Error),
    #[error("the opened data directory differs from its registration")]
    RootChanged,
    #[error("skill store {operation} failed: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("the skill package directory does not exist")]
    PackageMissing,
    #[error("the skill package directory is larger than the import bounds")]
    PackageTooLarge,
    #[error("the skill package could not be removed completely: {0}")]
    RemoveIncomplete(#[source] io::Error),
}

impl SkillStoreError {
    /// Stable machine-readable code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Root(_) => "store_root_unavailable",
            Self::RootChanged => "store_root_changed",
            Self::Io { .. } => "store_io",
            Self::PackageMissing => "package_missing",
            Self::PackageTooLarge => "package_store_too_large",
            Self::RemoveIncomplete(_) => "package_remove_incomplete",
        }
    }
}

/// Why loading a skill body failed. Each variant has a stable
/// [`code`](Self::code).
#[derive(Debug, Error)]
pub enum SkillLoadError {
    #[error("skill store failed: {0}")]
    Store(#[from] SkillStoreError),
    #[error("the stored SKILL.md differs from its registered size or digest")]
    Tampered,
    #[error("the stored SKILL.md is invalid: {0}")]
    Package(#[from] SkillPackageError),
}

impl SkillLoadError {
    /// Stable machine-readable code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Store(error) => error.code(),
            Self::Tampered => "package_tampered",
            Self::Package(error) => error.code(),
        }
    }
}

/// One file of an imported package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageFile {
    /// `/`-joined path below the package root.
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// The result of a successful import. It is not a registration: nothing can
/// load this package until the registry records it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedPackage {
    pub skill_id: Uuid,
    pub manifest: SkillManifest,
    /// SHA-256 of the canonical manifest of every file, as lowercase hex.
    pub package_sha256: String,
    pub skill_md_sha256: String,
    pub skill_md_bytes: u64,
    pub file_count: u32,
    pub total_bytes: u64,
    /// Every stored file, sorted by path in byte order.
    pub files: Vec<PackageFile>,
}

/// A verified skill body ready to label and send. It is untrusted text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedSkill {
    pub manifest: SkillManifest,
    pub body: String,
    pub skill_md_sha256: String,
    pub skill_md_bytes: u64,
}

/// Import the folder `source` into private storage under `data_root`.
///
/// `source` must be an absolute operator-selected path whose final component
/// equals the skill name. Every regular file is copied, and none is executed.
/// A failure removes what this import created. If it cannot, the error is
/// [`SkillImportError::Orphaned`]; the leftover directory is inert.
///
/// # Errors
///
/// Returns the first rule the source or the store breaks.
pub fn import_package(
    data_root: &Path,
    source: &Path,
) -> Result<ImportedPackage, SkillImportError> {
    let root = DirectoryRecord::capture(data_root).map_err(SkillStoreError::Root)?;
    import_package_in(&root, source)
}

/// Import against a registered data directory, as the control store records it.
pub(crate) fn import_package_in(
    data_root: &DirectoryRecord,
    source: &Path,
) -> Result<ImportedPackage, SkillImportError> {
    import_with(data_root, source, &mut Faults::default())
}

fn import_with(
    data_root: &DirectoryRecord,
    source: &Path,
    faults: &mut Faults,
) -> Result<ImportedPackage, SkillImportError> {
    let package = read_source(source)?;
    let packages = open_packages(data_root, true)?;
    let skill_id = Uuid::new_v4();
    let name = skill_id.hyphenated().to_string();
    if let Err(cause) = write_package(&packages, &name, &package.files, faults) {
        let cause = SkillImportError::Store(cause);
        if faults.skip_cleanup {
            return Err(SkillImportError::Orphaned {
                skill_id,
                cause: Box::new(cause),
            });
        }
        return Err(match remove_named(&packages, &name) {
            Ok(()) | Err(SkillStoreError::PackageMissing) => cause,
            Err(_) => SkillImportError::Orphaned {
                skill_id,
                cause: Box::new(cause),
            },
        });
    }
    let skill_md = package
        .files
        .iter()
        .find(|file| file.path == SKILL_MD)
        .ok_or(SkillImportError::MissingSkillMd)?;
    Ok(ImportedPackage {
        skill_id,
        package_sha256: canonical_package_digest(&package.files),
        skill_md_sha256: skill_md.sha256.clone(),
        skill_md_bytes: len_u64(&skill_md.bytes),
        file_count: u32::try_from(package.files.len()).unwrap_or(u32::MAX),
        total_bytes: package.total_bytes,
        files: package
            .files
            .iter()
            .map(|file| PackageFile {
                path: file.path.clone(),
                size: len_u64(&file.bytes),
                sha256: file.sha256.clone(),
            })
            .collect(),
        manifest: package.manifest,
    })
}

/// Load and verify the body of a registered skill.
///
/// Opens `skills-v1/packages/<skill_id>/SKILL.md` through retained directory
/// handles, refuses a size or digest that differs from the registry's, parses
/// it again, and returns the body.
///
/// # Errors
///
/// Returns [`SkillLoadError::Tampered`] on any mismatch.
pub fn load_skill_body(
    data_root: &Path,
    skill_id: Uuid,
    expected_skill_md_sha256: &str,
    expected_skill_md_bytes: u64,
) -> Result<LoadedSkill, SkillLoadError> {
    let root = DirectoryRecord::capture(data_root).map_err(SkillStoreError::Root)?;
    load_skill_body_in(
        &root,
        skill_id,
        expected_skill_md_sha256,
        expected_skill_md_bytes,
    )
}

/// Load against a registered data directory, as the control store records it.
pub(crate) fn load_skill_body_in(
    data_root: &DirectoryRecord,
    skill_id: Uuid,
    expected_skill_md_sha256: &str,
    expected_skill_md_bytes: u64,
) -> Result<LoadedSkill, SkillLoadError> {
    let packages = open_packages(data_root, false)?;
    let package = packages
        .open_dir(OsStr::new(&skill_id.hyphenated().to_string()))
        .map_err(missing_or_io("open package"))?;
    let mut file = package
        .open_file(OsStr::new(SKILL_MD), false)
        .map_err(missing_or_io("open SKILL.md"))?;
    let limit = u64::try_from(MAX_SKILL_MD_BYTES).unwrap_or(u64::MAX);
    let size = file.metadata().map_err(io_op("stat SKILL.md"))?.len();
    if size != expected_skill_md_bytes || size > limit {
        return Err(SkillLoadError::Tampered);
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(io_op("read SKILL.md"))?;
    if len_u64(&bytes) != expected_skill_md_bytes
        || !sha256_hex(&bytes).eq_ignore_ascii_case(expected_skill_md_sha256)
    {
        return Err(SkillLoadError::Tampered);
    }
    let parsed = parse_skill_md(&bytes)?;
    Ok(LoadedSkill {
        manifest: parsed.manifest,
        body: parsed.body,
        skill_md_sha256: sha256_hex(&bytes),
        skill_md_bytes: expected_skill_md_bytes,
    })
}

/// Remove the stored package, one entry at a time, without recursion through
/// links. Only that package directory is visited, within the import bounds.
/// The operator's original folder is never touched.
///
/// # Errors
///
/// Returns [`SkillStoreError::PackageMissing`] when the directory is absent and
/// [`SkillStoreError::RemoveIncomplete`] when an entry cannot be removed.
pub fn remove_package(data_root: &Path, skill_id: Uuid) -> Result<(), SkillStoreError> {
    let root = DirectoryRecord::capture(data_root).map_err(SkillStoreError::Root)?;
    remove_package_in(&root, skill_id)
}

/// Remove against a registered data directory, as the control store records it.
pub(crate) fn remove_package_in(
    data_root: &DirectoryRecord,
    skill_id: Uuid,
) -> Result<(), SkillStoreError> {
    let packages = open_packages(data_root, false)?;
    remove_named(&packages, &skill_id.hyphenated().to_string())
}

/// Bind the private root to the registered object, including creation time.
/// A matching path alone cannot substitute another application-data directory.
fn open_root(expected: &DirectoryRecord) -> Result<PrivateDirectory, SkillStoreError> {
    let path = expected.live_path().map_err(SkillStoreError::Root)?;
    let root = PrivateDirectory::open(&path).map_err(SkillStoreError::Root)?;
    let identity = root.identity().map_err(SkillStoreError::Root)?;
    let created = root.created().map_err(SkillStoreError::Root)?;
    if identity != expected.native_identity() || !expected.matches_created(created) {
        return Err(SkillStoreError::RootChanged);
    }
    Ok(root)
}

/// Open `skills-v1/packages`, creating the two directories when `create` is set.
fn open_packages(
    expected: &DirectoryRecord,
    create: bool,
) -> Result<PrivateDirectory, SkillStoreError> {
    let root = open_root(expected)?;
    let store = open_or_create(&root, STORE_DIRECTORY, create)?;
    open_or_create(&store, PACKAGES_DIRECTORY, create)
}

fn open_or_create(
    parent: &PrivateDirectory,
    name: &str,
    create: bool,
) -> Result<PrivateDirectory, SkillStoreError> {
    let name = OsStr::new(name);
    if parent.exists(name).map_err(io_op("inspect directory"))? {
        return parent.open_dir(name).map_err(io_op("open directory"));
    }
    if !create {
        return Err(SkillStoreError::PackageMissing);
    }
    let created = parent.create_dir(name).map_err(io_op("create directory"))?;
    parent.sync().map_err(io_op("sync directory"))?;
    Ok(created)
}

/// Create the package directory and every file exclusively, then sync.
fn write_package(
    packages: &PrivateDirectory,
    name: &str,
    files: &[SourceFile],
    faults: &mut Faults,
) -> Result<(), SkillStoreError> {
    faults.step("create package directory")?;
    let package = packages
        .create_dir(OsStr::new(name))
        .map_err(io_op("create package directory"))?;
    let mut directories: Vec<(String, PrivateDirectory)> = Vec::new();
    for file in files {
        let (parent, leaf) = match file.path.rsplit_once('/') {
            Some((parent, leaf)) => (parent, leaf),
            None => ("", file.path.as_str()),
        };
        let mut prefix = String::new();
        for part in parent.split('/').filter(|part| !part.is_empty()) {
            let next = if prefix.is_empty() {
                part.to_owned()
            } else {
                format!("{prefix}/{part}")
            };
            if !directories.iter().any(|(path, _)| *path == next) {
                faults.step("create directory")?;
                let created = {
                    let holder = lookup(&package, &directories, &prefix)?;
                    holder
                        .create_dir(OsStr::new(part))
                        .map_err(io_op("create directory"))?
                };
                directories.push((next.clone(), created));
            }
            prefix = next;
        }
        faults.step("create file")?;
        let holder = lookup(&package, &directories, parent)?;
        let mut handle = holder
            .create_file(OsStr::new(leaf))
            .map_err(io_op("create file"))?;
        handle.write_all(&file.bytes).map_err(io_op("write file"))?;
        handle.sync_all().map_err(io_op("sync file"))?;
    }
    // Children before parents: deepest directories first.
    directories.sort_by_key(|(path, _)| std::cmp::Reverse(path.matches('/').count()));
    for (_, directory) in &directories {
        directory.sync().map_err(io_op("sync directory"))?;
    }
    package.sync().map_err(io_op("sync package directory"))?;
    packages.sync().map_err(io_op("sync packages directory"))
}

/// The retained handle for `path`, where the empty path is the package root.
fn lookup<'a>(
    package: &'a PrivateDirectory,
    directories: &'a [(String, PrivateDirectory)],
    path: &str,
) -> Result<&'a PrivateDirectory, SkillStoreError> {
    if path.is_empty() {
        return Ok(package);
    }
    directories
        .iter()
        .find(|(candidate, _)| candidate == path)
        .map(|(_, directory)| directory)
        .ok_or(SkillStoreError::Io {
            operation: "find retained directory",
            source: io::Error::other("directory handle is not retained"),
        })
}

/// Remove `<name>` and everything below it, then its entry in `packages`.
fn remove_named(packages: &PrivateDirectory, name: &str) -> Result<(), SkillStoreError> {
    let entry = OsStr::new(name);
    if !packages.exists(entry).map_err(io_op("inspect package"))? {
        return Err(SkillStoreError::PackageMissing);
    }
    let package = packages.open_dir(entry).map_err(io_op("open package"))?;
    let mut budget = MAX_PACKAGE_FILES
        .saturating_add(MAX_PACKAGE_DIRECTORIES)
        .saturating_add(1);
    remove_tree(&package, 0, &mut budget)?;
    drop(package);
    packages
        .remove_dir(entry)
        .map_err(SkillStoreError::RemoveIncomplete)?;
    packages.sync().map_err(SkillStoreError::RemoveIncomplete)
}

/// Remove the contents of `directory` one native entry at a time. Recursion is
/// bounded by `MAX_PACKAGE_DEPTH`; the entry budget bounds total work.
fn remove_tree(
    directory: &PrivateDirectory,
    depth: usize,
    budget: &mut usize,
) -> Result<(), SkillStoreError> {
    let names = directory
        .names(MAX_DIRECTORY_ENTRIES)
        .map_err(|_| SkillStoreError::PackageTooLarge)?;
    for name in names {
        *budget = budget
            .checked_sub(1)
            .ok_or(SkillStoreError::PackageTooLarge)?;
        if let Ok(child) = directory.open_dir(&name) {
            if depth >= MAX_PACKAGE_DEPTH {
                return Err(SkillStoreError::PackageTooLarge);
            }
            remove_tree(&child, depth.saturating_add(1), budget)?;
            drop(child);
            directory
                .remove_dir(&name)
                .map_err(SkillStoreError::RemoveIncomplete)?;
        } else {
            directory
                .remove_file(&name)
                .map_err(SkillStoreError::RemoveIncomplete)?;
        }
    }
    directory.sync().map_err(SkillStoreError::RemoveIncomplete)
}

fn len_u64(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
}

fn io_op(operation: &'static str) -> impl Fn(io::Error) -> SkillStoreError {
    move |source| SkillStoreError::Io { operation, source }
}

/// A missing package or file is reported as such; everything else is I/O.
fn missing_or_io(operation: &'static str) -> impl Fn(io::Error) -> SkillStoreError {
    move |source| {
        if omgcode_platform::broker::is_missing_entry(&source) {
            SkillStoreError::PackageMissing
        } else {
            SkillStoreError::Io { operation, source }
        }
    }
}

/// Test-only failure injection. The default injects nothing.
#[derive(Default)]
struct Faults {
    /// Fail the step with this zero-based index.
    fail_at: Option<usize>,
    /// Leave the partial package in place instead of cleaning it up, as a
    /// process that stopped mid-import would.
    skip_cleanup: bool,
    steps: usize,
}

impl Faults {
    fn step(&mut self, operation: &'static str) -> Result<(), SkillStoreError> {
        let index = self.steps;
        self.steps = self.steps.saturating_add(1);
        if self.fail_at == Some(index) {
            return Err(SkillStoreError::Io {
                operation,
                source: io::Error::other("injected failure"),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::test_support::private_data_root;

    struct Fixture {
        data: std::path::PathBuf,
        record: DirectoryRecord,
        source: std::path::PathBuf,
        _temporary: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let data = temporary.path().join("data");
        private_data_root(&data);
        let record = DirectoryRecord::capture(&data).expect("capture data root");
        let source = temporary.path().join("demo");
        fs::create_dir_all(source.join("references")).expect("source directories");
        fs::write(
            source.join("SKILL.md"),
            "---\nname: demo\ndescription: A demo skill.\n---\nDo the demo.\n",
        )
        .expect("write SKILL.md");
        fs::write(source.join("references").join("guide.md"), "guide").expect("write guide");
        Fixture {
            data,
            record,
            source,
            _temporary: temporary,
        }
    }

    fn package_names(fixture: &Fixture) -> Vec<String> {
        let packages = fixture.data.join(STORE_DIRECTORY).join(PACKAGES_DIRECTORY);
        match fs::read_dir(packages) {
            Ok(entries) => entries
                .map(|entry| {
                    entry
                        .expect("a package directory entry can be read")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("package directory could not be inspected: {error}"),
        }
    }

    #[test]
    fn given_a_failure_after_the_first_file_when_importing_then_the_partial_package_is_removed() {
        let fixture = fixture();
        for fail_at in 0..4 {
            let mut faults = Faults {
                fail_at: Some(fail_at),
                ..Faults::default()
            };
            let error = import_with(&fixture.record, &fixture.source, &mut faults)
                .expect_err("injected failure");
            assert_eq!(error.code(), "store_io", "step {fail_at}: {error}");
            assert_eq!(
                package_names(&fixture),
                Vec::<String>::new(),
                "step {fail_at}"
            );
        }
        import_package_in(&fixture.record, &fixture.source).expect("clean import afterwards");
    }

    #[test]
    fn given_an_interrupted_import_when_cleanup_cannot_run_then_the_orphan_remains_and_a_wrong_size_is_refused()
     {
        let fixture = fixture();
        let mut faults = Faults {
            fail_at: Some(3),
            skip_cleanup: true,
            ..Faults::default()
        };
        let error = import_with(&fixture.record, &fixture.source, &mut faults)
            .expect_err("injected failure");
        assert_eq!(error.code(), "package_orphaned");
        let SkillImportError::Orphaned { skill_id, .. } = error else {
            panic!("expected an orphan report");
        };
        assert_eq!(
            package_names(&fixture),
            vec![skill_id.hyphenated().to_string()]
        );
        // A caller-supplied size that does not match the stored SKILL.md is
        // refused. The registry decision is tested through
        // `ControlStore::load_enabled_skill` in `control::skill_package`.
        let load = load_skill_body_in(&fixture.record, skill_id, &sha256_hex(b"x"), 1)
            .expect_err("a mismatched size does not load");
        assert_eq!(load.code(), "package_tampered");
        remove_package_in(&fixture.record, skill_id).expect("orphan removal");
        assert_eq!(package_names(&fixture), Vec::<String>::new());
    }
}
