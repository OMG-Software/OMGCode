//! CPython installations.
//!
//! On Windows an installation keeps `python.exe` beside its `Lib` folder; it
//! is found under `%LOCALAPPDATA%\Programs\Python`, the install manager's
//! `%LOCALAPPDATA%\Python\pythoncore-*`, `Program Files`, the drive root, and
//! the operator's `PATH`. On Linux it is the parent of a `bin` holding
//! `python3` and a `lib/python3.*` folder: `/usr`, `/usr/local`, pyenv, and
//! the operator's `PATH`. A virtual environment (`pyvenv.cfg`) or a launcher
//! shim with no standard library is skipped. The version comes from the
//! `python3NN.dll` or `lib/python3.N` names; nothing is run.

use std::path::{Path, PathBuf};

use super::{
    Candidate, DiscoveryHost, ProposalNote, ToolchainFamily, child_directories,
    child_directories_named, file_name, install_of, is_file, newest, push_unique, real_directory,
    version_key,
};

pub(super) fn detect(host: &DiscoveryHost) -> Vec<Candidate> {
    let mut homes: Vec<PathBuf> = Vec::new();
    if host.is_windows() {
        if let Some(local) = &host.local_app_data {
            for home in child_directories(&local.join("Programs").join("Python")) {
                push_unique(&mut homes, home);
            }
            for home in child_directories_named(&local.join("Python"), "pythoncore-") {
                push_unique(&mut homes, home);
            }
        }
        for program_files in &host.program_files {
            for home in child_directories_named(program_files, "Python") {
                push_unique(&mut homes, home);
            }
        }
        for home in child_directories_named(&host.root, "Python") {
            push_unique(&mut homes, home);
        }
        for home in host.search_directories("python.exe") {
            push_unique(&mut homes, home);
        }
    } else {
        let mut bins = vec![host.under_root("usr/bin"), host.under_root("usr/local/bin")];
        if let Some(pyenv) = host.under_home(".pyenv/versions") {
            bins.extend(child_directories(&pyenv).into_iter().map(|v| v.join("bin")));
        }
        bins.extend(host.search_directories("python3"));
        for bin in bins {
            if is_file(&bin.join("python3"))
                && let Some(home) = install_of(&bin)
            {
                push_unique(&mut homes, home);
            }
        }
    }
    homes
        .iter()
        .filter_map(|home| candidate(host, home))
        .collect()
}

fn candidate(host: &DiscoveryHost, home: &Path) -> Option<Candidate> {
    if is_file(&home.join("pyvenv.cfg")) {
        return None;
    }
    let (version, path_entries) = if host.is_windows() {
        if !is_file(&home.join("python.exe")) || !real_directory(&home.join("Lib")) {
            return None;
        }
        let mut entries = vec![home.to_path_buf()];
        let scripts = home.join("Scripts");
        if real_directory(&scripts) {
            entries.push(scripts);
        }
        (windows_version(home), entries)
    } else {
        let library = newest(child_directories_named(&home.join("lib"), "python3."))?;
        let version = file_name(&library).map(|name| name.trim_start_matches("python").to_owned());
        (version, vec![home.join("bin")])
    };
    let mut candidate = Candidate::new(
        ToolchainFamily::Python,
        "python",
        version.clone().unwrap_or_default(),
    );
    candidate.version = version;
    candidate.qualified = true;
    candidate.note(ProposalNote::OfflineDependencies);
    candidate.roots.push(home.to_path_buf());
    candidate.path_entries = path_entries;
    Some(candidate)
}

/// `3.13` from a `python313.dll` beside `python.exe`.
fn windows_version(home: &Path) -> Option<String> {
    let entries = std::fs::read_dir(home).ok()?;
    entries
        .take(super::MAX_LISTED_ENTRIES)
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| {
            let lower = name.to_ascii_lowercase();
            let digits = lower.strip_prefix("python3")?.strip_suffix(".dll")?;
            (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| format!("3.{digits}"))
        })
        .max_by_key(|version| version_key(version))
}
