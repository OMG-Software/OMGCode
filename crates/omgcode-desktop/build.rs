//! Declares every application command to Tauri's permission layer.
//!
//! With `AppManifest::commands`, each command gets generated `allow-<name>` and
//! `deny-<name>` permissions and is refused unless a capability grants it to the
//! calling window. Without a manifest, every registered command is open to
//! every window. The list is shared with `src/policy.rs` so the two cannot drift.

include!("src/command_names.rs");

fn main() {
    let manifest = tauri_build::AppManifest::new().commands(COMMAND_NAMES);
    let attributes = tauri_build::Attributes::new().app_manifest(manifest);
    if let Err(error) = tauri_build::try_build(attributes) {
        eprintln!("tauri build script failed: {error:#}");
        std::process::exit(1);
    }
}
