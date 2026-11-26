//! Discovery of installed language toolchains (ADR-0103).
//!
//! A trusted, host-side service for the desktop `settings` window. It looks in
//! each family's standard install locations, in the operator's own `PATH`, and
//! in the operator's `RUSTUP_HOME`, `JAVA_HOME`, and `GOROOT`, and proposes
//! toolchain profiles for the operator to select and confirm.
//!
//! What it does and does not do:
//!
//! - It lists directories, checks that files exist, and reads a few small
//!   plain-text files that installers write, such as rustup's own
//!   `settings.toml`, a JDK's `release`, and Go's `VERSION`, each bounded. It
//!   starts no program, runs no version probe, and reads no registry key.
//! - It never reads repository configuration such as `rust-toolchain.toml`,
//!   `.nvmrc`, `go.mod`, or `.python-version`. A directory inside the
//!   development root can still appear through the operator's `PATH`; core
//!   refuses it as a toolchain root, and the window shows that refusal.
//! - A proposal grants nothing. Adding one goes through
//!   [`ControlStore::add_profile`](crate::control::ControlStore::add_profile)
//!   with the operator's native confirmation and expected revision, which
//!   captures and checks every directory again.
//!
//! Each family has one detector in a fixed table; adding a family adds a module
//! and one table entry. The shared flow adds the system part every confined
//! run needs, removes nested and duplicate directories, names the proposals,
//! and applies the bounds.

use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::control::{MAX_PROFILE_NAME_CHARS, ProfileDeclaration, ProfilePlatform};

mod c_cpp;
mod dotnet;
mod go;
mod java;
mod node;
mod perl;
mod python;
mod ruby;
mod rust;

#[cfg(test)]
mod tests;

/// Most proposals one discovery returns; no more profiles can be recorded.
pub const MAX_PROPOSALS: usize = 32;
/// Most installations proposed under one base name, such as `python`.
const MAX_PER_NAME: usize = 8;
/// Most entries read from one directory listing.
const MAX_LISTED_ENTRIES: usize = 512;
/// Most operator `PATH` entries considered.
const MAX_SEARCH_DIRECTORIES: usize = 256;
/// Most bytes read from one installer-written text file.
const MAX_TEXT_BYTES: u64 = 64 * 1024;

/// The detectors, one per family, in the order their proposals are listed.
/// Each looks only at the host it is given and returns its candidates; the
/// shared flow in [`discover`] does everything else.
const DETECTORS: [fn(&DiscoveryHost) -> Vec<Candidate>; 9] = [
    rust::detect,
    c_cpp::detect,
    java::detect,
    python::detect,
    perl::detect,
    go::detect,
    node::detect,
    dotnet::detect,
    ruby::detect,
];

/// A language family that discovery can propose a profile for.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolchainFamily {
    /// Rust toolchains installed by rustup.
    Rust,
    /// MSVC, Clang, and GCC.
    CCpp,
    /// JDKs and Java runtimes.
    Java,
    /// CPython.
    Python,
    /// Perl.
    Perl,
    /// Go.
    Go,
    /// Node.js.
    Node,
    /// .NET.
    DotNet,
    /// Ruby.
    Ruby,
}

impl ToolchainFamily {
    /// A fixed display label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::CCpp => "C and C++",
            Self::Java => "Java",
            Self::Python => "Python",
            Self::Perl => "Perl",
            Self::Go => "Go",
            Self::Node => "Node.js",
            Self::DotNet => ".NET",
            Self::Ruby => "Ruby",
        }
    }
}

/// A fixed remark about a proposal. None of them changes what is recorded.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalNote {
    /// This kind of toolchain has not passed the confined acceptance runs.
    NotQualified,
    /// A Windows MSVC-target toolchain, but no MSVC compiler and Windows SDK
    /// were found to link with.
    NoMsvc,
    /// Package downloads fail in an offline run.
    OfflineDependencies,
    /// Node on Windows: children with piped output never return.
    NodeWindowsChildren,
    /// A Java runtime without `javac`.
    JavaRuntimeOnly,
    /// A .NET runtime without an SDK.
    DotnetRuntimeOnly,
}

