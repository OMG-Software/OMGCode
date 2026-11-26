//! Ruby installations.
//!
//! An installation is the parent of a `bin` holding `ruby` beside a
//! `lib/ruby` folder: RubyInstaller's `C:\Ruby*` folders, `/usr`,
//! `/usr/local`, rbenv, RVM, and the operator's `PATH`. The version is the
//! newest `lib/ruby/<version>` name; nothing is run.

use std::path::{Path, PathBuf};

use super::{
    Candidate, DiscoveryHost, ProposalNote, ToolchainFamily, child_directories,
    child_directories_named, file_name, install_of, is_file, newest, push_unique, real_directory,
};

pub(super) fn detect(host: &DiscoveryHost) -> Vec<Candidate> {
    let mut bins: Vec<PathBuf> = Vec::new();
    if host.is_windows() {
        bins.extend(
            child_directories_named(&host.root, "Ruby")
                .into_iter()
                .map(|home| home.join("bin")),
        );
    } else {
        bins.push(host.under_root("usr/bin"));
        bins.push(host.under_root("usr/local/bin"));
        for manager in [".rbenv/versions", ".rvm/rubies"] {
            if let Some(versions) = host.under_home(manager) {
                bins.extend(
                    child_directories(&versions)
                        .into_iter()
                        .map(|v| v.join("bin")),
                );
            }
        }
    }
    bins.extend(host.search_directories(&host.exe("ruby")));
    let mut homes: Vec<PathBuf> = Vec::new();
    for bin in bins {
        if is_file(&bin.join(host.exe("ruby")))
            && let Some(home) = install_of(&bin)
        {
            push_unique(&mut homes, home);
        }
    }
    homes.iter().filter_map(|home| candidate(home)).collect()
}

fn candidate(home: &Path) -> Option<Candidate> {
    let library = home.join("lib").join("ruby");
    if !real_directory(&library) {
        return None;
    }
    let version = newest(child_directories(&library).into_iter().filter(|child| {
        file_name(child).is_some_and(|name| name.starts_with(|c: char| c.is_ascii_digit()))
    }))
    .as_deref()
    .and_then(file_name);
    let mut candidate = Candidate::new(
        ToolchainFamily::Ruby,
        "ruby",
        version.clone().unwrap_or_default(),
    );
    candidate.version = version;
    candidate.note(ProposalNote::OfflineDependencies);
    candidate.roots.push(home.to_path_buf());
    candidate.path_entries.push(home.join("bin"));
    Some(candidate)
}
