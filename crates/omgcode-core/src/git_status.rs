//! The Git status of a registered workspace, read for display only
//! (ADR-0085).
//!
//! This reads the repository with the pure-Rust `gix` library and runs no
//! program. Git metadata is untrusted: a model with write access can edit
//! `.git/config` and `.gitattributes`, and a Git implementation that honours
//! `filter.<name>.clean`, `filter.<name>.process`, or `core.fsmonitor` would
//! run the program those keys name. Four controls keep that from happening:
//!
//! - The repository is opened with `gix`'s isolated permissions: no system,
//!   global, user, or environment configuration, no `include` or
//!   `includeIf`, and no Git installation is consulted.
//! - A section filter rejects every section that comes from the repository's
//!   own `.git/config` or worktree configuration. `gix` builds its filter
//!   drivers and diff drivers only from accepted sections, so no
//!   `filter.<name>.*` or `diff.<name>.*` command is ever known to the
//!   status run, and a `filter=<name>` attribute then names no driver.
//!   Built-in conversions (end-of-line, `text`, `ident`) stay in process.
//! - The `gix` crate is built without its `fsmonitor` feature, so nothing
//!   reads `core.fsmonitor`, and without any network, credential, or
//!   transport client feature.
//! - The status run neither writes the index nor any other file, checks
//!   out nothing, and ignores submodules.
//!
//! The scope is the registered workspace: the same anchored open as the
//! branch reader ([`crate::git_head`]), then a repository opened at exactly
//! that folder with no search of parent folders. A `.git` file (a linked
//! worktree or a submodule) is not followed, so the repository cannot lie
//! outside the workspace.
//!
//! Residual reads, none of which runs a program: the repository's own
//! `.git/config` is parsed for values such as `core.autocrlf`, and the
//! ignore, attributes, object, and index files the repository names are read.
//! A configured `core.excludesFile`, `core.attributesFile`, or
//! `objects/info/alternates` entry can name a file outside the workspace; its
//! content only changes which paths are listed, and it is never shown.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use gix::bstr::ByteSlice;
use gix::status::index_worktree::Item as WorktreeItem;

use crate::control::{ControlError, ControlStore, FileScope};
use crate::git_head::{MAX_HEAD_BYTES, OpenedScope, open_scope};
use crate::policy::WorkspaceId;
use omgcode_platform::broker::{RelativePath, is_missing_entry};

/// The most changed paths reported. More set `truncated`.
pub const MAX_CHANGED_PATHS: usize = 1000;
/// The longest time one status run may take before it is interrupted.
pub const STATUS_DEADLINE: Duration = Duration::from_secs(10);
/// The most renames the index-to-`HEAD` comparison considers.
const RENAME_LIMIT: usize = 100;

/// How a path differs between `HEAD` and the index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexChange {
    /// Present in the index, absent from `HEAD`.
    Added,
    /// Content or mode differs.
    Modified,
    /// Present in `HEAD`, absent from the index.
    Deleted,
    /// Moved from another path.
    Renamed,
    /// A file became a link or the reverse.
    TypeChange,
}

/// How a path differs between the index and the worktree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeChange {
    /// Content or mode differs, or an intent-to-add entry exists.
    Modified,
    /// Present in the index, absent from the worktree.
    Deleted,
    /// Not tracked. A folder is one entry.
    Untracked,
    /// An unresolved merge conflict.
    Conflicted,
}

/// One changed path. A path may differ in both comparisons.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChangedPath {
    /// The path relative to the workspace, with `/` separators and a trailing
    /// `/` for an untracked folder. Lossy for non-Unicode names, so it is for
    /// display only and must never be used as a path.
    pub path: String,
    /// The `HEAD`-to-index state, if any.
    pub index: Option<IndexChange>,
    /// The index-to-worktree state, if any.
    pub worktree: Option<WorktreeChange>,
}

/// The result of reading a workspace's status.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceStatus {
    /// The workspace folder has no `.git` entry.
    NotRepository,
    /// The repository could not be read. The text is fixed, never file
    /// content.
    Unavailable(&'static str),
    /// The changed paths, sorted by path, at most [`MAX_CHANGED_PATHS`].
    Changes {
        /// The changed paths.
        paths: Vec<ChangedPath>,
        /// Whether more paths changed than are listed.
        truncated: bool,
    },
}

