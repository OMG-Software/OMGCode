//! Reading a registered workspace's Git branch for display (ADR-0064).

use std::fs;
use std::path::{Path, PathBuf};

use omgcode_core::control::{ControlError, ControlStore};
use omgcode_core::git_head::{WorkspaceHead, workspace_head};
use omgcode_core::policy::WorkspaceId;
use tempfile::TempDir;
use uuid::Uuid;

struct Fixture {
    _temporary: TempDir,
    root: PathBuf,
    control: ControlStore,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("private temporary directory");
        let data = temporary.path().join("application-data");
        let root = temporary.path().join("development-root");
        fs::create_dir(&root).expect("create development root");
        let mut control = ControlStore::open(&data).expect("open control store");
        control
            .set_development_root(&root, control.revision())
            .expect("select development root");
        Self {
            _temporary: temporary,
            root,
            control,
        }
    }

    fn workspace(&mut self, name: &str) -> (PathBuf, WorkspaceId) {
        let folder = self.root.join(name);
        fs::create_dir_all(&folder).expect("create workspace");
        let registration = self
            .control
            .register_workspace(&folder, self.control.revision())
            .expect("register workspace");
        (folder, registration.id())
    }

    fn head(&self, id: WorkspaceId) -> WorkspaceHead {
        workspace_head(&self.control, id).expect("workspace head")
    }
}

/// `path` as Git writes an absolute `gitdir:` pointer: the canonical long
/// name, without the verbatim prefix, with forward slashes on Windows. The
/// core compares names only, so a short (8.3) spelling, such as a CI
/// runner's `TEMP` directory, would not match the recorded root.
fn git_spelling(path: &Path) -> String {
    let canonical = fs::canonicalize(path).expect("canonical path");
    let text = canonical.display().to_string();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    text.replace('\\', "/")
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, text).expect("write file");
}

#[test]
fn reads_the_branch_of_a_repository_workspace() {
    let mut fixture = Fixture::new();
    let (folder, id) = fixture.workspace("repo");
    write(
        &folder.join(".git/HEAD"),
        "ref: refs/heads/feature/status-bar\n",
    );
    assert_eq!(
        fixture.head(id),
        WorkspaceHead::Branch("feature/status-bar".to_owned())
    );
    write(
        &folder.join(".git/HEAD"),
        "0123456789abcdef0123456789abcdef01234567\n",
    );
    assert_eq!(
        fixture.head(id),
        WorkspaceHead::Detached("0123456".to_owned())
    );
}

#[test]
fn reports_a_folder_without_git_as_no_repository() {
    let mut fixture = Fixture::new();
    let (_folder, id) = fixture.workspace("plain");
    assert_eq!(fixture.head(id), WorkspaceHead::NotRepository);
}

#[test]
fn follows_a_linked_worktree_pointer_only_inside_the_development_root() {
    let mut fixture = Fixture::new();
    let main_git = fixture.root.join("main/.git");
    write(
        &main_git.join("worktrees/linked/HEAD"),
        "ref: refs/heads/codex/linked\n",
    );
    let (folder, id) = fixture.workspace("linked");
    // Git writes an absolute pointer for a worktree, with forward slashes on
    // Windows too.
    let target = main_git.join("worktrees/linked");
    write(
        &folder.join(".git"),
        &format!("gitdir: {}\n", git_spelling(&target)),
    );
    assert_eq!(
        fixture.head(id),
        WorkspaceHead::Branch("codex/linked".to_owned())
    );

    // A relative pointer, as a submodule has, resolves from the workspace.
    write(
        &folder.join(".git"),
        "gitdir: ../main/.git/worktrees/linked\n",
    );
    assert_eq!(
        fixture.head(id),
        WorkspaceHead::Branch("codex/linked".to_owned())
    );

    // A Git directory outside the development root is not read.
    let outside = tempfile::tempdir().expect("outside directory");
    write(&outside.path().join("HEAD"), "ref: refs/heads/secret\n");
    write(
        &folder.join(".git"),
        &format!("gitdir: {}\n", outside.path().display()),
    );
    assert!(matches!(fixture.head(id), WorkspaceHead::Unreadable(_)));
    write(&folder.join(".git"), "gitdir: ../../../escape\n");
    assert!(matches!(fixture.head(id), WorkspaceHead::Unreadable(_)));
    write(&folder.join(".git"), "not a pointer\n");
    assert!(matches!(fixture.head(id), WorkspaceHead::Unreadable(_)));
}

#[test]
fn refuses_an_oversized_or_hostile_head() {
    let mut fixture = Fixture::new();
    let (folder, id) = fixture.workspace("hostile");
    write(&folder.join(".git/HEAD"), &"x".repeat(64 * 1024));
    assert!(matches!(fixture.head(id), WorkspaceHead::Unreadable(_)));
    write(
        &folder.join(".git/HEAD"),
        "ref: refs/heads/\u{1b}[2Jcleared\n",
    );
    assert!(matches!(fixture.head(id), WorkspaceHead::Unreadable(_)));
}

#[test]
fn refuses_an_unknown_workspace() {
    let fixture = Fixture::new();
    let unknown: WorkspaceId = Uuid::new_v4().to_string().parse().expect("workspace id");
    assert!(matches!(
        workspace_head(&fixture.control, unknown),
        Err(ControlError::UnknownWorkspace)
    ));
}
