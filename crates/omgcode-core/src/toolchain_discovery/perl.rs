//! Perl installations.
//!
//! On Windows: Strawberry Perl (`C:\Strawberry`, with its MinGW `c\bin`),
//! ActiveState Perl (`C:\Perl64`, `C:\Perl`), and a `perl.exe` on the
//! operator's `PATH` whose installation holds `lib\Config.pm`, which skips
//! the MSYS Perl inside Git for Windows. On Linux: `/usr`, `/usr/local`,
//! perlbrew, and the operator's `PATH`. The version comes from the library
//! folder names; nothing is run.

use std::path::{Path, PathBuf};

use super::{
    Candidate, DiscoveryHost, ProposalNote, ToolchainFamily, child_directories,
    child_directories_named, file_name, install_of, is_file, newest, push_unique, real_directory,
};

pub(super) fn detect(host: &DiscoveryHost) -> Vec<Candidate> {
    if host.is_windows() {
        windows(host)
    } else {
        linux(host)
    }
}

fn windows(host: &DiscoveryHost) -> Vec<Candidate> {
    let mut found = Vec::new();
    let strawberry = host.under_root("Strawberry");
    let perl = strawberry.join("perl");
    if is_file(&perl.join("bin").join("perl.exe")) {
        let mut paths = Vec::new();
        for entry in [
            perl.join("site").join("bin"),
            perl.join("bin"),
            strawberry.join("c").join("bin"),
        ] {
            if real_directory(&entry) {
                paths.push(entry);
            }
        }
        found.push(candidate(&strawberry, &perl, paths));
    }
    let mut homes: Vec<PathBuf> = vec![host.under_root("Perl64"), host.under_root("Perl")];
    for bin in host.search_directories("perl.exe") {
        if let Some(home) = install_of(&bin) {
            push_unique(&mut homes, home);
        }
    }
    for home in homes {
        if home.starts_with(&strawberry)
            || !is_file(&home.join("bin").join("perl.exe"))
            || !is_file(&home.join("lib").join("Config.pm"))
        {
            continue;
        }
        let paths = [home.join("site").join("bin"), home.join("bin")]
            .into_iter()
            .filter(|entry| real_directory(entry))
            .collect();
        found.push(candidate(&home, &home, paths));
    }
    found
}

fn linux(host: &DiscoveryHost) -> Vec<Candidate> {
    let mut bins = vec![host.under_root("usr/bin"), host.under_root("usr/local/bin")];
    if let Some(perlbrew) = host.under_home("perl5/perlbrew/perls") {
        bins.extend(
            child_directories(&perlbrew)
                .into_iter()
                .map(|v| v.join("bin")),
        );
    }
    bins.extend(host.search_directories("perl"));
    let mut homes: Vec<PathBuf> = Vec::new();
    for bin in bins {
        if is_file(&bin.join("perl"))
            && let Some(home) = install_of(&bin)
        {
            push_unique(&mut homes, home);
        }
    }
    homes
        .iter()
        .map(|home| candidate(home, home, vec![home.join("bin")]))
        .collect()
}

/// A Perl rooted at `root`, whose own tree is `perl`.
fn candidate(root: &Path, perl: &Path, path_entries: Vec<PathBuf>) -> Candidate {
    // `lib/perl5/5.38.2` (perlbrew, `/usr/local`) or `share/perl/5.38.2`
    // (Debian); Strawberry and ActiveState keep no versioned folder.
    let version = newest(
        child_directories_named(&perl.join("lib").join("perl5"), "5.")
            .into_iter()
            .chain(child_directories_named(
                &perl.join("share").join("perl"),
                "5.",
            )),
    )
    .as_deref()
    .and_then(file_name);
    let mut candidate = Candidate::new(
        ToolchainFamily::Perl,
        "perl",
        version.clone().unwrap_or_default(),
    );
    candidate.version = version;
    candidate.note(ProposalNote::OfflineDependencies);
    candidate.roots.push(root.to_path_buf());
    candidate.path_entries = path_entries;
    candidate
}
