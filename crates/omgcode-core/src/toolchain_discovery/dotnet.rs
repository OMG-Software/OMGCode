//! .NET installations.
//!
//! An installation keeps the `dotnet` host beside its `shared` folder:
//! `Program Files\dotnet`, `~/.dotnet`, `/usr/share/dotnet`,
//! `/usr/lib/dotnet`, `/usr/lib64/dotnet`, `/opt/dotnet`, and the operator's
//! `PATH`. The version is the newest `sdk/<version>` name, or the newest
//! runtime when there is no SDK; nothing is run.
//!
//! Each proposal sets `DOTNET_ROOT` and turns off what a first run would
//! otherwise attempt: telemetry, the banner, and the HTTPS development
//! certificate. NuGet's cache defaults to the run's private home.

use std::path::{Path, PathBuf};

use super::{
    Candidate, DiscoveryHost, ProposalNote, ToolchainFamily, child_directories, file_name, is_file,
    newest, push_unique, real_directory,
};

pub(super) fn detect(host: &DiscoveryHost) -> Vec<Candidate> {
    let mut homes: Vec<PathBuf> = Vec::new();
    if host.is_windows() {
        for program_files in &host.program_files {
            push_unique(&mut homes, program_files.join("dotnet"));
        }
    } else {
        for relative in [
            "usr/share/dotnet",
            "usr/lib/dotnet",
            "usr/lib64/dotnet",
            "opt/dotnet",
        ] {
            push_unique(&mut homes, host.under_root(relative));
        }
    }
    if let Some(user) = host.under_home(".dotnet") {
        push_unique(&mut homes, user);
    }
    for directory in host.search_directories(&host.exe("dotnet")) {
        push_unique(&mut homes, directory);
    }
    homes
        .iter()
        .filter_map(|home| candidate(host, home))
        .collect()
}

fn candidate(host: &DiscoveryHost, home: &Path) -> Option<Candidate> {
    if !real_directory(home)
        || !is_file(&home.join(host.exe("dotnet")))
        || !real_directory(&home.join("shared"))
    {
        return None;
    }
    let sdk = newest(child_directories(&home.join("sdk")))
        .as_deref()
        .and_then(file_name);
    let runtime = newest(child_directories(
        &home.join("shared").join("Microsoft.NETCore.App"),
    ))
    .as_deref()
    .and_then(file_name);
    let (variant, version) = match (&sdk, &runtime) {
        (Some(sdk), _) => (sdk.clone(), Some(format!("SDK {sdk}"))),
        (None, Some(runtime)) => (runtime.clone(), Some(format!("runtime {runtime}"))),
        (None, None) => (String::new(), None),
    };
    let mut candidate = Candidate::new(ToolchainFamily::DotNet, "dotnet", variant);
    candidate.version = version;
    if !candidate.set_path_variable("DOTNET_ROOT", home) {
        return None;
    }
    candidate.set_variable("DOTNET_CLI_TELEMETRY_OPTOUT", "1");
    candidate.set_variable("DOTNET_NOLOGO", "1");
    candidate.set_variable("DOTNET_GENERATE_ASPNET_CERTIFICATE", "false");
    if sdk.is_none() {
        candidate.note(ProposalNote::DotnetRuntimeOnly);
    }
    candidate.note(ProposalNote::OfflineDependencies);
    candidate.roots.push(home.to_path_buf());
    candidate.path_entries.push(home.to_path_buf());
    Some(candidate)
}
