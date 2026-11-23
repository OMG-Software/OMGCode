//! The names in one directory of a registered workspace, for the operator's own
//! view (ADR-0083).
//!
//! This is not a model action and not a policy decision. It does not need the
//! workspace's `read` preference, writes no audit record, and grants nothing: a
//! listing never authorizes a later read, edit, or run. Like
//! [`crate::git_head::workspace_head`], it opens the registered development
//! root and workspace by their recorded identities and works only through those
//! handles, so no path is resolved from outside the workspace and no link is
//! followed. The platform broker refuses the `.OMGCode` store, and this module
//! also hides its name from a listing.
//!
//! A listing returns names and kinds. It reads no file content and no size.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

use omgcode_platform::broker::{self, EntryKind, RelativePath, Workspace, is_reserved_name};
use serde::Serialize;
use thiserror::Error;

use crate::control::{ControlError, ControlStore};
use crate::policy::WorkspaceId;

/// The most entries one listing returns. It equals the broker's bound.
pub const MAX_ENTRIES: usize = broker::MAX_DIRECTORY_ENTRIES;
/// The most path components a request may carry.
pub const MAX_DEPTH: usize = 32;
/// The most Unicode scalar values in one path component.
pub const MAX_COMPONENT_CHARS: usize = 255;

/// What an entry is, as far as the broker observed without following it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryClass {
    /// A directory. It may be listed in turn.
    Directory,
    /// A regular file.
    File,
    /// Anything else the broker does not open: a link or reparse point, a
    /// special file, or an entry that could not be classified.
    Other,
}

/// One entry of a listing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkspaceEntry {
    /// The name. When `lossy` it is for display only: it is not the native
    /// name and must never be sent back as a path component.
    pub name: String,
    /// What the entry is.
    pub kind: EntryClass,
    /// The native name is not valid Unicode, so `name` has replacement
    /// characters.
    pub lossy: bool,
}

/// A bounded listing of one directory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkspaceListing {
    /// Directories first, then every other entry, each group by name.
    pub entries: Vec<WorkspaceEntry>,
    /// More entries exist than were returned. Which ones are left out is
    /// decided by the file system's order, not by the sort.
    pub truncated: bool,
}

