//! Go installations.
//!
//! Looks in the operator's `GOROOT`, `Program Files\Go`, `/usr/local/go`,
//! `/usr/lib/go-*`, `~/sdk/go*`, and the operator's `PATH`. The version is
//! the first line of the installation's `VERSION` file; nothing is run.
//!
//! Besides `GOROOT`, each proposal sets the values the Go acceptance run used
//! to keep a run inside its profile: `GOTOOLCHAIN=local`, so a module's
//! `toolchain` line can never select or download another Go; `GOENV=off`, so
//! no user configuration file is read; and `GOFLAGS=-buildvcs=false`, so the
//! build does not try to run Git. The build and module caches default to the
//! run's private home.

use std::path::{Path, PathBuf};

use super::{
    Candidate, DiscoveryHost, ProposalNote, ToolchainFamily, child_directories_named, file_name,
    install_of, is_file, push_unique, read_text, real_directory,
};

pub(super) fn detect(host: &DiscoveryHost) -> Vec<Candidate> {
    let mut homes: Vec<PathBuf> = Vec::new();
    if let Some(goroot) = &host.goroot {
        push_unique(&mut homes, goroot.clone());
    }
    if host.is_windows() {
        for program_files in &host.program_files {
            push_unique(&mut homes, program_files.join("Go"));
        }
    } else {
        push_unique(&mut homes, host.under_root("usr/local/go"));
        for home in child_directories_named(&host.under_root("usr/lib"), "go") {
            push_unique(&mut homes, home);
        }
    }
    if let Some(sdk) = host.under_home("sdk") {
        for home in child_directories_named(&sdk, "go") {
            push_unique(&mut homes, home);
        }
    }
    for bin in host.search_directories(&host.exe("go")) {
        if let Some(home) = install_of(&bin) {
            push_unique(&mut homes, home);
        }
    }
    homes
        .iter()
        .filter_map(|home| candidate(host, home))
        .collect()
}

fn candidate(host: &DiscoveryHost, home: &Path) -> Option<Candidate> {
    let bin = home.join("bin");
    if !real_directory(home)
        || !is_file(&bin.join(host.exe("go")))
        || !(is_file(&home.join("VERSION")) || real_directory(&home.join("src").join("runtime")))
    {
        return None;
    }
    let version = read_text(&home.join("VERSION"))
        .and_then(|text| text.lines().next().map(str::trim).map(str::to_owned))
        .map(|line| line.trim_start_matches("go").to_owned())
        .filter(|line| !line.is_empty())
        .or_else(|| file_name(home));
    let mut candidate = Candidate::new(
        ToolchainFamily::Go,
        "go",
        version.clone().unwrap_or_default(),
    );
    candidate.version = version;
    candidate.qualified = true;
    if !candidate.set_path_variable("GOROOT", home) {
        return None;
    }
    candidate.set_variable("GOTOOLCHAIN", "local");
    candidate.set_variable("GOENV", "off");
    candidate.set_variable("GOFLAGS", "-buildvcs=false");
    candidate.note(ProposalNote::OfflineDependencies);
    candidate.roots.push(home.to_path_buf());
    candidate.path_entries.push(bin);
    Some(candidate)
}
