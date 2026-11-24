//! The `main` window's file tree (ADR-0083): one directory of the open
//! session's registered workspace, names and kinds only, bounded, and without
//! the workspace's `read` preference.

mod common;

use std::fs;

use omgcode_app::create_session;
use omgcode_app::desktop::{DesktopService, WorkspaceListing};
use omgcode_core::control::ControlStore;
use omgcode_core::policy::WorkspaceId;
use omgcode_core::workspace_entries::{entries_scope, list_entries};
use serde_json::json;
use uuid::Uuid;

use common::{Fixture, Workspace, json_of, text_only, wire};

fn new_session(service: &DesktopService, workspace: &Workspace) -> Uuid {
    service
        .session_create(wire(json!({"workspace_id": workspace.id, "title": "tree"})))
        .expect("create session")
        .session_id
}

fn list(
    service: &DesktopService,
    session: Uuid,
    path: &[&str],
) -> Result<WorkspaceListing, omgcode_app::desktop::DesktopError> {
    service.workspace_entries(wire(json!({"session_id": session, "path": path})))
}

fn names(listing: &WorkspaceListing) -> Vec<(String, &'static str)> {
    json_of(listing)["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| {
            (
                entry["name"].as_str().expect("name").to_owned(),
                match entry["kind"].as_str().expect("kind") {
                    "directory" => "directory",
                    "file" => "file",
                    _ => "other",
                },
            )
        })
        .collect()
}

/// A workspace whose `read` preference is `deny` still shows its tree: the
/// view is the operator's own and is not a model action.
#[test]
fn lists_directories_first_then_by_name_without_the_read_preference() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    fs::create_dir_all(workspace.path.join("src").join("nested")).expect("src");
    fs::create_dir(workspace.path.join("Docs")).expect("docs");
    fs::write(workspace.path.join("beta.txt"), "secret contents").expect("file");
    fs::write(workspace.path.join("Alpha.txt"), "x").expect("file");
    fs::write(workspace.path.join("src").join("lib.rs"), "x").expect("file");
    let service = fixture.service("http://127.0.0.1:1234/v1");
    let session = new_session(&service, &workspace);

    let root = list(&service, session, &[]).expect("root");
    assert_eq!(
        names(&root),
        [
            ("Docs".to_owned(), "directory"),
            ("src".to_owned(), "directory"),
            ("Alpha.txt".to_owned(), "file"),
            ("beta.txt".to_owned(), "file"),
        ]
    );
    assert!(!root.truncated);
    // Names and kinds only: no size, no content.
    let text = serde_json::to_string(&root).expect("json");
    assert!(!text.contains("secret contents"));
    for entry in json_of(&root)["entries"].as_array().expect("entries") {
        let keys: Vec<&String> = entry.as_object().expect("object").keys().collect();
        assert_eq!(keys, ["kind", "lossy", "name", "openable"]);
        // Every directory here can be listed next; a file never can.
        assert_eq!(entry["openable"], entry["kind"] == "directory");
    }

    let nested = list(&service, session, &["src"]).expect("src");
    assert_eq!(
        names(&nested),
        [
            ("nested".to_owned(), "directory"),
            ("lib.rs".to_owned(), "file")
        ]
    );
    assert_eq!(
        names(&list(&service, session, &["src", "nested"]).expect("nested")),
        []
    );
}

/// The scope is taken while the control store, which is the settings lock, is
/// open. The listing itself needs no store, so it runs after the store is
/// released, even while another holder has the store open again.
#[test]
fn a_listing_needs_no_open_settings_store() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    fs::create_dir(workspace.path.join("src")).expect("src");
    let id: WorkspaceId = workspace.id.to_string().parse().expect("workspace id");
    let scope = {
        let control = ControlStore::open(&fixture.data).expect("control store");
        entries_scope(&control, id, &[]).expect("scope")
    };

    let _held = ControlStore::open(&fixture.data).expect("the lock is free again");
    let listing = list_entries(&scope).expect("listing");
    assert_eq!(names(&listing), [("src".to_owned(), "directory")]);
}

