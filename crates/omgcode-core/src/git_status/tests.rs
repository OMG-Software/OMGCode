use std::fs;
use std::path::Path;

use super::*;

const SIGNATURE: &str = "Test <test@example.invalid> 1700000000 +0000";

/// A repository with one commit holding `tracked.txt` and `staged.txt`, made
/// with `gix` alone so no Git program is needed.
fn repository(directory: &Path) {
    let repo = gix::ThreadSafeRepository::init_opts(
        directory,
        gix::create::Kind::WithWorktree,
        gix::create::Options::default(),
        gix::open::Options::isolated(),
    )
    .expect("init")
    .to_thread_local();
    let mut editor = repo.empty_tree().edit().expect("editor");
    for (name, text) in [("tracked.txt", "one\n"), ("staged.txt", "two\n")] {
        fs::write(directory.join(name), text).expect("file");
        let id = repo.write_blob(text.as_bytes()).expect("blob").detach();
        editor
            .upsert(name, gix::object::tree::EntryKind::Blob, id)
            .expect("upsert");
    }
    let tree = editor.write().expect("tree").detach();
    let signature = gix::actor::SignatureRef::from_bytes(SIGNATURE.as_bytes()).expect("sig");
    repo.commit_as(
        signature,
        signature,
        "HEAD",
        "first",
        tree,
        std::iter::empty::<gix::ObjectId>(),
    )
    .expect("commit");
    let index = repo.index_from_tree(&tree).expect("index");
    write_index(&repo, &index);
}

fn write_index(repo: &gix::Repository, index: &gix::index::File) {
    let mut file = fs::File::create(repo.index_path()).expect("index file");
    index
        .write_to(&mut file, gix::index::write::Options::default())
        .expect("write index");
}

fn changes(directory: &Path) -> (Vec<ChangedPath>, bool) {
    match status_of_directory(directory) {
        WorkspaceStatus::Changes { paths, truncated } => (paths, truncated),
        other => panic!("{other:?}"),
    }
}

fn find<'a>(paths: &'a [ChangedPath], name: &str) -> &'a ChangedPath {
    paths
        .iter()
        .find(|entry| entry.path == name)
        .unwrap_or_else(|| panic!("{name} in {paths:?}"))
}

#[test]
fn a_clean_repository_has_no_changes() {
    let temp = tempfile::tempdir().expect("temp");
    repository(temp.path());
    assert_eq!(changes(temp.path()), (Vec::new(), false));
}

#[test]
fn reports_worktree_and_untracked_changes_and_collapses_untracked_folders() {
    let temp = tempfile::tempdir().expect("temp");
    repository(temp.path());
    fs::write(temp.path().join("tracked.txt"), "changed and longer\n").expect("modify");
    fs::remove_file(temp.path().join("staged.txt")).expect("delete");
    fs::write(temp.path().join("loose.txt"), "x").expect("untracked");
    fs::create_dir_all(temp.path().join("newdir/inner")).expect("dir");
    fs::write(temp.path().join("newdir/inner/a.txt"), "a").expect("a");
    fs::write(temp.path().join("newdir/b.txt"), "b").expect("b");

    let (paths, truncated) = changes(temp.path());
    assert!(!truncated);
    assert_eq!(
        find(&paths, "tracked.txt").worktree,
        Some(WorktreeChange::Modified)
    );
    assert_eq!(
        find(&paths, "staged.txt").worktree,
        Some(WorktreeChange::Deleted)
    );
    assert_eq!(
        find(&paths, "loose.txt").worktree,
        Some(WorktreeChange::Untracked)
    );
    // One entry for the whole untracked folder, as `git status` shows it.
    assert_eq!(
        find(&paths, "newdir/").worktree,
        Some(WorktreeChange::Untracked)
    );
    assert!(
        paths
            .iter()
            .all(|entry| !entry.path.starts_with("newdir/") || entry.path == "newdir/")
    );
    assert!(paths.windows(2).all(|pair| pair[0].path <= pair[1].path));
}

#[test]
fn reports_staged_changes_against_head() {
    let temp = tempfile::tempdir().expect("temp");
    repository(temp.path());
    // Stage a new file, a modification, and a deletion by writing an index
    // built from a second tree.
    let repo = gix::open_opts(temp.path(), open_options()).expect("open");
    let head = repo.head_tree_id().expect("head tree").detach();
    let mut editor = repo.edit_tree(head).expect("editor");
    for (name, text) in [
        (
            "added.txt",
            "fresh
",
        ),
        (
            "tracked.txt",
            "staged edit
",
        ),
    ] {
        let id = repo.write_blob(text.as_bytes()).expect("blob").detach();
        editor
            .upsert(name, gix::object::tree::EntryKind::Blob, id)
            .expect("upsert");
        fs::write(temp.path().join(name), text).expect("file");
    }
    editor.remove("staged.txt").expect("remove");
    fs::remove_file(temp.path().join("staged.txt")).expect("delete");
    let tree = editor.write().expect("tree").detach();
    let index = repo.index_from_tree(&tree).expect("index");
    write_index(&repo, &index);

    let (paths, _) = changes(temp.path());
    assert_eq!(find(&paths, "added.txt").index, Some(IndexChange::Added));
    assert_eq!(
        find(&paths, "tracked.txt").index,
        Some(IndexChange::Modified)
    );
    assert_eq!(find(&paths, "staged.txt").index, Some(IndexChange::Deleted));
}

