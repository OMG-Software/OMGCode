//! C and C++ compilers.
//!
//! On Windows: the newest x64-hosted MSVC toolset with the newest Windows 10
//! SDK, laid out as the acceptance runs used them (`INCLUDE`, `LIB`, and
//! `VCINSTALLDIR`, which also lets rustc find the linker on `PATH`); LLVM
//! Clang with the same MSVC and SDK parts; and MinGW GCC. On Linux: GCC and
//! Clang. No `vswhere`, compiler, or other program is run: the toolsets are
//! found from their directory layout alone. ARM64-hosted MSVC is not
//! proposed.

use std::path::{Path, PathBuf};

use super::{
    Candidate, DiscoveryHost, ToolchainFamily, child_directories, file_name, install_of, is_file,
    newest, program_directories, real_directory,
};

/// The MSVC and Windows SDK parts a C, C++, or Rust MSVC-target profile
/// needs.
pub(super) struct MsvcParts {
    tools: PathBuf,
    kits: PathBuf,
    path_entries: Vec<PathBuf>,
    include: String,
    lib: String,
    vc_install_dir: String,
    tools_version: String,
    sdk_version: String,
}

impl MsvcParts {
    /// Add the roots, `PATH` entries, and variables to `candidate`.
    pub(super) fn merge_into(&self, candidate: &mut Candidate) {
        candidate.roots.push(self.tools.clone());
        candidate.roots.push(self.kits.clone());
        candidate
            .path_entries
            .extend(self.path_entries.iter().cloned());
        candidate.set_variable("INCLUDE", &self.include);
        candidate.set_variable("LIB", &self.lib);
        candidate.set_variable("VCINSTALLDIR", &self.vc_install_dir);
    }
}

/// The newest x64-hosted MSVC toolset, `<Program Files>/Microsoft Visual
/// Studio/<release>/<edition>/VC/Tools/MSVC/<version>`, and the newest
/// `Windows Kits/10` SDK with x64 `um` and `ucrt` libraries.
pub(super) fn msvc(host: &DiscoveryHost) -> Option<MsvcParts> {
    if !host.is_windows() {
        return None;
    }
    let tools = newest(
        host.program_files
            .iter()
            .flat_map(|program_files| {
                child_directories(&program_files.join("Microsoft Visual Studio"))
            })
            .flat_map(|release| child_directories(&release))
            .flat_map(|edition| child_directories(&edition.join("VC").join("Tools").join("MSVC")))
            .filter(|tools| is_file(&host_bin(tools).join("cl.exe"))),
    )?;
    let kits = host
        .program_files
        .iter()
        .map(|program_files| program_files.join("Windows Kits").join("10"))
        .find(|kits| real_directory(kits))?;
    let sdk = newest(
        child_directories(&kits.join("Lib"))
            .into_iter()
            .filter(|version| {
                real_directory(&version.join("um").join("x64"))
                    && real_directory(&version.join("ucrt").join("x64"))
            }),
    )?;
    let sdk_version = file_name(&sdk)?;
    let tools_version = file_name(&tools)?;
    // `tools` is `<VC>/Tools/MSVC/<version>`.
    let vc = tools.ancestors().nth(3)?;
    let include_root = kits.join("Include").join(&sdk_version);
    let include = host.list_value(&[
        tools.join("include"),
        include_root.join("ucrt"),
        include_root.join("um"),
        include_root.join("shared"),
    ])?;
    let lib = host.list_value(&[
        tools.join("lib").join("x64"),
        sdk.join("ucrt").join("x64"),
        sdk.join("um").join("x64"),
    ])?;
    let mut path_entries = vec![host_bin(&tools)];
    // The SDK's resource compiler and manifest tool, which build systems call.
    let sdk_bin = kits.join("bin").join(&sdk_version).join("x64");
    if real_directory(&sdk_bin) {
        path_entries.push(sdk_bin);
    }
    Some(MsvcParts {
        vc_install_dir: format!("{}\\", vc.to_str()?),
        tools,
        kits,
        path_entries,
        include,
        lib,
        tools_version,
        sdk_version,
    })
}

