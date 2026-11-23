//! Per-workspace custom instructions and their composition with the global
//! ones (ADR-0080).
//!
//! Each workspace's text is in
//! `<data_root>/desktop-v1/workspace-instructions/<workspace id>.json`. It is
//! operator-typed, untrusted-origin text: like the global instructions, it is
//! sent as a labelled user-level message, never as a system message, and it
//! grants nothing. Only the `settings` window writes it, for a registered
//! workspace; `chat_send` reads it for the session's recorded workspace.
//!
//! The file follows the storage contract of [`super::preferences`]: a bounded,
//! strict read that refuses a link or non-regular entry, and an atomic
//! replace. Saving empty text removes the file.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::error::DesktopError;
use super::preferences::{self, LoadError, MAX_SAVED_INSTRUCTIONS_BYTES};

/// The directory under the data root's desktop directory.
const DIRECTORY: &str = "workspace-instructions";
/// The only file format version this build reads and writes.
const VERSION: u32 = 1;
/// Largest file read or written. JSON escaping can grow the text up to six
/// times (`\u0000` for one byte), so this leaves room for that.
const MAX_FILE_BYTES: u64 = 128 * 1024;
/// Heading of the global instructions when a workspace adds its own.
const GLOBAL_HEADING: &str = "Global instructions:\n";
/// Heading of a workspace's instructions.
const WORKSPACE_HEADING: &str = "Workspace instructions:\n";

/// The file's on-disk shape.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InstructionsFile {
    version: u32,
    instructions: String,
}

fn directory(data_root: &Path) -> PathBuf {
    data_root.join(preferences::DIRECTORY).join(DIRECTORY)
}

/// A UUID's hyphenated text has only hex digits and hyphens, so it is a safe
/// file name on every platform.
fn file_name(workspace: Uuid) -> String {
    format!("{}.json", workspace.hyphenated())
}

/// Check the bound shared by save and load.
fn check(instructions: &str) -> Result<(), String> {
    if instructions.len() > MAX_SAVED_INSTRUCTIONS_BYTES {
        return Err(format!(
            "The workspace instructions are longer than {} KiB.",
            MAX_SAVED_INSTRUCTIONS_BYTES / 1024
        ));
    }
    Ok(())
}

/// Read one workspace's instructions; empty when none are saved.
///
/// # Errors
///
/// A [`LoadError`] when the file exists but cannot be used.
pub(super) fn load(data_root: &Path, workspace: Uuid) -> Result<String, LoadError> {
    let path = directory(data_root).join(file_name(workspace));
    let Some(bytes) = preferences::read_bounded(&path, MAX_FILE_BYTES)? else {
        return Ok(String::new());
    };
    let parsed: InstructionsFile = serde_json::from_slice(&bytes).map_err(|_| LoadError::Format)?;
    if parsed.version != VERSION {
        return Err(LoadError::Format);
    }
    check(&parsed.instructions).map_err(|_| LoadError::Invalid)?;
    Ok(parsed.instructions)
}

/// Replace one workspace's instructions. Empty text removes the file.
///
/// The caller checks that the workspace is registered and holds the service's
/// preferences lock.
///
/// # Errors
///
/// `invalid_request` for text over the bound, `preferences` when the file
/// cannot be written or removed. A failed save leaves the previous file.
pub(super) fn save(
    data_root: &Path,
    workspace: Uuid,
    instructions: &str,
) -> Result<(), DesktopError> {
    check(instructions).map_err(DesktopError::invalid_request)?;
    let directory = directory(data_root);
    let name = file_name(workspace);
    if instructions.is_empty() {
        return match fs::remove_file(directory.join(&name)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(DesktopError::workspace_instructions()),
        };
    }
    let bytes = serde_json::to_vec_pretty(&InstructionsFile {
        version: VERSION,
        instructions: instructions.to_owned(),
    })
    .map_err(|_| DesktopError::workspace_instructions())?;
    preferences::write_atomic(&directory, &name, &bytes)
        .map_err(|_| DesktopError::workspace_instructions())
}