/// Why a listing was not produced.
#[derive(Debug, Error)]
pub enum EntriesError {
    /// A path component is not acceptable. The text is a fixed reason.
    #[error("invalid directory path: {0}")]
    InvalidPath(&'static str),
    /// The settings store, the registration, or the workspace's recorded
    /// identity could not be used.
    #[error("workspace unavailable: {0}")]
    Control(#[from] ControlError),
    /// The workspace folder changed since it was registered.
    #[error("the workspace folder changed")]
    Changed,
    /// The directory is missing, is not a directory, or could not be listed.
    #[error("the directory cannot be listed: {0}")]
    Native(#[source] io::Error),
}

/// Validate request components and join them into a broker path.
///
/// Each component must be non-empty, not `.` or `..`, free of separators, NUL
/// and the reserved store name, and at most [`MAX_COMPONENT_CHARS`] characters;
/// at most [`MAX_DEPTH`] components are allowed. An empty slice names the
/// workspace itself. The broker validates the result again for the host.
pub fn validate_components(components: &[String]) -> Result<RelativePath, EntriesError> {
    if components.len() > MAX_DEPTH {
        return Err(EntriesError::InvalidPath("too many components"));
    }
    let mut path = PathBuf::new();
    for component in components {
        if component.is_empty() {
            return Err(EntriesError::InvalidPath("empty component"));
        }
        if component == "." || component == ".." {
            return Err(EntriesError::InvalidPath("dot component"));
        }
        if component.chars().count() > MAX_COMPONENT_CHARS {
            return Err(EntriesError::InvalidPath("component too long"));
        }
        if component.contains(['/', '\\', '\0']) {
            return Err(EntriesError::InvalidPath("separator or NUL in component"));
        }
        if is_reserved_name(std::ffi::OsStr::new(component)) {
            return Err(EntriesError::InvalidPath("reserved name"));
        }
        path.push(component);
    }
    RelativePath::new(&path).map_err(|error| {
        if error.kind() == io::ErrorKind::InvalidInput {
            EntriesError::InvalidPath("not a valid workspace-relative path")
        } else {
            EntriesError::Native(error)
        }
    })
}

/// List one directory of a registered workspace.
///
/// `components` are validated by [`validate_components`] before any file
/// system access. The workspace is found only through `control` and
/// `workspace_id`; the caller supplies no host path.
///
/// # Errors
///
/// [`EntriesError::InvalidPath`] before any access, [`EntriesError::Control`]
/// for an unknown workspace or an unusable root, [`EntriesError::Changed`] when
/// either folder's creation time no longer matches the record, and
/// [`EntriesError::Native`] for a missing directory, a file, or a refused name.
pub fn list_workspace_entries(
    control: &ControlStore,
    workspace_id: WorkspaceId,
    components: &[String],
) -> Result<WorkspaceListing, EntriesError> {
    let target = validate_components(components)?;
    let scope = control.file_scope(workspace_id)?;
    let root_path = scope
        .root
        .live_path()
        .map_err(|error| EntriesError::Control(ControlError::Io(error)))?;
    let workspace_path = scope
        .workspace
        .live_path()
        .map_err(|error| EntriesError::Control(ControlError::Io(error)))?;
    let relative = workspace_path
        .strip_prefix(&root_path)
        .ok()
        .and_then(|path| RelativePath::new(path).ok())
        .ok_or(EntriesError::Changed)?;
    let workspace = Workspace::open(
        &root_path,
        scope.root.native_identity(),
        &relative,
        scope.workspace.native_identity(),
    )
    .map_err(EntriesError::Native)?;
    let (root_created, workspace_created) =
        workspace.creation_times().map_err(EntriesError::Native)?;
    if !scope.root.matches_created(root_created)
        || !scope.workspace.matches_created(workspace_created)
    {
        return Err(EntriesError::Changed);
    }
    let listing = workspace
        .list(&target, MAX_ENTRIES, broker::MAX_DIRECTORY_NAME_UNITS)
        .map_err(EntriesError::Native)?;
    Ok(WorkspaceListing {
        entries: shape(listing.entries),
        truncated: listing.truncated,
    })
}

/// Hide the store, name each entry, and sort directories first, then by name
/// (case-insensitively, then exactly, so the order is total).
fn shape(entries: Vec<broker::DirectoryEntry>) -> Vec<WorkspaceEntry> {
    let mut shaped: Vec<WorkspaceEntry> = entries
        .into_iter()
        .filter(|entry| !is_reserved_name(&entry.name))
        .map(|entry| {
            let kind = match entry.kind {
                EntryKind::Directory => EntryClass::Directory,
                EntryKind::File => EntryClass::File,
                EntryKind::Blocked => EntryClass::Other,
            };
            entry_of(entry.name, kind)
        })
        .collect();
    shaped.sort_by_cached_key(|entry| {
        (
            entry.kind != EntryClass::Directory,
            entry.name.to_lowercase(),
            entry.name.clone(),
        )
    });
    shaped
}

fn entry_of(name: OsString, kind: EntryClass) -> WorkspaceEntry {
    match name.into_string() {
        Ok(name) => WorkspaceEntry {
            name,
            kind,
            lossy: false,
        },
        Err(native) => WorkspaceEntry {
            name: native.to_string_lossy().into_owned(),
            kind,
            lossy: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn components(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn invalid(names: &[&str]) -> bool {
        matches!(
            validate_components(&components(names)),
            Err(EntriesError::InvalidPath(_))
        )
    }

    #[test]
    fn accepts_the_workspace_itself_and_ordinary_nested_names() {
        assert!(validate_components(&[]).expect("root").is_empty());
        let nested =
            validate_components(&components(&["src", "main", "caf\u{e9} dir"])).expect("nested");
        assert_eq!(
            nested.to_path_buf(),
            PathBuf::from("src/main/caf\u{e9} dir")
        );
    }

    #[test]
    fn refuses_empty_dot_traversal_separator_and_nul_components() {
        for bad in [
            &[""][..],
            &["."],
            &[".."],
            &["src", ".."],
            &["a/b"],
            &["a\\b"],
            &["a\0b"],
            &["/etc"],
            &["src", ""],
        ] {
            assert!(invalid(bad), "{bad:?}");
        }
    }

    #[test]
    fn refuses_the_store_name_in_any_case_and_at_any_depth() {
        for bad in [
            &[".OMGCode"][..],
            &[".omgcode"],
            &["src", ".OMGCode"],
            &[".OMGCODE", "ledger"],
        ] {
            assert!(invalid(bad), "{bad:?}");
        }
    }

    #[test]
    fn bounds_the_depth_and_the_component_length() {
        let deep = vec!["d".to_owned(); MAX_DEPTH];
        assert!(validate_components(&deep).is_ok());
        let too_deep = vec!["d".to_owned(); MAX_DEPTH + 1];
        assert!(matches!(
            validate_components(&too_deep),
            Err(EntriesError::InvalidPath(_))
        ));
        let longest = vec!["n".repeat(MAX_COMPONENT_CHARS)];
        assert!(validate_components(&longest).is_ok());
        let too_long = vec!["n".repeat(MAX_COMPONENT_CHARS + 1)];
        assert!(matches!(
            validate_components(&too_long),
            Err(EntriesError::InvalidPath(_))
        ));
    }

    fn native(name: &str, kind: EntryKind) -> broker::DirectoryEntry {
        broker::DirectoryEntry {
            name: OsString::from(name),
            kind,
        }
    }

    #[test]
    fn sorts_directories_first_then_by_name_and_hides_the_store() {
        let shaped = shape(vec![
            native("zeta.txt", EntryKind::File),
            native(".OMGCode", EntryKind::Directory),
            native("beta", EntryKind::Directory),
            native("Alpha.txt", EntryKind::File),
            native("alpha", EntryKind::Directory),
            native("link", EntryKind::Blocked),
        ]);
        let order: Vec<(&str, EntryClass)> = shaped
            .iter()
            .map(|entry| (entry.name.as_str(), entry.kind))
            .collect();
        assert_eq!(
            order,
            [
                ("alpha", EntryClass::Directory),
                ("beta", EntryClass::Directory),
                ("Alpha.txt", EntryClass::File),
                ("link", EntryClass::Other),
                ("zeta.txt", EntryClass::File),
            ]
        );
        assert!(shaped.iter().all(|entry| !entry.lossy));
    }

    #[cfg(unix)]
    #[test]
    fn marks_a_name_that_is_not_unicode_as_lossy() {
        use std::os::unix::ffi::OsStringExt;
        let entry = entry_of(OsString::from_vec(vec![b'a', 0xff, b'b']), EntryClass::File);
        assert!(entry.lossy);
        assert_eq!(entry.name, "a\u{fffd}b");
    }
}