impl ProposalNote {
    /// The fixed text shown with the proposal.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::NotQualified => {
                "This toolchain has not passed OMGCode's confined acceptance runs, so it may need roots or variables that are not proposed here."
            }
            Self::NoMsvc => {
                "No MSVC compiler and Windows SDK were found, so linking fails. Install the Visual Studio C++ build tools and generate the list again."
            }
            Self::OfflineDependencies => {
                "Runs are offline unless the trusted CLI passes --network, and model runs never are. Dependencies that are not already in the workspace, such as crates from crates.io, fail to download."
            }
            Self::NodeWindowsChildren => {
                "On Windows, a child process with piped output never returns inside the container; run node --test with --test-isolation=none."
            }
            Self::JavaRuntimeOnly => "No javac was found: this is a runtime without the compiler.",
            Self::DotnetRuntimeOnly => {
                "No .NET SDK was found: this is a runtime, so dotnet build is unavailable."
            }
        }
    }
}

/// One proposed toolchain profile. It is advice for the operator: nothing is
/// recorded or granted until the operator selects and confirms it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolchainProposal {
    family: ToolchainFamily,
    name: String,
    version: Option<String>,
    read_only_roots: Vec<PathBuf>,
    path_entries: Vec<PathBuf>,
    environment: Vec<(String, String)>,
    notes: Vec<ProposalNote>,
}

impl ToolchainProposal {
    /// The family.
    #[must_use]
    pub const fn family(&self) -> ToolchainFamily {
        self.family
    }

    /// The proposed profile name. It satisfies core's name rule.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// A version taken from directory or file names, for display only. It is
    /// untrusted text: make it display-safe before showing it.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// The proposed read-only roots, in order.
    #[must_use]
    pub fn read_only_roots(&self) -> &[PathBuf] {
        &self.read_only_roots
    }

    /// The proposed `PATH` entries, in order. Each lies inside a root.
    #[must_use]
    pub fn path_entries(&self) -> &[PathBuf] {
        &self.path_entries
    }

    /// The proposed variables. Values are derived setup values such as an
    /// installation path; none is a secret.
    #[must_use]
    pub fn environment(&self) -> &[(String, String)] {
        &self.environment
    }

    /// Fixed remarks for the operator.
    #[must_use]
    pub fn notes(&self) -> &[ProposalNote] {
        &self.notes
    }

    /// The declaration that [`ControlStore::add_profile`] would record.
    ///
    /// [`ControlStore::add_profile`]: crate::control::ControlStore::add_profile
    #[must_use]
    pub fn declaration(&self) -> ProfileDeclaration {
        ProfileDeclaration {
            name: self.name.clone(),
            read_only_roots: self.read_only_roots.clone(),
            path_entries: self.path_entries.clone(),
            environment: self.environment.clone(),
        }
    }