/// A registered workspace's recorded folders, taken while the control store is
/// open so the status can be read after it is released.
///
/// A status run can take up to [`STATUS_DEADLINE`], and an open control store
/// is the settings lock, so holding it for the run would refuse a send or a
/// settings change as busy. [`workspace_status`] still opens the workspace by
/// the recorded native identities, so a replaced folder is refused. A
/// workspace unregistered between the two calls is read once more for display,
/// which grants nothing.
pub struct StatusScope(FileScope);

/// Take the scope of a registered workspace for [`workspace_status`].
///
/// # Errors
///
/// The control errors of [`ControlStore`] for an unknown workspace, a missing
/// development root, or an unhealthy store.
pub fn status_scope(
    control: &ControlStore,
    workspace_id: WorkspaceId,
) -> Result<StatusScope, ControlError> {
    control.file_scope(workspace_id).map(StatusScope)
}

/// Read the status of a workspace whose scope [`status_scope`] took. Call it
/// after the control store is released.
///
/// Every repository problem is a [`WorkspaceStatus::Unavailable`] or
/// [`WorkspaceStatus::NotRepository`] value.
#[must_use]
pub fn workspace_status(scope: &StatusScope) -> WorkspaceStatus {
    let OpenedScope {
        workspace_path,
        workspace,
        ..
    } = match open_scope(&scope.0) {
        Ok(opened) => opened,
        Err(reason) => return WorkspaceStatus::Unavailable(reason),
    };
    // `.git/HEAD` read through the anchored handle proves `.git` is a real
    // folder inside the workspace, not a link or a pointer file.
    let head = RelativePath::new(Path::new(".git/HEAD"))
        .and_then(|path| workspace.read(&path, MAX_HEAD_BYTES));
    match head {
        Ok(_) => {}
        Err(error) if is_missing_entry(&error) => {
            return match RelativePath::new(Path::new(".git"))
                .and_then(|path| workspace.read(&path, MAX_HEAD_BYTES))
            {
                Err(error) if is_missing_entry(&error) => WorkspaceStatus::NotRepository,
                _ => WorkspaceStatus::Unavailable(
                    "the .git entry is not a plain folder, so linked worktrees are not shown",
                ),
            };
        }
        Err(_) => {
            return WorkspaceStatus::Unavailable("the .git entry cannot be read");
        }
    }
    status_of_directory(&workspace_path)
}

/// The status of the repository whose worktree is exactly `directory`.
pub(crate) fn status_of_directory(directory: &Path) -> WorkspaceStatus {
    let Ok(repository) = gix::open_opts(directory, open_options()) else {
        return WorkspaceStatus::Unavailable("the repository cannot be read");
    };
    if !is_exactly(&repository, directory) {
        return WorkspaceStatus::Unavailable("the repository is not the workspace's own");
    }
    match collect(&repository) {
        Ok((paths, truncated)) => WorkspaceStatus::Changes { paths, truncated },
        Err(reason) => WorkspaceStatus::Unavailable(reason),
    }
}

/// Options that read nothing but the repository itself.
fn open_options() -> gix::open::Options {
    gix::open::Options::isolated().filter_config_section(reject_repository_config)
}

/// Accept no section from the repository's `.git/config` or worktree
/// configuration. Isolated permissions already remove every other file.
fn reject_repository_config(meta: &gix::config::file::Metadata) -> bool {
    !matches!(
        meta.source,
        gix::config::Source::Local | gix::config::Source::Worktree
    )
}

/// Whether `repository` is the plain `<directory>/.git` repository: not bare,
/// its worktree is `directory`, and its Git folder is `directory/.git`. A
/// `core.worktree` or a `gitdir:` pointer that moved either is refused.
fn is_exactly(repository: &gix::Repository, directory: &Path) -> bool {
    let Some(workdir) = repository.workdir() else {
        return false;
    };
    let (Ok(expected), Ok(actual)) = (
        std::fs::canonicalize(directory),
        std::fs::canonicalize(workdir),
    ) else {
        return false;
    };
    let (Ok(expected_git), Ok(actual_git)) = (
        std::fs::canonicalize(directory.join(".git")),
        std::fs::canonicalize(repository.git_dir()),
    ) else {
        return false;
    };
    expected == actual && expected_git == actual_git
}

