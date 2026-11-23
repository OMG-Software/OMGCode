//! Skill changes from the `settings` window (ADR-0082).
//!
//! Each method calls the same `ControlStore` operation as `omgcode skills`
//! (`import_skill`, `set_skill_enablement`, `remove_skill_and_package`) with
//! the same expected-revision check and validation. The window adds no second
//! rule and never names a path:
//!
//! - **Import** takes its folder only from [`SettingsPrompt::pick_folder`].
//!   The skill arrives disabled and nothing in it is run, so no confirmation
//!   follows the folder choice. Core's refusals come back as controlled errors
//!   with fixed text; a message never carries a path or a recorded file name.
//! - **Enabling** a skill, for the application or for one workspace and
//!   including an `inherit` that leaves it enabled there, makes its text
//!   loadable into a turn as a labelled untrusted message. A native
//!   confirmation that names the skill comes first; this module composes it.
//!   Disabling needs none.
//! - **Removing** a skill is confirmed natively and says what happens to the
//!   stored copy.
//!
//! As for every settings change, the store is released before a dialog and the
//! change is applied with the revision the operator confirmed, so a change made
//! meanwhile is refused as `stale_revision`.

use omgcode_core::control::{
    ControlError, ControlStore, SkillOperationError, SkillRecord, SkillRefusal, SkillScope,
    SkillSetting,
};
use omgcode_core::skills::SkillName;
use omgcode_core::turn::display_safe_line;
use serde::Deserialize;
use uuid::Uuid;

use super::DesktopService;
use super::error::DesktopError;
use super::settings::overview_of;
use super::settings_changes::{
    ChangeGuard, Confirmation, FolderPurpose, SettingsPrompt, check_revision, confirm,
};
use super::state::folder_of_directory;
use super::wire::{SettingsOverview, SettingsRevisionRequest};

/// Longest description the confirmation shows, in characters. The description
/// is untrusted package text.
const MAX_SHOWN_DESCRIPTION_CHARS: usize = 200;

/// What a skill's setting is changed to.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SkillChoice {
    /// Enable the skill at the scope.
    Enabled,
    /// Disable the skill at the scope.
    Disabled,
    /// Remove a workspace's override. Only a workspace scope accepts it.
    Inherit,
}

impl From<SkillChoice> for SkillSetting {
    fn from(choice: SkillChoice) -> Self {
        match choice {
            SkillChoice::Enabled => Self::Enabled,
            SkillChoice::Disabled => Self::Disabled,
            SkillChoice::Inherit => Self::Inherit,
        }
    }
}

/// Arguments of `settings_set_skill_state`. A `null` workspace is the
/// application scope.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SkillStateRequest {
    /// The settings revision the window last read.
    pub revision: u64,
    /// A registered skill name.
    pub name: String,
    /// The workspace whose override changes, or `null` for the application
    /// setting.
    pub workspace_id: Option<Uuid>,
    /// The new setting.
    pub state: SkillChoice,
}

/// Arguments of `settings_remove_skill`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SkillRemoveRequest {
    /// The settings revision the window last read.
    pub revision: u64,
    /// A registered skill name.
    pub name: String,
}

impl DesktopError {
    /// Map a skill registry refusal. Its text is fixed.
    fn from_skill_refusal(refusal: SkillRefusal) -> Self {
        Self::skill(refusal.code(), refusal.to_string())
    }

    /// Map a failed import. A message is fixed text or core's static reason; a
    /// folder name, file name, or path is never shown, because it comes from
    /// the operator's disk and may name an absolute path.
    fn from_skill_operation(error: &SkillOperationError) -> Self {
        let code = error.code();
        match error {
            SkillOperationError::Control(ControlError::Skill(refusal))
            | SkillOperationError::Registration {
                error: ControlError::Skill(refusal),
                orphaned_package: None,
            } => Self::from_skill_refusal(*refusal),
            SkillOperationError::Control(other)
            | SkillOperationError::Registration {
                error: other,
                orphaned_package: None,
            } => Self::from_control(other),
            SkillOperationError::Import(import) => Self::skill(code, import_message(import)),
            SkillOperationError::Registration { .. } => Self::skill(
                code,
                "The skill was copied but not registered, and the copy could not be removed. The copy is inert.",
            ),
            SkillOperationError::RegistrationUncertain { .. } => Self::skill(
                code,
                "The registration may or may not have been recorded. The settings are read again; if the skill is listed it is registered, and otherwise the stored copy is inert.",
            ),
            SkillOperationError::Unavailable(_) | SkillOperationError::Load(_) => {
                Self::skill(code, "The skill could not be used.")
            }
        }
    }

    /// The skill was removed, but its stored copy was not fully deleted.
    fn skill_package_orphaned() -> Self {
        Self::skill(
            "package_orphaned",
            "The skill was removed, but its stored copy was not fully deleted. The copy is inert, and nothing can load it.",
        )
    }
}

