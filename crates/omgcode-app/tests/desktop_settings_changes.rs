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
use omgcode_core::control::{ContainerGrantAccess, ContainerGrantState, ControlStore};
use omgcode_core::policy::{Access, PermissionSet};
use serde_json::json;

use common::{
    Fixture, MockServer, Step, gate, read_tools, settings_json, text, text_only, wait_arrived,
    wait_finished, wire,
};

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

fn permissions(workspace_id: uuid::Uuid, value: u64, flags: [bool; 4]) -> PermissionsChangeRequest {
    permissions_with_web_fetch(workspace_id, value, flags, "deny")
}

fn permissions_with_web_fetch(
    workspace_id: uuid::Uuid,
    value: u64,
    [model_tools, read, write, execute]: [bool; 4],
    web_fetch: &str,
) -> PermissionsChangeRequest {
    wire(json!({
        "workspace_id": workspace_id,
        "revision": value,
        "permissions": {
            "model_tools": model_tools,
            "read": if read { "allow" } else { "deny" },
            "write": if write { "prompt" } else { "deny" },
            "execute": if execute { "prompt" } else { "deny" },
            "task_network": "deny",
            "host_services": "deny",
            "web_fetch": web_fetch,
            "write_tools": {
                "edit": "allow",
                "multi_edit": "allow",
                "create": "allow",
                "rename": "allow",
                "delete": "allow",
            },
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

    // On Windows the same consent grants the workspace's container its
    // folder, one more record (ADR-0107).
    assert_eq!(overview.revision, if cfg!(windows) { 3 } else { 2 });
    assert_eq!(overview.workspaces.len(), 1);
    let workspace = &overview.workspaces[0];
    assert!(workspace.folder_display.ends_with("project"));
    let defaults = PermissionSet::default();
    assert_eq!(workspace.model_tools, defaults.model_tools);
    assert_eq!(
        [
            workspace.read,
            workspace.write,
            workspace.execute,
            workspace.task_network,
            workspace.host_services,
        ],
        [
            defaults.read,
            defaults.write,
            defaults.execute,
            defaults.task_network,
            defaults.host_services,
        ]
    );
    assert_eq!(workspace.write_tools, defaults.write_tools);
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
            .contains("starts with read (allow) and write (prompt) on"),
        "{}",
        confirmation.message
    );
    for part in ["AppContainer", "read and write", "access-control list"] {
        assert_eq!(
            confirmation.message.contains(part),
            cfg!(windows),
            "{part}: {}",
            confirmation.message
        );
    }
    let store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("exists");
    let grants = store.container_grants();
    if cfg!(windows) {
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].grant().access(), ContainerGrantAccess::ReadWrite);
        assert_eq!(grants[0].state(), ContainerGrantState::Active);
        assert_eq!(
            *grants[0].grant().workspace_id().as_uuid(),
            workspace.workspace_id
        );
    } else {
        assert!(grants.is_empty());
    }
    drop(store);

    // Registering the folder again reuses the workspace and grants nothing
    // more, because its grant is recorded.
    let again = ScriptedPrompt::new().folder(Some(&project)).answer(true);
    let repeated = service(&fixture.data)
        .settings_register_workspace(revision(overview.revision), &again)
        .expect("registered again");
    assert_eq!(repeated.revision, overview.revision);
    assert_eq!(repeated.workspaces.len(), 1);
    fixture
        .revoke_grants()
        .expect("every container grant is revoked");
}

/// Unregistering revokes the workspace's grants first, so on Windows the one
/// registration made is gone with it, and the confirmation names its folder
/// (ADR-0107).
#[test]
fn unregistering_revokes_the_grant_registration_made() {
    let fixture = Fixture::new();
    let project = fixture.development.join("project");
    fs::create_dir_all(&project).expect("project");
    let registered = service(&fixture.data)
        .settings_register_workspace(
            revision(1),
            &ScriptedPrompt::new().folder(Some(&project)).answer(true),
        )
        .expect("registered");
    let workspace = registered.workspaces[0].workspace_id;

    let prompt = ScriptedPrompt::new().answer(true);
    let overview = service(&fixture.data)
        .settings_unregister_workspace(for_workspace(workspace, registered.revision), &prompt)
        .expect("unregistered");

    assert!(overview.workspaces.is_empty());
    let message = &prompt.confirmations()[0].message;
    assert_eq!(
        message.contains("access-control list"),
        cfg!(windows),
        "{message}"
    );
    // The folder's own line, and on Windows its grant's line; a parent
    // folder whose name contains `project` is not counted.
    assert_eq!(
        message
            .lines()
            .filter(|line| line.trim_end().ends_with("project"))
            .count(),
        if cfg!(windows) { 2 } else { 1 },
        "{message}"
    );
    let store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("exists");
    assert!(store.container_grants().is_empty());
    assert_eq!(store.workspaces().count(), 0);
    drop(store);
    fixture
        .revoke_grants()
        .expect("every container grant is revoked");
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
    assert_eq!(overview.workspaces[0].execute, Access::Prompt);
    let shown = confirmed.confirmations();
    assert_eq!(shown.len(), 1);
    assert!(
        shown[0]
            .message
            .starts_with("Widen execute for this workspace?"),
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
    assert!(
        !listed.model_tools
            && listed.read == Access::Deny
            && listed.write == Access::Deny
            && listed.execute == Access::Deny
    );
}

/// `web_fetch` is an explicit wire flag: a desktop permission change sets it,
/// and the workspace's host allowlist is untouched by it.
#[test]
fn a_desktop_permission_change_sets_web_fetch_and_keeps_the_hosts() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace(
        "project",
        PermissionSet {
            web_fetch: Access::Allow,
            ..read_tools()
        },
    );
    {
        let mut store = ControlStore::open(&fixture.data).expect("control store");
        let id = store
            .registered_workspace(workspace.id)
            .expect("registered")
            .id();
        let revision = store.revision();
        store
            .add_web_fetch_host(id, "docs.example.com", revision)
            .expect("host");
    }
    let start = current_revision(&fixture.data);
    let prompt = ScriptedPrompt::new();

    // Allow to deny narrows: no dialog, and the flag is the one sent.
    let overview = service(&fixture.data)
        .settings_set_permissions(
            permissions_with_web_fetch(workspace.id, start, [false, false, false, false], "deny"),
            &prompt,
        )
        .expect("narrowed");

    assert!(prompt.asked().is_empty());
    assert!(!overview.workspaces[0].model_tools);
    assert_eq!(overview.workspaces[0].web_fetch, Access::Deny);
    assert_eq!(overview.workspaces[0].web_fetch_hosts, ["docs.example.com"]);
    let store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("store exists");
    let registered = store
        .registered_workspace(workspace.id)
        .expect("registered");
    assert_eq!(registered.permissions().web_fetch, Access::Deny);
    let hosts = store.web_fetch_hosts(registered.id());
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].as_str(), "docs.example.com");
}

