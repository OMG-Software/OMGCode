//! Toolchain profile, profile assignment, package-network profile, and
//! container grant changes from the desktop `settings` window (ADR-0088,
//! ADR-0107). A scripted prompt stands in for the native dialogs, as in
//! `desktop_settings_skills`.
//!
//! The hand-made grant command is tested up to its confirmation on Windows,
//! and its platform refusal elsewhere. An accepted profile assignment applies
//! a real read-execute grant to a temporary toolchain folder on Windows, as
//! the CLI tests do; dropping the fixture revokes every recorded grant, which
//! also deletes the AppContainer profile.

mod common;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use omgcode_app::TokenSource;
use omgcode_app::desktop::{
    Confirmation, ContainerGrantRequest, DesktopConfig, DesktopError, DesktopService,
    FolderPurpose, PackageProfileAddRequest, ProfileAssignmentRequest, ProfileRemoveRequest,
    SettingsOverview, SettingsPrompt, ToolchainProfileAddRequest, WebHostChangeRequest,
};
use omgcode_core::control::{
    ContainerGrantAccess, ContainerGrantState, ControlStore, PackageProfileDeclaration,
};
use omgcode_core::turn::display_safe_line;
use serde_json::json;
use uuid::Uuid;

use common::{Fixture, wire};

#[derive(Debug, PartialEq)]
enum Asked {
    Folder(FolderPurpose, Option<PathBuf>),
    Confirm(Confirmation),
}

/// Returns scripted folders and answers in order. Running out of either fails
/// the test, so an unexpected dialog is caught.
#[derive(Default)]
struct ScriptedPrompt {
    folders: RefCell<VecDeque<Option<PathBuf>>>,
    answers: RefCell<VecDeque<bool>>,
    asked: RefCell<Vec<Asked>>,
    /// Runs while the confirmation is open, to change state meanwhile.
    during_confirm: RefCell<Option<Box<dyn FnOnce()>>>,
}

impl ScriptedPrompt {
    fn new() -> Self {
        Self::default()
    }

    fn folder(self, folder: Option<&Path>) -> Self {
        self.folders
            .borrow_mut()
            .push_back(folder.map(Path::to_path_buf));
        self
    }

    fn answer(self, confirmed: bool) -> Self {
        self.answers.borrow_mut().push_back(confirmed);
        self
    }

    fn meanwhile(self, action: impl FnOnce() + 'static) -> Self {
        *self.during_confirm.borrow_mut() = Some(Box::new(action));
        self
    }

    fn asked(&self) -> Vec<Asked> {
        self.asked.take()
    }

    fn confirmations(&self) -> Vec<Confirmation> {
        self.asked()
            .into_iter()
            .filter_map(|asked| match asked {
                Asked::Confirm(confirmation) => Some(confirmation),
                Asked::Folder(..) => None,
            })
            .collect()
    }
}

impl SettingsPrompt for ScriptedPrompt {
    fn pick_folder(&self, purpose: FolderPurpose, start: Option<&Path>) -> Option<PathBuf> {
        self.asked
            .borrow_mut()
            .push(Asked::Folder(purpose, start.map(Path::to_path_buf)));
        self.folders
            .borrow_mut()
            .pop_front()
            .expect("an unexpected folder dialog")
    }

    fn confirm(&self, confirmation: &Confirmation) -> bool {
        self.asked
            .borrow_mut()
            .push(Asked::Confirm(confirmation.clone()));
        if let Some(action) = self.during_confirm.borrow_mut().take() {
            action();
        }
        self.answers
            .borrow_mut()
            .pop_front()
            .expect("an unexpected confirmation")
    }
}

fn service(data: &Path) -> DesktopService {
    DesktopService::open(DesktopConfig {
        data_root: data.to_path_buf(),
        endpoint: "http://127.0.0.1:1234/v1".to_owned(),
        token_source: TokenSource::Environment,
        stream: false,
    })
    .expect("open desktop service")
}

fn current_revision(data: &Path) -> u64 {
    ControlStore::open_existing(data)
        .expect("open")
        .expect("store exists")
        .revision()
}

fn error_of(result: Result<SettingsOverview, DesktopError>) -> DesktopError {
    result.expect_err("the change is refused")
}

fn add_toolchain(
    revision: u64,
    name: &str,
    environment: &[(&str, &str)],
    roots: u8,
    paths: u8,
) -> ToolchainProfileAddRequest {
    wire(json!({
        "revision": revision,
        "name": name,
        "environment": environment
            .iter()
            .map(|(name, value)| json!({ "name": name, "value": value }))
            .collect::<Vec<_>>(),
        "root_count": roots,
        "path_entry_count": paths,
    }))
}

fn remove(revision: u64, name: &str) -> ProfileRemoveRequest {
    wire(json!({ "revision": revision, "name": name }))
}

fn add_package(revision: u64, name: &str, origins: &[&str]) -> PackageProfileAddRequest {
    wire(json!({ "revision": revision, "name": name, "origins": origins }))
}

/// A toolchain root outside the data root and the development root, with a
/// `bin` folder inside it.
fn tool_root(fixture: &Fixture, name: &str) -> PathBuf {
    let root = fixture
        .data
        .parent()
        .expect("temporary root")
        .join("toolchains")
        .join(name);
    fs::create_dir_all(root.join("bin")).expect("toolchain root");
    root
}

/// Register one workspace with core's primitive, which grants nothing (the
/// service's registration grants its folder on Windows, ADR-0107); returns
/// its identifier.
fn with_workspace(fixture: &Fixture) -> Uuid {
    let project = fixture.development.join("project");
    fs::create_dir_all(&project).expect("project");
    let mut store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("exists");
    let revision = store.revision();
    *store
        .register_workspace(&project, revision)
        .expect("registered")
        .id()
        .as_uuid()
}

fn assignment(revision: u64, workspace: Uuid, name: &str) -> ProfileAssignmentRequest {
    wire(json!({ "revision": revision, "workspace_id": workspace, "name": name }))
}

