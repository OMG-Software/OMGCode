//! JDKs and Java runtimes.
//!
//! Looks in the operator's `JAVA_HOME`, the vendors' Windows install folders,
//! `/usr/lib/jvm`, `/usr/lib64/jvm`, `/opt`, SDKMAN, and IntelliJ's `~/.jdks`,
//! and follows a `java` on the operator's `PATH` to its installation. The
//! version comes from the JDK's own `release` file; nothing is run. Each
//! proposal sets `JAVA_HOME`.

use std::path::{Path, PathBuf};

use super::{
    Candidate, DiscoveryHost, ProposalNote, ToolchainFamily, child_directories, file_name,
    install_of, is_file, push_unique, read_text,
};

/// Windows vendor folders under `Program Files` that hold one JDK per child.
const WINDOWS_VENDORS: [&str; 9] = [
    "Java",
    "Eclipse Adoptium",
    "Microsoft",
    "Zulu",
    "Amazon Corretto",
    "BellSoft",
    "OpenJDK",
    "Semeru",
    "Eclipse Foundation",
];

pub(super) fn detect(host: &DiscoveryHost) -> Vec<Candidate> {
    let mut homes: Vec<PathBuf> = Vec::new();
    if let Some(java_home) = &host.java_home {
        push_unique(&mut homes, java_home.clone());
    }
    if host.is_windows() {
        for program_files in &host.program_files {
            for vendor in WINDOWS_VENDORS {
                for home in child_directories(&program_files.join(vendor)) {
                    push_unique(&mut homes, home);
                }
            }
        }
    } else {
        for parent in ["usr/lib/jvm", "usr/lib64/jvm", "opt"] {
            for home in child_directories(&host.under_root(parent)) {
                push_unique(&mut homes, home);
            }
        }
        if let Some(sdkman) = host.under_home(".sdkman/candidates/java") {
            for home in child_directories(&sdkman) {
                push_unique(&mut homes, home);
            }
        }
    }
    if let Some(jdks) = host.under_home(".jdks") {
        for home in child_directories(&jdks) {
            push_unique(&mut homes, home);
        }
    }
    for bin in host.search_directories(&host.exe("java")) {
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
    if !is_file(&bin.join(host.exe("java"))) {
        return None;
    }
    let version = release_version(home).or_else(|| file_name(home));
    // `21.0.2` and `java-21-openjdk-amd64` are named `java-21`; another JDK
    // 21 gets `java-21-2`.
    let major = version
        .as_deref()
        .and_then(|text| {
            text.split(['.', '+', '-', '_'])
                .find(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        })
        .map(str::to_owned)
        .or_else(|| version.clone())
        .unwrap_or_default();
    let mut candidate = Candidate::new(ToolchainFamily::Java, "java", major);
    candidate.version = version;
    if !candidate.set_path_variable("JAVA_HOME", home) {
        return None;
    }
    if !is_file(&bin.join(host.exe("javac"))) {
        candidate.note(ProposalNote::JavaRuntimeOnly);
    }
    candidate.note(ProposalNote::OfflineDependencies);
    candidate.roots.push(home.to_path_buf());
    candidate.path_entries.push(bin);
    Some(candidate)
}

/// `JAVA_VERSION` from the JDK's `release` file, such as `21.0.2`.
fn release_version(home: &Path) -> Option<String> {
    read_text(&home.join("release"))?.lines().find_map(|line| {
        let value = line.strip_prefix("JAVA_VERSION=")?;
        let value = value.trim().trim_matches('"');
        (!value.is_empty()).then(|| value.to_owned())
    })
}
