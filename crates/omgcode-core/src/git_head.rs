//! The Git branch a registered workspace has checked out, read for display
//! only (ADR-0064).
//!
//! This runs no Git program and reads no Git configuration, hook, ref, or
//! object. It reads one small file, `HEAD`, through the anchored broker: the
//! development root and the workspace are opened by their recorded identities,
//! every read is relative to those handles, and no link is followed. Git
//! metadata is untrusted, so the result is bounded, validated, and only ever
//! shown to the operator. It is not model context and grants nothing.
//!
//! - A workspace whose `.git` is a directory has its branch read from
//!   `.git/HEAD`.
//! - A workspace whose `.git` is a file (a linked worktree or a submodule)
//!   names its Git directory with `gitdir: <path>`. That directory is read only
//!   when the path lies inside the development root; the read is then made
//!   from the root's handle. A path outside the root is not followed.
//!   Inside means by name: the pointer is compared lexically with the
//!   recorded (canonical) root, so a pointer that spells the root another way,
//!   such as an 8.3 short name, a junction, or a `subst` drive, is reported
//!   as unreadable. Resolving it would mean opening a path that repository
//!   content names, which could reach outside the root or a network share.
//! - A workspace folder that is not the top of a repository has no branch.

use std::io;
use std::path::{Component, Path, PathBuf};

use omgcode_platform::broker::{RelativePath, Workspace, is_missing_entry};

use crate::control::{ControlError, ControlStore};
use crate::policy::WorkspaceId;

/// The most bytes read from `HEAD` or from a `.git` pointer file.
pub const MAX_HEAD_BYTES: usize = 4096;
/// The longest branch name reported. Longer names are reported as unreadable.
pub const MAX_BRANCH_BYTES: usize = 255;
/// The abbreviated length of a detached commit identifier.
const ABBREVIATED_COMMIT: usize = 7;

/// What a workspace has checked out, as far as its `HEAD` file says.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceHead {
    /// A branch: `HEAD` names `refs/heads/<name>`, and this is `<name>`.
    Branch(String),
    /// A detached commit, abbreviated to seven hexadecimal digits.
    Detached(String),
    /// The workspace folder has no `.git` entry.
    NotRepository,
    /// A `.git` entry exists, but the branch cannot be shown. The text is a
    /// fixed reason, never file content.
    Unreadable(&'static str),
}

/// Read the branch of a registered workspace.
///
/// # Errors
///
/// The control errors of [`ControlStore`] for an unknown workspace, a missing
/// development root, or an unhealthy store. Every filesystem problem is a
/// [`WorkspaceHead::Unreadable`] or [`WorkspaceHead::NotRepository`] value
/// instead, because a branch is display text and its absence is not a fault.
pub fn workspace_head(
    control: &ControlStore,
    workspace_id: WorkspaceId,
) -> Result<WorkspaceHead, ControlError> {
    let scope = control.file_scope(workspace_id)?;
    let (Ok(root_path), Ok(workspace_path)) = (scope.root.live_path(), scope.workspace.live_path())
    else {
        return Ok(WorkspaceHead::Unreadable("the workspace path is invalid"));
    };
    let Some(relative) = workspace_path
        .strip_prefix(&root_path)
        .ok()
        .and_then(|path| RelativePath::new(path).ok())
    else {
        return Ok(WorkspaceHead::Unreadable(
            "the workspace is not beneath the development root",
        ));
    };
    let Ok(workspace) = Workspace::open(
        &root_path,
        scope.root.native_identity(),
        &relative,
        scope.workspace.native_identity(),
    ) else {
        return Ok(WorkspaceHead::Unreadable("the workspace cannot be opened"));
    };
    match workspace.creation_times() {
        Ok((root_created, workspace_created))
            if scope.root.matches_created(root_created)
                && scope.workspace.matches_created(workspace_created) => {}
        _ => return Ok(WorkspaceHead::Unreadable("the workspace folder changed")),
    }

    if let Ok(bytes) = read_small(&workspace, Path::new(".git/HEAD")) {
        return Ok(parse_head(&bytes));
    }
    let pointer = match read_small(&workspace, Path::new(".git")) {
        Ok(bytes) => bytes,
        Err(error) if is_missing_entry(&error) => {
            return Ok(WorkspaceHead::NotRepository);
        }
        Err(_) => return Ok(WorkspaceHead::Unreadable("the .git entry cannot be read")),
    };
    let Some(target) = gitdir_target(&pointer) else {
        return Ok(WorkspaceHead::Unreadable(
            "the .git file does not name a Git directory",
        ));
    };
    let absolute = if target.is_absolute() {
        target
    } else {
        workspace_path.join(target)
    };
    let Some(inside) = normalize(&absolute).and_then(|path| strip_root(&path, &root_path)) else {
        return Ok(WorkspaceHead::Unreadable(
            "the linked Git directory is outside the development root",
        ));
    };
    // Read from the root's own handle, so the linked directory is reached
    // without leaving the root or following a link.
    let Ok(empty) = RelativePath::new(Path::new("")) else {
        return Ok(WorkspaceHead::Unreadable(
            "the development root cannot be opened",
        ));
    };
    let identity = scope.root.native_identity();
    let Ok(root) = Workspace::open(&root_path, identity, &empty, identity) else {
        return Ok(WorkspaceHead::Unreadable(
            "the development root cannot be opened",
        ));
    };
    match root.creation_times() {
        Ok((created, _)) if scope.root.matches_created(created) => {}
        _ => return Ok(WorkspaceHead::Unreadable("the development root changed")),
    }
    match read_small(&root, &inside.join("HEAD")) {
        Ok(bytes) => Ok(parse_head(&bytes)),
        Err(_) => Ok(WorkspaceHead::Unreadable(
            "the linked Git directory cannot be read",
        )),
    }
}