/// The recorded grants as `(access, state, directory display)`, in order.
fn recorded_grants(data: &Path) -> Vec<(ContainerGrantAccess, ContainerGrantState, String)> {
    ControlStore::open_existing(data)
        .expect("open")
        .expect("exists")
        .container_grants()
        .iter()
        .map(|status| {
            (
                status.grant().access(),
                status.state(),
                status.grant().directory_display(),
            )
        })
        .collect()
}

/// The display text core records for the directory at `path`: its canonical
/// path, made display-safe. Display text is compared, never reopened.
fn displayed(path: &Path) -> String {
    display_safe_line(
        &fs::canonicalize(path)
            .expect("canonical path")
            .to_string_lossy(),
    )
}

/// Record a toolchain profile through the service.
fn profile_added(fixture: &Fixture, name: &str) -> SettingsOverview {
    let root = tool_root(fixture, name);
    let prompt = ScriptedPrompt::new().folder(Some(&root)).answer(true);
    service(&fixture.data)
        .settings_add_toolchain_profile(
            add_toolchain(current_revision(&fixture.data), name, &[], 1, 0),
            &prompt,
        )
        .expect("profile added")
}

#[test]
fn a_profile_is_added_from_dialogs_and_one_confirmation_that_lists_everything() {
    let fixture = Fixture::new();
    let first = tool_root(&fixture, "first");
    let second = tool_root(&fixture, "second");
    let path_entry = first.join("bin");
    let prompt = ScriptedPrompt::new()
        .folder(Some(&first))
        .folder(Some(&second))
        .folder(Some(&path_entry))
        .answer(true);
    let before = current_revision(&fixture.data);

    let overview = service(&fixture.data)
        .settings_add_toolchain_profile(
            add_toolchain(
                before,
                "rust",
                &[("RUSTUP_HOME", "SECRET-LOOKING-VALUE")],
                2,
                1,
            ),
            &prompt,
        )
        .expect("added");

    let asked = prompt.asked();
    assert_eq!(asked.len(), 4);
    assert_eq!(asked[0], Asked::Folder(FolderPurpose::ToolchainRoot, None));
    assert_eq!(asked[1], Asked::Folder(FolderPurpose::ToolchainRoot, None));
    // The PATH dialog starts at the first root the operator chose.
    assert_eq!(
        asked[2],
        Asked::Folder(FolderPurpose::ToolchainPathEntry, Some(first.clone()))
    );
    let Asked::Confirm(confirmation) = &asked[3] else {
        panic!("the last dialog is the confirmation");
    };
    let message = &confirmation.message;
    for expected in [
        "rust",
        "first",
        "second",
        "bin",
        "RUSTUP_HOME",
        "plain text",
    ] {
        assert!(message.contains(expected), "{expected} in {message}");
    }
    // Values are never shown in a dialog.
    assert!(!message.contains("SECRET-LOOKING-VALUE"), "{message}");

    assert_eq!(overview.revision, before + 1);
    assert_eq!(overview.toolchain_profiles.len(), 1);
    let profile = &overview.toolchain_profiles[0];
    assert_eq!(profile.name, "rust");
    assert_eq!(profile.read_only_roots.len(), 2);
    assert_eq!(profile.path_entries.len(), 1);
    assert_eq!(profile.environment_names, ["RUSTUP_HOME"]);
    // The same record the CLI path makes.
    let store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("exists");
    assert!(store.profile("rust").is_some());
}

#[test]
fn a_closed_dialog_a_declined_confirmation_or_a_stale_revision_adds_nothing() {
    let fixture = Fixture::new();
    let first = tool_root(&fixture, "first");
    let before = current_revision(&fixture.data);

    // The operator closes the second root dialog: no confirmation is shown.
    let closed = ScriptedPrompt::new().folder(Some(&first)).folder(None);
    let error = error_of(
        service(&fixture.data)
            .settings_add_toolchain_profile(add_toolchain(before, "rust", &[], 2, 0), &closed),
    );
    assert_eq!(error.code(), "cancelled");
    assert!(closed.confirmations().is_empty());

    let declined = ScriptedPrompt::new().folder(Some(&first)).answer(false);
    let error = error_of(
        service(&fixture.data)
            .settings_add_toolchain_profile(add_toolchain(before, "rust", &[], 1, 0), &declined),
    );
    assert_eq!(error.code(), "cancelled");

    // A stale revision is refused before any dialog: the prompt has no script.
    let silent = ScriptedPrompt::new();
    let error = error_of(
        service(&fixture.data)
            .settings_add_toolchain_profile(add_toolchain(before + 3, "rust", &[], 1, 0), &silent),
    );
    assert_eq!(error.code(), "stale_revision");
    assert!(silent.asked().is_empty());
    assert_eq!(current_revision(&fixture.data), before);
}

#[test]
fn a_change_made_while_the_operator_confirms_a_profile_is_not_overwritten() {
    let fixture = Fixture::new();
    let root = tool_root(&fixture, "first");
    let before = current_revision(&fixture.data);
    let data = fixture.data.clone();
    let other = tool_root(&fixture, "other");
    let prompt = ScriptedPrompt::new()
        .folder(Some(&root))
        .answer(true)
        .meanwhile(move || {
            let mut store = ControlStore::open_existing(&data)
                .expect("open")
                .expect("exists");
            let revision = store.revision();
            store
                .add_profile(
                    &omgcode_core::control::ProfileDeclaration {
                        name: "intervening".into(),
                        read_only_roots: vec![other],
                        path_entries: Vec::new(),
                        environment: Vec::new(),
                    },
                    revision,
                )
                .expect("an intervening change");
        });

    let error = error_of(
        service(&fixture.data)
            .settings_add_toolchain_profile(add_toolchain(before, "rust", &[], 1, 0), &prompt),
    );

    assert_eq!(error.code(), "stale_revision");
    let store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("exists");
    assert!(store.profile("rust").is_none());
    assert!(store.profile("intervening").is_some());
}

