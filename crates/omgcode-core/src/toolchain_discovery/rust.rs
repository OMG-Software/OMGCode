//! Rust toolchains installed by rustup.
//!
//! Each proposal names one toolchain directory, such as
//! `~/.rustup/toolchains/stable-x86_64-pc-windows-msvc`, and puts its own `bin`
//! on `PATH`. The rustup proxies in `~/.cargo/bin` are not used: running the
//! toolchain's `cargo` and `rustc` directly keeps the operator's cargo home,
//! which can hold `credentials.toml`, out of the profile, and no proxy ever
//! reads a repository's `rust-toolchain.toml`. So no `RUSTUP_*` variable is
//! needed. Cargo's own home is then the run's private `HOME`.

use std::path::Path;

use super::{
    Candidate, DiscoveryHost, ProposalNote, ToolchainFamily, c_cpp, child_directories, file_name,
    is_file, read_text,
};

pub(super) fn detect(host: &DiscoveryHost) -> Vec<Candidate> {
    let Some(rustup) = host.rustup_home.as_deref() else {
        return Vec::new();
    };
    let default = default_toolchain(rustup);
    let msvc = c_cpp::msvc(host);
    child_directories(&rustup.join("toolchains"))
        .into_iter()
        .filter_map(|toolchain| {
            let name = file_name(&toolchain)?;
            let bin = toolchain.join("bin");
            if !is_file(&bin.join(host.exe("rustc"))) {
                return None;
            }
            let mut candidate = Candidate::new(ToolchainFamily::Rust, "rust", name.clone());
            candidate.preferred = default
                .as_deref()
                .is_some_and(|chosen| name == chosen || name.starts_with(&format!("{chosen}-")));
            candidate.qualified = true;
            candidate.version = Some(name.clone());
            candidate.roots.push(toolchain);
            candidate.path_entries.push(bin);
            candidate.note(ProposalNote::OfflineDependencies);
            if host.is_windows() && name.ends_with("-pc-windows-msvc") {
                match &msvc {
                    Some(parts) => parts.merge_into(&mut candidate),
                    None => candidate.note(ProposalNote::NoMsvc),
                }
            }
            Some(candidate)
        })
        .collect()
}

/// rustup's default toolchain, from the `settings.toml` rustup keeps in the
/// operator's rustup home. A repository's `rust-toolchain.toml` is never read.
fn default_toolchain(rustup: &Path) -> Option<String> {
    read_text(&rustup.join("settings.toml"))?
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "default_toolchain").then(|| value.trim().trim_matches('"').to_owned())
        })
        .filter(|name| !name.is_empty())
}