    /// A SHA-256 digest, as 64 lowercase hex digits, of everything that would
    /// be recorded. The window selects proposals by digest, so a selection
    /// made from an earlier list fails to match when anything changed.
    #[must_use]
    pub fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        let mut field = |bytes: &[u8]| {
            hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
            hasher.update(bytes);
        };
        field(b"omgcode.toolchain-proposal.v1");
        field(self.family.label().as_bytes());
        field(self.name.as_bytes());
        field(self.version.as_deref().unwrap_or("").as_bytes());
        for list in [&self.read_only_roots, &self.path_entries] {
            field(&u64::try_from(list.len()).unwrap_or(u64::MAX).to_le_bytes());
            for path in list {
                field(path.as_os_str().as_encoded_bytes());
            }
        }
        field(
            &u64::try_from(self.environment.len())
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        for (name, value) in &self.environment {
            field(name.as_bytes());
            field(value.as_bytes());
        }
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

/// The host locations discovery reads. [`from_environment`] describes this
/// computer; [`rooted_at`] describes a fixture tree, so each platform's layout
/// can be tested on either host.
///
/// [`from_environment`]: Self::from_environment
/// [`rooted_at`]: Self::rooted_at
#[derive(Clone, Debug)]
pub struct DiscoveryHost {
    platform: ProfilePlatform,
    /// Linux: the filesystem root. Windows: the system drive's root.
    root: PathBuf,
    /// Windows only: the Windows directory.
    windows_dir: Option<PathBuf>,
    home: Option<PathBuf>,
    /// Windows only.
    local_app_data: Option<PathBuf>,
    /// Windows only.
    roaming_app_data: Option<PathBuf>,
    /// Windows only, without duplicates.
    program_files: Vec<PathBuf>,
    search_path: Vec<PathBuf>,
    rustup_home: Option<PathBuf>,
    java_home: Option<PathBuf>,
    goroot: Option<PathBuf>,
}

impl DiscoveryHost {
    /// This computer, read from the trusted process's own environment.
    /// Relative values are ignored. `None` where profiles are unsupported.
    #[must_use]
    pub fn from_environment() -> Option<Self> {
        let platform = ProfilePlatform::current()?;
        let absolute = |name: &str| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        };
        let search_path = std::env::var_os("PATH")
            .map(|value| {
                std::env::split_paths(&value)
                    .filter(|path| path.is_absolute())
                    .take(MAX_SEARCH_DIRECTORIES)
                    .collect()
            })
            .unwrap_or_default();
        let mut host = match platform {
            ProfilePlatform::Windows => {
                let home = absolute("USERPROFILE");
                let root = std::env::var_os("SystemDrive")
                    .map(|drive| {
                        let mut root = drive;
                        root.push("\\");
                        PathBuf::from(root)
                    })
                    .filter(|path| path.is_absolute())
                    .unwrap_or_else(|| PathBuf::from("C:\\"));
                let mut program_files: Vec<PathBuf> = Vec::new();
                for name in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
                    if let Some(path) = absolute(name)
                        && !program_files.iter().any(|known| same_text(known, &path))
                    {
                        program_files.push(path);
                    }
                }
                Self {
                    platform,
                    root,
                    windows_dir: absolute("SystemRoot"),
                    rustup_home: absolute("RUSTUP_HOME")
                        .or_else(|| home.as_ref().map(|home| home.join(".rustup"))),
                    home,
                    local_app_data: absolute("LOCALAPPDATA"),
                    roaming_app_data: absolute("APPDATA"),
                    program_files,
                    search_path: Vec::new(),
                    java_home: None,
                    goroot: None,
                }
            }
            ProfilePlatform::Linux => {
                let home = absolute("HOME");
                Self {
                    platform,
                    root: PathBuf::from("/"),
                    windows_dir: None,
                    rustup_home: absolute("RUSTUP_HOME")
                        .or_else(|| home.as_ref().map(|home| home.join(".rustup"))),
                    home,
                    local_app_data: None,
                    roaming_app_data: None,
                    program_files: Vec::new(),
                    search_path: Vec::new(),
                    java_home: None,
                    goroot: None,
                }
            }
        };
        host.search_path = search_path;
        host.java_home = absolute("JAVA_HOME");
        host.goroot = absolute("GOROOT");
        Some(host)
    }