/// The text for a refused import. Variants that carry a path or a name from
/// the source folder get fixed text; the rest use core's own message.
fn import_message(error: &omgcode_core::skills::SkillImportError) -> String {
    use omgcode_core::skills::SkillImportError as E;
    match error {
        E::SourceUnreadable { .. } => "The skill folder could not be read.".to_owned(),
        E::EntryRefused(_) => {
            "The skill folder holds a link, special file, or multi-link file.".to_owned()
        }
        E::EntryNameInvalid(_) => {
            "The skill folder holds a file or folder name that is not allowed.".to_owned()
        }
        E::EntryNameCollision(_) => {
            "The skill folder holds two names that differ only in letter case.".to_owned()
        }
        E::TooManyEntries(_) => "A directory in the skill folder has too many entries.".to_owned(),
        E::FileTooLarge(_) => "A file in the skill folder is too large.".to_owned(),
        E::NameDirectoryMismatch { .. } => {
            "The skill's name must equal the name of its folder.".to_owned()
        }
        E::Store(_) => "The private skill store could not be used.".to_owned(),
        E::Orphaned { .. } => {
            "The import failed and left an inert copy in the private skill store.".to_owned()
        }
        other => display_safe_line(&other.to_string()),
    }
}

/// Whether a record is effectively enabled at one scope: for a workspace its
/// override, else the application setting, and an unset skill is disabled.
fn enabled_at(store: &ControlStore, name: &SkillName, scope: SkillScope) -> bool {
    let workspace_id = match scope {
        SkillScope::Workspace { workspace_id } => Some(workspace_id),
        SkillScope::Application => None,
    };
    store
        .skill_statuses(workspace_id)
        .into_iter()
        .find(|status| status.skill.name() == name)
        .is_some_and(|status| match workspace_id {
            Some(_) => status.effective.unwrap_or(false),
            None => status.application.unwrap_or(false),
        })
}

/// What a change leaves enabled at the scope.
fn enabled_after(
    store: &ControlStore,
    name: &SkillName,
    scope: SkillScope,
    setting: SkillSetting,
) -> bool {
    match (scope, setting) {
        (_, SkillSetting::Enabled) => true,
        (_, SkillSetting::Disabled) => false,
        // Without its override a workspace follows the application setting.
        (_, SkillSetting::Inherit) => enabled_at(store, name, SkillScope::Application),
    }
}

/// The native confirmation for enabling a skill. Every word is fixed text or a
/// display-safe value; the description is bounded and untrusted.
fn enable_confirmation(record: &SkillRecord, where_text: &str) -> Confirmation {
    let description: String = display_safe_line(record.description())
        .chars()
        .take(MAX_SHOWN_DESCRIPTION_CHARS)
        .collect();
    Confirmation {
        title: "Enable a skill",
        message: format!(
            "Enable the skill {} {where_text}?\n\n{description}\n\nWhere it is enabled, you can select it for a chat turn, and its text is then sent to the model as an untrusted message. Enabling grants no permission and runs nothing.",
            display_safe_line(record.name().as_str()),
        ),
    }
}

/// The native confirmation for removing a skill.
fn remove_confirmation(record: &SkillRecord) -> Confirmation {
    Confirmation {
        title: "Remove a skill",
        message: format!(
            "Remove the skill {}?\n\nIts registration and its application and workspace settings are deleted, and so is its stored copy in OMGCode's private storage. The folder it was imported from is not touched. To use it again, import the folder again; it then starts disabled.\n\nA chat turn that already loaded it ends when it next checks the skill.",
            display_safe_line(record.name().as_str()),
        ),
    }
}

fn skill_name(value: &str) -> Result<SkillName, DesktopError> {
    SkillName::new(value).map_err(|_| DesktopError::invalid_request("The skill name is not valid."))
}

