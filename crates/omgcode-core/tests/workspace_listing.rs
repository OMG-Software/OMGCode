//! Read-only workspace listing for an open control store.

use std::fs;

use omgcode_core::control::ControlStore;
use tempfile::TempDir;

fn store_with(names: &[&str]) -> (TempDir, ControlStore, Vec<std::path::PathBuf>) {
    let temporary = TempDir::new().expect("private temporary directory");
    let data = temporary.path().join("application-data");
    let root = temporary.path().join("development-root");
    fs::create_dir(&root).expect("create development root");
    let mut control = ControlStore::open(&data).expect("open control store");
    control
        .set_development_root(&root, control.revision())
        .expect("select development root");
    let mut directories = Vec::new();
    for name in names {
        let directory = root.join(name);
        fs::create_dir(&directory).expect("create workspace");
        control
            .register_workspace(&directory, control.revision())
            .expect("register workspace");
        directories.push(directory);
    }
    (temporary, control, directories)
}

#[test]
fn an_empty_store_lists_no_workspaces() {
    let (_temporary, control, _) = store_with(&[]);
    assert_eq!(control.workspaces().count(), 0);
}

#[test]
fn workspaces_are_listed_in_registration_order_with_their_recorded_directory() {
    let (_temporary, control, directories) = store_with(&["zeta", "alpha", "mid"]);

    let listed: Vec<_> = control.workspaces().collect();

    assert_eq!(listed.len(), 3);
    for (registration, directory) in listed.iter().zip(&directories) {
        let recorded = registration.directory_path().expect("native path");
        assert_eq!(
            fs::canonicalize(&recorded).expect("recorded path exists"),
            fs::canonicalize(directory).expect("directory exists")
        );
        assert!(
            registration.directory_display().ends_with(
                directory
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("name")
            ),
            "{}",
            registration.directory_display()
        );
        assert_eq!(
            control.workspace(registration.id()).map(|found| found.id()),
            Some(registration.id())
        );
    }
    // The order is stable across calls.
    let first: Vec<_> = control
        .workspaces()
        .map(|workspace| workspace.id())
        .collect();
    let second: Vec<_> = control
        .workspaces()
        .map(|workspace| workspace.id())
        .collect();
    assert_eq!(first, second);
}

#[test]
fn directory_display_text_escapes_hidden_formatting_characters() {
    // NTFS and most Unix filesystems admit U+202E in a name.
    let (_temporary, control, _) = store_with(&["name\u{202e}gpj"]);

    let registration = control.workspaces().next().expect("one workspace");
    let display = registration.directory_display();

    assert!(display.contains("name\\u{202e}gpj"), "{display}");
    assert!(!display.contains('\u{202e}'));
    // The path used to reopen the directory is untouched.
    assert!(
        registration
            .directory_path()
            .expect("native path")
            .to_string_lossy()
            .contains('\u{202e}')
    );
}