#[test]
fn raising_web_fetch_is_confirmed_and_names_what_it_permits() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", read_tools());
    let service = service(&fixture.data);
    let flags = [true, true, false, false];

    // Deny to prompt, then prompt to allow, each raises and is confirmed.
    for (wanted, was) in [("prompt", "deny"), ("allow", "prompt")] {
        let start = current_revision(&fixture.data);
        let declined = ScriptedPrompt::new().answer(false);
        assert_eq!(
            code(service.settings_set_permissions(
                permissions_with_web_fetch(workspace.id, start, flags, wanted),
                &declined,
            )),
            "cancelled"
        );
        assert_eq!(current_revision(&fixture.data), start);

        let confirmed = ScriptedPrompt::new().answer(true);
        let overview = service
            .settings_set_permissions(
                permissions_with_web_fetch(workspace.id, start, flags, wanted),
                &confirmed,
            )
            .expect("web fetch raised");
        assert_eq!(overview.revision, start + 1);
        let shown = confirmed.confirmations();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].title, "Allow more access");
        assert!(
            shown[0]
                .message
                .starts_with("Widen web fetch for this workspace?"),
            "{}",
            shown[0].message
        );
        assert!(
            shown[0].message.contains(&format!(
                "- web fetch: {was} to {wanted}. the model may fetch public HTTPS pages; under prompt each fetch asks you, and under allow pages on this workspace's allowed hosts are fetched without asking while other hosts still ask."
            )),
            "{}",
            shown[0].message
        );
    }
    let store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("store exists");
    assert_eq!(
        store
            .registered_workspace(workspace.id)
            .expect("registered")
            .permissions()
            .web_fetch,
        Access::Allow
    );
}

#[test]
fn raising_web_fetch_straight_from_deny_to_allow_is_confirmed() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", read_tools());
    let start = current_revision(&fixture.data);
    let confirmed = ScriptedPrompt::new().answer(true);

    service(&fixture.data)
        .settings_set_permissions(
            permissions_with_web_fetch(workspace.id, start, [true, true, false, false], "allow"),
            &confirmed,
        )
        .expect("deny to allow");

    let shown = confirmed.confirmations();
    assert_eq!(shown.len(), 1);
    assert!(
        shown[0].message.contains("- web fetch: deny to allow."),
        "{}",
        shown[0].message
    );
}