#[test]
fn core_refusals_come_back_as_controlled_errors_and_record_nothing() {
    let fixture = Fixture::new();
    let root = tool_root(&fixture, "first");
    let before = current_revision(&fixture.data);

    // Counts outside their bounds are refused before any dialog.
    for (roots, paths) in [(0, 0), (17, 0), (1, 17)] {
        let silent = ScriptedPrompt::new();
        let error = error_of(service(&fixture.data).settings_add_toolchain_profile(
            add_toolchain(before, "rust", &[], roots, paths),
            &silent,
        ));
        assert_eq!(
            error.code(),
            "invalid_request",
            "{roots} roots, {paths} paths"
        );
        assert!(silent.asked().is_empty());
    }

    // Core's name rule.
    let prompt = ScriptedPrompt::new().folder(Some(&root)).answer(true);
    let error = error_of(
        service(&fixture.data)
            .settings_add_toolchain_profile(add_toolchain(before, "bad name", &[], 1, 0), &prompt),
    );
    assert_eq!(error.code(), "control");
    assert!(error.message().contains("profile names"), "{error}");

    // Core's reserved-variable rule.
    let prompt = ScriptedPrompt::new().folder(Some(&root)).answer(true);
    let error = error_of(service(&fixture.data).settings_add_toolchain_profile(
        add_toolchain(before, "rust", &[("PATH", "x")], 1, 0),
        &prompt,
    ));
    assert_eq!(error.code(), "control");
    assert!(error.message().contains("runner"), "{error}");

    // A PATH entry outside every root.
    let outside = tool_root(&fixture, "outside");
    let prompt = ScriptedPrompt::new()
        .folder(Some(&root))
        .folder(Some(&outside))
        .answer(true);
    let error = error_of(
        service(&fixture.data)
            .settings_add_toolchain_profile(add_toolchain(before, "rust", &[], 1, 1), &prompt),
    );
    assert_eq!(error.code(), "control");
    assert!(error.message().contains("PATH"), "{error}");

    // A root inside the development root.
    let inside = fixture.development.join("inside");
    fs::create_dir_all(&inside).expect("inside");
    let prompt = ScriptedPrompt::new().folder(Some(&inside)).answer(true);
    let error = error_of(
        service(&fixture.data)
            .settings_add_toolchain_profile(add_toolchain(before, "rust", &[], 1, 0), &prompt),
    );
    assert_eq!(error.code(), "control");

    // An existing name.
    let added = profile_added(&fixture, "taken");
    let prompt = ScriptedPrompt::new().folder(Some(&root)).answer(true);
    let error = error_of(service(&fixture.data).settings_add_toolchain_profile(
        add_toolchain(added.revision, "TAKEN", &[], 1, 0),
        &prompt,
    ));
    assert_eq!(error.code(), "control");
    assert!(error.message().contains("already exists"), "{error}");
    assert_eq!(current_revision(&fixture.data), added.revision);
}

#[test]
fn removing_a_profile_is_confirmed_by_name_and_applied_with_the_confirmed_revision() {
    let fixture = Fixture::new();
    let added = profile_added(&fixture, "rust");

    let declined = ScriptedPrompt::new().answer(false);
    let error = error_of(
        service(&fixture.data)
            .settings_remove_toolchain_profile(remove(added.revision, "rust"), &declined),
    );
    assert_eq!(error.code(), "cancelled");
    let message = declined.confirmations().remove(0).message;
    assert!(message.contains("rust"), "{message}");
    assert!(
        message.contains("not assigned to any workspace"),
        "{message}"
    );
    assert_eq!(current_revision(&fixture.data), added.revision);

    // Stale and unknown are refused before any dialog.
    let silent = ScriptedPrompt::new();
    let error = error_of(
        service(&fixture.data)
            .settings_remove_toolchain_profile(remove(added.revision + 1, "rust"), &silent),
    );
    assert_eq!(error.code(), "stale_revision");
    let error = error_of(
        service(&fixture.data)
            .settings_remove_toolchain_profile(remove(added.revision, "missing"), &silent),
    );
    assert_eq!(error.code(), "control");
    assert!(silent.asked().is_empty());

    let accepted = ScriptedPrompt::new().answer(true);
    let overview = service(&fixture.data)
        .settings_remove_toolchain_profile(remove(added.revision, "rust"), &accepted)
        .expect("removed");
    assert!(overview.toolchain_profiles.is_empty());
    assert_eq!(overview.revision, added.revision + 1);
}

#[test]
fn a_removal_is_refused_when_the_settings_change_during_the_confirmation() {
    let fixture = Fixture::new();
    let added = profile_added(&fixture, "rust");
    let data = fixture.data.clone();
    let other = tool_root(&fixture, "other");
    let prompt = ScriptedPrompt::new().answer(true).meanwhile(move || {
        let mut store = ControlStore::open_existing(&data)
            .expect("open")
            .expect("exists");
        let revision = store.revision();
        store
            .add_profile(
                &omgcode_core::control::ProfileDeclaration {
                    name: "intervening".into(),
                    read_only_roots: vec![other],
                    path_entries: Vec::new(),
                    environment: Vec::new(),
                },
                revision,
            )
            .expect("an intervening change");
    });
    let error = error_of(
        service(&fixture.data)
            .settings_remove_toolchain_profile(remove(added.revision, "rust"), &prompt),
    );
    assert_eq!(error.code(), "stale_revision");
    let store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("exists");
    assert!(store.profile("rust").is_some());
}