    /// A host whose standard locations all lie under `base`, laid out as on
    /// `platform`: on Windows `Program Files`, `Program Files (x86)`,
    /// `Windows`, and `Users/operator`; on Linux `usr`, `opt`, `etc`, and
    /// `home/operator`. The search path is empty. For tests and fixtures.
    #[must_use]
    pub fn rooted_at(platform: ProfilePlatform, base: &Path) -> Self {
        match platform {
            ProfilePlatform::Windows => {
                let home = base.join("Users").join("operator");
                Self {
                    platform,
                    root: base.to_path_buf(),
                    windows_dir: Some(base.join("Windows")),
                    local_app_data: Some(home.join("AppData").join("Local")),
                    roaming_app_data: Some(home.join("AppData").join("Roaming")),
                    program_files: vec![
                        base.join("Program Files"),
                        base.join("Program Files (x86)"),
                    ],
                    search_path: Vec::new(),
                    rustup_home: Some(home.join(".rustup")),
                    java_home: None,
                    goroot: None,
                    home: Some(home),
                }
            }
            ProfilePlatform::Linux => {
                let home = base.join("home").join("operator");
                Self {
                    platform,
                    root: base.to_path_buf(),
                    windows_dir: None,
                    local_app_data: None,
                    roaming_app_data: None,
                    program_files: Vec::new(),
                    search_path: Vec::new(),
                    rustup_home: Some(home.join(".rustup")),
                    java_home: None,
                    goroot: None,
                    home: Some(home),
                }
            }
        }
    }

    /// Replace the operator `PATH` entries, keeping at most 256 absolute ones.
    #[must_use]
    pub fn with_search_path(mut self, directories: Vec<PathBuf>) -> Self {
        self.search_path = directories
            .into_iter()
            .filter(|path| path.is_absolute())
            .take(MAX_SEARCH_DIRECTORIES)
            .collect();
        self
    }

    /// Set the operator's `JAVA_HOME`.
    #[must_use]
    pub fn with_java_home(mut self, java_home: PathBuf) -> Self {
        self.java_home = Some(java_home).filter(|path| path.is_absolute());
        self
    }

    /// Set the operator's `GOROOT`.
    #[must_use]
    pub fn with_goroot(mut self, goroot: PathBuf) -> Self {
        self.goroot = Some(goroot).filter(|path| path.is_absolute());
        self
    }

    /// The platform whose layout this host has.
    #[must_use]
    pub const fn platform(&self) -> ProfilePlatform {
        self.platform
    }

    const fn is_windows(&self) -> bool {
        matches!(self.platform, ProfilePlatform::Windows)
    }

    /// A program's file name on this host.
    fn exe(&self, stem: &str) -> String {
        if self.is_windows() {
            format!("{stem}.exe")
        } else {
            stem.to_owned()
        }
    }

    /// `relative`, written with `/`, under the filesystem or drive root.
    fn under_root(&self, relative: &str) -> PathBuf {
        joined_path(&self.root, relative)
    }

    /// `relative`, written with `/`, under the home directory.
    fn under_home(&self, relative: &str) -> Option<PathBuf> {
        self.home.as_deref().map(|home| joined_path(home, relative))
    }

