//! Toolchain profile, profile assignment, package-network profile, and
//! container grant changes from the `settings` window (ADR-0088, ADR-0107).
//!
//! Each method calls the same `ControlStore` operation as the trusted CLI
//! (`workspace profile-add`, `profile-remove`, `profile-assign`,
//! `profile-unassign`, `package-profile-add`, `package-profile-remove`,
//! `container-grant`, `container-revoke`) with the same expected-revision
//! check and validation, and records the same events. The window adds no
//! second rule and names no path:
//!
//! - Assigning a toolchain profile to a workspace is the operator's consent
//!   to grant that workspace's container read and execute on the profile's
//!   folders it cannot read yet; the confirmation names each one, and core
//!   refuses the assignment if the folders changed after it was shown.
//!   Unassigning, removing a profile, and unregistering a workspace revoke
//!   the grants no remaining assignment needs.
//!
//! - Every folder (a toolchain root, a `PATH` entry, a folder granted read
//!   and execute) comes only from [`SettingsPrompt::pick_folder`], one dialog
//!   per folder. The request carries how many of each, never a path. A
//!   read-write grant opens no dialog: core accepts it only on the
//!   workspace's own directory, so its folder is the recorded one (ADR-0104).
//! - Adding a toolchain profile widens what a run can see, adding a package
//!   profile records origins, and a grant changes a folder's access-control
//!   list on disk. Each is applied only after a native confirmation that this
//!   module composes from fixed text and display-safe values. Variable
//!   **values** are never shown in a dialog; their names are.
//! - Removing a profile or revoking a grant is confirmed natively too.
//!   Removals and revocations stop the runs they withdraw, as the CLI does
//!   (`runs_not_stopped`, `runs_unchecked`).
//! - Core has no edit operation for any of the three, so there is none here.
//! - Registry credentials stay CLI-only; removing a package profile does
//!   unbind and delete the credentials bound to its origins, as the CLI does,
//!   and the confirmation says so.
//!
//! As for every settings change, the store is released before a dialog and the
//! change is applied with the revision the operator confirmed, so a change made
//! meanwhile is refused as `stale_revision`.

use std::path::PathBuf;

use omgcode_core::control::{
    AssignmentFailure, CONTAINER_GRANTS_SUPPORTED, ContainerGrantAccess, ControlError,
    ControlStore, CredentialStore, MAX_PACKAGE_ORIGIN_BYTES, MAX_PACKAGE_ORIGINS,
    MAX_PROFILE_PATH_ENTRIES, MAX_PROFILE_ROOTS, MAX_PROFILE_VARIABLES, MAX_VARIABLE_NAME_BYTES,
    MAX_VARIABLE_VALUE_BYTES, PackageProfileDeclaration, PlannedFolder, ProfileAccessPlan,
    ProfileDeclaration, delete_unbound_credential, parse_origin,
};
use omgcode_core::run;
use omgcode_core::turn::display_safe_line;
use serde::Deserialize;
use uuid::Uuid;

use super::DesktopService;
use super::error::DesktopError;
use super::settings::overview_of;
use super::settings_changes::{
    AlsoWithdrawn, ChangeGuard, Confirmation, FolderPurpose, RevokingChange, SettingsPrompt,
    check_revision, confirm, revocation_outcome, rollback_withdrawn, shown,
};
use super::state::folder_of_directory;
use super::wire::SettingsOverview;

/// Longest name a request may carry, in bytes. Core's rule is stricter and is
/// applied by core; this only bounds what a dialog could show.
const MAX_NAME_BYTES: usize = 256;

/// One extra environment variable of a toolchain profile. The value is stored
/// in plain text by design (execution guide); it is never shown in a dialog.
#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentEntry {
    /// The variable name.
    pub name: String,
    /// The variable value.
    pub value: String,
}

impl std::fmt::Debug for EnvironmentEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EnvironmentEntry")
            .field("name", &self.name)
            .field("value", &"(not shown)")
            .finish()
    }
}

/// Arguments of `settings_add_toolchain_profile`. No path is carried: Rust
/// opens `root_count` folder dialogs, then `path_entry_count` more.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolchainProfileAddRequest {
    /// The settings revision the window last read.
    pub revision: u64,
    /// The profile name; core checks its rules.
    pub name: String,
    /// Extra environment variables.
    pub environment: Vec<EnvironmentEntry>,
    /// How many read-only roots to choose, 1 to 16.
    pub root_count: u8,
    /// How many `PATH` entries to choose, 0 to 16.
    pub path_entry_count: u8,
}

/// Arguments of `settings_remove_toolchain_profile` and
/// `settings_remove_package_profile`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProfileRemoveRequest {
    /// The settings revision the window last read.
    pub revision: u64,
    /// The recorded profile name.
    pub name: String,
}

/// Arguments of `settings_assign_toolchain_profile` and
/// `settings_unassign_toolchain_profile` (ADR-0107).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProfileAssignmentRequest {
    /// The settings revision the window last read.
    pub revision: u64,
    /// A registered workspace.
    pub workspace_id: Uuid,
    /// The recorded profile name.
    pub name: String,
}