/// Assigning a profile is confirmed with the workspace, the profile, and on
/// Windows the folder its container is granted; the confirmation is the
/// consent to that grant (ADR-0107). On Windows the accepted assignment
/// applies a real read-execute grant to a temporary folder, which dropping
/// the fixture revokes.
#[test]
fn assigning_a_profile_is_confirmed_with_the_folders_it_grants() {
    let fixture = Fixture::new();
    let workspace = with_workspace(&fixture);
    let added = profile_added(&fixture, "rust");
    let shown_root = Path::new("toolchains").join("rust").display().to_string();
    let before = added.revision;

    let declined = ScriptedPrompt::new().answer(false);
    let error = error_of(
        service(&fixture.data)
            .settings_assign_toolchain_profile(assignment(before, workspace, "rust"), &declined),
    );
    assert_eq!(error.code(), "cancelled");
    let confirmation = declined.confirmations().remove(0);
    assert_eq!(confirmation.title, "Use a toolchain profile in a workspace");
    for part in ["rust", "project"] {
        assert!(
            confirmation.message.contains(part),
            "{part}: {}",
            confirmation.message
        );
    }
    for part in [
        shown_root.as_str(),
        "read and execute",
        "access-control list",
        "AppContainer",
    ] {
        assert_eq!(
            confirmation.message.contains(part),
            cfg!(windows),
            "{part}: {}",
            confirmation.message
        );
    }
    assert_eq!(current_revision(&fixture.data), before);
    assert!(recorded_grants(&fixture.data).is_empty());

    // A stale revision, an unknown workspace, and an unknown profile are
    // refused before any dialog.
    let silent = ScriptedPrompt::new();
    for (request, code) in [
        (assignment(before + 1, workspace, "rust"), "stale_revision"),
        (
            assignment(before, Uuid::new_v4(), "rust"),
            "unknown_workspace",
        ),
        (assignment(before, workspace, "missing"), "control"),
    ] {
        let error =
            error_of(service(&fixture.data).settings_assign_toolchain_profile(request, &silent));
        assert_eq!(error.code(), code, "{error}");
    }
    assert!(silent.asked().is_empty());

    let accepted = ScriptedPrompt::new().answer(true);
    let overview = service(&fixture.data)
        .settings_assign_toolchain_profile(assignment(before, workspace, "rust"), &accepted)
        .expect("assigned");
    assert_eq!(overview.workspaces[0].profiles, ["rust"]);
    if cfg!(windows) {
        assert_eq!(
            overview.revision,
            before + 2,
            "a grant, then the assignment"
        );
        assert_eq!(
            recorded_grants(&fixture.data),
            [(
                ContainerGrantAccess::ReadExecute,
                ContainerGrantState::Active,
                displayed(&tool_root(&fixture, "rust"))
            )]
        );
    } else {
        assert_eq!(overview.revision, before + 1);
        assert!(recorded_grants(&fixture.data).is_empty());
    }

    // Assigning it again is refused before any dialog.
    let error = error_of(service(&fixture.data).settings_assign_toolchain_profile(
        assignment(overview.revision, workspace, "rust"),
        &silent,
    ));
    assert_eq!(error.code(), "control");
    assert!(silent.asked().is_empty());
    fixture
        .revoke_grants()
        .expect("every container grant is revoked");
}

/// Unassigning revokes the grants only that profile needed and says so;
/// unassigning a profile that is not assigned is refused before any dialog.
#[test]
fn unassigning_a_profile_revokes_the_grants_it_alone_needed() {
    let fixture = Fixture::new();
    let workspace = with_workspace(&fixture);
    profile_added(&fixture, "rust");
    let shown_root = Path::new("toolchains").join("rust").display().to_string();
    let assigned = service(&fixture.data)
        .settings_assign_toolchain_profile(
            assignment(current_revision(&fixture.data), workspace, "rust"),
            &ScriptedPrompt::new().answer(true),
        )
        .expect("assigned");

    let declined = ScriptedPrompt::new().answer(false);
    let error = error_of(service(&fixture.data).settings_unassign_toolchain_profile(
        assignment(assigned.revision, workspace, "rust"),
        &declined,
    ));
    assert_eq!(error.code(), "cancelled");
    let confirmation = declined.confirmations().remove(0);
    for part in ["rust", "project", "stopped"] {
        assert!(
            confirmation.message.contains(part),
            "{part}: {}",
            confirmation.message
        );
    }
    for part in [shown_root.as_str(), "access-control list"] {
        assert_eq!(
            confirmation.message.contains(part),
            cfg!(windows),
            "{part}: {}",
            confirmation.message
        );
    }
    assert_eq!(current_revision(&fixture.data), assigned.revision);

    let overview = service(&fixture.data)
        .settings_unassign_toolchain_profile(
            assignment(assigned.revision, workspace, "rust"),
            &ScriptedPrompt::new().answer(true),
        )
        .expect("unassigned");
    assert!(overview.workspaces[0].profiles.is_empty());
    // A revocation needs no revision; the unassignment is one record.
    assert_eq!(overview.revision, assigned.revision + 1);
    assert!(recorded_grants(&fixture.data).is_empty());

    let silent = ScriptedPrompt::new();
    let error = error_of(service(&fixture.data).settings_unassign_toolchain_profile(
        assignment(overview.revision, workspace, "rust"),
        &silent,
    ));
    assert_eq!(error.code(), "control");
    assert!(error.message().contains("not assigned"), "{error}");
    assert!(silent.asked().is_empty());
    fixture
        .revoke_grants()
        .expect("every container grant is revoked");
}

/// Removing an assigned profile names the workspace it leaves and, on
/// Windows, the folder whose grant it revokes.
#[test]
fn removing_an_assigned_profile_names_its_workspace_and_revokes_its_grant() {
    let fixture = Fixture::new();
    let workspace = with_workspace(&fixture);
    profile_added(&fixture, "rust");
    let shown_root = Path::new("toolchains").join("rust").display().to_string();
    let assigned = service(&fixture.data)
        .settings_assign_toolchain_profile(
            assignment(current_revision(&fixture.data), workspace, "rust"),
            &ScriptedPrompt::new().answer(true),
        )
        .expect("assigned");

    let prompt = ScriptedPrompt::new().answer(true);
    let overview = service(&fixture.data)
        .settings_remove_toolchain_profile(remove(assigned.revision, "rust"), &prompt)
        .expect("removed");

    let message = prompt.confirmations().remove(0).message;
    assert!(message.contains("project"), "{message}");
    assert_eq!(
        message.contains(shown_root.as_str()),
        cfg!(windows),
        "{message}"
    );
    assert!(overview.toolchain_profiles.is_empty());
    assert!(overview.workspaces[0].profiles.is_empty());
    assert!(recorded_grants(&fixture.data).is_empty());
    fixture
        .revoke_grants()
        .expect("every container grant is revoked");
}

