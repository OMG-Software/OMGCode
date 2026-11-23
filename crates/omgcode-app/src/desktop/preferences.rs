//! Saved chat preferences of the desktop `main` window (ADR-0059 decision 8).
//!
//! The file is `<data_root>/desktop-v1/chat-preferences.json`. It holds
//! non-authority defaults for the composer: the model name, budgets,
//! compaction settings, and custom instructions. Nothing in it is a permission,
//! an endpoint, a provider choice, or a secret, and `chat_send` validates every
//! value again when a turn uses it.
//!
//! # Storage contract
//!
//! - The data root is trusted application storage chosen by the operator.
//!   This module does not authorize paths or defend against a hostile parent
//!   directory; it refuses a link or any non-regular file at the final name.
//! - A read is bounded to [`MAX_FILE_BYTES`] and parsed strictly: unknown
//!   fields, a different version, or a value over its bound make the file
//!   unreadable, never partially applied.
//! - A write goes to a new temporary file in the same directory (created
//!   exclusively, mode 0600 on Unix), is flushed to disk, and then renamed
//!   over the final name, so a reader sees the old file or the new one. On
//!   Unix the directory is also synced. Two concurrent saves race; the last
//!   rename wins, and each file is complete. Each save uses its own
//!   temporary name (process identifier, a process-wide counter, and the
//!   time), and a failed save removes only a temporary file it created.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::error::DesktopError;
use super::wire::ChatPreferencesDto;
use crate::chat::validate_tool_call_limit;

/// The directory under the data root that holds desktop-only files.
const DIRECTORY: &str = "desktop-v1";
/// The preferences file name.
const FILE_NAME: &str = "chat-preferences.json";
/// The only file format version this build reads and writes.
const VERSION: u32 = 1;
/// Largest model name accepted. Model identifiers are short; this only bounds
/// what is stored.
pub(super) const MAX_MODEL_BYTES: usize = 256;
/// Largest custom-instruction text, equal to the `chat_send` prefilter.
pub(super) const MAX_INSTRUCTIONS_BYTES: usize = 64 * 1024;
/// Largest file read or written. JSON escaping can grow the instructions up to
/// six times (`\u0000` for one byte), so this leaves room for that.
const MAX_FILE_BYTES: u64 = 512 * 1024;

/// The file's on-disk shape.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PreferencesFile {
    version: u32,
    preferences: ChatPreferencesDto,
}

/// Why a preferences file could not be used.
#[derive(Debug)]
pub(super) enum LoadError {
    /// The file could not be opened or read, or is not a regular file.
    Io,
    /// The file is larger than [`MAX_FILE_BYTES`].
    TooLarge,
    /// The file is not valid JSON of the expected shape and version.
    Format,
    /// A value is over its bound.
    Invalid,
}

fn file_path(data_root: &Path) -> PathBuf {
    data_root.join(DIRECTORY).join(FILE_NAME)
}

/// Check every bound. The same check runs on save and on load.
fn check(preferences: &ChatPreferencesDto) -> Result<(), String> {
    if preferences.model.len() > MAX_MODEL_BYTES {
        return Err("The model name is too long.".to_owned());
    }
    if preferences.model.chars().any(char::is_control) {
        return Err("The model name contains a control character.".to_owned());
    }
    if preferences.instructions.len() > MAX_INSTRUCTIONS_BYTES {
        return Err("The custom instructions are too long.".to_owned());
    }
    validate_tool_call_limit(preferences.max_tool_calls)
}

/// Read the saved preferences.
///
/// `Ok(None)` means no file exists.
///
/// # Errors
///
/// A [`LoadError`] when the file exists but cannot be used.
pub(super) fn load(data_root: &Path) -> Result<Option<ChatPreferencesDto>, LoadError> {
    let path = file_path(data_root);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(LoadError::Io),
    };
    if !metadata.file_type().is_file() {
        return Err(LoadError::Io);
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(LoadError::TooLarge);
    }
    let file = File::open(&path).map_err(|_| LoadError::Io)?;
    let mut bytes = Vec::new();
    // One byte more than the bound shows a file that grew after the check.
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| LoadError::Io)?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_FILE_BYTES) {
        return Err(LoadError::TooLarge);
    }
    let parsed: PreferencesFile = serde_json::from_slice(&bytes).map_err(|_| LoadError::Format)?;
    if parsed.version != VERSION {
        return Err(LoadError::Format);
    }
    check(&parsed.preferences).map_err(|_| LoadError::Invalid)?;
    Ok(Some(parsed.preferences))
}