/// Arguments of `settings_add_package_profile`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PackageProfileAddRequest {
    /// The settings revision the window last read.
    pub revision: u64,
    /// The profile name; core checks its rules.
    pub name: String,
    /// One to eight origins as typed; core parses them.
    pub origins: Vec<String>,
}

/// The access a container grant gives.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ContainerAccessChoice {
    /// Read and execute.
    ReadExecute,
    /// Read and write; core limits it to the workspace directory.
    ReadWrite,
}

impl From<ContainerAccessChoice> for ContainerGrantAccess {
    fn from(choice: ContainerAccessChoice) -> Self {
        match choice {
            ContainerAccessChoice::ReadExecute => Self::ReadExecute,
            ContainerAccessChoice::ReadWrite => Self::ReadWrite,
        }
    }
}

/// Arguments of `settings_grant_container_folder`. The folder comes from a
/// native dialog.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ContainerGrantRequest {
    /// The settings revision the window last read.
    pub revision: u64,
    /// A registered workspace.
    pub workspace_id: Uuid,
    /// The access to grant.
    pub access: ContainerAccessChoice,
}

/// Arguments of `settings_revoke_container_grant`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ContainerRevokeRequest {
    /// The settings revision the window last read. Core's revocation needs
    /// none; the window's own check keeps a change made meanwhile from being
    /// applied under a confirmation that described something else.
    pub revision: u64,
    /// The grant identifier.
    pub grant_id: Uuid,
}

impl DesktopError {
    /// Container grants exist only on Windows.
    pub(super) fn unsupported_platform() -> Self {
        Self::execution(
            "unsupported_platform",
            "Container grants exist only on Windows.",
        )
    }

    /// A request outside the bounds the window enforces before any dialog.
    fn execution_request(message: &'static str) -> Self {
        Self::invalid_request(message)
    }

    /// Map a grant or revocation failure. The operating system's own text is
    /// never shown, because it can name a path or an account.
    pub(super) fn from_grant(error: &ControlError) -> Self {
        match error {
            ControlError::Container(_) => Self::execution(
                "container_access_failed",
                "The folder's access list could not be changed or read back. If the grant was recorded it stays pending or revoking; the settings list its state, and Revoke tries again.",
            ),
            other => Self::from_control(other),
        }
    }

    /// An assignment that failed, as [`Self::from_grant`] reports its cause,
    /// with the count of grants it made that could not be revoked afterwards;
    /// the settings list each one, and **Revoke** tries again. A grant revoked
    /// whose AppContainer profile alone could not be deleted holds no access
    /// and is not counted, but the message says the profile was left. The
    /// operating system's own text is never shown.
    pub(super) fn assignment_failed(failure: &AssignmentFailure) -> Self {
        let cause = Self::from_grant(&failure.error);
        let failures = &failure.released.failures;
        if failures.is_empty() {
            return cause;
        }
        let left = failures
            .iter()
            .filter(|(_, error)| !matches!(error, ControlError::ProfileRemoval(_)))
            .count();
        let mut message = format!("{}.", cause.message().trim_end_matches('.'));
        if left > 0 {
            message.push_str(&format!(
                " {left} container grant(s) the failed assignment made could not be revoked; the settings list each one, and Revoke tries again."
            ));
        }
        if left < failures.len() {
            message.push_str(
                " A grant was revoked, but the workspace's AppContainer profile could not be deleted; it holds no access now.",
            );
        }
        Self::execution(cause.code(), message)
    }

    /// The package profile was removed, but credentials bound to it were not
    /// all deleted from the native store.
    fn package_credentials_orphaned(count: usize) -> Self {
        Self::execution(
            "credential_orphaned",
            format!(
                "The package-network profile was removed, but {count} stored registry key(s) bound to it could not be deleted and may remain in the operating system's credential store with no profile naming them. Inspect them with the trusted CLI."
            ),
        )
    }
}

/// `name` as a bounded, display-safe line.
fn named(name: &str) -> String {
    display_safe_line(name).chars().take(80).collect()
}

fn check_name(name: &str) -> Result<(), DesktopError> {
    if name.len() > MAX_NAME_BYTES {
        return Err(DesktopError::execution_request("The name is too long."));
    }
    Ok(())
}

const fn platform_text() -> &'static str {
    if cfg!(windows) { "Windows" } else { "Linux" }
}

const fn access_text(access: ContainerGrantAccess) -> &'static str {
    match access {
        ContainerGrantAccess::ReadExecute => "read and execute",
        ContainerGrantAccess::ReadWrite => "read and write",
    }
}

/// The text of a native confirmation for a new toolchain profile: name,
/// every root and `PATH` entry, and the variable **names**.
fn toolchain_add_confirmation(
    name: &str,
    roots: &[PathBuf],
    path_entries: &[PathBuf],
    environment: &[EnvironmentEntry],
) -> Confirmation {
    let list = |paths: &[PathBuf]| {
        if paths.is_empty() {
            "  (none)".to_owned()
        } else {
            paths
                .iter()
                .map(|path| format!("  {}", shown(path)))
                .collect::<Vec<_>>()
                .join("\n")
        }
    };
    let variables = if environment.is_empty() {
        "(none)".to_owned()
    } else {
        environment
            .iter()
            .map(|entry| display_safe_line(&entry.name))
            .collect::<Vec<_>>()
            .join(", ")
    };
    Confirmation {
        title: "Add a toolchain profile",
        message: format!(
            "Add the toolchain profile {} for {}?\n\nRead-only roots (a run that selects the profile can read and execute everything beneath them):\n{}\n\nPATH entries:\n{}\n\nEnvironment variables (names only; values are stored in plain text and shown by status):\n  {variables}\n\nA profile grants nothing by itself. Assign it to a workspace to let that workspace's runs use it; a run still needs the workspace's execute preference.",
            named(name),
            platform_text(),
            list(roots),
            list(path_entries),
        ),
    }
}