#[test]
fn bounds_the_list_and_flags_truncation() {
    let temp = tempfile::tempdir().expect("temp");
    repository(temp.path());
    for number in 0..MAX_CHANGED_PATHS + 5 {
        fs::write(temp.path().join(format!("f{number:05}.txt")), "x").expect("file");
    }
    let (paths, truncated) = changes(temp.path());
    assert!(truncated);
    assert_eq!(paths.len(), MAX_CHANGED_PATHS);
}

#[test]
fn a_folder_without_a_repository_is_not_an_error_state_for_the_status_run() {
    let temp = tempfile::tempdir().expect("temp");
    assert!(matches!(
        status_of_directory(temp.path()),
        WorkspaceStatus::Unavailable(_)
    ));
}

#[test]
fn a_malformed_repository_is_a_controlled_error() {
    let temp = tempfile::tempdir().expect("temp");
    fs::create_dir(temp.path().join(".git")).expect("git dir");
    fs::write(temp.path().join(".git/HEAD"), "ref: refs/heads/main\n").expect("head");
    assert!(matches!(
        status_of_directory(temp.path()),
        WorkspaceStatus::Unavailable(_)
    ));

    // A real repository with a corrupt index also fails without a panic.
    let other = tempfile::tempdir().expect("temp");
    repository(other.path());
    fs::write(other.path().join(".git/index"), b"not an index at all").expect("corrupt");
    assert!(matches!(
        status_of_directory(other.path()),
        WorkspaceStatus::Unavailable(_)
    ));
}

#[test]
fn refuses_a_core_worktree_that_moves_the_worktree() {
    let temp = tempfile::tempdir().expect("temp");
    repository(temp.path());
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let config = temp.path().join(".git/config");
    let mut text = fs::read_to_string(&config).expect("config");
    text.push_str(&format!(
        "[core]\n\tworktree = {}\n",
        elsewhere.path().display().to_string().replace('\\', "/")
    ));
    fs::write(&config, text).expect("config");
    // The local configuration is not honoured, so the worktree stays the
    // workspace and nothing is read from `elsewhere`.
    fs::write(elsewhere.path().join("secret.txt"), "x").expect("secret");
    let status = status_of_directory(temp.path());
    match status {
        WorkspaceStatus::Changes { paths, .. } => {
            assert!(paths.iter().all(|entry| entry.path != "secret.txt"));
        }
        WorkspaceStatus::Unavailable(_) => {}
        WorkspaceStatus::NotRepository => panic!("a repository"),
    }
}

/// The command a configured driver or hook would run: it creates `marker`.
/// `sh` is used on both platforms, as Git runs configured commands through it.
fn marker_command(marker: &Path) -> String {
    format!(
        "echo executed > '{}'",
        marker.display().to_string().replace('\\', "/")
    )
}

/// The key security test: configured commands never run.
#[test]
fn configured_filter_and_fsmonitor_commands_never_run() {
    let temp = tempfile::tempdir().expect("temp");
    let work = temp.path().join("work");
    fs::create_dir(&work).expect("work");
    repository(&work);
    let marker = temp.path().join("marker-executed");
    let command = marker_command(&marker);

    let config = work.join(".git/config");
    let mut text = fs::read_to_string(&config).expect("config");
    text.push_str(&format!(
        "[filter \"evil\"]\n\tclean = {command}\n\tsmudge = {command}\n\tprocess = {command}\n\trequired = true\n\
         [core]\n\tfsmonitor = {command}\n\thooksPath = {hooks}\n\
         [diff \"evil\"]\n\ttextconv = {command}\n\tcommand = {command}\n\
         [diff]\n\texternal = {command}\n",
        hooks = temp.path().join("hooks").display().to_string().replace('\\', "/"),
    ));
    fs::write(&config, text).expect("config");
    fs::write(work.join(".gitattributes"), "* filter=evil diff=evil\n").expect("attributes");
    fs::create_dir(temp.path().join("hooks")).expect("hooks");

    // Change content with a different size so every comparison must look at
    // the file, then add untracked files.
    fs::write(work.join("tracked.txt"), "a much longer change\n").expect("modify");
    fs::write(work.join("staged.txt"), "two\n").expect("same size rewrite");
    fs::write(work.join("added.txt"), "new\n").expect("add");

    let (paths, _) = changes(&work);
    assert_eq!(
        find(&paths, "tracked.txt").worktree,
        Some(WorktreeChange::Modified)
    );
    assert!(
        !marker.exists(),
        "a configured command ran during the status run"
    );
}