#[test]
fn a_package_profile_is_confirmed_with_the_origins_core_parsed() {
    let fixture = Fixture::new();
    let before = current_revision(&fixture.data);
    let declined = ScriptedPrompt::new().answer(false);
    let error = error_of(service(&fixture.data).settings_add_package_profile(
        add_package(before, "crates", &["https://Crates.IO/api"]),
        &declined,
    ));
    assert_eq!(error.code(), "cancelled");
    let message = declined.confirmations().remove(0).message;
    // The canonical form, with the explicit port, not the typed text.
    assert!(message.contains("https://crates.io:443/api/"), "{message}");
    assert!(message.contains("configuration only"), "{message}");
    assert!(message.contains("no network access"), "{message}");
    assert_eq!(current_revision(&fixture.data), before);

    let accepted = ScriptedPrompt::new().answer(true);
    let overview = service(&fixture.data)
        .settings_add_package_profile(
            add_package(before, "crates", &["https://Crates.IO/api"]),
            &accepted,
        )
        .expect("added");
    assert_eq!(overview.package_profiles.len(), 1);
    assert_eq!(overview.package_profiles[0].credentials_bound, 0);
    assert_eq!(overview.revision, before + 1);
}

#[test]
fn a_bad_package_profile_is_refused_before_any_dialog() {
    let fixture = Fixture::new();
    let before = current_revision(&fixture.data);
    let cases: [(&str, Vec<&str>, &str); 6] = [
        ("no-origins", vec![], "invalid_request"),
        (
            "too-many",
            vec![
                "https://a.example/",
                "https://b.example/",
                "https://c.example/",
                "https://d.example/",
                "https://e.example/",
                "https://f.example/",
                "https://g.example/",
                "https://h.example/",
                "https://i.example/",
            ],
            "invalid_request",
        ),
        (
            "credentials",
            vec!["https://user:pass@host.example/"],
            "control",
        ),
        ("scheme", vec!["ftp://host.example/"], "control"),
        ("query", vec!["https://host.example/?a=b"], "control"),
        ("empty", vec![""], "control"),
    ];
    for (name, origins, code) in cases {
        let silent = ScriptedPrompt::new();
        let error = error_of(
            service(&fixture.data)
                .settings_add_package_profile(add_package(before, name, &origins), &silent),
        );
        assert_eq!(error.code(), code, "{name}: {error}");
        assert!(silent.asked().is_empty(), "{name}");
    }
    assert_eq!(current_revision(&fixture.data), before);

    // A stale revision is refused before the dialog too.
    let silent = ScriptedPrompt::new();
    let error = error_of(service(&fixture.data).settings_add_package_profile(
        add_package(before + 1, "crates", &["https://crates.io/"]),
        &silent,
    ));
    assert_eq!(error.code(), "stale_revision");
    assert!(silent.asked().is_empty());
}

#[test]
fn a_package_profile_name_that_exists_is_refused_by_core_after_confirmation() {
    let fixture = Fixture::new();
    let mut store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("exists");
    let revision = store.revision();
    store
        .add_package_profile(
            &PackageProfileDeclaration {
                name: "crates".into(),
                origins: vec!["https://crates.io/".into()],
            },
            revision,
        )
        .expect("profile");
    drop(store);
    let prompt = ScriptedPrompt::new().answer(true);
    let error = error_of(service(&fixture.data).settings_add_package_profile(
        add_package(
            current_revision(&fixture.data),
            "CRATES",
            &["https://other.example/"],
        ),
        &prompt,
    ));
    assert_eq!(error.code(), "control");
    assert!(error.message().contains("already exists"), "{error}");
}

#[test]
fn removing_a_package_profile_says_no_credential_is_bound_and_removes_it() {
    let fixture = Fixture::new();
    let mut store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("exists");
    let revision = store.revision();
    store
        .add_package_profile(
            &PackageProfileDeclaration {
                name: "crates".into(),
                origins: vec!["https://crates.io/".into()],
            },
            revision,
        )
        .expect("profile");
    drop(store);
    let now = current_revision(&fixture.data);

    let declined = ScriptedPrompt::new().answer(false);
    let error = error_of(
        service(&fixture.data).settings_remove_package_profile(remove(now, "crates"), &declined),
    );
    assert_eq!(error.code(), "cancelled");
    let message = declined.confirmations().remove(0).message;
    assert!(message.contains("crates"), "{message}");
    assert!(
        message.contains("No registry credential is bound"),
        "{message}"
    );

    let silent = ScriptedPrompt::new();
    let error = error_of(
        service(&fixture.data).settings_remove_package_profile(remove(now, "missing"), &silent),
    );
    assert_eq!(error.code(), "control");
    let error = error_of(
        service(&fixture.data).settings_remove_package_profile(remove(now + 1, "crates"), &silent),
    );
    assert_eq!(error.code(), "stale_revision");
    assert!(silent.asked().is_empty());

    let accepted = ScriptedPrompt::new().answer(true);
    let overview = service(&fixture.data)
        .settings_remove_package_profile(remove(now, "crates"), &accepted)
        .expect("removed");
    assert!(overview.package_profiles.is_empty());
}

#[test]
fn a_request_carries_no_path_and_refuses_unknown_fields() {
    let with_path: Result<ToolchainProfileAddRequest, _> = serde_json::from_value(json!({
        "revision": 1, "name": "a", "environment": [], "root_count": 1,
        "path_entry_count": 0, "roots": ["/x"],
    }));
    assert!(with_path.is_err());
    let grant_path: Result<ContainerGrantRequest, _> = serde_json::from_value(json!({
        "revision": 1,
        "workspace_id": "6f1c1f0e-8f42-4c5b-9a55-0d2a3c4b5d6e",
        "access": "read_write",
        "path": "C:\\x",
    }));
    assert!(grant_path.is_err());
}

fn host_change(revision: u64, workspace: Uuid, pattern: &str) -> WebHostChangeRequest {
    wire(json!({ "revision": revision, "workspace_id": workspace, "pattern": pattern }))
}

