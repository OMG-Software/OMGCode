//! Settings changes from the desktop `settings` window (ADR-0059 phase S2,
//! ADR-0060). A scripted prompt stands in for the native dialogs: it returns
//! the folders and answers a test chooses and records what it was shown.

mod common;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use omgcode_app::TokenSource;
use omgcode_app::desktop::{
    Confirmation, DesktopConfig, DesktopError, DesktopService, FolderPurpose,
    PermissionsChangeRequest, SettingsOverview, SettingsPrompt, SettingsRevisionRequest,
    WorkspaceRevisionRequest,
};
use omgcode_core::control::ControlStore;
use omgcode_core::policy::PermissionSet;
use serde_json::json;

use common::{Fixture, read_tools, wire};

/// What the scripted prompt was asked.
#[derive(Debug, PartialEq)]
enum Asked {
    Folder(FolderPurpose, Option<PathBuf>),
    Confirm(Confirmation),
}

/// Returns scripted folders and answers in order. Running out of either
/// fails the test, so an unexpected dialog is caught.
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

fn revision(value: u64) -> SettingsRevisionRequest {
    wire(json!({ "revision": value }))
}

fn for_workspace(workspace_id: uuid::Uuid, value: u64) -> WorkspaceRevisionRequest {
    wire(json!({ "workspace_id": workspace_id, "revision": value }))
}

fn permissions(
    workspace_id: uuid::Uuid,
    value: u64,
    [model_tools, read, write, execute]: [bool; 4],
) -> PermissionsChangeRequest {
    wire(json!({
        "workspace_id": workspace_id,
        "revision": value,
        "permissions": {
            "model_tools": model_tools,
            "read": read,
            "write": write,
            "execute": execute,
            "task_network": false,
            "host_services": false,
        },
    }))
}

fn code(result: Result<SettingsOverview, DesktopError>) -> &'static str {
    result.expect_err("the change is refused").code()
}

fn current_revision(data: &Path) -> u64 {
    ControlStore::open_existing(data)
        .expect("open")
        .expect("store exists")
        .revision()
}

/// A folder beside the data root and the development root.
fn outside_folder(fixture: &Fixture, name: &str) -> PathBuf {
    let folder = fixture.data.parent().expect("temporary root").join(name);
    fs::create_dir_all(&folder).expect("outside folder");
    folder
}

#[test]
fn reading_the_overview_creates_an_empty_settings_store() {
    let fixture = Fixture::bare();
    assert!(
        ControlStore::open_existing(&fixture.data)
            .expect("open")
            .is_none()
    );

    let overview = service(&fixture.data)
        .settings_overview()
        .expect("overview");

    assert_eq!(overview.revision, 0);
    assert_eq!(overview.development_root, None);
    assert!(overview.workspaces.is_empty());
    let store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("the overview created the store");
    assert_eq!(store.revision(), 0);
    assert!(!store.has_development_root());
}

#[test]
fn choosing_a_root_after_confirmation_records_it() {
    let fixture = Fixture::bare();
    let service = service(&fixture.data);
    let prompt = ScriptedPrompt::new()
        .folder(Some(&fixture.development))
        .answer(true);

    let overview = service
        .settings_choose_root(revision(0), &prompt)
        .expect("root set");

    assert_eq!(overview.revision, 1);
    let root = overview.development_root.expect("a root");
    assert!(root.ends_with("development"), "{root}");
    let asked = prompt.asked();
    assert_eq!(
        asked[0],
        Asked::Folder(FolderPurpose::DevelopmentRoot, None)
    );
    let Asked::Confirm(confirmation) = &asked[1] else {
        panic!("a confirmation follows the folder: {asked:?}");
    };
    assert_eq!(confirmation.title, "Set the development root");
    assert!(
        confirmation.message.contains("development"),
        "{}",
        confirmation.message
    );
}

#[test]
fn a_closed_folder_dialog_or_a_declined_confirmation_changes_nothing() {
    let fixture = Fixture::bare();
    let service = service(&fixture.data);

    let closed = ScriptedPrompt::new().folder(None);
    assert_eq!(
        code(service.settings_choose_root(revision(0), &closed)),
        "cancelled"
    );
    // No confirmation is shown for a folder that was not chosen.
    assert_eq!(closed.asked().len(), 1);

    let declined = ScriptedPrompt::new()
        .folder(Some(&fixture.development))
        .answer(false);
    assert_eq!(
        code(service.settings_choose_root(revision(0), &declined)),
        "cancelled"
    );

    let store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("store");
    assert_eq!(store.revision(), 0);
    assert!(!store.has_development_root());
}

#[test]
fn a_stale_revision_is_refused_before_any_dialog() {
    let fixture = Fixture::new();
    let prompt = ScriptedPrompt::new();
    assert_eq!(
        code(service(&fixture.data).settings_choose_root(revision(0), &prompt)),
        "stale_revision"
    );
    assert!(prompt.asked().is_empty());
}