    /// The directories holding `program` among the operator's `PATH`
    /// entries, in `PATH` order without duplicates. On Linux each is the
    /// directory of the program's resolved file, so `/usr/bin/java` leads to
    /// the JDK it links to. Windows app execution aliases are skipped.
    fn search_directories(&self, program: &str) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = Vec::new();
        for directory in &self.search_path {
            if directory.components().any(|component| {
                component
                    .as_os_str()
                    .to_str()
                    .is_some_and(|text| text.eq_ignore_ascii_case("WindowsApps"))
            }) {
                continue;
            }
            let file = directory.join(program);
            if !is_file(&file) {
                continue;
            }
            let resolved = if self.is_windows() {
                Some(directory.clone())
            } else {
                fs::canonicalize(&file)
                    .ok()
                    .and_then(|real| real.parent().map(Path::to_path_buf))
            };
            if let Some(resolved) = resolved
                && !found.contains(&resolved)
            {
                found.push(resolved);
            }
        }
        found
    }

    /// The roots and `PATH` entries every profile on this host gets: the
    /// Windows `System32` directory, or Linux `/usr` and any top-level `bin`,
    /// `lib`, `lib64`, or `lib32` that is a real directory rather than a link
    /// into `/usr`. These hold the system loader, libraries, and shell that
    /// the acceptance-tested profiles all listed.
    fn system_part(&self) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let mut roots = Vec::new();
        let mut path_entries = Vec::new();
        match self.platform {
            ProfilePlatform::Windows => {
                if let Some(system) = self
                    .windows_dir
                    .as_ref()
                    .map(|windows| windows.join("System32"))
                    .filter(|system| real_directory(system))
                {
                    roots.push(system.clone());
                    path_entries.push(system);
                }
            }
            ProfilePlatform::Linux => {
                let usr = self.under_root("usr");
                if real_directory(&usr) {
                    let bin = usr.join("bin");
                    roots.push(usr);
                    if real_directory(&bin) {
                        path_entries.push(bin);
                    }
                }
                for name in ["bin", "lib", "lib64", "lib32"] {
                    let directory = self.under_root(name);
                    if real_directory(&directory) {
                        if name == "bin" {
                            path_entries.push(directory.clone());
                        }
                        roots.push(directory);
                    }
                }
            }
        }
        (roots, path_entries)
    }

    /// Whether `root` is too broad to propose: the filesystem or drive root,
    /// or the home directory or one of its ancestors.
    fn too_broad(&self, root: &Path) -> bool {
        root == self.root
            || root.parent().is_none()
            || self
                .home
                .as_deref()
                .is_some_and(|home| home.starts_with(root))
    }

    /// Join `paths` into one list variable with this host's separator.
    /// `None` when a path is not UTF-8 or contains the separator.
    fn list_value(&self, paths: &[PathBuf]) -> Option<String> {
        let separator = if self.is_windows() { ';' } else { ':' };
        let mut parts = Vec::with_capacity(paths.len());
        for path in paths {
            let text = path.to_str()?;
            if text.contains(separator) {
                return None;
            }
            parts.push(text);
        }
        Some(parts.join(&separator.to_string()))
    }
}

/// One installation a detector found, before the shared flow completes it.
#[derive(Clone, Debug)]
struct Candidate {
    family: ToolchainFamily,
    /// The bare name for the preferred installation, such as `rust`.
    base_name: &'static str,
    /// What tells this installation apart in a longer name, such as a version.
    variant: String,
    version: Option<String>,
    /// Listed and named first under its base name, such as rustup's default.
    preferred: bool,
    /// Whether this kind of toolchain passed the confined acceptance runs.
    qualified: bool,
    roots: Vec<PathBuf>,
    path_entries: Vec<PathBuf>,
    environment: Vec<(String, String)>,
    notes: Vec<ProposalNote>,
}