/// Replace the saved preferences.
///
/// # Errors
///
/// `invalid_request` for a value over its bound, `preferences` when the file
/// cannot be written. A failed save leaves the previous file in place.
pub(super) fn save(data_root: &Path, preferences: &ChatPreferencesDto) -> Result<(), DesktopError> {
    check(preferences).map_err(DesktopError::invalid_request)?;
    let bytes = serde_json::to_vec_pretty(&PreferencesFile {
        version: VERSION,
        preferences: preferences.clone(),
    })
    .map_err(|_| DesktopError::preferences())?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_FILE_BYTES) {
        return Err(DesktopError::invalid_request(
            "The preferences are too large.",
        ));
    }
    let directory = data_root.join(DIRECTORY);
    create_directory(&directory).map_err(|_| DesktopError::preferences())?;
    replace(&directory, &directory.join(temporary_name()), &bytes)
        .map_err(|_| DesktopError::preferences())?;
    sync_directory(&directory).map_err(|_| DesktopError::preferences())
}

/// Write `bytes` through the new file `temporary` and rename it over the
/// preferences file in `directory`.
///
/// The temporary file is removed on failure only when this call created it,
/// so a name collision never deletes another save's file.
fn replace(directory: &Path, temporary: &Path, bytes: &[u8]) -> io::Result<()> {
    let file = create_new(temporary)?;
    let written =
        write_synced(file, bytes).and_then(|()| fs::rename(temporary, directory.join(FILE_NAME)));
    if written.is_err() {
        // Best-effort cleanup of the file this call created; it holds no
        // secret. A failed removal leaves a stray file that no load reads,
        // and the save is reported as failed either way.
        let _ = fs::remove_file(temporary);
    }
    written
}

/// Distinguishes saves within this process, even within one clock tick.
static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A temporary name unique to this process, this save, and this moment.
fn temporary_name() -> String {
    let sequence = SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("{FILE_NAME}.{}-{sequence}-{nanos}.tmp", std::process::id())
}

/// Create the desktop directory, private on Unix.
fn create_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

/// Create a new file at `path`, failing if any entry already exists there.
fn create_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Write `bytes` to `file` and flush it to disk.
fn write_synced(mut file: File, bytes: &[u8]) -> io::Result<()> {
    file.write_all(bytes)?;
    file.sync_all()
}

/// Make the rename durable by syncing the directory.
#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