impl DesktopService {
    /// `settings` only. Ask the operator for a skill folder and import it
    /// through core, disabled, as `omgcode skills add` does.
    ///
    /// # Errors
    ///
    /// `cancelled` when the operator closes the dialog; `stale_revision`;
    /// `settings_change_running`; `busy`; the skill codes of a refused import
    /// (`skill_name_taken`, `too_many_skills`, `skill_md_missing`,
    /// `skill_name_directory_mismatch`, `entry_refused`, and the other
    /// package codes); `registration_uncertain`; `package_orphaned`.
    pub fn settings_import_skill(
        &self,
        request: SettingsRevisionRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
        }
        let folder = prompt
            .pick_folder(FolderPurpose::Skill, None)
            .ok_or_else(DesktopError::cancelled)?;
        // The folder is the operator's choice. Make it absolute without
        // resolving links; core opens the final component without following a
        // link, as it does for the trusted CLI.
        let source = std::path::absolute(&folder)
            .map_err(|_| DesktopError::invalid_request("The skill folder is not usable."))?;
        let mut store = self.inner.open_or_create_control()?;
        store
            .import_skill(&source, request.revision)
            .map_err(|error| DesktopError::from_skill_operation(&error))?;
        Ok(overview_of(&self.inner, store))
    }

    /// `settings` only. Set one skill's application setting or one
    /// workspace's override. A change that leaves the skill enabled at the
    /// scope first needs a native confirmation that names it.
    ///
    /// # Errors
    ///
    /// `invalid_request` for a malformed name; `skill_not_found`;
    /// `skill_workspace_unknown`; `skill_inherit_needs_workspace`;
    /// `cancelled`; `stale_revision`; `settings_change_running`; `busy`.
    pub fn settings_set_skill_state(
        &self,
        request: SkillStateRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let name = skill_name(&request.name)?;
        let setting = SkillSetting::from(request.state);
        let (scope, confirmation) = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let record = store
                .skill(&name)
                .ok_or_else(|| DesktopError::from_skill_refusal(SkillRefusal::NotFound))?;
            let (scope, where_text) = match request.workspace_id {
                None => (
                    SkillScope::Application,
                    "for every workspace without its own setting".to_owned(),
                ),
                Some(id) => {
                    let workspace = store
                        .registered_workspace(id)
                        .ok_or_else(DesktopError::unknown_workspace)?;
                    (
                        SkillScope::Workspace {
                            workspace_id: workspace.id(),
                        },
                        format!(
                            "for the workspace {}",
                            folder_of_directory(workspace.directory_path())
                        ),
                    )
                }
            };
            // Core refuses `inherit` at the application scope; refuse it here
            // before any dialog, with core's own error.
            if scope == SkillScope::Application && setting == SkillSetting::Inherit {
                return Err(DesktopError::from_skill_refusal(
                    SkillRefusal::InheritNeedsWorkspace,
                ));
            }
            let enables =
                enabled_after(&store, &name, scope, setting) && !enabled_at(&store, &name, scope);
            (
                scope,
                enables.then(|| enable_confirmation(record, &where_text)),
            )
        };
        if let Some(confirmation) = confirmation {
            confirm(prompt, &confirmation)?;
        }
        let mut store = self.inner.open_or_create_control()?;
        store
            .set_skill_enablement(&name, scope, setting, request.revision)
            .map_err(|error| DesktopError::from_control(&error))?;
        Ok(overview_of(&self.inner, store))
    }

    /// `settings` only. After a native confirmation, remove a skill's
    /// registration and delete its stored copy, as `omgcode skills remove`
    /// does. The operator's source folder is never touched.
    ///
    /// # Errors
    ///
    /// `invalid_request` for a malformed name; `skill_not_found`;
    /// `cancelled`; `stale_revision`; `settings_change_running`; `busy`;
    /// `package_orphaned` when the removal is recorded but the stored copy was
    /// not fully deleted (the copy is inert).
    pub fn settings_remove_skill(
        &self,
        request: SkillRemoveRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let name = skill_name(&request.name)?;
        let confirmation = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let record = store
                .skill(&name)
                .ok_or_else(|| DesktopError::from_skill_refusal(SkillRefusal::NotFound))?;
            remove_confirmation(record)
        };
        confirm(prompt, &confirmation)?;
        let mut store = self.inner.open_or_create_control()?;
        let removed = store
            .remove_skill_and_package(&name, request.revision)
            .map_err(|error| match error {
                ControlError::Skill(refusal) => DesktopError::from_skill_refusal(refusal),
                other => DesktopError::from_control(&other),
            })?;
        if removed.package_error.is_some() {
            return Err(DesktopError::skill_package_orphaned());
        }
        Ok(overview_of(&self.inner, store))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_skill_choice_names_the_core_setting() {
        assert_eq!(
            SkillSetting::from(SkillChoice::Enabled),
            SkillSetting::Enabled
        );
        assert_eq!(
            SkillSetting::from(SkillChoice::Disabled),
            SkillSetting::Disabled
        );
        assert_eq!(
            SkillSetting::from(SkillChoice::Inherit),
            SkillSetting::Inherit
        );
    }

    #[test]
    fn a_request_refuses_unknown_fields_and_a_path() {
        let ok: Result<SkillStateRequest, _> = serde_json::from_value(serde_json::json!({
            "revision": 1, "name": "a", "workspace_id": null, "state": "enabled"
        }));
        assert!(ok.is_ok());
        let path: Result<SkillRemoveRequest, _> = serde_json::from_value(serde_json::json!({
            "revision": 1, "name": "a", "path": "C:\\x"
        }));
        assert!(path.is_err());
    }
}