impl Candidate {
    fn new(family: ToolchainFamily, base_name: &'static str, variant: impl Into<String>) -> Self {
        Self {
            family,
            base_name,
            variant: variant.into(),
            version: None,
            preferred: false,
            qualified: false,
            roots: Vec::new(),
            path_entries: Vec::new(),
            environment: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// Add a variable whose value is a path; `false` when it is not UTF-8.
    fn set_path_variable(&mut self, name: &str, path: &Path) -> bool {
        match path.to_str() {
            Some(text) => {
                self.environment.push((name.to_owned(), text.to_owned()));
                true
            }
            None => false,
        }
    }

    fn set_variable(&mut self, name: &str, value: &str) {
        self.environment.push((name.to_owned(), value.to_owned()));
    }

    fn note(&mut self, note: ProposalNote) {
        if !self.notes.contains(&note) {
            self.notes.push(note);
        }
    }

    /// Add the system part, drop duplicate and nested roots and duplicate
    /// `PATH` entries, and refuse a candidate with a root that is too broad
    /// or a `PATH` entry outside every root.
    fn complete(
        mut self,
        host: &DiscoveryHost,
        system: &(Vec<PathBuf>, Vec<PathBuf>),
    ) -> Option<Self> {
        self.roots.extend(system.0.iter().cloned());
        self.path_entries.extend(system.1.iter().cloned());
        if self.roots.iter().any(|root| host.too_broad(root)) {
            return None;
        }
        let mut roots: Vec<PathBuf> = Vec::new();
        for (index, root) in self.roots.iter().enumerate() {
            let covered = self.roots.iter().enumerate().any(|(other, outer)| {
                other != index && root.starts_with(outer) && (root != outer || other < index)
            });
            if !covered {
                roots.push(root.clone());
            }
        }
        self.roots = roots;
        let mut path_entries: Vec<PathBuf> = Vec::new();
        for entry in &self.path_entries {
            if !path_entries.contains(entry) {
                path_entries.push(entry.clone());
            }
        }
        self.path_entries = path_entries;
        if self.roots.is_empty()
            || !self
                .path_entries
                .iter()
                .all(|entry| self.roots.iter().any(|root| entry.starts_with(root)))
        {
            return None;
        }
        if !self.qualified {
            self.note(ProposalNote::NotQualified);
        }
        Some(self)
    }

    /// Whether two candidates would record the same profile apart from the
    /// name.
    fn same_profile(&self, other: &Self) -> bool {
        self.roots == other.roots
            && self.path_entries == other.path_entries
            && self.environment == other.environment
    }
}

/// Find the toolchains installed on `host` and propose a profile for each.
///
/// Proposals are listed by family, the preferred installation of each name
/// first and then by descending version. At most [`MAX_PROPOSALS`] are
/// returned. A proposal can still be refused by core, for example because a
/// root overlaps the development root or its name is already recorded; check
/// it with [`ControlStore::check_profile`] before offering it.
///
/// [`ControlStore::check_profile`]: crate::control::ControlStore::check_profile
#[must_use]
pub fn discover(host: &DiscoveryHost) -> Vec<ToolchainProposal> {
    let system = host.system_part();
    let mut ordered: Vec<Candidate> = Vec::new();
    for detect in DETECTORS {
        let mut found: Vec<Candidate> = detect(host)
            .into_iter()
            .filter_map(|candidate| candidate.complete(host, &system))
            .collect();
        // Each detector's base names keep the order the detector found them.
        let mut names: Vec<&'static str> = Vec::new();
        for candidate in &found {
            if !names.contains(&candidate.base_name) {
                names.push(candidate.base_name);
            }
        }
        // Stable sorts: preferred first, then by descending version.
        found.sort_by_key(|candidate| std::cmp::Reverse(version_key(&candidate.variant)));
        found.sort_by_key(|candidate| !candidate.preferred);
        let mut kept: Vec<Candidate> = Vec::new();
        for candidate in found {
            let under_name = kept
                .iter()
                .filter(|other| other.base_name == candidate.base_name)
                .count();
            // The same installation found twice, such as through `PATH` and
            // a standard location. GCC and Clang in `/usr` share every root
            // but are different tools, so only one base name is compared.
            let duplicate = kept.iter().any(|other| {
                other.base_name == candidate.base_name && other.same_profile(&candidate)
            });
            if under_name < MAX_PER_NAME && !duplicate {
                kept.push(candidate);
            }
        }
        for name in names {
            ordered.extend(kept.iter().filter(|c| c.base_name == name).cloned());
        }
    }
    let mut taken: Vec<String> = Vec::new();
    let mut proposals = Vec::new();
    for candidate in ordered.into_iter().take(MAX_PROPOSALS) {
        let first_of_name = !taken
            .iter()
            .any(|name| name.eq_ignore_ascii_case(candidate.base_name));
        let name = unique_name(
            &if first_of_name {
                candidate.base_name.to_owned()
            } else {
                profile_name(candidate.base_name, &candidate.variant)
            },
            &taken,
        );
        taken.push(name.clone());
        proposals.push(ToolchainProposal {
            family: candidate.family,
            name,
            version: candidate.version,
            read_only_roots: candidate.roots,
            path_entries: candidate.path_entries,
            environment: candidate.environment,
            notes: candidate.notes,
        });
    }
    proposals
}

/// `base-variant` with every character outside core's name rule replaced by
/// `-`, cut to the name limit.
fn profile_name(base: &str, variant: &str) -> String {
    let cleaned: String = variant
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || "._-".contains(character) {
                character
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(|character| "._-".contains(character));
    let mut name = if cleaned.is_empty() {
        base.to_owned()
    } else {
        format!("{base}-{cleaned}")
    };
    // Every character is ASCII, so a byte cut is a character cut.
    name.truncate(MAX_PROFILE_NAME_CHARS);
    name
}

/// `name`, or `name-2`, `name-3`, and so on, whichever no earlier proposal
/// took, compared case-insensitively.
fn unique_name(name: &str, taken: &[String]) -> String {
    let free = |candidate: &str| !taken.iter().any(|t| t.eq_ignore_ascii_case(candidate));
    if free(name) {
        return name.to_owned();
    }
    for suffix in 2..=MAX_PROPOSALS + 1 {
        let tail = format!("-{suffix}");
        let mut stem = name.to_owned();
        stem.truncate(MAX_PROFILE_NAME_CHARS.saturating_sub(tail.len()));
        let candidate = format!("{stem}{tail}");
        if free(&candidate) {
            return candidate;
        }
    }
    // Unreachable in practice: at most `MAX_PROPOSALS` names are taken, so
    // one of the suffixes above is free. Returning the name lets core refuse
    // the duplicate instead of panicking.
    name.to_owned()
}

/// The numbers in `text`, in order, for comparing versions: `1.99.1` sorts
/// after `1.9.2`. Text without digits sorts first.
fn version_key(text: &str) -> Vec<u64> {
    text.split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<u64>().unwrap_or(u64::MAX))
        .collect()
}

/// The entry of `entries` whose file name has the highest [`version_key`].
fn newest(entries: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    entries
        .into_iter()
        .max_by_key(|path| version_key(&file_name(path).unwrap_or_default()))
}

/// The final component as text, when it is UTF-8.
fn file_name(path: &Path) -> Option<String> {
    path.file_name()?.to_str().map(str::to_owned)
}

/// `base` joined with `relative`, whose components are written with `/`.
fn joined_path(base: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .filter(|part| !part.is_empty())
        .fold(base.to_path_buf(), |path, part| path.join(part))
}

/// Whether `path` is a directory itself, not a symbolic link, junction, or
/// other name surrogate for one.
fn real_directory(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
}

/// Whether `path` names a file, following links.
fn is_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// The real directories directly inside `path`, sorted by name. At most 512
/// entries are read.
fn child_directories(path: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .take(MAX_LISTED_ENTRIES)
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|child| real_directory(child))
        .collect();
    found.sort();
    found
}

/// The real directories inside `path` whose names start with `prefix`,
/// compared case-insensitively.
fn child_directories_named(path: &Path, prefix: &str) -> Vec<PathBuf> {
    child_directories(path)
        .into_iter()
        .filter(|child| {
            file_name(child).is_some_and(|name| {
                name.len() >= prefix.len()
                    && name
                        .get(..prefix.len())
                        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
            })
        })
        .collect()
}

/// The text of a small installer-written file, at most 64 KiB of it, when it
/// is UTF-8.
fn read_text(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_TEXT_BYTES).read_to_string(&mut text).ok()?;
    Some(text)
}

/// Whether two paths have the same text, ignoring ASCII case.
fn same_text(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

/// The parent of a `bin` directory, which is the installation it belongs
/// to. `None` for a directory with no parent or a relative one.
fn install_of(bin: &Path) -> Option<PathBuf> {
    let parent = bin.parent()?;
    if parent
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }
    Some(parent.to_path_buf())
}

/// The directories that hold `program`: each of `standard` that holds it, then
/// the operator's `PATH` entries that do, without duplicates.
fn program_directories(
    host: &DiscoveryHost,
    standard: Vec<PathBuf>,
    program: &str,
) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for directory in standard {
        if real_directory(&directory) && is_file(&directory.join(program)) {
            push_unique(&mut found, directory);
        }
    }
    for directory in host.search_directories(program) {
        push_unique(&mut found, directory);
    }
    found
}

/// Add `path` to `list` unless it is already there.
fn push_unique(list: &mut Vec<PathBuf>, path: PathBuf) {
    if !list.contains(&path) {
        list.push(path);
    }
}