/// The custom instructions one turn sends: the global text alone when the
/// workspace has none, so earlier turns' manifests keep their shape, and
/// otherwise both under headings, global first.
///
/// # Errors
///
/// `invalid_request` when the composed text is longer than the core's bound
/// on custom instructions, so the operator learns which setting to shorten.
pub(super) fn compose(global: &str, workspace: &str) -> Result<String, DesktopError> {
    let composed = if workspace.is_empty() {
        global.to_owned()
    } else if global.is_empty() {
        format!("{WORKSPACE_HEADING}{workspace}")
    } else {
        format!("{GLOBAL_HEADING}{global}\n\n{WORKSPACE_HEADING}{workspace}")
    };
    if composed.len() > MAX_SAVED_INSTRUCTIONS_BYTES {
        return Err(DesktopError::invalid_request(format!(
            "The global and workspace instructions together are longer than {} KiB. Shorten them in Settings.",
            MAX_SAVED_INSTRUCTIONS_BYTES / 1024
        )));
    }
    Ok(composed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "omgcode-instructions-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("scratch directory");
        path
    }

    #[test]
    fn a_saved_text_loads_back_and_empty_text_removes_it() {
        let root = scratch("round-trip");
        let workspace = Uuid::new_v4();
        assert_eq!(load(&root, workspace).expect("none saved"), "");

        save(&root, workspace, "Use tabs.\nRun the tests.").expect("save");
        assert_eq!(
            load(&root, workspace).expect("load"),
            "Use tabs.\nRun the tests."
        );
        // Another workspace has none.
        assert_eq!(load(&root, Uuid::new_v4()).expect("other"), "");

        save(&root, workspace, "").expect("remove");
        assert!(!directory(&root).join(file_name(workspace)).exists());
        assert_eq!(load(&root, workspace).expect("removed"), "");
        // Removing what is not there succeeds.
        save(&root, workspace, "").expect("remove again");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn text_over_the_bound_is_refused_and_the_largest_fits_escaped() {
        let root = scratch("bounds");
        let workspace = Uuid::new_v4();
        let error = save(
            &root,
            workspace,
            &"i".repeat(MAX_SAVED_INSTRUCTIONS_BYTES + 1),
        )
        .expect_err("too long");
        assert_eq!(error.code(), "invalid_request");
        assert!(!directory(&root).exists());

        let largest = "\u{1}".repeat(MAX_SAVED_INSTRUCTIONS_BYTES);
        save(&root, workspace, &largest).expect("the largest value fits");
        assert_eq!(load(&root, workspace).expect("load"), largest);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn damaged_files_are_reported_not_applied() {
        let root = scratch("damaged");
        let workspace = Uuid::new_v4();
        let folder = directory(&root);
        fs::create_dir_all(&folder).expect("directory");
        let path = folder.join(file_name(workspace));
        for bytes in [
            &b"not json"[..],
            br#"{"version":2,"instructions":"x"}"#,
            br#"{"version":1,"instructions":"x","extra":true}"#,
        ] {
            fs::write(&path, bytes).expect("write case");
            assert!(matches!(load(&root, workspace), Err(LoadError::Format)));
        }
        fs::remove_file(&path).expect("remove file");
        fs::create_dir(&path).expect("a directory at the file name");
        assert!(matches!(load(&root, workspace), Err(LoadError::Io)));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn composition_keeps_global_alone_and_heads_both_otherwise() {
        assert_eq!(compose("", "").expect("empty"), "");
        assert_eq!(compose("Be brief.", "").expect("global"), "Be brief.");
        assert_eq!(
            compose("", "Use tabs.").expect("workspace"),
            "Workspace instructions:\nUse tabs."
        );
        assert_eq!(
            compose("Be brief.", "Use tabs.").expect("both"),
            "Global instructions:\nBe brief.\n\nWorkspace instructions:\nUse tabs."
        );
    }

    #[test]
    fn composition_over_the_core_bound_names_the_settings() {
        let half = "h".repeat(MAX_SAVED_INSTRUCTIONS_BYTES / 2);
        let error = compose(&half, &half).expect_err("headings push it over");
        assert_eq!(error.code(), "invalid_request");
        // Either text alone at the bound fits.
        let full = "f".repeat(MAX_SAVED_INSTRUCTIONS_BYTES);
        assert_eq!(compose(&full, "").expect("global alone"), full);
    }
}