#[test]
fn narrowing_web_fetch_needs_no_dialog() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace(
        "project",
        PermissionSet {
            web_fetch: Access::Allow,
            ..read_tools()
        },
    );
    let start = current_revision(&fixture.data);
    let prompt = ScriptedPrompt::new();

    let overview = service(&fixture.data)
        .settings_set_permissions(
            permissions_with_web_fetch(workspace.id, start, [true, true, false, false], "prompt"),
            &prompt,
        )
        .expect("narrowed");

    assert!(prompt.asked().is_empty());
    assert_eq!(overview.workspaces[0].web_fetch, Access::Prompt);
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
    assert!(
        !recorded.model_tools
            && recorded.read == Access::Deny
            && recorded.write == Access::Deny
            && recorded.execute == Access::Deny
    );
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

/// Start one chat turn against `endpoint`.
fn send(
    service: &DesktopService,
    session: uuid::Uuid,
    endpoint: &str,
) -> Result<omgcode_app::desktop::ChatStatus, DesktopError> {
    service.chat_send(wire(json!({
        "session_id": session,
        "message": "hi",
        "settings": settings_json(endpoint),
        "skills": [],
    })))
}

fn instructions_file(fixture: &Fixture, workspace: uuid::Uuid) -> PathBuf {
    fixture
        .data
        .join("desktop-v1")
        .join("workspace-instructions")
        .join(format!("{workspace}.json"))
}

/// The confirmation names the folder and what is removed, one event removes
/// the registration and the saved instructions, the folder and the session
/// stay, and a later turn in the session chats without tools and without the
/// workspace text (ADR-0081).
#[test]
fn unregistering_confirms_removes_the_instructions_and_keeps_the_sessions() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", read_tools());
    let server = MockServer::start(vec![Step::reply(text("still here"))]);
    let service = fixture.service(&server.endpoint);
    let session = service
        .session_create(wire(json!({"workspace_id": workspace.id, "title": "kept"})))
        .expect("session")
        .session_id;
    service
        .settings_set_workspace_instructions(wire(json!({
            "workspace_id": workspace.id,
            "instructions": "Use tabs."
        })))
        .expect("instructions");
    let file = instructions_file(&fixture, workspace.id);
    assert!(file.exists());
    let start = current_revision(&fixture.data);

    let prompt = ScriptedPrompt::new().answer(true);
    let overview = service
        .settings_unregister_workspace(for_workspace(workspace.id, start), &prompt)
        .expect("unregistered");

    assert!(overview.workspaces.is_empty());
    assert_eq!(overview.revision, start + 1);
    let confirmations = prompt.confirmations();
    assert_eq!(confirmations.len(), 1);
    let message = &confirmations[0].message;
    assert!(message.contains("project"), "names the folder: {message}");
    for part in [
        "preferences",
        "instructions",
        "package cache",
        "skill overrides",
        "not touched",
        "without tools",
    ] {
        assert!(message.contains(part), "{part}: {message}");
    }
    assert!(!file.exists(), "the instructions file is deleted");
    assert!(workspace.path.is_dir(), "the folder stays");

    // The session stays listed, and a turn in it is text-only, with none of
    // the workspace text even if a file reappears (a concurrent save).
    let tree = service
        .session_tree(wire(
            json!({"view": "active", "sort": "updated", "descending": true}),
        ))
        .expect("tree");
    assert_eq!(tree.indexed_sessions, 1);
    fs::create_dir_all(file.parent().expect("parent")).expect("directory");
    fs::write(&file, br#"{"version":1,"instructions":"Use tabs."}"#).expect("stray file");
    send(&service, session, &server.endpoint).expect("send");
    assert_eq!(
        wait_finished(&service, session).state,
        omgcode_app::desktop::ChatState::Completed
    );
    let (_, requests) = server.finish_recorded();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].body.contains("Use tabs."));
    assert!(!requests[0].body.contains("workspace_read_file"));
}

