//! Skill changes from the desktop `settings` window (ADR-0082). A scripted
//! prompt stands in for the native dialogs, as in `desktop_settings_changes`.

mod common;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use omgcode_app::TokenSource;
use omgcode_app::desktop::{
    Confirmation, DesktopConfig, DesktopError, DesktopService, FolderPurpose, SettingsOverview,
    SettingsPrompt, SettingsRevisionRequest, SkillRemoveRequest, SkillStateRequest,
};
use omgcode_core::control::{ControlStore, SkillScope, SkillSetting};
use omgcode_core::skills::SkillName;
use serde_json::json;

use common::{Fixture, wire};

#[derive(Debug, PartialEq)]
enum Asked {
    Folder(FolderPurpose),
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
                Asked::Folder(_) => None,
            })
            .collect()
    }
}

impl SettingsPrompt for ScriptedPrompt {
    fn pick_folder(&self, purpose: FolderPurpose, _start: Option<&Path>) -> Option<PathBuf> {
        self.asked.borrow_mut().push(Asked::Folder(purpose));
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

fn state(
    value: u64,
    name: &str,
    workspace_id: Option<uuid::Uuid>,
    choice: &str,
) -> SkillStateRequest {
    wire(json!({
        "revision": value,
        "name": name,
        "workspace_id": workspace_id,
        "state": choice,
    }))
}

fn remove(value: u64, name: &str) -> SkillRemoveRequest {
    wire(json!({ "revision": value, "name": name }))
}

fn error_of(result: Result<SettingsOverview, DesktopError>) -> DesktopError {
    result.expect_err("the change is refused")
}

/// A skill folder outside the data root and the development root.
fn skill_folder(fixture: &Fixture, name: &str, description: &str) -> PathBuf {
    let folder = fixture
        .data
        .parent()
        .expect("temporary root")
        .join("skill-source")
        .join(name);
    fs::create_dir_all(&folder).expect("skill folder");
    fs::write(
        folder.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\nFollow these steps.\n"),
    )
    .expect("SKILL.md");
    folder
}

fn current_revision(data: &Path) -> u64 {
    ControlStore::open_existing(data)
        .expect("open")
        .expect("store exists")
        .revision()
}

/// Import `name` through the service and return the new overview.
fn imported(fixture: &Fixture, name: &str) -> SettingsOverview {
    let folder = skill_folder(fixture, name, "Reviews a change.");
    let prompt = ScriptedPrompt::new().folder(Some(&folder));
    service(&fixture.data)
        .settings_import_skill(revision(current_revision(&fixture.data)), &prompt)
        .expect("imported")
}

/// Register one workspace through the service; returns its identifier.
fn with_workspace(fixture: &Fixture) -> uuid::Uuid {
    let project = fixture.development.join("project");
    fs::create_dir_all(&project).expect("project");
    let prompt = ScriptedPrompt::new().folder(Some(&project)).answer(true);
    let overview = service(&fixture.data)
        .settings_register_workspace(revision(current_revision(&fixture.data)), &prompt)
        .expect("registered");
    overview.workspaces[0].workspace_id
}

#[test]
fn an_import_asks_for_a_folder_only_and_registers_the_skill_disabled() {
    let fixture = Fixture::new();
    let folder = skill_folder(&fixture, "review", "Reviews a change.");
    let prompt = ScriptedPrompt::new().folder(Some(&folder));
    let before = current_revision(&fixture.data);

    let overview = service(&fixture.data)
        .settings_import_skill(revision(before), &prompt)
        .expect("imported");

    // Importing grants nothing, so the folder is the only dialog.
    assert_eq!(prompt.asked(), [Asked::Folder(FolderPurpose::Skill)]);
    assert_eq!(overview.revision, before + 1);
    assert_eq!(overview.skills.len(), 1);
    assert_eq!(overview.skills[0].name, "review");
    assert_eq!(overview.skills[0].application, None);
    assert!(overview.skills[0].workspace_overrides.is_empty());
    // The same record the CLI path makes: registered, source untouched.
    let store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("exists");
    assert_eq!(store.skills().len(), 1);
    assert!(folder.join("SKILL.md").is_file());
}

#[test]
fn a_closed_dialog_or_a_stale_revision_imports_nothing() {
    let fixture = Fixture::new();
    let before = current_revision(&fixture.data);
    let closed = ScriptedPrompt::new().folder(None);
    let error = error_of(service(&fixture.data).settings_import_skill(revision(before), &closed));
    assert_eq!(error.code(), "cancelled");

    // A stale revision is refused before any dialog: the prompt has no script.
    let silent = ScriptedPrompt::new();
    let error =
        error_of(service(&fixture.data).settings_import_skill(revision(before + 7), &silent));
    assert_eq!(error.code(), "stale_revision");
    assert!(silent.asked().is_empty());
    assert_eq!(current_revision(&fixture.data), before);
}

#[test]
fn a_refused_import_returns_core_codes_with_no_path() {
    let fixture = Fixture::new();
    // No SKILL.md.
    let empty = fixture
        .data
        .parent()
        .expect("temporary root")
        .join("skill-source")
        .join("empty-skill");
    fs::create_dir_all(&empty).expect("empty folder");
    let prompt = ScriptedPrompt::new().folder(Some(&empty));
    let before = current_revision(&fixture.data);
    let error = error_of(service(&fixture.data).settings_import_skill(revision(before), &prompt));
    assert_eq!(error.code(), "skill_md_missing");
    assert!(!error.message().contains("skill-source"), "{error}");

    // The folder name differs from the skill name.
    let mismatch = skill_folder(&fixture, "right-name", "Text.");
    let renamed = mismatch.with_file_name("wrong-name");
    fs::rename(&mismatch, &renamed).expect("rename");
    let prompt = ScriptedPrompt::new().folder(Some(&renamed));
    let error = error_of(service(&fixture.data).settings_import_skill(revision(before), &prompt));
    assert_eq!(error.code(), "skill_name_directory_mismatch");
    assert!(!error.message().contains("wrong-name"), "{error}");
    assert_eq!(current_revision(&fixture.data), before);

    // A second import of the same name.
    let after = imported(&fixture, "review").revision;
    let folder = skill_folder(&fixture, "review", "Again.");
    let prompt = ScriptedPrompt::new().folder(Some(&folder));
    let error = error_of(service(&fixture.data).settings_import_skill(revision(after), &prompt));
    assert_eq!(error.code(), "skill_name_taken");
}

#[test]
fn enabling_for_the_application_is_confirmed_by_name_and_disabling_is_not() {
    let fixture = Fixture::new();
    let revision_now = imported(&fixture, "review").revision;

    let declined = ScriptedPrompt::new().answer(false);
    let error = error_of(
        service(&fixture.data)
            .settings_set_skill_state(state(revision_now, "review", None, "enabled"), &declined),
    );
    assert_eq!(error.code(), "cancelled");
    let confirmations = declined.confirmations();
    assert_eq!(confirmations.len(), 1);
    let message = &confirmations[0].message;
    assert!(message.contains("review"), "{message}");
    assert!(message.contains("untrusted"), "{message}");
    assert!(message.contains("Reviews a change."), "{message}");
    assert_eq!(current_revision(&fixture.data), revision_now);

    let accepted = ScriptedPrompt::new().answer(true);
    let overview = service(&fixture.data)
        .settings_set_skill_state(state(revision_now, "review", None, "enabled"), &accepted)
        .expect("enabled");
    assert_eq!(overview.skills[0].application, Some(true));

    // Enabling again changes nothing that needs a question.
    let again = ScriptedPrompt::new();
    let overview = service(&fixture.data)
        .settings_set_skill_state(state(overview.revision, "review", None, "enabled"), &again)
        .expect("still enabled");
    assert!(again.asked().is_empty());

    // Disabling asks nothing.
    let silent = ScriptedPrompt::new();
    let overview = service(&fixture.data)
        .settings_set_skill_state(
            state(overview.revision, "review", None, "disabled"),
            &silent,
        )
        .expect("disabled");
    assert!(silent.asked().is_empty());
    assert_eq!(overview.skills[0].application, Some(false));
}

#[test]
fn a_workspace_override_that_enables_is_confirmed_and_names_the_workspace() {
    let fixture = Fixture::new();
    let workspace = with_workspace(&fixture);
    let revision_now = imported(&fixture, "review").revision;

    let accepted = ScriptedPrompt::new().answer(true);
    let overview = service(&fixture.data)
        .settings_set_skill_state(
            state(revision_now, "review", Some(workspace), "enabled"),
            &accepted,
        )
        .expect("enabled in the workspace");
    let message = accepted.confirmations().remove(0).message;
    assert!(message.contains("project"), "{message}");
    assert_eq!(overview.skills[0].workspace_overrides.len(), 1);
    assert!(overview.skills[0].workspace_overrides[0].enabled);

    // Disabling there asks nothing.
    let silent = ScriptedPrompt::new();
    let overview = service(&fixture.data)
        .settings_set_skill_state(
            state(overview.revision, "review", Some(workspace), "disabled"),
            &silent,
        )
        .expect("disabled in the workspace");
    assert!(silent.asked().is_empty());
    assert!(!overview.skills[0].workspace_overrides[0].enabled);
}

#[test]
fn an_inherit_that_results_in_enabled_is_confirmed_and_one_that_does_not_is_not() {
    let fixture = Fixture::new();
    let workspace = with_workspace(&fixture);
    let mut now = imported(&fixture, "review").revision;
    // Enabled for the application, disabled in the workspace.
    let enable = ScriptedPrompt::new().answer(true);
    now = service(&fixture.data)
        .settings_set_skill_state(state(now, "review", None, "enabled"), &enable)
        .expect("enabled")
        .revision;
    let silent = ScriptedPrompt::new();
    now = service(&fixture.data)
        .settings_set_skill_state(state(now, "review", Some(workspace), "disabled"), &silent)
        .expect("disabled in the workspace")
        .revision;

    // Inheriting turns it on there, so Rust asks first.
    let declined = ScriptedPrompt::new().answer(false);
    let error = error_of(
        service(&fixture.data)
            .settings_set_skill_state(state(now, "review", Some(workspace), "inherit"), &declined),
    );
    assert_eq!(error.code(), "cancelled");
    assert_eq!(declined.confirmations().len(), 1);
    let accepted = ScriptedPrompt::new().answer(true);
    let overview = service(&fixture.data)
        .settings_set_skill_state(state(now, "review", Some(workspace), "inherit"), &accepted)
        .expect("inherited");
    assert_eq!(accepted.confirmations().len(), 1);
    assert!(overview.skills[0].workspace_overrides.is_empty());

    // With the application disabled an inherit leaves it disabled: no question.
    let silent = ScriptedPrompt::new();
    let now = service(&fixture.data)
        .settings_set_skill_state(
            state(overview.revision, "review", None, "disabled"),
            &silent,
        )
        .expect("application disabled")
        .revision;
    let overview = service(&fixture.data)
        .settings_set_skill_state(state(now, "review", Some(workspace), "inherit"), &silent)
        .expect("inherit, still disabled");
    assert!(silent.asked().is_empty());
    assert_eq!(overview.skills[0].application, Some(false));
}

#[test]
fn a_bad_scope_or_name_is_refused_before_any_dialog() {
    let fixture = Fixture::new();
    let workspace = with_workspace(&fixture);
    let now = imported(&fixture, "review").revision;
    let silent = ScriptedPrompt::new();
    let svc = service(&fixture.data);

    let error =
        error_of(svc.settings_set_skill_state(state(now, "review", None, "inherit"), &silent));
    assert_eq!(error.code(), "skill_inherit_needs_workspace");
    let error =
        error_of(svc.settings_set_skill_state(state(now, "missing", None, "enabled"), &silent));
    assert_eq!(error.code(), "skill_not_found");
    let error = error_of(svc.settings_set_skill_state(
        state(now, "review", Some(uuid::Uuid::from_u128(9)), "enabled"),
        &silent,
    ));
    assert_eq!(error.code(), "unknown_workspace");
    let error = error_of(
        svc.settings_set_skill_state(state(now, "../escape", Some(workspace), "enabled"), &silent),
    );
    assert_eq!(error.code(), "invalid_request");
    assert!(silent.asked().is_empty());
    assert_eq!(current_revision(&fixture.data), now);
}

#[test]
fn a_change_made_while_the_operator_decides_is_not_overwritten() {
    let fixture = Fixture::new();
    let now = imported(&fixture, "review").revision;
    let data = fixture.data.clone();
    let prompt = ScriptedPrompt::new().answer(true).meanwhile(move || {
        let mut store = ControlStore::open_existing(&data)
            .expect("open")
            .expect("exists");
        let revision = store.revision();
        store
            .set_skill_enablement(
                &SkillName::new("review").expect("name"),
                SkillScope::Application,
                SkillSetting::Disabled,
                revision,
            )
            .expect("an intervening change");
    });

    let error = error_of(
        service(&fixture.data)
            .settings_set_skill_state(state(now, "review", None, "enabled"), &prompt),
    );

    assert_eq!(error.code(), "stale_revision");
    let store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("exists");
    // The intervening disable stands; the confirmed enable was not applied over it.
    assert_eq!(store.skill_statuses(None)[0].application, Some(false));
}

#[test]
fn removing_is_confirmed_by_name_and_deletes_the_stored_copy_but_not_the_source() {
    let fixture = Fixture::new();
    let overview = imported(&fixture, "review");
    let folder = fixture
        .data
        .parent()
        .expect("temporary root")
        .join("skill-source")
        .join("review");
    let skill_id = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("exists")
        .skills()[0]
        .skill_id();
    let package = fixture
        .data
        .join("skills-v1")
        .join("packages")
        .join(skill_id.to_string());
    assert!(package.is_dir(), "the stored copy exists after import");

    let declined = ScriptedPrompt::new().answer(false);
    let error = error_of(
        service(&fixture.data)
            .settings_remove_skill(remove(overview.revision, "review"), &declined),
    );
    assert_eq!(error.code(), "cancelled");
    let message = declined.confirmations().remove(0).message;
    assert!(message.contains("review"), "{message}");
    assert!(message.contains("stored copy"), "{message}");
    assert!(message.contains("not touched"), "{message}");
    assert!(package.is_dir(), "a declined removal keeps the copy");

    let accepted = ScriptedPrompt::new().answer(true);
    let removed = service(&fixture.data)
        .settings_remove_skill(remove(overview.revision, "review"), &accepted)
        .expect("removed");
    assert!(removed.skills.is_empty());
    assert!(!package.exists(), "the stored copy is deleted");
    assert!(folder.join("SKILL.md").is_file(), "the source is kept");

    let error = error_of(
        service(&fixture.data).settings_remove_skill(remove(removed.revision, "review"), &accepted),
    );
    assert_eq!(error.code(), "skill_not_found");
}

#[test]
fn requests_carry_no_path_and_refuse_unknown_fields() {
    let with_path = serde_json::from_value::<SkillRemoveRequest>(
        json!({ "revision": 1, "name": "review", "path": "C:\\skills" }),
    );
    assert!(with_path.is_err());
    let with_path = serde_json::from_value::<SkillStateRequest>(
        json!({ "revision": 1, "name": "a", "workspace_id": null, "state": "enabled", "folder": "x" }),
    );
    assert!(with_path.is_err());
    let bad_state = serde_json::from_value::<SkillStateRequest>(
        json!({ "revision": 1, "name": "a", "workspace_id": null, "state": "maybe" }),
    );
    assert!(bad_state.is_err());
    let with_path = serde_json::from_value::<SettingsRevisionRequest>(
        json!({ "revision": 1, "folder": "C:\\skills" }),
    );
    assert!(with_path.is_err());
}