#[test]
fn a_change_made_while_the_operator_decides_is_not_overwritten() {
    let fixture = Fixture::bare();
    let service = service(&fixture.data);
    let other_root = outside_folder(&fixture, "other-root");
    let data = fixture.data.clone();
    let development = fixture.development.clone();
    // The trusted CLI sets a root while the confirmation is open.
    let prompt = ScriptedPrompt::new()
        .folder(Some(&other_root))
        .answer(true)
        .meanwhile(move || {
            let mut control = ControlStore::open(&data).expect("control");
            control
                .set_development_root(&development, control.revision())
                .expect("concurrent root");
        });

    assert_eq!(
        code(service.settings_choose_root(revision(0), &prompt)),
        "stale_revision"
    );
    let overview = service.settings_overview().expect("overview");
    assert_eq!(overview.revision, 1);
    assert!(
        overview
            .development_root
            .expect("root")
            .ends_with("development")
    );
}

#[test]
fn a_root_that_overlaps_application_storage_is_refused_by_core() {
    let fixture = Fixture::bare();
    let prompt = ScriptedPrompt::new()
        .folder(Some(fixture.data.parent().expect("temporary root")))
        .answer(true);
    let result = service(&fixture.data).settings_choose_root(revision(0), &prompt);
    let error = result.expect_err("refused");
    assert_eq!(error.code(), "control");
    assert!(
        error.message().contains("must not overlap"),
        "{}",
        error.message()
    );
    assert_eq!(current_revision(&fixture.data), 0);
}

#[test]
fn registering_needs_a_root_first_and_shows_no_dialog_without_one() {
    let fixture = Fixture::bare();
    let prompt = ScriptedPrompt::new();
    assert_eq!(
        code(service(&fixture.data).settings_register_workspace(revision(0), &prompt)),
        "no_development_root"
    );
    assert!(prompt.asked().is_empty());
}

#[test]
fn registering_starts_at_the_root_and_uses_the_default_preferences() {
    let fixture = Fixture::new();
    let project = fixture.development.join("project");
    fs::create_dir_all(&project).expect("project");
    let prompt = ScriptedPrompt::new().folder(Some(&project)).answer(true);

    let overview = service(&fixture.data)
        .settings_register_workspace(revision(1), &prompt)
        .expect("registered");

    assert_eq!(overview.revision, 2);
    assert_eq!(overview.workspaces.len(), 1);
    let workspace = &overview.workspaces[0];
    assert!(workspace.folder_display.ends_with("project"));
    let defaults = PermissionSet::default();
    assert_eq!(
        [
            workspace.model_tools,
            workspace.read,
            workspace.write,
            workspace.execute,
            workspace.task_network,
            workspace.host_services,
        ],
        [
            defaults.model_tools,
            defaults.read,
            defaults.write,
            defaults.execute,
            defaults.task_network,
            defaults.host_services,
        ]
    );
    let asked = prompt.asked();
    let Asked::Folder(purpose, Some(start)) = &asked[0] else {
        panic!("the folder dialog starts at the root: {asked:?}");
    };
    assert_eq!(*purpose, FolderPurpose::Workspace);
    assert!(start.ends_with("development"), "{}", start.display());
    let Asked::Confirm(confirmation) = &asked[1] else {
        panic!("a confirmation follows: {asked:?}");
    };
    assert!(
        confirmation
            .message
            .contains("starts with read and write on"),
        "{}",
        confirmation.message
    );
}

#[test]
fn a_folder_outside_the_root_is_refused_by_core() {
    let fixture = Fixture::new();
    let outside = outside_folder(&fixture, "elsewhere");
    let prompt = ScriptedPrompt::new().folder(Some(&outside)).answer(true);
    assert_eq!(
        code(service(&fixture.data).settings_register_workspace(revision(1), &prompt)),
        "control"
    );
    assert_eq!(current_revision(&fixture.data), 1);
}

#[test]
fn turning_a_preference_on_needs_a_confirmation_that_names_it() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", read_tools());
    let service = service(&fixture.data);
    let start = current_revision(&fixture.data);

    // Declined: nothing changes.
    let declined = ScriptedPrompt::new().answer(false);
    assert_eq!(
        code(service.settings_set_permissions(
            permissions(workspace.id, start, [true, true, false, true]),
            &declined,
        )),
        "cancelled"
    );
    assert_eq!(current_revision(&fixture.data), start);

    // Confirmed: execute is on, and the confirmation named only execute.
    let confirmed = ScriptedPrompt::new().answer(true);
    let overview = service
        .settings_set_permissions(
            permissions(workspace.id, start, [true, true, false, true]),
            &confirmed,
        )
        .expect("execute on");
    assert_eq!(overview.revision, start + 1);
    assert!(overview.workspaces[0].execute);
    let shown = confirmed.confirmations();
    assert_eq!(shown.len(), 1);
    assert!(
        shown[0]
            .message
            .starts_with("Turn on execute for this workspace?"),
        "{}",
        shown[0].message
    );
    assert!(shown[0].message.contains("project"), "{}", shown[0].message);
}