fn hosts_of(overview: &SettingsOverview) -> Vec<&str> {
    overview.workspaces[0]
        .web_fetch_hosts
        .iter()
        .map(String::as_str)
        .collect()
}

/// Add `pattern` through the service with a confirmed dialog.
fn host_added(fixture: &Fixture, workspace: Uuid, pattern: &str) -> SettingsOverview {
    let prompt = ScriptedPrompt::new().answer(true);
    service(&fixture.data)
        .settings_add_web_fetch_host(
            host_change(current_revision(&fixture.data), workspace, pattern),
            &prompt,
        )
        .expect("host added")
}

#[test]
fn a_web_host_is_confirmed_with_the_canonical_pattern_and_the_folder() {
    let fixture = Fixture::new();
    let workspace = with_workspace(&fixture);
    let before = current_revision(&fixture.data);
    let prompt = ScriptedPrompt::new().answer(true);

    let overview = service(&fixture.data)
        .settings_add_web_fetch_host(host_change(before, workspace, "Docs.Example.COM."), &prompt)
        .expect("host added");

    assert_eq!(overview.revision, before + 1);
    assert_eq!(hosts_of(&overview), ["docs.example.com"]);
    let shown = prompt.confirmations();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].title, "Allow a web host");
    assert!(shown[0].message.contains("docs.example.com"));
    assert!(shown[0].message.contains("project"), "{}", shown[0].message);
    assert!(!shown[0].message.contains("Docs.Example"));
    assert!(!shown[0].message.contains("every subdomain"));
    // The recorded list is what the store holds.
    let store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("store");
    assert_eq!(store.revision(), before + 1);
}

#[test]
fn a_wildcard_host_confirmation_says_it_covers_every_subdomain() {
    let fixture = Fixture::new();
    let workspace = with_workspace(&fixture);
    let prompt = ScriptedPrompt::new().answer(true);
    let overview = service(&fixture.data)
        .settings_add_web_fetch_host(
            host_change(current_revision(&fixture.data), workspace, "*.example.com"),
            &prompt,
        )
        .expect("wildcard added");
    assert_eq!(hosts_of(&overview), ["*.example.com"]);
    let shown = prompt.confirmations();
    assert!(
        shown[0].message.contains(
            "*.example.com covers every subdomain of example.com, at any depth, but not example.com itself."
        ),
        "{}",
        shown[0].message
    );
}

#[test]
fn removing_a_web_host_is_confirmed_and_applied() {
    let fixture = Fixture::new();
    let workspace = with_workspace(&fixture);
    host_added(&fixture, workspace, "a.example.com");
    let overview = host_added(&fixture, workspace, "b.example.com");
    assert_eq!(hosts_of(&overview), ["a.example.com", "b.example.com"]);
    let before = current_revision(&fixture.data);

    let prompt = ScriptedPrompt::new().answer(true);
    let overview = service(&fixture.data)
        .settings_remove_web_fetch_host(host_change(before, workspace, "A.example.com"), &prompt)
        .expect("host removed");

    assert_eq!(hosts_of(&overview), ["b.example.com"]);
    assert_eq!(overview.revision, before + 1);
    let shown = prompt.confirmations();
    assert_eq!(shown[0].title, "Stop allowing a web host");
    assert!(shown[0].message.contains("a.example.com"));
    assert!(shown[0].message.contains("project"), "{}", shown[0].message);
}

#[test]
fn a_declined_web_host_change_records_nothing() {
    let fixture = Fixture::new();
    let workspace = with_workspace(&fixture);
    host_added(&fixture, workspace, "keep.example.com");
    let before = current_revision(&fixture.data);

    let declined_add = ScriptedPrompt::new().answer(false);
    let error = error_of(service(&fixture.data).settings_add_web_fetch_host(
        host_change(before, workspace, "new.example.com"),
        &declined_add,
    ));
    assert_eq!(error.code(), "cancelled");
    let declined_remove = ScriptedPrompt::new().answer(false);
    let error = error_of(service(&fixture.data).settings_remove_web_fetch_host(
        host_change(before, workspace, "keep.example.com"),
        &declined_remove,
    ));
    assert_eq!(error.code(), "cancelled");

    assert_eq!(current_revision(&fixture.data), before);
    let overview = service(&fixture.data)
        .settings_overview()
        .expect("overview");
    assert_eq!(hosts_of(&overview), ["keep.example.com"]);
}

#[test]
fn a_bad_web_host_request_is_refused_with_its_code_before_any_dialog() {
    let fixture = Fixture::new();
    let workspace = with_workspace(&fixture);
    host_added(&fixture, workspace, "listed.example.com");
    let before = current_revision(&fixture.data);
    let long = "a".repeat(300);
    let cases: [(&str, &str); 9] = [
        ("", "web_host_empty"),
        ("localhost", "web_host_reserved"),
        ("10.0.0.1", "web_host_ip_literal"),
        ("single", "web_host_too_few_labels"),
        ("bad_host.example.com", "web_host_bad_character"),
        ("-bad.example.com", "web_host_bad_label"),
        ("caf\u{e9}.example.com", "web_host_not_ascii"),
        (&long, "web_host_too_long"),
        ("*.*.example.com", "web_host_bad_character"),
    ];
    for (pattern, code) in cases {
        let silent = ScriptedPrompt::new();
        let error = error_of(
            service(&fixture.data)
                .settings_add_web_fetch_host(host_change(before, workspace, pattern), &silent),
        );
        assert_eq!(error.code(), code, "add {pattern:?}: {error}");
        assert!(silent.asked().is_empty(), "add {pattern:?}");
        let error = error_of(
            service(&fixture.data)
                .settings_remove_web_fetch_host(host_change(before, workspace, pattern), &silent),
        );
        assert_eq!(error.code(), code, "remove {pattern:?}: {error}");
        assert!(silent.asked().is_empty(), "remove {pattern:?}");
    }

    // A duplicate, an unlisted removal, and an unknown workspace.
    let silent = ScriptedPrompt::new();
    let error = error_of(service(&fixture.data).settings_add_web_fetch_host(
        host_change(before, workspace, "LISTED.example.com"),
        &silent,
    ));
    assert_eq!(error.code(), "web_host_duplicate");
    let error = error_of(service(&fixture.data).settings_remove_web_fetch_host(
        host_change(before, workspace, "other.example.com"),
        &silent,
    ));
    assert_eq!(error.code(), "web_host_not_listed");
    let error = error_of(service(&fixture.data).settings_add_web_fetch_host(
        host_change(before, Uuid::new_v4(), "new.example.com"),
        &silent,
    ));
    assert_eq!(error.code(), "unknown_workspace");
    assert!(silent.asked().is_empty());
    assert_eq!(current_revision(&fixture.data), before);
}