#[test]
fn the_checkpoint_store_is_hidden_and_refused() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    fs::create_dir(workspace.path.join(".OMGCode")).expect("store");
    fs::write(workspace.path.join("a.txt"), "x").expect("file");
    let service = fixture.service("http://127.0.0.1:1234/v1");
    let session = new_session(&service, &workspace);

    let root = list(&service, session, &[]).expect("root");
    assert_eq!(names(&root), [("a.txt".to_owned(), "file")]);
    for path in [&[".OMGCode"][..], &[".omgcode"], &["a", ".OMGCode"]] {
        let error = list(&service, session, path).expect_err("store refused");
        assert_eq!(error.code(), "invalid_request", "{path:?}");
    }
}

#[test]
fn unacceptable_components_are_refused_before_any_listing() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    fs::create_dir(workspace.path.join("src")).expect("src");
    let service = fixture.service("http://127.0.0.1:1234/v1");
    let session = new_session(&service, &workspace);

    let long = "n".repeat(256);
    let deep: Vec<&str> = vec!["d"; 33];
    for path in [
        &[".."][..],
        &["."],
        &[""],
        &["src", ".."],
        &["src/nested"],
        &["src\nested"],
        &["src\0"],
        &["C:"],
        &[long.as_str()],
        deep.as_slice(),
    ] {
        let error = list(&service, session, path).expect_err("refused");
        assert_eq!(error.code(), "invalid_request", "{path:?}");
    }
    // A directory that does not exist, and a file, are controlled failures.
    fs::write(workspace.path.join("file.txt"), "x").expect("file");
    for path in [&["missing"][..], &["file.txt"]] {
        let error = list(&service, session, path).expect_err("not listable");
        assert_eq!(error.code(), "invalid_request", "{path:?}");
        assert!(!error.message().contains("project"), "{}", error.message());
    }
}

#[test]
fn a_directory_over_the_bound_is_truncated_and_flagged() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let many = workspace.path.join("many");
    fs::create_dir(&many).expect("many");
    for index in 0..520 {
        fs::write(many.join(format!("f{index:04}.txt")), "").expect("file");
    }
    let service = fixture.service("http://127.0.0.1:1234/v1");
    let session = new_session(&service, &workspace);

    let listing = list(&service, session, &["many"]).expect("listing");
    assert_eq!(names(&listing).len(), 512);
    assert!(listing.truncated);
}

#[test]
fn an_unregistered_workspace_and_an_unknown_session_are_refused() {
    // A session recorded without a settings store has no registered workspace.
    let fixture = Fixture::bare();
    let directory = fixture.development.join("loose");
    fs::create_dir_all(&directory).expect("directory");
    fs::write(directory.join("a.txt"), "x").expect("file");
    let journal = create_session(&fixture.data, &directory, "loose").expect("session");
    let session = journal.snapshot().session_id;
    drop(journal);
    let service = fixture.service("http://127.0.0.1:1234/v1");

    let error = list(&service, session, &[]).expect_err("unregistered");
    assert_eq!(error.code(), "unknown_workspace");

    let error = list(&service, Uuid::new_v4(), &[]).expect_err("unknown session");
    assert_eq!(error.code(), "not_found");
}

#[test]
fn a_request_with_a_host_path_field_or_without_a_path_is_not_a_request() {
    let rejected = |value: serde_json::Value| {
        serde_json::from_value::<omgcode_app::desktop::WorkspaceEntriesRequest>(value).is_err()
    };
    let id = Uuid::new_v4();
    assert!(rejected(json!({"session_id": id})));
    assert!(rejected(
        json!({"session_id": id, "path": [], "root": "C:\\"})
    ));
    assert!(rejected(json!({"session_id": id, "path": "src"})));
}