/// Run the comparisons and merge them by path.
fn collect(repository: &gix::Repository) -> Result<(Vec<ChangedPath>, bool), &'static str> {
    let interrupt = Arc::new(AtomicBool::new(false));
    let (finished, waiting) = mpsc::channel::<()>();
    let watchdog = {
        let interrupt = Arc::clone(&interrupt);
        std::thread::Builder::new()
            .name("omgcode-git-status-deadline".to_owned())
            .spawn(move || {
                if waiting.recv_timeout(STATUS_DEADLINE) == Err(mpsc::RecvTimeoutError::Timeout) {
                    interrupt.store(true, Ordering::Release);
                }
            })
            .map_err(|_| "the status run cannot start")?
    };
    let outcome = run(repository, &interrupt);
    let timed_out = interrupt.load(Ordering::Acquire);
    drop(finished);
    let _ = watchdog.join();
    if timed_out {
        return Err("reading the status took too long");
    }
    outcome
}

fn run(
    repository: &gix::Repository,
    interrupt: &Arc<AtomicBool>,
) -> Result<(Vec<ChangedPath>, bool), &'static str> {
    let rewrites = gix::diff::Rewrites {
        limit: RENAME_LIMIT,
        ..gix::diff::Rewrites::default()
    };
    let platform = repository
        .status(gix::progress::Discard)
        .map_err(|_| "the status cannot be computed")?
        .index_worktree_submodules(None)
        .index_worktree_rewrites(None)
        .tree_index_track_renames(gix::status::tree_index::TrackRenames::Given(rewrites))
        .untracked_files(gix::status::UntrackedFiles::Collapsed)
        .should_interrupt_owned(Arc::clone(interrupt));
    let iterator = platform
        .into_iter(Vec::<gix::bstr::BString>::new())
        .map_err(|_| "the status cannot be computed")?;

    let mut merged: BTreeMap<Vec<u8>, (Option<IndexChange>, Option<WorktreeChange>)> =
        BTreeMap::new();
    let mut truncated = false;
    for item in iterator {
        let item = item.map_err(|_| "the status cannot be computed")?;
        let Some((path, index, worktree)) = classify(&item) else {
            continue;
        };
        if !merged.contains_key(&path) && merged.len() >= MAX_CHANGED_PATHS {
            truncated = true;
            break;
        }
        let entry = merged.entry(path).or_default();
        entry.0 = index.or(entry.0);
        entry.1 = worktree.or(entry.1);
    }
    let paths = merged
        .into_iter()
        .map(|(path, (index, worktree))| ChangedPath {
            path: String::from_utf8_lossy(&path).into_owned(),
            index,
            worktree,
        })
        .collect();
    Ok((paths, truncated))
}

type Classified = (Vec<u8>, Option<IndexChange>, Option<WorktreeChange>);

fn classify(item: &gix::status::Item) -> Option<Classified> {
    use gix::status::Item;
    match item {
        Item::TreeIndex(change) => {
            use gix::diff::index::ChangeRef as Change;
            let (path, state) = match change {
                Change::Addition { location, .. } => (location, IndexChange::Added),
                Change::Deletion { location, .. } => (location, IndexChange::Deleted),
                Change::Modification {
                    location,
                    previous_entry_mode,
                    entry_mode,
                    ..
                } => (
                    location,
                    if previous_entry_mode
                        .to_tree_entry_mode()
                        .map(|mode| mode.kind())
                        == entry_mode.to_tree_entry_mode().map(|mode| mode.kind())
                    {
                        IndexChange::Modified
                    } else {
                        IndexChange::TypeChange
                    },
                ),
                Change::Rewrite { location, .. } => (location, IndexChange::Renamed),
            };
            Some((path.as_bytes().to_vec(), Some(state), None))
        }
        Item::IndexWorktree(item) => {
            use gix::status::index_worktree::iter::Summary;
            let mut path = item.rela_path().as_bytes().to_vec();
            let state = match item.summary()? {
                Summary::Removed => WorktreeChange::Deleted,
                Summary::Added => {
                    if matches!(
                        item,
                        WorktreeItem::DirectoryContents { entry, .. }
                            if entry.disk_kind.is_some_and(|kind| kind != gix::dir::entry::Kind::File
                                && kind != gix::dir::entry::Kind::Symlink)
                    ) && path.last() != Some(&b'/')
                    {
                        path.push(b'/');
                    }
                    WorktreeChange::Untracked
                }
                Summary::Conflict => WorktreeChange::Conflicted,
                Summary::Modified
                | Summary::TypeChange
                | Summary::IntentToAdd
                | Summary::Renamed
                | Summary::Copied => WorktreeChange::Modified,
            };
            Some((path, None, Some(state)))
        }
    }
}

#[cfg(test)]
mod tests;
