//! Toolchain profile, package-network profile, and container grant changes
//! from the `settings` window (ADR-0088).
//!
//! Each method calls the same `ControlStore` operation as the trusted CLI
//! (`workspace profile-add`, `profile-remove`, `package-profile-add`,
//! `package-profile-remove`, `container-grant`, `container-revoke`) with the
//! same expected-revision check and validation, and records the same events.
//! The window adds no second rule and names no path:
//!
//! - Every folder (a toolchain root, a `PATH` entry, a granted folder) comes
//!   only from [`SettingsPrompt::pick_folder`], one dialog per folder. The
//!   request carries how many of each, never a path.
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
    CONTAINER_GRANTS_SUPPORTED, ContainerGrantAccess, ControlError, CredentialStore,
    MAX_PACKAGE_ORIGIN_BYTES, MAX_PACKAGE_ORIGINS, MAX_PROFILE_PATH_ENTRIES, MAX_PROFILE_ROOTS,
    MAX_PROFILE_VARIABLES, MAX_VARIABLE_NAME_BYTES, MAX_VARIABLE_VALUE_BYTES,
    PackageProfileDeclaration, ProfileDeclaration, delete_unbound_credential, parse_origin,
};
use omgcode_core::run;
use omgcode_core::turn::display_safe_line;
use serde::Deserialize;
use uuid::Uuid;

use super::DesktopService;
use super::error::DesktopError;
use super::settings::overview_of;
use super::settings_changes::{
    AlsoWithdrawn, ChangeGuard, Confirmation, FolderPurpose, SettingsPrompt, check_revision,
    confirm, shown,
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
            "Add the toolchain profile {} for {}?\n\nRead-only roots (a run that selects the profile can read and execute everything beneath them):\n{}\n\nPATH entries:\n{}\n\nEnvironment variables (names only; values are stored in plain text and shown by status):\n  {variables}\n\nA profile grants nothing by itself. A run still needs the workspace's execute preference and the trusted CLI run command.",
            named(name),
            platform_text(),
            list(roots),
            list(path_entries),
        ),
    }
}

/// The native confirmation for a container grant.
fn grant_confirmation(
    workspace_folder: &str,
    access: ContainerGrantAccess,
    folder: &str,
) -> Confirmation {
    Confirmation {
        title: "Grant a folder to a workspace",
        message: format!(
            "Give the workspace
{workspace_folder}
{} access to the folder
{folder}?

This changes the folder's access-control list on disk for that workspace's AppContainer, and it stays until you revoke it. Runs in the workspace can then use the folder, including everything beneath it.",
            access_text(access),
        ),
    }
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
            "Revoke the {} grant on
{directory}
for the workspace
{workspace}?

This removes the entry from the folder's access-control list on disk. Runs of that workspace that started before are stopped.",
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
    /// profile as `omgcode workspace profile-remove` does. A run already
    /// started keeps its recorded profile; the runs the settings then
    /// withdraw are stopped as for the CLI.
    ///
    /// # Errors
    ///
    /// `control` for an unknown profile; `cancelled`; `stale_revision`;
    /// `settings_change_running`; `busy`; `runs_not_stopped`;
    /// `runs_unchecked`.
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
            Confirmation {
                title: "Remove a toolchain profile",
                message: format!(
                    "Remove the toolchain profile {}?\n\nNew runs can no longer select it. A run that already started keeps the profile it recorded. Add the profile again to use it.",
                    named(status.profile().name()),
                ),
            }
        };
        confirm(prompt, &confirmation)?;
        self.apply_checked(|store| {
            store
                .remove_profile(&request.name, request.revision)
                .map(|_| ())
                .map_err(|error| DesktopError::from_control(&error))
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

    /// `settings` only. Windows only. Ask the operator for a folder and,
    /// after a native confirmation that names the workspace, the folder and
    /// the access and says the folder's access-control list changes on disk,
    /// grant it to the workspace's container as `omgcode workspace
    /// container-grant` does.
    ///
    /// # Errors
    ///
    /// `unsupported_platform` outside Windows; `unknown_workspace`;
    /// `cancelled`; `stale_revision`; `settings_change_running`; `busy`;
    /// `control` with core's reason (a scope or overlap refusal, an existing
    /// grant); `container_access_failed` when the grant was recorded but the
    /// list could not be changed (it stays `pending`; revoke it).
    pub fn settings_grant_container_folder(
        &self,
        request: ContainerGrantRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        if !CONTAINER_GRANTS_SUPPORTED {
            return Err(DesktopError::unsupported_platform());
        }
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let (workspace_id, workspace_folder) = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let workspace = store
                .registered_workspace(request.workspace_id)
                .ok_or_else(DesktopError::unknown_workspace)?;
            (
                workspace.id(),
                folder_of_directory(workspace.directory_path()),
            )
        };
        let folder = prompt
            .pick_folder(FolderPurpose::ContainerGrant, None)
            .ok_or_else(DesktopError::cancelled)?;
        let access = ContainerGrantAccess::from(request.access);
        confirm(
            prompt,
            &grant_confirmation(&workspace_folder, access, &shown(&folder)),
        )?;
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

    #[test]
    fn the_grant_and_revoke_confirmations_name_everything_they_change() {
        let grant = grant_confirmation("/dev/a", ContainerGrantAccess::ReadWrite, "/dev/b");
        for expected in [
            "/dev/a",
            "/dev/b",
            "read and write",
            "access-control list",
            "AppContainer",
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
