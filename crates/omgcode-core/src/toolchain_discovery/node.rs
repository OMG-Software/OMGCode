//! Node.js installations.
//!
//! On Windows an installation keeps `node.exe` beside its `node_modules`
//! folder: `Program Files\nodejs`, nvm-windows versions under
//! `%APPDATA%\nvm`, and the operator's `PATH`. A shim folder with no
//! `node_modules`, such as Volta's, is skipped. On Linux it is the parent of
//! a `bin` holding `node`: `/usr`, `/usr/local`, nvm, and the operator's
//! `PATH`. The version comes from an nvm folder name; nothing is run.
//!
//! On Windows each proposal sets `NODE_OPTIONS=--preserve-symlinks
//! --preserve-symlinks-main`, because Node's module loader otherwise reads
//! every ancestor directory and fails inside the container. A distribution
//! Node in `/usr` also gets `/etc/ssl`, where its OpenSSL configuration is.

use std::path::{Path, PathBuf};

use super::{
    Candidate, DiscoveryHost, ProposalNote, ToolchainFamily, child_directories,
    child_directories_named, file_name, install_of, is_file, push_unique, real_directory,
};

pub(super) fn detect(host: &DiscoveryHost) -> Vec<Candidate> {
    let mut homes: Vec<PathBuf> = Vec::new();
    if host.is_windows() {
        for program_files in &host.program_files {
            push_unique(&mut homes, program_files.join("nodejs"));
        }
        if let Some(roaming) = &host.roaming_app_data {
            for home in child_directories_named(&roaming.join("nvm"), "v") {
                push_unique(&mut homes, home);
            }
        }
        for home in host.search_directories("node.exe") {
            push_unique(&mut homes, home);
        }
    } else {
        let mut bins = vec![host.under_root("usr/bin"), host.under_root("usr/local/bin")];
        if let Some(nvm) = host.under_home(".nvm/versions/node") {
            bins.extend(child_directories(&nvm).into_iter().map(|v| v.join("bin")));
        }
        bins.extend(host.search_directories("node"));
        for bin in bins {
            if is_file(&bin.join("node"))
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
    let version = file_name(home)
        .filter(|name| {
            name.strip_prefix('v')
                .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        })
        .map(|name| name.trim_start_matches('v').to_owned());
    let mut candidate = Candidate::new(
        ToolchainFamily::Node,
        "node",
        version.clone().unwrap_or_default(),
    );
    candidate.version = version;
    candidate.qualified = true;
    candidate.note(ProposalNote::OfflineDependencies);
    candidate.roots.push(home.to_path_buf());
    if host.is_windows() {
        if !is_file(&home.join("node.exe")) || !real_directory(&home.join("node_modules")) {
            return None;
        }
        candidate.path_entries.push(home.to_path_buf());
        candidate.set_variable(
            "NODE_OPTIONS",
            "--preserve-symlinks --preserve-symlinks-main",
        );
        candidate.note(ProposalNote::NodeWindowsChildren);
    } else {
        candidate.path_entries.push(home.join("bin"));
        let ssl = host.under_root("etc/ssl");
        if *home == host.under_root("usr") && real_directory(&ssl) {
            candidate.roots.push(ssl);
        }
    }
    Some(candidate)
}