#[test]
fn turning_preferences_off_applies_without_a_dialog() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", read_tools());
    let start = current_revision(&fixture.data);
    let prompt = ScriptedPrompt::new();

    let overview = service(&fixture.data)
        .settings_set_permissions(
            permissions(workspace.id, start, [false, false, false, false]),
            &prompt,
        )
        .expect("narrowed");

    assert!(prompt.asked().is_empty());
    let listed = &overview.workspaces[0];
    assert!(!listed.model_tools && !listed.read && !listed.write && !listed.execute);
}

/// A run journal that cannot be read never keeps access in place: the
/// narrowing is recorded, and the change reports that runs were not all
/// checked. Run watchers still stop withdrawn runs from the recorded settings
/// history (ADR-0061).
#[test]
fn an_unreadable_run_journal_still_records_the_narrowing_and_reports_it() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", read_tools());
    let start = current_revision(&fixture.data);
    fs::write(fixture.data.join("runs-v1.jsonl"), b"{\"sequence\":").expect("partial record");
    let prompt = ScriptedPrompt::new();

    let result = service(&fixture.data).settings_set_permissions(
        permissions(workspace.id, start, [false, false, false, false]),
        &prompt,
    );

    assert_eq!(code(result), "runs_not_stopped");
    assert!(prompt.asked().is_empty());
    assert_eq!(current_revision(&fixture.data), start + 1);
    let store = ControlStore::open_existing(&fixture.data)
        .expect("open settings")
        .expect("settings exist");
    let recorded = store
        .workspaces()
        .find(|registered| *registered.id().as_uuid() == workspace.id)
        .expect("registered")
        .permissions();
    assert!(!recorded.model_tools && !recorded.read && !recorded.write && !recorded.execute);
}

#[test]
fn an_unknown_workspace_is_refused_before_any_dialog() {
    let fixture = Fixture::new();
    let prompt = ScriptedPrompt::new();
    let unknown = uuid::Uuid::new_v4();
    assert_eq!(
        code(
            service(&fixture.data).settings_set_permissions(
                permissions(unknown, 1, [true, true, true, true]),
                &prompt,
            )
        ),
        "unknown_workspace"
    );
    assert_eq!(
        code(
            service(&fixture.data)
                .settings_choose_package_cache(for_workspace(unknown, 1), &prompt)
        ),
        "unknown_workspace"
    );
    assert!(prompt.asked().is_empty());
}

#[test]
fn a_package_cache_root_is_set_after_confirmation_and_cleared_without_one() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", read_tools());
    let cache = outside_folder(&fixture, "package-cache");
    let service = service(&fixture.data);
    let start = current_revision(&fixture.data);

    let prompt = ScriptedPrompt::new().folder(Some(&cache)).answer(true);
    let overview = service
        .settings_choose_package_cache(for_workspace(workspace.id, start), &prompt)
        .expect("cache set");
    let shown = overview.workspaces[0]
        .package_cache_root
        .clone()
        .expect("a cache root");
    assert!(shown.ends_with("package-cache"), "{shown}");
    assert_eq!(
        prompt.asked()[0],
        Asked::Folder(FolderPurpose::PackageCache, None)
    );

    let overview = service
        .settings_clear_package_cache(for_workspace(workspace.id, overview.revision))
        .expect("cache cleared");
    assert_eq!(overview.workspaces[0].package_cache_root, None);
    assert!(cache.exists(), "clearing leaves the folder in place");
}

#[test]
fn a_second_change_while_one_waits_for_the_operator_is_refused() {
    let fixture = Fixture::bare();
    let service = std::rc::Rc::new(service(&fixture.data));
    let inner = std::rc::Rc::clone(&service);
    let nested = RefCell::new(None);
    let outcome = std::rc::Rc::new(nested);
    let seen = std::rc::Rc::clone(&outcome);
    let prompt = ScriptedPrompt::new()
        .folder(Some(&fixture.development))
        .answer(false)
        .meanwhile(move || {
            let again = ScriptedPrompt::new();
            let result = inner.settings_choose_root(revision(0), &again);
            *seen.borrow_mut() = Some(result.expect_err("refused").code());
        });

    assert_eq!(
        code(service.settings_choose_root(revision(0), &prompt)),
        "cancelled"
    );
    assert_eq!(*outcome.borrow(), Some("settings_change_running"));
    // The flag is cleared once the first change ends.
    let next = ScriptedPrompt::new()
        .folder(Some(&fixture.development))
        .answer(true);
    assert!(service.settings_choose_root(revision(0), &next).is_ok());
}

#[test]
fn requests_carry_no_path_and_refuse_unknown_fields() {
    let with_path = json!({ "revision": 0, "path": "C:\\Windows" });
    assert!(serde_json::from_value::<SettingsRevisionRequest>(with_path).is_err());
    let missing_flag = json!({
        "workspace_id": uuid::Uuid::new_v4(),
        "revision": 0,
        "permissions": { "model_tools": true, "read": true, "write": true, "execute": true },
    });
    assert!(serde_json::from_value::<PermissionsChangeRequest>(missing_flag).is_err());
    let extra = json!({
        "workspace_id": uuid::Uuid::new_v4(),
        "revision": 0,
        "folder": "/etc",
    });
    assert!(serde_json::from_value::<WorkspaceRevisionRequest>(extra).is_err());
}