/// The x64-hosted, x64-targeting compiler directory of an MSVC toolset.
fn host_bin(tools: &Path) -> PathBuf {
    tools.join("bin").join("Hostx64").join("x64")
}

pub(super) fn detect(host: &DiscoveryHost) -> Vec<Candidate> {
    if host.is_windows() {
        windows(host)
    } else {
        linux(host)
    }
}

fn windows(host: &DiscoveryHost) -> Vec<Candidate> {
    let mut found = Vec::new();
    let msvc = msvc(host);
    if let Some(parts) = &msvc {
        let mut candidate =
            Candidate::new(ToolchainFamily::CCpp, "msvc", parts.tools_version.clone());
        candidate.version = Some(format!(
            "MSVC {}, Windows SDK {}",
            parts.tools_version, parts.sdk_version
        ));
        candidate.qualified = true;
        parts.merge_into(&mut candidate);
        found.push(candidate);
    }
    for program_files in &host.program_files {
        let llvm = program_files.join("LLVM");
        let bin = llvm.join("bin");
        if !is_file(&bin.join("clang.exe")) {
            continue;
        }
        let version = newest(child_directories(&llvm.join("lib").join("clang")))
            .as_deref()
            .and_then(file_name);
        let mut candidate = Candidate::new(
            ToolchainFamily::CCpp,
            "clang",
            version.clone().unwrap_or_default(),
        );
        candidate.version = version;
        candidate.roots.push(llvm);
        candidate.path_entries.push(bin);
        match &msvc {
            Some(parts) => parts.merge_into(&mut candidate),
            None => candidate.note(super::ProposalNote::NoMsvc),
        }
        found.push(candidate);
    }
    let standard: Vec<PathBuf> = [
        "msys64/ucrt64",
        "msys64/mingw64",
        "msys64/clang64",
        "mingw64",
    ]
    .iter()
    .map(|relative| host.under_root(relative).join("bin"))
    .collect();
    for bin in program_directories(host, standard, "gcc.exe") {
        if let Some(candidate) = gnu(host, &bin, "gcc", false) {
            found.push(candidate);
        }
    }
    found
}

fn linux(host: &DiscoveryHost) -> Vec<Candidate> {
    let standard = vec![host.under_root("usr/bin"), host.under_root("usr/local/bin")];
    let mut found = Vec::new();
    for bin in program_directories(host, standard.clone(), "gcc") {
        if let Some(candidate) = gnu(host, &bin, "gcc", true) {
            found.push(candidate);
        }
    }
    for bin in program_directories(host, standard, "clang") {
        if let Some(mut candidate) = gnu(host, &bin, "clang", false) {
            if candidate.version.is_none() {
                // Debian and Ubuntu install Clang under `/usr/lib/llvm-<N>`.
                let version = newest(super::child_directories_named(
                    &host.under_root("usr/lib"),
                    "llvm-",
                ))
                .as_deref()
                .and_then(file_name)
                .map(|name| name.trim_start_matches("llvm-").to_owned());
                candidate.variant = version.clone().unwrap_or_default();
                candidate.version = version;
            }
            found.push(candidate);
        }
    }
    found
}

/// A GCC or Clang installation whose compiler is in `bin`. The version is
/// the newest `lib/gcc/<target>/<version>` or `lib/clang/<version>` name.
fn gnu(
    host: &DiscoveryHost,
    bin: &Path,
    base_name: &'static str,
    qualified: bool,
) -> Option<Candidate> {
    let install = install_of(bin)?;
    let library = install.join("lib").join(base_name);
    let version_directories = if base_name == "gcc" {
        child_directories(&library)
            .into_iter()
            .flat_map(|target| child_directories(&target))
            .collect()
    } else {
        child_directories(&library)
    };
    let version = newest(version_directories).as_deref().and_then(file_name);
    let mut candidate = Candidate::new(
        ToolchainFamily::CCpp,
        base_name,
        version.clone().unwrap_or_default(),
    );
    candidate.version = version;
    // GCC 13.3 on Linux passed the acceptance runs; MinGW and Clang did not.
    candidate.qualified = qualified && !host.is_windows();
    candidate.roots.push(install);
    candidate.path_entries.push(bin.to_path_buf());
    Some(candidate)
}
