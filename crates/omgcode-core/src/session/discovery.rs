//! Bounded discovery of session journals below a trusted data root.
//!
//! Discovery walks only `<data_root>/sessions/<YYYY>/<MM>/<DD>/` and lists
//! files whose names match the writer's exact pattern. It never follows a
//! symbolic link or Windows reparse point, never opens a journal, and never
//! reads journal content. A listing is a locator, not validation: inspect or
//! open a journal to learn whether it is valid.

use std::fs::{self, DirEntry, FileType};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use uuid::Uuid;

use super::{SessionError, filename};

/// The most journals one discovery may list.
const MAX_DISCOVERED_JOURNALS: usize = 50_000;
/// The most directory entries one discovery may examine, at every level.
const MAX_DIRECTORY_ENTRIES: usize = 500_000;
/// Directory levels below `sessions`: year, month, day.
const DATE_LEVELS: usize = 3;
/// Required ASCII digit counts for the year, month, and day directory names.
const DATE_WIDTHS: [usize; DATE_LEVELS] = [4, 2, 2];
const SESSIONS_DIRECTORY: &str = "sessions";

/// One journal file found by discovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredJournal {
    /// The session UUID taken from the file name. Replay must still confirm it.
    pub session_id: Uuid,
    /// The journal path: the absolute form of the data root joined with
    /// `relative_path`. Symbolic links are not resolved.
    pub path: PathBuf,
    /// The path below the data root, such as
    /// `sessions/2026/11/16/omgcode-...jsonl`. It is a stable key.
    pub relative_path: PathBuf,
    /// The file size in bytes when discovery listed it.
    pub size: u64,
    /// The file's last-modified time as reported by the operating system.
    pub modified: SystemTime,
}

/// The result of one discovery walk.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Discovery {
    /// Journals sorted by `relative_path`.
    pub journals: Vec<DiscoveredJournal>,
    /// Symbolic links and reparse points that were skipped at any level.
    pub skipped_links: u64,
    /// Entries that do not match the layout, including non-UTF-8 names,
    /// wrongly named files and directories, and entries that vanished mid-walk.
    pub unrecognized: u64,
}

/// List journals below `data_root` in a deterministic order.
///
/// `data_root` must come from trusted application settings. A missing
/// `sessions` directory yields an empty result. A `sessions` directory that is
/// itself a link is skipped and counted. Concurrent changes can make a listing
/// stale; it is not a snapshot.
///
/// # Errors
///
/// Returns `SessionError::LimitReached` after more than 50,000 journals or
/// 500,000 directory entries, and `SessionError::Io` when a directory cannot be
/// read.
pub fn discover(data_root: &Path) -> Result<Discovery, SessionError> {
    discover_with_limits(data_root, MAX_DISCOVERED_JOURNALS, MAX_DIRECTORY_ENTRIES)
}

/// Run `discover` with explicit caps so tests can reach them cheaply.
fn discover_with_limits(
    data_root: &Path,
    max_journals: usize,
    max_entries: usize,
) -> Result<Discovery, SessionError> {
    let absolute_root = std::path::absolute(data_root)?;
    let mut walk = Walk {
        absolute_root,
        max_journals,
        max_entries,
        examined: 0,
        discovery: Discovery::default(),
    };
    let sessions = data_root.join(SESSIONS_DIRECTORY);
    let metadata = match fs::symlink_metadata(&sessions) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(walk.discovery),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
        walk.discovery.skipped_links += 1;
        return Ok(walk.discovery);
    }
    if !metadata.is_dir() {
        walk.discovery.unrecognized += 1;
        return Ok(walk.discovery);
    }
    walk.directory(&sessions, Path::new(SESSIONS_DIRECTORY), 0)?;
    let mut discovery = walk.discovery;
    discovery
        .journals
        .sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(discovery)
}

/// Find the one journal path for `session_id` below `data_root`.
///
/// A UI addresses sessions by UUID and never supplies a path. This runs a full
/// `discover` walk and compares file-name UUIDs. It returns `None` when no
/// journal carries the UUID.
///
/// # Errors
///
/// Returns `SessionError::InvalidJournal` when two files carry the same UUID,
/// because choosing one would be a guess. It also returns every error of
/// `discover`.
pub fn locate(data_root: &Path, session_id: Uuid) -> Result<Option<PathBuf>, SessionError> {
    let mut matches = discover(data_root)?
        .journals
        .into_iter()
        .filter(|journal| journal.session_id == session_id);
    let first = matches.next();
    if matches.next().is_some() {
        return Err(SessionError::InvalidJournal(
            "more than one journal has this session UUID",
        ));
    }
    Ok(first.map(|journal| journal.path))
}