/// The native confirmation for the read-write grant on the workspace's own
/// folder. It says who gets the access (the workspace's sandbox) because the
/// workspace and the folder are the same path (ADR-0104), and that
/// registering normally grants it (ADR-0107), since this dialog restores a
/// grant that registration skipped or that was revoked later.
fn workspace_folder_grant_confirmation(workspace_folder: &str) -> Confirmation {
    Confirmation {
        title: "Let a workspace's commands use its folder",
        message: format!(
            "Let the commands OMGCode runs for the workspace
{workspace_folder}
read and write that folder and everything beneath it?

OMGCode runs each of the workspace's commands in the workspace's own AppContainer, a Windows sandbox that starts with no access to your files. Registering a folder normally grants it this access, but the sandbox has no read-write grant on this folder now. This adds the sandbox to the folder's access-control list on disk, and it stays until you revoke it. Every run in the workspace needs it."
        ),
    }
}

/// The native confirmation for a read-and-execute grant on a chosen folder.
fn read_execute_grant_confirmation(workspace_folder: &str, folder: &str) -> Confirmation {
    Confirmation {
        title: "Let a workspace's commands read a folder",
        message: format!(
            "Let the commands OMGCode runs for the workspace
{workspace_folder}
read and execute the folder
{folder}
and everything beneath it?

This adds the workspace's AppContainer, the Windows sandbox its commands run in, to the folder's access-control list on disk, and it stays until you revoke it."
        ),
    }
}