#[test]
fn a_web_host_beyond_the_hundredth_is_refused_before_any_dialog() {
    let fixture = Fixture::new();
    let workspace = with_workspace(&fixture);
    assert_eq!(omgcode_core::control::MAX_WEB_FETCH_HOSTS, 100);
    for index in 0..omgcode_core::control::MAX_WEB_FETCH_HOSTS {
        host_added(&fixture, workspace, &format!("h{index}.example.com"));
    }
    let before = current_revision(&fixture.data);
    let silent = ScriptedPrompt::new();
    let error =
        error_of(service(&fixture.data).settings_add_web_fetch_host(
            host_change(before, workspace, "extra.example.com"),
            &silent,
        ));
    assert_eq!(error.code(), "web_host_too_many");
    assert!(silent.asked().is_empty());
    assert_eq!(current_revision(&fixture.data), before);
}

#[test]
fn a_stale_web_host_revision_is_refused_before_and_after_the_dialog() {
    let fixture = Fixture::new();
    let workspace = with_workspace(&fixture);
    let before = current_revision(&fixture.data);

    let silent = ScriptedPrompt::new();
    let error =
        error_of(service(&fixture.data).settings_add_web_fetch_host(
            host_change(before + 1, workspace, "a.example.com"),
            &silent,
        ));
    assert_eq!(error.code(), "stale_revision");
    assert!(silent.asked().is_empty());

    // A change made while the confirmation is open is not overwritten.
    let data = fixture.data.clone();
    let prompt = ScriptedPrompt::new().answer(true).meanwhile(move || {
        let mut control = ControlStore::open(&data).expect("control");
        let id = control.workspaces().next().expect("a workspace").id();
        control
            .add_web_fetch_host(id, "other.example.com", control.revision())
            .expect("concurrent host");
    });
    let error = error_of(
        service(&fixture.data)
            .settings_add_web_fetch_host(host_change(before, workspace, "a.example.com"), &prompt),
    );
    assert_eq!(error.code(), "stale_revision");
    let overview = service(&fixture.data)
        .settings_overview()
        .expect("overview");
    assert_eq!(hosts_of(&overview), ["other.example.com"]);
}

#[test]
fn a_web_host_request_refuses_unknown_fields() {
    let with_path: Result<WebHostChangeRequest, _> = serde_json::from_value(json!({
        "revision": 1,
        "workspace_id": "6f1c1f0e-8f42-4c5b-9a55-0d2a3c4b5d6e",
        "pattern": "a.example.com",
        "folder": "C:\\x",
    }));
    assert!(with_path.is_err());
}

#[cfg(not(windows))]
mod unsupported {
    use super::*;
    use omgcode_app::desktop::ContainerRevokeRequest;

    #[test]
    fn container_grants_are_refused_before_any_dialog_outside_windows() {
        let fixture = Fixture::new();
        let workspace = with_workspace(&fixture);
        let now = current_revision(&fixture.data);
        let silent = ScriptedPrompt::new();

        let grant: ContainerGrantRequest = wire(json!({
            "revision": now, "workspace_id": workspace, "access": "read_execute",
        }));
        let error =
            error_of(service(&fixture.data).settings_grant_container_folder(grant, &silent));
        assert_eq!(error.code(), "unsupported_platform");

        let revoke: ContainerRevokeRequest = wire(json!({
            "revision": now, "grant_id": Uuid::new_v4(),
        }));
        let error =
            error_of(service(&fixture.data).settings_revoke_container_grant(revoke, &silent));
        assert_eq!(error.code(), "unsupported_platform");

        assert!(silent.asked().is_empty());
        assert_eq!(current_revision(&fixture.data), now);
        let overview = service(&fixture.data)
            .settings_overview()
            .expect("overview");
        assert!(!overview.container_grants_supported);
    }
}

#[cfg(windows)]
mod windows {
    use super::*;

    fn grant_request(revision: u64, workspace: Uuid, access: &str) -> ContainerGrantRequest {
        wire(json!({ "revision": revision, "workspace_id": workspace, "access": access }))
    }

    #[test]
    fn a_grant_is_confirmed_with_the_workspace_folder_and_access_and_declining_changes_nothing() {
        let fixture = Fixture::new();
        let workspace = with_workspace(&fixture);
        let now = current_revision(&fixture.data);
        let tools = tool_root(&fixture, "granted");
        let prompt = ScriptedPrompt::new().folder(Some(&tools)).answer(false);

        let error = error_of(service(&fixture.data).settings_grant_container_folder(
            grant_request(now, workspace, "read_execute"),
            &prompt,
        ));

        assert_eq!(error.code(), "cancelled");
        let asked = prompt.asked();
        assert_eq!(asked[0], Asked::Folder(FolderPurpose::ContainerGrant, None));
        let Asked::Confirm(confirmation) = &asked[1] else {
            panic!("the confirmation follows the folder");
        };
        let message = &confirmation.message;
        for expected in [
            "project",
            "granted",
            "read and execute",
            "access-control list",
            "AppContainer",
        ] {
            assert!(message.contains(expected), "{expected} in {message}");
        }
        assert_eq!(current_revision(&fixture.data), now);
        let store = ControlStore::open_existing(&fixture.data)
            .expect("open")
            .expect("exists");
        assert!(store.container_grants().is_empty());
    }