fn read_small(workspace: &Workspace, path: &Path) -> io::Result<Vec<u8>> {
    workspace.read(&RelativePath::new(path)?, MAX_HEAD_BYTES)
}

/// Interpret the bytes of a `HEAD` file.
pub fn parse_head(bytes: &[u8]) -> WorkspaceHead {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return WorkspaceHead::Unreadable("HEAD is not text");
    };
    let line = text.strip_suffix('\n').unwrap_or(text);
    let line = line.strip_suffix('\r').unwrap_or(line);
    if let Some(reference) = line.strip_prefix("ref: ") {
        let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        if name.is_empty()
            || name.len() > MAX_BRANCH_BYTES
            || name.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return WorkspaceHead::Unreadable("HEAD names an unusual reference");
        }
        return WorkspaceHead::Branch(name.to_owned());
    }
    if (line.len() == 40 || line.len() == 64) && line.bytes().all(|b| b.is_ascii_hexdigit()) {
        return WorkspaceHead::Detached(line[..ABBREVIATED_COMMIT].to_ascii_lowercase());
    }
    WorkspaceHead::Unreadable("HEAD has an unknown form")
}

/// The path a `.git` pointer file names, if it has the `gitdir: ` form.
fn gitdir_target(bytes: &[u8]) -> Option<PathBuf> {
    let text = std::str::from_utf8(bytes).ok()?;
    let line = text.lines().next()?.trim_end_matches('\r');
    let target = line.strip_prefix("gitdir: ")?;
    if target.is_empty() || target.chars().any(char::is_control) {
        return None;
    }
    Some(PathBuf::from(target))
}

/// Resolve `.` and `..` by name only. The anchored read that follows decides
/// what is actually opened.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
}

/// `path` below `root` as a relative name, comparing components without case
/// on Windows, whose file systems here ignore it.
fn strip_root(path: &Path, root: &Path) -> Option<PathBuf> {
    let mut rest = path.components();
    for expected in root.components() {
        let actual = rest.next()?;
        if !same_component(actual, expected) {
            return None;
        }
    }
    let inside: PathBuf = rest.collect();
    if inside.as_os_str().is_empty() {
        return None;
    }
    Some(inside)
}

/// The recorded root may be an extended-length path (`\\?\C:\...`) while a
/// pointer is usually `C:/...`, so drive and share prefixes compare by their
/// parts, and names compare without ASCII case.
#[cfg(windows)]
fn same_component(actual: Component<'_>, expected: Component<'_>) -> bool {
    use std::path::Prefix;
    fn key(prefix: Prefix<'_>) -> Option<String> {
        match prefix {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                Some(format!("disk:{}", char::from(drive.to_ascii_uppercase())))
            }
            Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => Some(format!(
                "unc:{}/{}",
                server.to_string_lossy().to_ascii_lowercase(),
                share.to_string_lossy().to_ascii_lowercase()
            )),
            _ => None,
        }
    }
    match (actual, expected) {
        (Component::Prefix(a), Component::Prefix(b)) => {
            matches!((key(a.kind()), key(b.kind())), (Some(x), Some(y)) if x == y)
        }
        (Component::Prefix(_), _) | (_, Component::Prefix(_)) => false,
        _ => actual
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&expected.as_os_str().to_string_lossy()),
    }
}

#[cfg(not(windows))]
fn same_component(actual: Component<'_>, expected: Component<'_>) -> bool {
    actual == expected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_branch_a_detached_commit_and_refuses_other_forms() {
        assert_eq!(
            parse_head(b"ref: refs/heads/codex/ollama-provider\n"),
            WorkspaceHead::Branch("codex/ollama-provider".to_owned())
        );
        assert_eq!(
            parse_head(b"ref: refs/heads/main\r\n"),
            WorkspaceHead::Branch("main".to_owned())
        );
        assert_eq!(
            parse_head(b"ref: refs/remotes/origin/main"),
            WorkspaceHead::Branch("refs/remotes/origin/main".to_owned())
        );
        assert_eq!(
            parse_head(b"5BA2E8A0123456789abcdef0123456789abcdef0\n"),
            WorkspaceHead::Detached("5ba2e8a".to_owned())
        );
        for bad in [
            &b"ref: \n"[..],
            b"ref: refs/heads/a b",
            b"ref: refs/heads/\x1b[31mred",
            b"\xff\xfe",
            b"not a head",
            b"5ba2e8a",
        ] {
            assert!(
                matches!(parse_head(bad), WorkspaceHead::Unreadable(_)),
                "{bad:?}"
            );
        }
        let long = format!("ref: refs/heads/{}", "a".repeat(MAX_BRANCH_BYTES + 1));
        assert!(matches!(
            parse_head(long.as_bytes()),
            WorkspaceHead::Unreadable(_)
        ));
    }

    #[test]
    fn reads_only_a_gitdir_pointer_and_resolves_names_lexically() {
        assert_eq!(
            gitdir_target(b"gitdir: ../.git/worktrees/x\n"),
            Some(PathBuf::from("../.git/worktrees/x"))
        );
        assert_eq!(gitdir_target(b"something else"), None);
        assert_eq!(gitdir_target(b"gitdir: "), None);
        let root = std::env::temp_dir();
        let inside = root.join("repo").join("sub").join("..").join(".git");
        assert_eq!(
            normalize(&inside).and_then(|path| strip_root(&path, &root)),
            Some(PathBuf::from("repo").join(".git"))
        );
        let escape = root.join("..").join("elsewhere");
        assert_eq!(
            normalize(&escape).and_then(|path| strip_root(&path, &root)),
            None
        );
        assert_eq!(strip_root(&root, &root), None);
    }
}