/// `folders` as indented display-safe lines.
fn folder_lines<'a>(folders: impl IntoIterator<Item = &'a str>) -> String {
    folders
        .into_iter()
        .map(|folder| format!("  {}", display_safe_line(folder)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The part of a confirmation that names what an assignment grants and
/// leaves out (ADR-0107). Empty when nothing is granted or left out.
pub(super) fn access_plan_text(plan: &ProfileAccessPlan) -> String {
    let mut text = String::new();
    if !plan.to_grant().is_empty() {
        text.push_str(&format!(
            "\n\nOMGCode runs the workspace's commands in its AppContainer, a Windows sandbox that starts with no access to your files. So that they can use the toolchain, the sandbox is added to the access-control list on disk of each of these folders, with read and execute access to everything beneath it. The access stays until no profile assigned to the workspace needs it:\n{}",
            folder_lines(plan.to_grant().iter().map(PlannedFolder::display)),
        ));
    }
    if !plan.needs_administrator().is_empty() {
        text.push_str(&format!(
            "\n\nOnly an administrator can change the access-control list of these folders, so they are not granted, and a run that needs one is refused:\n{}",
            folder_lines(plan.needs_administrator().iter().map(PlannedFolder::display)),
        ));
    }
    if !plan.unknown().is_empty() {
        text.push_str(&format!(
            "\n\nThe access of these folders could not be read, or they are no longer where they were recorded, so they are not granted:\n{}",
            folder_lines(plan.unknown().iter().map(PlannedFolder::display)),
        ));
    }
    text
}

/// The native confirmation for assigning a toolchain profile to a workspace.
fn assign_confirmation(
    workspace_folder: &str,
    name: &str,
    plan: &ProfileAccessPlan,
) -> Confirmation {
    Confirmation {
        title: "Use a toolchain profile in a workspace",
        message: format!(
            "Let the commands OMGCode runs for the workspace\n{workspace_folder}\nuse the toolchain profile {}?{}\n\nThe workspace can then select the profile for its runs. Each run still needs the workspace's execute preference.",
            named(name),
            access_plan_text(plan),
        ),
    }
}

/// The native confirmation for unassigning a toolchain profile; `revoked`
/// holds the display-safe folders of the grants it releases.
fn unassign_confirmation(workspace_folder: &str, name: &str, revoked: &[String]) -> Confirmation {
    let revoked = if revoked.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nThe workspace's AppContainer, the Windows sandbox its commands run in, is removed from the access-control list of each folder no other profile of the workspace needs, and every run of the workspace that started before is stopped:\n{}",
            folder_lines(revoked.iter().map(String::as_str)),
        )
    };
    Confirmation {
        title: "Stop using a toolchain profile in a workspace",
        message: format!(
            "Stop the commands OMGCode runs for the workspace\n{workspace_folder}\nfrom using the toolchain profile {}?\n\nRuns of the workspace that use the profile are stopped.{revoked}",
            named(name),
        ),
    }
}

/// The native confirmation for removing a toolchain profile. `workspaces`
/// holds, for each workspace it is assigned to, the workspace folder and the
/// display-safe folders of the grants the removal releases there.
fn remove_confirmation(name: &str, workspaces: &[(String, Vec<String>)]) -> Confirmation {
    let assigned = if workspaces.is_empty() {
        "\n\nIt is not assigned to any workspace.".to_owned()
    } else {
        let mut text = "\n\nIt is assigned to these workspaces, which can no longer select it; their runs that use it are stopped:".to_owned();
        for (workspace, revoked) in workspaces {
            text.push_str(&format!("\n  {workspace}"));
            if !revoked.is_empty() {
                text.push_str(&format!(
                    "\n  Its AppContainer, the Windows sandbox its commands run in, is removed from the access-control list of these folders, and every run of the workspace that started before is stopped:\n{}",
                    revoked
                        .iter()
                        .map(|folder| format!("    {folder}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ));
            }
        }
        text
    };
    Confirmation {
        title: "Remove a toolchain profile",
        message: format!(
            "Remove the toolchain profile {}?{assigned}\n\nAdd the profile again, and assign it, to use it.",
            named(name),
        ),
    }
}

/// The display-safe folder of each grant in `grants` that is still recorded.
fn grant_folders(store: &ControlStore, grants: &[Uuid]) -> Vec<String> {
    grants
        .iter()
        .filter_map(|id| {
            store
                .container_grants()
                .iter()
                .find(|status| status.grant().id() == *id)
                .map(|status| display_safe_line(&status.grant().directory_display()))
        })
        .collect()
}

/// The native confirmation for revoking a container grant.
fn revoke_confirmation(
    access: ContainerGrantAccess,
    directory: &str,
    workspace: &str,
) -> Confirmation {
    Confirmation {
        title: "Revoke a container grant",
        message: format!(
            "Revoke the {} access of the commands OMGCode runs for the workspace
{workspace}
to the folder
{directory}?

This removes the workspace's AppContainer, the Windows sandbox those commands run in, from the folder's access-control list on disk. Runs of that workspace that started before are stopped.",
            access_text(access),
        ),
    }
}

impl DesktopService {
    /// `settings` only. Ask the operator for each read-only root and `PATH`
    /// entry in a folder dialog, then, after a native confirmation that lists
    /// them and the variable names, record a toolchain profile as
    /// `omgcode workspace profile-add` does.
    ///
    /// # Errors
    ///
    /// `invalid_request` for a count or size outside its bound; `cancelled`
    /// when the operator closes a dialog or declines; `stale_revision`;
    /// `settings_change_running`; `busy`; `control` with core's reason (a bad
    /// name or variable, an existing name, a folder that overlaps the
    /// development root or application storage, a `PATH` entry outside a
    /// root).
    pub fn settings_add_toolchain_profile(
        &self,
        request: ToolchainProfileAddRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        check_name(&request.name)?;
        let root_count = usize::from(request.root_count);
        let path_count = usize::from(request.path_entry_count);
        if !(1..=MAX_PROFILE_ROOTS).contains(&root_count) {
            return Err(DesktopError::execution_request(
                "A profile needs 1 to 16 read-only roots.",
            ));
        }
        if path_count > MAX_PROFILE_PATH_ENTRIES {
            return Err(DesktopError::execution_request(
                "A profile has at most 16 PATH entries.",
            ));
        }
        if request.environment.len() > MAX_PROFILE_VARIABLES
            || request.environment.iter().any(|entry| {
                entry.name.len() > MAX_VARIABLE_NAME_BYTES
                    || entry.value.len() > MAX_VARIABLE_VALUE_BYTES
            })
        {
            return Err(DesktopError::execution_request(
                "A profile has at most 64 variables, with names of at most 128 bytes and values of at most 4096 bytes.",
            ));
        }
        {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
        }
        let mut roots = Vec::with_capacity(root_count);
        for _ in 0..root_count {
            roots.push(
                prompt
                    .pick_folder(FolderPurpose::ToolchainRoot, None)
                    .ok_or_else(DesktopError::cancelled)?,
            );
        }
        let mut path_entries = Vec::with_capacity(path_count);
        for _ in 0..path_count {
            // A PATH entry lies inside a root, so the dialog starts at the
            // first root the operator chose.
            path_entries.push(
                prompt
                    .pick_folder(
                        FolderPurpose::ToolchainPathEntry,
                        roots.first().map(PathBuf::as_path),
                    )
                    .ok_or_else(DesktopError::cancelled)?,
            );
        }
        confirm(
            prompt,
            &toolchain_add_confirmation(&request.name, &roots, &path_entries, &request.environment),
        )?;
        let mut store = self.inner.open_or_create_control()?;
        store
            .add_profile(
                &ProfileDeclaration {
                    name: request.name,
                    read_only_roots: roots,
                    path_entries,
                    environment: request
                        .environment
                        .into_iter()
                        .map(|entry| (entry.name, entry.value))
                        .collect(),
                },
                request.revision,
            )
            .map_err(|error| DesktopError::from_control(&error))?;
        Ok(overview_of(&self.inner, store))
    }

    /// `settings` only. After a native confirmation, remove a toolchain
    /// profile as `omgcode workspace profile-remove` does. Every assignment of
    /// it ends, so its runs are stopped, and in each workspace it was assigned
    /// to, the grants no other assigned profile needs are revoked, which stops
    /// that workspace's runs too (ADR-0107).
    ///
    /// # Errors
    ///
    /// `control` for an unknown profile; `cancelled`; `stale_revision`;
    /// `settings_change_running`; `busy`; `container_access_failed` or
    /// `revocation_incomplete` when the profile was removed but a grant was
    /// not revoked; `runs_not_stopped`; `runs_unchecked`.
    pub fn settings_remove_toolchain_profile(
        &self,
        request: ProfileRemoveRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        check_name(&request.name)?;
        let confirmation = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let status = store
                .profile(&request.name)
                .ok_or_else(|| DesktopError::from_control(&ControlError::UnknownProfile))?;
            let released = store
                .grants_released_by_removal(&request.name)
                .map_err(|error| DesktopError::from_control(&error))?;
            let workspaces = released
                .iter()
                .map(|(workspace_id, grants)| {
                    let folder = store
                        .registered_workspace(*workspace_id.as_uuid())
                        .map_or_else(String::new, |workspace| {
                            folder_of_directory(workspace.directory_path())
                        });
                    (folder, grant_folders(&store, grants))
                })
                .collect::<Vec<_>>();
            remove_confirmation(status.profile().name(), &workspaces)
        };
        confirm(prompt, &confirmation)?;
        self.apply_reporting(|store| {
            let before = run::pending_runs(store);
            let (_, revocations) = store
                .remove_profile_with_access(&request.name, request.revision)
                .map_err(|error| DesktopError::from_control(&error))?;
            Ok(revocation_outcome(
                before,
                &revocations,
                RevokingChange::Recorded,
            ))
        })
    }

    /// `settings` only. Assign a recorded toolchain profile to a workspace as
    /// `omgcode workspace profile-assign` does, after a native confirmation
    /// that names the workspace, the profile, and, on Windows, each folder the
    /// assignment grants the workspace's container read and execute and each
    /// folder it leaves out (ADR-0107). The confirmation is the operator's
    /// consent to those grants. Core plans the folders again under the lock
    /// and refuses a list that changed meanwhile.
    ///
    /// # Errors
    ///
    /// `unknown_workspace`; `control` for an unknown profile, one of another
    /// platform, one already assigned, or folders that changed since the
    /// confirmation; `cancelled`; `stale_revision`;
    /// `settings_change_running`; `busy`; `container_access_failed` when a
    /// grant was recorded but the folder's list could not be changed (the
    /// profile stays unassigned and the grant `pending`; revoke it). A failed
    /// assignment whose rollback revoked a grant, or tried to, first waits
    /// for the workspace's runs that withdrew, and adds to its message any it
    /// could not confirm stopped.
    pub fn settings_assign_toolchain_profile(
        &self,
        request: ProfileAssignmentRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        check_name(&request.name)?;
        let (workspace_id, plan, confirmation) = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let workspace = store
                .registered_workspace(request.workspace_id)
                .ok_or_else(DesktopError::unknown_workspace)?;
            let name = store
                .profile(&request.name)
                .ok_or_else(|| DesktopError::from_control(&ControlError::UnknownProfile))?
                .profile()
                .name()
                .to_owned();
            let plan = store
                .plan_profile_access(workspace.id(), &request.name)
                .map_err(|error| DesktopError::from_control(&error))?;
            let confirmation = assign_confirmation(
                &folder_of_directory(workspace.directory_path()),
                &name,
                &plan,
            );
            (workspace.id(), plan, confirmation)
        };
        confirm(prompt, &confirmation)?;
        let (listed, also, error) = {
            let mut store = self.inner.open_or_create_control()?;
            let before = run::pending_runs(&store);
            let failure = match store.assign_profile_with_access(
                workspace_id,
                &request.name,
                &plan,
                request.revision,
            ) {
                Ok(_) => return Ok(overview_of(&self.inner, store)),
                Err(failure) => failure,
            };
            let error = DesktopError::assignment_failed(&failure);
            let Some(also) = rollback_withdrawn(before, &failure) else {
                return Err(error);
            };
            (run::withdrawn_runs(&store), also, error)
        };
        Err(self.failed_after_wait(listed, also, error))
    }

    /// `settings` only. After a native confirmation, unassign a toolchain
    /// profile from a workspace as `omgcode workspace profile-unassign` does:
    /// its runs there are stopped, and the workspace's grants that no other
    /// assigned profile needs are revoked, which stops every run of the
    /// workspace that started before (ADR-0107). The workspace's read-write
    /// grant on its own folder is never revoked here.
    ///
    /// # Errors
    ///
    /// `unknown_workspace`; `control` for an unknown or unassigned profile;
    /// `cancelled`; `stale_revision`; `settings_change_running`; `busy`;
    /// `container_access_failed` or `revocation_incomplete` when the profile
    /// was unassigned but a grant was not revoked; `runs_not_stopped`;
    /// `runs_unchecked`.
    pub fn settings_unassign_toolchain_profile(
        &self,
        request: ProfileAssignmentRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        check_name(&request.name)?;
        let (workspace_id, confirmation) = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let workspace = store
                .registered_workspace(request.workspace_id)
                .ok_or_else(DesktopError::unknown_workspace)?;
            let name = store
                .profile(&request.name)
                .ok_or_else(|| DesktopError::from_control(&ControlError::UnknownProfile))?
                .profile()
                .name()
                .to_owned();
            let released = store
                .grants_released_by_unassignment(workspace.id(), &request.name)
                .map_err(|error| DesktopError::from_control(&error))?;
            let confirmation = unassign_confirmation(
                &folder_of_directory(workspace.directory_path()),
                &name,
                &grant_folders(&store, &released),
            );
            (workspace.id(), confirmation)
        };
        confirm(prompt, &confirmation)?;
        self.apply_reporting(|store| {
            let before = run::pending_runs(store);
            let (_, revocations) = store
                .unassign_profile_with_access(workspace_id, &request.name, request.revision)
                .map_err(|error| DesktopError::from_control(&error))?;
            Ok(revocation_outcome(
                before,
                &revocations,
                RevokingChange::Recorded,
            ))
        })
    }

    /// `settings` only. After a native confirmation that lists the origins
    /// core parsed, record a package-network profile as `omgcode workspace
    /// package-profile-add` does. The profile is configuration only: no run
    /// uses it and it authorizes no network access.
    ///
    /// # Errors
    ///
    /// `invalid_request` for a count or size outside its bound; `control`
    /// with core's reason for a bad name or origin; `cancelled`;
    /// `stale_revision`; `settings_change_running`; `busy`.
    pub fn settings_add_package_profile(
        &self,
        request: PackageProfileAddRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        check_name(&request.name)?;
        if request.origins.is_empty() || request.origins.len() > MAX_PACKAGE_ORIGINS {
            return Err(DesktopError::execution_request(
                "A profile needs 1 to 8 origins.",
            ));
        }
        if request
            .origins
            .iter()
            .any(|origin| origin.len() > MAX_PACKAGE_ORIGIN_BYTES)
        {
            return Err(DesktopError::execution_request("An origin is too long."));
        }
        // Core parses each origin; the confirmation shows what it parsed, so
        // the operator confirms what will be recorded.
        let rendered = request
            .origins
            .iter()
            .map(|origin| parse_origin(origin).map(|parsed| parsed.render()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| DesktopError::from_control(&error))?;
        {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
        }
        confirm(
            prompt,
            &Confirmation {
                title: "Add a package-network profile",
                message: format!(
                    "Add the package-network profile {} with these origins?\n\n{}\n\nA profile is configuration only. No run uses it and it authorizes no network access. A trusted package fetch can name it.",
                    named(&request.name),
                    rendered
                        .iter()
                        .map(|origin| format!("  {}", display_safe_line(origin)))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
            },
        )?;
        let mut store = self.inner.open_or_create_control()?;
        store
            .add_package_profile(
                &PackageProfileDeclaration {
                    name: request.name,
                    origins: request.origins,
                },
                request.revision,
            )
            .map_err(|error| DesktopError::from_control(&error))?;
        Ok(overview_of(&self.inner, store))
    }

    /// `settings` only. After a native confirmation, remove a package-network
    /// profile as `omgcode workspace package-profile-remove` does. Every
    /// registry credential bound to its origins is unbound by the same
    /// recorded event, and its stored key is deleted; the confirmation says
    /// how many.
    ///
    /// # Errors
    ///
    /// `control` for an unknown profile; `cancelled`; `stale_revision`;
    /// `credential_unavailable` or `credential_locked` (the native store is
    /// checked before anything is removed); `credential_orphaned` (the
    /// profile is removed but a stored key was not deleted);
    /// `settings_change_running`; `busy`; `runs_not_stopped`;
    /// `runs_unchecked`.
    pub fn settings_remove_package_profile(
        &self,
        request: ProfileRemoveRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        check_name(&request.name)?;
        let (confirmation, bound) = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let status = store
                .package_profile(&request.name)
                .ok_or_else(|| DesktopError::from_control(&ControlError::UnknownPackageProfile))?;
            let bound = status.credentials().len();
            let credentials_text = if bound == 0 {
                "No registry credential is bound to it.".to_owned()
            } else {
                format!(
                    "{bound} registry credential(s) bound to its origins are unbound, and their stored keys are deleted from this computer's credential store and cannot be recovered here."
                )
            };
            (
                Confirmation {
                    title: "Remove a package-network profile",
                    message: format!(
                        "Remove the package-network profile {}?\n\n{credentials_text}\n\nThe profile is configuration only; no run used it.",
                        named(status.profile().name()),
                    ),
                },
                bound,
            )
        };
        confirm(prompt, &confirmation)?;
        // Open the native store before anything is removed, so an unavailable
        // or locked store refuses the removal instead of stranding a key.
        let credentials = if bound > 0 {
            Some(
                CredentialStore::open(&self.inner.data_root)
                    .map_err(|error| DesktopError::from_credential(&error))?,
            )
        } else {
            None
        };
        self.apply_checked(|store| {
            let removal = store
                .remove_package_profile(&request.name, request.revision)
                .map_err(|error| DesktopError::from_control(&error))?;
            let orphans = removal
                .credentials
                .iter()
                .filter(|binding| match &credentials {
                    Some(credentials) => delete_unbound_credential(credentials, binding).is_some(),
                    None => true,
                })
                .count();
            if orphans == 0 {
                Ok(())
            } else {
                Err(DesktopError::package_credentials_orphaned(orphans))
            }
        })
    }

    /// `settings` only. Windows only. Grant a folder to the workspace's
    /// container as `omgcode workspace container-grant` does, after a native
    /// confirmation that names the workspace, the folder and the access and
    /// says the folder's access-control list changes on disk.
    ///
    /// For read and execute, the operator chooses the folder in a native
    /// dialog. For read and write, no dialog opens: core accepts that access
    /// only on the workspace's own directory, so the folder is the recorded
    /// one, and core still checks that the directory at that path is the
    /// recorded directory before it records anything (ADR-0104).
    ///
    /// # Errors
    ///
    /// `unsupported_platform` outside Windows; `unknown_workspace`;
    /// `invalid_request` when the recorded workspace path cannot be used on
    /// this system; `cancelled`; `stale_revision`; `settings_change_running`;
    /// `busy`; `control` with core's reason (a scope or overlap refusal, an
    /// existing grant, a workspace folder that was moved or replaced);
    /// `container_access_failed` when the grant was recorded but the list
    /// could not be changed (it stays `pending`; revoke it).
    pub fn settings_grant_container_folder(
        &self,
        request: ContainerGrantRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        if !CONTAINER_GRANTS_SUPPORTED {
            return Err(DesktopError::unsupported_platform());
        }
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let (workspace_id, workspace_path) = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let workspace = store
                .registered_workspace(request.workspace_id)
                .ok_or_else(DesktopError::unknown_workspace)?;
            (workspace.id(), workspace.directory_path())
        };
        let workspace_folder = folder_of_directory(workspace_path.clone());
        let (folder, confirmation) = match request.access {
            ContainerAccessChoice::ReadWrite => {
                let folder = workspace_path.ok_or_else(|| {
                    DesktopError::execution_request(
                        "The workspace path is not usable on this system.",
                    )
                })?;
                (
                    folder,
                    workspace_folder_grant_confirmation(&workspace_folder),
                )
            }
            ContainerAccessChoice::ReadExecute => {
                let folder = prompt
                    .pick_folder(FolderPurpose::ContainerGrant, None)
                    .ok_or_else(DesktopError::cancelled)?;
                let confirmation =
                    read_execute_grant_confirmation(&workspace_folder, &shown(&folder));
                (folder, confirmation)
            }
        };
        let access = ContainerGrantAccess::from(request.access);
        confirm(prompt, &confirmation)?;
        let mut store = self.inner.open_or_create_control()?;
        let outcome = store.grant_container_access(workspace_id, &folder, access, request.revision);
        match outcome {
            Ok(_) => Ok(overview_of(&self.inner, store)),
            Err(error) => Err(DesktopError::from_grant(&error)),
        }
    }

    /// `settings` only. Windows only. After a native confirmation, remove one
    /// recorded grant's access-control entry as `omgcode workspace
    /// container-revoke` does, and stop the workspace's runs that started
    /// before it.
    ///
    /// # Errors
    ///
    /// `unsupported_platform` outside Windows; `control` for an unknown grant;
    /// `cancelled`; `stale_revision`; `settings_change_running`; `busy`;
    /// `container_access_failed` (the grant stays `revoking`; revoke again);
    /// `runs_not_stopped`; `runs_unchecked`.
    pub fn settings_revoke_container_grant(
        &self,
        request: ContainerRevokeRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        if !CONTAINER_GRANTS_SUPPORTED {
            return Err(DesktopError::unsupported_platform());
        }
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let confirmation = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let status = store
                .container_grants()
                .iter()
                .find(|status| status.grant().id() == request.grant_id)
                .ok_or_else(|| DesktopError::from_control(&ControlError::UnknownGrant))?;
            let grant = status.grant();
            let workspace = store
                .registered_workspace(*grant.workspace_id().as_uuid())
                .map_or_else(
                    || "a workspace that is no longer registered".to_owned(),
                    |workspace| folder_of_directory(workspace.directory_path()),
                );
            revoke_confirmation(
                grant.access(),
                &display_safe_line(&grant.directory_display()),
                &workspace,
            )
        };
        confirm(prompt, &confirmation)?;
        self.apply_checked_also(|store| {
            // The same revision the operator confirmed, checked under the
            // lock this change holds: core's revocation needs none.
            check_revision(store, request.revision)?;
            // As the CLI does: list the workspace's pending runs before the
            // revocation, and report an unreadable journal only afterwards.
            let affected = store
                .container_grant_workspace(request.grant_id)
                .map(|workspace_id| run::workspace_runs(store, workspace_id));
            store
                .revoke_container_grant(request.grant_id)
                .map_err(|error| DesktopError::from_grant(&error))?;
            Ok(match affected {
                Some(Ok(runs)) => AlsoWithdrawn {
                    runs,
                    unchecked: false,
                },
                Some(Err(_)) => AlsoWithdrawn {
                    runs: Vec::new(),
                    unchecked: true,
                },
                None => AlsoWithdrawn::default(),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omgcode_core::control::Revocations;

    fn failure(error: ControlError, failures: Vec<(Uuid, ControlError)>) -> AssignmentFailure {
        AssignmentFailure {
            error,
            released: Revocations {
                failures,
                ..Revocations::default()
            },
        }
    }

    /// A failed assignment whose rollback revoked everything reports its
    /// cause as a grant failure does; one that left grants also counts them,
    /// without the operating system's text. A grant whose profile alone
    /// could not be deleted is not counted, but the message says the profile
    /// was left.
    #[test]
    fn a_failed_assignment_counts_the_grants_it_left() {
        let os = || std::io::Error::other("operating system text");
        let profile_left = " A grant was revoked, but the workspace's AppContainer profile could not be deleted; it holds no access now.";
        let cause = DesktopError::from_grant(&ControlError::LimitReached);
        let clean = DesktopError::assignment_failed(&failure(ControlError::LimitReached, vec![]));
        assert_eq!(clean.code(), cause.code());
        assert_eq!(clean.message(), cause.message());

        let alone = DesktopError::assignment_failed(&failure(
            ControlError::LimitReached,
            vec![(Uuid::new_v4(), ControlError::ProfileRemoval(os()))],
        ));
        assert_eq!(alone.code(), cause.code());
        assert_eq!(
            alone.message(),
            format!("{}.{profile_left}", cause.message().trim_end_matches('.'))
        );

        let left = DesktopError::assignment_failed(&failure(
            ControlError::Container(os()),
            vec![
                (Uuid::new_v4(), ControlError::Container(os())),
                (Uuid::new_v4(), ControlError::ProfileRemoval(os())),
            ],
        ));
        assert_eq!(left.code(), "container_access_failed");
        let message = left.message();
        assert!(
            message.contains(
                " 1 container grant(s) the failed assignment made could not be revoked; the settings list each one, and Revoke tries again."
            ),
            "{message}"
        );
        assert!(message.ends_with(profile_left), "{message}");
        assert!(!message.contains(".."), "{message}");
        assert!(!message.contains("operating system text"), "{message}");
    }

    #[test]
    fn the_grant_and_revoke_confirmations_name_everything_they_change() {
        let grant = read_execute_grant_confirmation("/dev/a", "/tools");
        for expected in [
            "/dev/a",
            "/tools",
            "read and execute",
            "access-control list",
            "AppContainer",
            "sandbox",
        ] {
            assert!(
                grant.message.contains(expected),
                "{expected}: {}",
                grant.message
            );
        }
        let revoke = revoke_confirmation(ContainerGrantAccess::ReadExecute, "/tools", "/dev/a");
        for expected in [
            "/tools",
            "/dev/a",
            "read and execute",
            "access-control list",
            "AppContainer",
            "stopped",
        ] {
            assert!(
                revoke.message.contains(expected),
                "{expected}: {}",
                revoke.message
            );
        }
    }

    #[test]
    fn the_workspace_folder_confirmation_says_the_sandbox_gets_the_access() {
        let grant = workspace_folder_grant_confirmation("O:\\ShippingQuotes");
        let message = &grant.message;
        // The folder is named once, as the workspace's: the operator is not
        // asked to give a folder access to itself.
        assert_eq!(
            message.matches("O:\\ShippingQuotes").count(),
            1,
            "{message}"
        );
        for expected in [
            "commands OMGCode runs for the workspace",
            "read and write that folder",
            "AppContainer",
            "Windows sandbox",
            "Registering a folder normally grants it this access",
            "has no read-write grant on this folder now",
            "access-control list",
            "until you revoke it",
            "Every run in the workspace needs it",
        ] {
            assert!(message.contains(expected), "{expected}: {message}");
        }
    }

    #[test]
    fn a_choice_names_the_core_access() {
        assert_eq!(
            ContainerGrantAccess::from(ContainerAccessChoice::ReadExecute),
            ContainerGrantAccess::ReadExecute
        );
        assert_eq!(
            ContainerGrantAccess::from(ContainerAccessChoice::ReadWrite),
            ContainerGrantAccess::ReadWrite
        );
    }

    #[test]
    fn a_request_refuses_unknown_fields_and_a_path() {
        let path: Result<ToolchainProfileAddRequest, _> =
            serde_json::from_value(serde_json::json!({
                "revision": 1, "name": "a", "environment": [], "root_count": 1,
                "path_entry_count": 0, "roots": ["C:\\x"]
            }));
        assert!(path.is_err());
        let grant: Result<ContainerGrantRequest, _> = serde_json::from_value(serde_json::json!({
            "revision": 1,
            "workspace_id": "6f1c1f0e-8f42-4c5b-9a55-0d2a3c4b5d6e",
            "access": "read_write",
            "path": "C:\\x"
        }));
        assert!(grant.is_err());
    }

    #[test]
    fn an_environment_entry_never_prints_its_value() {
        let entry = EnvironmentEntry {
            name: "A".to_owned(),
            value: "SECRET-VALUE".to_owned(),
        };
        assert!(!format!("{entry:?}").contains("SECRET-VALUE"));
    }
}