    #[test]
    fn a_read_write_grant_opens_no_folder_dialog_and_names_the_workspace_folder_once() {
        let fixture = Fixture::new();
        let workspace = with_workspace(&fixture);
        let now = current_revision(&fixture.data);
        // No folder is scripted: a folder dialog would fail the test.
        let prompt = ScriptedPrompt::new().answer(false);

        let error =
            error_of(service(&fixture.data).settings_grant_container_folder(
                grant_request(now, workspace, "read_write"),
                &prompt,
            ));

        assert_eq!(error.code(), "cancelled");
        let asked = prompt.asked();
        assert_eq!(asked.len(), 1, "{asked:?}");
        let Asked::Confirm(confirmation) = &asked[0] else {
            panic!("only the confirmation is shown");
        };
        let message = &confirmation.message;
        // The folder is shown once, as the workspace's; the temporary root's
        // name cannot end a line with the workspace's own name.
        let folder_lines = message
            .lines()
            .filter(|line| line.ends_with("project"))
            .count();
        assert_eq!(folder_lines, 1, "{message}");
        for expected in [
            "read and write that folder",
            "AppContainer",
            "sandbox",
            "access-control list",
        ] {
            assert!(message.contains(expected), "{expected} in {message}");
        }
        assert_eq!(current_revision(&fixture.data), now);
        let store = ControlStore::open_existing(&fixture.data)
            .expect("open")
            .expect("exists");
        assert!(store.container_grants().is_empty());
    }

    #[test]
    fn a_confirmed_read_write_grant_reaches_core_with_the_confirmed_revision() {
        let fixture = Fixture::new();
        let workspace = with_workspace(&fixture);
        let now = current_revision(&fixture.data);
        let other = tool_root(&fixture, "other");
        let data = fixture.data.clone();
        let prompt = ScriptedPrompt::new().answer(true).meanwhile(move || {
            let mut store = ControlStore::open_existing(&data)
                .expect("open")
                .expect("exists");
            let revision = store.revision();
            store
                .add_profile(
                    &omgcode_core::control::ProfileDeclaration {
                        name: "intervening".into(),
                        read_only_roots: vec![other],
                        path_entries: Vec::new(),
                        environment: Vec::new(),
                    },
                    revision,
                )
                .expect("an intervening change");
        });

        let error =
            error_of(service(&fixture.data).settings_grant_container_folder(
                grant_request(now, workspace, "read_write"),
                &prompt,
            ));

        // The grant went to core without a dialog, and core refused the
        // revision before it recorded or changed anything: no ACL changes here.
        assert_eq!(error.code(), "stale_revision");
        assert_eq!(prompt.confirmations().len(), 1);
        let store = ControlStore::open_existing(&fixture.data)
            .expect("open")
            .expect("exists");
        assert!(store.container_grants().is_empty());
    }

    #[test]
    fn a_closed_dialog_a_stale_revision_and_an_unknown_workspace_grant_nothing() {
        let fixture = Fixture::new();
        let workspace = with_workspace(&fixture);
        let now = current_revision(&fixture.data);

        let closed = ScriptedPrompt::new().folder(None);
        let error = error_of(service(&fixture.data).settings_grant_container_folder(
            grant_request(now, workspace, "read_execute"),
            &closed,
        ));
        assert_eq!(error.code(), "cancelled");
        assert!(closed.confirmations().is_empty());

        let silent = ScriptedPrompt::new();
        let error = error_of(service(&fixture.data).settings_grant_container_folder(
            grant_request(now + 1, workspace, "read_execute"),
            &silent,
        ));
        assert_eq!(error.code(), "stale_revision");
        let error = error_of(service(&fixture.data).settings_grant_container_folder(
            grant_request(now, Uuid::new_v4(), "read_execute"),
            &silent,
        ));
        assert_eq!(error.code(), "unknown_workspace");
        assert!(silent.asked().is_empty());
        assert_eq!(current_revision(&fixture.data), now);
    }

    #[test]
    fn a_change_made_while_the_operator_confirms_a_grant_stops_the_grant() {
        let fixture = Fixture::new();
        let workspace = with_workspace(&fixture);
        let now = current_revision(&fixture.data);
        let tools = tool_root(&fixture, "granted");
        let other = tool_root(&fixture, "other");
        let data = fixture.data.clone();
        let prompt = ScriptedPrompt::new()
            .folder(Some(&tools))
            .answer(true)
            .meanwhile(move || {
                let mut store = ControlStore::open_existing(&data)
                    .expect("open")
                    .expect("exists");
                let revision = store.revision();
                store
                    .add_profile(
                        &omgcode_core::control::ProfileDeclaration {
                            name: "intervening".into(),
                            read_only_roots: vec![other],
                            path_entries: Vec::new(),
                            environment: Vec::new(),
                        },
                        revision,
                    )
                    .expect("an intervening change");
            });

        let error = error_of(service(&fixture.data).settings_grant_container_folder(
            grant_request(now, workspace, "read_execute"),
            &prompt,
        ));

        // Core refuses the revision before it records or changes anything.
        assert_eq!(error.code(), "stale_revision");
        let store = ControlStore::open_existing(&fixture.data)
            .expect("open")
            .expect("exists");
        assert!(store.container_grants().is_empty());
    }

    #[test]
    fn revoking_an_unknown_or_stale_grant_is_refused_before_any_dialog() {
        let fixture = Fixture::new();
        let now = current_revision(&fixture.data);
        let silent = ScriptedPrompt::new();
        let error = error_of(service(&fixture.data).settings_revoke_container_grant(
            wire(json!({ "revision": now, "grant_id": Uuid::new_v4() })),
            &silent,
        ));
        assert_eq!(error.code(), "control");
        let error = error_of(service(&fixture.data).settings_revoke_container_grant(
            wire(json!({ "revision": now + 1, "grant_id": Uuid::new_v4() })),
            &silent,
        ));
        assert_eq!(error.code(), "stale_revision");
        assert!(silent.asked().is_empty());
    }
}