/// Windows has no directory handle for this in the standard library. The
/// rename replaces the file atomically; its durability after power loss is
/// not claimed.
#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "omgcode-preferences-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("scratch directory");
        path
    }

    fn sample() -> ChatPreferencesDto {
        ChatPreferencesDto {
            model: "qwen3-coder".to_owned(),
            context_tokens: 32_768,
            max_output_tokens: 2048,
            max_tool_calls: 64,
            instructions: "Prefer short answers.".to_owned(),
            compact_threshold: 24_000,
            auto_compact: true,
            // Not the built-in value, so a round trip shows it was stored.
            enter_to_send: false,
        }
    }

    #[test]
    fn a_file_saved_before_enter_to_send_existed_reads_it_as_on() {
        let root = scratch("enter-to-send");
        let directory = root.join(DIRECTORY);
        fs::create_dir_all(&directory).expect("directory");
        fs::write(
            directory.join(FILE_NAME),
            br#"{"version":1,"preferences":{"model":"m","context_tokens":1,"max_output_tokens":1,"instructions":"","compact_threshold":1,"auto_compact":false}}"#,
        )
        .expect("write an older file");
        let loaded = load(&root).expect("load").expect("present");
        assert!(loaded.enter_to_send);
        assert_eq!(loaded.max_tool_calls, 128);
        assert_eq!(loaded.model, "m");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn a_missing_file_is_none() {
        let root = scratch("missing");
        assert!(matches!(load(&root), Ok(None)));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn a_saved_file_loads_back_and_leaves_no_temporary_file() {
        let root = scratch("round-trip");
        save(&root, &sample()).expect("save");
        assert_eq!(load(&root).expect("load"), Some(sample()));
        let names: Vec<String> = fs::read_dir(root.join(DIRECTORY))
            .expect("list")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, [FILE_NAME]);

        // A second save replaces the first.
        let mut changed = sample();
        changed.model = "other".to_owned();
        save(&root, &changed).expect("save again");
        assert_eq!(load(&root).expect("load"), Some(changed));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn values_over_their_bounds_are_refused_and_nothing_is_written() {
        let root = scratch("bounds");
        let mut long_model = sample();
        long_model.model = "m".repeat(MAX_MODEL_BYTES + 1);
        let mut control = sample();
        control.model = "model\u{1b}[2J".to_owned();
        let mut long_instructions = sample();
        long_instructions.instructions = "i".repeat(MAX_INSTRUCTIONS_BYTES + 1);
        let mut tool_calls = sample();
        tool_calls.max_tool_calls = 257;
        for preferences in [long_model, control, long_instructions, tool_calls] {
            let error = save(&root, &preferences).expect_err("refused");
            assert_eq!(error.code(), "invalid_request");
        }
        assert!(!root.join(DIRECTORY).exists());

        // The largest accepted instructions, all escaped, still fit the file.
        let mut largest = sample();
        largest.instructions = "\u{1}".repeat(MAX_INSTRUCTIONS_BYTES);
        save(&root, &largest).expect("the largest value fits");
        assert_eq!(load(&root).expect("load"), Some(largest));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn damaged_files_are_reported_not_applied() {
        let root = scratch("damaged");
        let directory = root.join(DIRECTORY);
        fs::create_dir_all(&directory).expect("directory");
        let path = directory.join(FILE_NAME);
        let cases: [(&[u8], &str); 5] = [
            (b"not json", "format"),
            (br#"{"version":2,"preferences":{"model":"","context_tokens":0,"max_output_tokens":1,"instructions":"","compact_threshold":0,"auto_compact":false}}"#, "format"),
            (br#"{"version":1,"preferences":{"model":"","context_tokens":0,"max_output_tokens":1,"instructions":"","compact_threshold":0,"auto_compact":false,"endpoint":"http://example.com"}}"#, "format"),
            (br#"{"version":1,"preferences":{"model":"a\u0007","context_tokens":0,"max_output_tokens":1,"instructions":"","compact_threshold":0,"auto_compact":false}}"#, "invalid"),
            (br#"{"version":1,"extra":true,"preferences":{"model":"","context_tokens":0,"max_output_tokens":1,"instructions":"","compact_threshold":0,"auto_compact":false}}"#, "format"),
        ];
        for (bytes, expected) in cases {
            fs::write(&path, bytes).expect("write case");
            let error = load(&root).expect_err("unusable");
            let kind = match error {
                LoadError::Format => "format",
                LoadError::Invalid => "invalid",
                LoadError::Io => "io",
                LoadError::TooLarge => "too_large",
            };
            assert_eq!(kind, expected);
        }

        let oversized = vec![b' '; usize::try_from(MAX_FILE_BYTES).expect("fits") + 1];
        fs::write(&path, oversized).expect("write oversized");
        assert!(matches!(load(&root), Err(LoadError::TooLarge)));

        fs::remove_file(&path).expect("remove file");
        fs::create_dir(&path).expect("a directory at the file name");
        assert!(matches!(load(&root), Err(LoadError::Io)));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn a_name_collision_fails_without_removing_the_other_file() {
        let root = scratch("collision");
        let directory = root.join(DIRECTORY);
        fs::create_dir_all(&directory).expect("directory");
        let temporary = directory.join(temporary_name());
        fs::write(&temporary, b"another save").expect("the other save's file");

        assert!(replace(&directory, &temporary, b"{}").is_err());
        assert_eq!(fs::read(&temporary).expect("still there"), b"another save");
        assert!(!directory.join(FILE_NAME).exists());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn temporary_names_differ_within_one_process() {
        let names: std::collections::HashSet<String> = (0..64).map(|_| temporary_name()).collect();
        assert_eq!(names.len(), 64);
    }

    #[test]
    fn a_directory_at_the_file_name_fails_the_save_and_is_kept() {
        let root = scratch("directory");
        let path = root.join(DIRECTORY).join(FILE_NAME);
        fs::create_dir_all(&path).expect("a directory at the file name");

        let error = save(&root, &sample()).expect_err("cannot replace a directory");
        assert_eq!(error.code(), "preferences");
        assert!(path.is_dir());
        let strays = fs::read_dir(root.join(DIRECTORY))
            .expect("list")
            .filter(|entry| {
                entry
                    .as_ref()
                    .is_ok_and(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            })
            .count();
        assert_eq!(strays, 0);
        fs::remove_dir_all(root).expect("cleanup");
    }
}