/// Mutable walk state with the shared entry budget.
struct Walk {
    absolute_root: PathBuf,
    max_journals: usize,
    max_entries: usize,
    examined: usize,
    discovery: Discovery,
}

impl Walk {
    /// Visit one directory. `level` 0 holds years, 1 months, 2 days, 3 files.
    fn directory(
        &mut self,
        directory: &Path,
        relative: &Path,
        level: usize,
    ) -> Result<(), SessionError> {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if level > 0 && error.kind() == io::ErrorKind::NotFound => {
                self.discovery.unrecognized += 1;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            self.examined += 1;
            if self.examined > self.max_entries {
                return Err(SessionError::LimitReached);
            }
            self.entry(&entry, relative, level)?;
        }
        Ok(())
    }

    /// Classify one entry without following it.
    fn entry(
        &mut self,
        entry: &DirEntry,
        relative: &Path,
        level: usize,
    ) -> Result<(), SessionError> {
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.discovery.unrecognized += 1;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        if file_type.is_symlink() || entry_is_reparse_point(entry, &file_type)? {
            self.discovery.skipped_links += 1;
            return Ok(());
        }
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            self.discovery.unrecognized += 1;
            return Ok(());
        };
        if let Some(width) = DATE_WIDTHS.get(level) {
            if file_type.is_dir() && has_ascii_digits(name, *width) {
                return self.directory(&entry.path(), &relative.join(name), level + 1);
            }
        } else if file_type.is_file()
            && let Some(session_id) = filename::parse_strict(name)
        {
            return self.journal(entry, relative, name, session_id);
        }
        self.discovery.unrecognized += 1;
        Ok(())
    }

    /// Record one matching file, or count it when it vanished.
    fn journal(
        &mut self,
        entry: &DirEntry,
        relative: &Path,
        name: &str,
        session_id: Uuid,
    ) -> Result<(), SessionError> {
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.discovery.unrecognized += 1;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        if self.discovery.journals.len() >= self.max_journals {
            return Err(SessionError::LimitReached);
        }
        let relative_path = relative.join(name);
        self.discovery.journals.push(DiscoveredJournal {
            session_id,
            path: self.absolute_root.join(&relative_path),
            relative_path,
            size: metadata.len(),
            modified: metadata.modified()?,
        });
        Ok(())
    }
}

/// Return whether `name` is exactly `width` ASCII digits.
fn has_ascii_digits(name: &str, width: usize) -> bool {
    name.len() == width && name.bytes().all(|byte| byte.is_ascii_digit())
}

/// Detect a Windows reparse point that `FileType` did not report as a link.
#[cfg(windows)]
fn entry_is_reparse_point(entry: &DirEntry, _file_type: &FileType) -> io::Result<bool> {
    match entry.metadata() {
        Ok(metadata) => Ok(is_reparse_point(&metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Other platforms report links through `FileType` alone.
#[cfg(not(windows))]
fn entry_is_reparse_point(_entry: &DirEntry, _file_type: &FileType) -> io::Result<bool> {
    Ok(false)
}

/// Return whether metadata carries the Windows reparse-point attribute.
#[cfg(windows)]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// Other platforms have no reparse points.
#[cfg(not(windows))]
fn is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::discover_with_limits;
    use crate::session::{SessionError, filename};
    use std::fs;
    use time::OffsetDateTime;
    use uuid::Uuid;

    fn day_with_journals(count: usize) -> tempfile::TempDir {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let day = temporary.path().join("sessions/2026/01/02");
        fs::create_dir_all(&day).expect("day directory");
        for _ in 0..count {
            let name = filename::journal_file_name(OffsetDateTime::now_utc(), Uuid::new_v4());
            fs::write(day.join(name), b"").expect("journal placeholder");
        }
        temporary
    }

    #[test]
    fn journal_cap_returns_limit_reached_only_when_exceeded() {
        let temporary = day_with_journals(3);
        let at_cap = discover_with_limits(temporary.path(), 3, 1_000).expect("at the cap");
        assert_eq!(at_cap.journals.len(), 3);
        assert!(matches!(
            discover_with_limits(temporary.path(), 2, 1_000),
            Err(SessionError::LimitReached)
        ));
    }

    #[test]
    fn entry_cap_counts_directories_and_files_together() {
        // Entries examined: year, month, day, then three files.
        let temporary = day_with_journals(3);
        discover_with_limits(temporary.path(), 100, 6).expect("exactly at the cap");
        assert!(matches!(
            discover_with_limits(temporary.path(), 100, 5),
            Err(SessionError::LimitReached)
        ));
    }
}