/// An instructions file that cannot be deleted does not skip the run check:
/// the removal is recorded, the withdrawn runs are still listed and waited
/// for, and a run problem is reported ahead of the remaining file, which no
/// turn sends. A folder in the file's place makes the deletion fail, and an
/// unreadable run journal makes the run check fail.
#[test]
fn a_failed_instructions_deletion_still_checks_the_withdrawn_runs() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", read_tools());
    let service = service(&fixture.data);
    let file = instructions_file(&fixture, workspace.id);
    fs::create_dir_all(&file).expect("a folder where the file belongs");
    let start = current_revision(&fixture.data);

    // Only the deletion fails: the change is recorded and the orphan named.
    let error = service
        .settings_unregister_workspace(
            for_workspace(workspace.id, start),
            &ScriptedPrompt::new().answer(true),
        )
        .expect_err("the file cannot be deleted");
    assert_eq!(error.code(), "instructions_orphaned");
    assert_eq!(current_revision(&fixture.data), start + 1);

    // With the run journal unreadable as well, the run check still runs and
    // its failure is reported first. An unchecked run list shares the
    // `runs_not_stopped` code and says that the runs could not be checked.
    let other = fixture.workspace("second", read_tools());
    let other_file = instructions_file(&fixture, other.id);
    fs::create_dir_all(&other_file).expect("a folder where the file belongs");
    fs::write(fixture.data.join("runs-v1.jsonl"), b"not a journal\n").expect("journal");
    let revision = current_revision(&fixture.data);
    let error = service
        .settings_unregister_workspace(
            for_workspace(other.id, revision),
            &ScriptedPrompt::new().answer(true),
        )
        .expect_err("the runs cannot be checked");
    assert_eq!(error.code(), "runs_not_stopped");
    assert!(
        error.message().contains("could not be checked"),
        "{}",
        error.message()
    );
    assert_eq!(current_revision(&fixture.data), revision + 1);
}

#[test]
fn unregistering_refuses_unknown_stale_or_declined_requests_without_changing_anything() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", read_tools());
    let service = service(&fixture.data);
    let start = current_revision(&fixture.data);

    let prompt = ScriptedPrompt::new();
    assert_eq!(
        code(
            service
                .settings_unregister_workspace(for_workspace(uuid::Uuid::new_v4(), start), &prompt)
        ),
        "unknown_workspace"
    );
    assert_eq!(
        code(
            service.settings_unregister_workspace(for_workspace(workspace.id, start - 1), &prompt)
        ),
        "stale_revision"
    );
    assert!(prompt.asked().is_empty(), "refused before any dialog");

    let declined = ScriptedPrompt::new().answer(false);
    assert_eq!(
        code(service.settings_unregister_workspace(for_workspace(workspace.id, start), &declined)),
        "cancelled"
    );
    assert_eq!(declined.confirmations().len(), 1);
    assert_eq!(current_revision(&fixture.data), start);

    // A change made while the operator decides is not overwritten.
    let data = fixture.data.clone();
    let id = workspace.id;
    let racing = ScriptedPrompt::new().answer(true).meanwhile(move || {
        self::service(&data)
            .settings_set_permissions(
                permissions(id, start, [false, true, false, false]),
                &ScriptedPrompt::new().answer(true),
            )
            .expect("a concurrent change");
    });
    assert_eq!(
        code(service.settings_unregister_workspace(for_workspace(workspace.id, start), &racing)),
        "stale_revision"
    );
    assert_eq!(
        service
            .settings_overview()
            .expect("overview")
            .workspaces
            .len(),
        1
    );
}

/// A turn running in a session of the workspace refuses the removal, before
/// the dialog; another workspace is not blocked, and once the turn ends the
/// removal succeeds.
#[test]
fn unregistering_is_refused_while_a_turn_runs_in_the_workspace() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let other = fixture.workspace("other", text_only());
    let (release, gated) = gate();
    let (announce, arrived) = std::sync::mpsc::channel();
    let server = MockServer::start(vec![Step::gated(text("done"), gated).announcing(announce)]);
    let service = fixture.service(&server.endpoint);
    let session = service
        .session_create(wire(json!({"workspace_id": workspace.id, "title": "busy"})))
        .expect("session")
        .session_id;
    send(&service, session, &server.endpoint).expect("send");
    wait_arrived(&arrived);

    let start = current_revision(&fixture.data);
    let prompt = ScriptedPrompt::new();
    assert_eq!(
        code(service.settings_unregister_workspace(for_workspace(workspace.id, start), &prompt)),
        "turn_running"
    );
    assert!(prompt.asked().is_empty(), "refused before any dialog");
    assert_eq!(current_revision(&fixture.data), start);

    let allowed = ScriptedPrompt::new().answer(true);
    service
        .settings_unregister_workspace(for_workspace(other.id, start), &allowed)
        .expect("another workspace is removable");

    drop(release);
    wait_finished(&service, session);
    let allowed = ScriptedPrompt::new().answer(true);
    service
        .settings_unregister_workspace(
            for_workspace(workspace.id, current_revision(&fixture.data)),
            &allowed,
        )
        .expect("removable after the turn");
    server.finish();
}
