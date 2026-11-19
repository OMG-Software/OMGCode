//! Settings changes from the `settings` window (ADR-0059 phase S2, ADR-0060).
//!
//! Each method changes one trusted setting through the same `ControlStore`
//! call as the trusted CLI, with the same expected-revision check, the same
//! validation, and the same audit event. The desktop adds no second rule.
//!
//! # The operator's dialogs
//!
//! A [`SettingsPrompt`] is implemented by the shell with native dialogs that
//! no webview can open, answer, or read:
//!
//! - A folder comes only from [`SettingsPrompt::pick_folder`]. No request
//!   carries a path, so a compromised window cannot name one.
//! - A change that widens access is applied only after
//!   [`SettingsPrompt::confirm`] returns `true`. This module composes the
//!   confirmation text from core's display-safe values; the caller supplies
//!   none of it. Widening changes are setting the development root,
//!   registering a workspace, turning any preference on, and choosing a
//!   package cache root. Turning preferences off and clearing a package cache
//!   root narrow access and need no confirmation.
//!
//! # Order and races
//!
//! Each change reads the store, checks the caller's revision, and releases the
//! store **before** any dialog, so the CLI and run watchers are never blocked
//! on an operator. It then reopens the store and applies the change with that
//! same revision. If anything changed meanwhile, core refuses the change as a
//! revision conflict (`stale_revision`), so what the operator confirmed is
//! exactly what is applied.
//!
//! After a change is recorded, runs that lost access are stopped and observed
//! as for the CLI (`omgcode_core::run::stop_withdrawn_runs`). A run not
//! observed to stop makes the command fail with `runs_not_stopped`, and the
//! change stays recorded.
//!
//! At most one change runs at a time in this service; a second one is refused
//! with `settings_change_running` instead of opening a second dialog.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use omgcode_core::control::{ControlError, ControlStore};
use omgcode_core::policy::PermissionSet;
use omgcode_core::run::{self, REVOCATION_WAIT};

use super::DesktopService;
use super::error::DesktopError;
use super::settings::overview_of;
use super::state::folder_of_directory;
use super::wire::{
    PermissionFlags, PermissionsChangeRequest, SettingsOverview, SettingsRevisionRequest,
    WorkspaceRevisionRequest,
};

/// What the operator is asked to choose a folder for. The shell uses it for
/// the dialog title.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FolderPurpose {
    /// The development root.
    DevelopmentRoot,
    /// A workspace to register below the development root.
    Workspace,
    /// One workspace's package cache root.
    PackageCache,
}

impl FolderPurpose {
    /// The fixed dialog title.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::DevelopmentRoot => "Choose the development root",
            Self::Workspace => "Choose a workspace folder",
            Self::PackageCache => "Choose a package cache folder",
        }
    }
}

/// One native confirmation. Every word is composed here from fixed text and
/// core's display-safe values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Confirmation {
    /// A fixed title.
    pub title: &'static str,
    /// The question and what it allows.
    pub message: String,
}

/// The operator's native dialogs, outside every webview.
///
/// The shell implements this with operating-system dialogs. Each call blocks
/// until the operator answers. An implementation that cannot show a dialog
/// must return `None` or `false`, so a failure never applies a change.
pub trait SettingsPrompt {
    /// Ask the operator to choose an existing folder, starting at `start`
    /// when given. `None` when the operator cancels or no dialog can be shown.
    fn pick_folder(&self, purpose: FolderPurpose, start: Option<&Path>) -> Option<PathBuf>;

    /// Ask the operator to confirm a change. `true` only on an explicit
    /// confirmation.
    fn confirm(&self, confirmation: &Confirmation) -> bool;
}

/// Holds the one-change-at-a-time flag and clears it on every path.
struct ChangeGuard<'a>(&'a AtomicBool);

impl<'a> ChangeGuard<'a> {
    fn take(flag: &'a AtomicBool) -> Result<Self, DesktopError> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self(flag))
            .map_err(|_| DesktopError::settings_change_running())
    }
}

impl Drop for ChangeGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// The preferences in display order, with what turning each on allows.
const FLAG_EFFECTS: [(&str, &str); 6] = [
    (
        "model tools",
        "chat offers the local model the workspace tools",
    ),
    ("read", "tools may read files in the workspace"),
    (
        "write",
        "approved model tools and trusted commands may change files in the workspace",
    ),
    (
        "execute",
        "the trusted runner and the approval-gated command tool may start programs in the workspace",
    ),
    ("task network", "a trusted run may select network access"),
    (
        "host services",
        "a trusted run may select host-service socket access (Linux)",
    ),
];

const fn flags_of(set: &PermissionSet) -> [bool; 6] {
    [
        set.model_tools,
        set.read,
        set.write,
        set.execute,
        set.task_network,
        set.host_services,
    ]
}

const fn permission_set(flags: PermissionFlags) -> PermissionSet {
    PermissionSet {
        read: flags.read,
        write: flags.write,
        execute: flags.execute,
        task_network: flags.task_network,
        model_tools: flags.model_tools,
        host_services: flags.host_services,
    }
}

/// The confirmation for turning preferences on, or `None` when none is
/// turned on.
fn raise_confirmation(
    folder: &str,
    current: &PermissionSet,
    wanted: &PermissionSet,
) -> Option<Confirmation> {
    let raised: Vec<(&str, &str)> = flags_of(current)
        .into_iter()
        .zip(flags_of(wanted))
        .zip(FLAG_EFFECTS)
        .filter(|((was, now), _)| !was && *now)
        .map(|(_, effect)| effect)
        .collect();
    if raised.is_empty() {
        return None;
    }
    let names: Vec<&str> = raised.iter().map(|(name, _)| *name).collect();
    let mut message = format!(
        "Turn on {} for this workspace?\n\n{folder}\n",
        names.join(", ")
    );
    for (name, effect) in &raised {
        message.push_str(&format!("\n- {name}: {effect}"));
    }
    Some(Confirmation {
        title: "Allow more access",
        message,
    })
}

fn check_revision(store: &ControlStore, expected: u64) -> Result<(), DesktopError> {
    if store.revision() == expected {
        Ok(())
    } else {
        Err(DesktopError::stale_revision())
    }
}

fn confirm(prompt: &dyn SettingsPrompt, confirmation: &Confirmation) -> Result<(), DesktopError> {
    if prompt.confirm(confirmation) {
        Ok(())
    } else {
        Err(DesktopError::cancelled())
    }
}

/// The display-safe text of a folder the operator chose.
fn shown(folder: &Path) -> String {
    folder_of_directory(Some(folder.to_path_buf()))
}

impl DesktopService {
    /// `settings` only. Ask the operator for a folder and, after a native
    /// confirmation, make it the development root.
    ///
    /// # Errors
    ///
    /// `cancelled` when the operator closes the dialog or declines;
    /// `stale_revision`; `settings_change_running`; `busy`; `control` with
    /// core's reason (for example an overlap with application storage);
    /// `runs_not_stopped`.
    pub fn settings_choose_root(
        &self,
        request: SettingsRevisionRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let start = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            store.development_root_path()
        };
        let folder = prompt
            .pick_folder(FolderPurpose::DevelopmentRoot, start.as_deref())
            .ok_or_else(DesktopError::cancelled)?;
        confirm(
            prompt,
            &Confirmation {
                title: "Set the development root",
                message: format!(
                    "Use this folder as the development root?\n\n{}\n\nWorkspaces must be inside it. Registered workspaces outside it can no longer be used.",
                    shown(&folder)
                ),
            },
        )?;
        self.apply(|store| store.set_development_root(&folder, request.revision))
    }

    /// `settings` only. Ask the operator for a folder below the development
    /// root and, after a native confirmation, register it as a workspace with
    /// the default preferences. Registering a folder again reuses its
    /// identifier and changes nothing.
    ///
    /// # Errors
    ///
    /// `no_development_root` before any dialog when no root is set; otherwise
    /// as [`settings_choose_root`](Self::settings_choose_root).
    pub fn settings_register_workspace(
        &self,
        request: SettingsRevisionRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let start = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            if !store.has_development_root() {
                return Err(DesktopError::from_control(&ControlError::NoRoot));
            }
            store.development_root_path()
        };
        let folder = prompt
            .pick_folder(FolderPurpose::Workspace, start.as_deref())
            .ok_or_else(DesktopError::cancelled)?;
        let defaults = PermissionSet::default();
        let on: Vec<&str> = flags_of(&defaults)
            .into_iter()
            .zip(FLAG_EFFECTS)
            .filter(|(enabled, _)| *enabled)
            .map(|(_, (name, _))| name)
            .collect();
        confirm(
            prompt,
            &Confirmation {
                title: "Register a workspace",
                message: format!(
                    "Register this folder as a workspace?\n\n{}\n\nA new workspace starts with {} on and everything else off.",
                    shown(&folder),
                    on.join(" and ")
                ),
            },
        )?;
        self.apply(|store| {
            store
                .register_workspace(&folder, request.revision)
                .map(drop)
        })
    }

    /// `settings` only. Set one workspace's complete preference set. When any
    /// preference is turned on, a native confirmation lists each one and what
    /// it allows; turning preferences off needs none.
    ///
    /// # Errors
    ///
    /// `unknown_workspace`; otherwise as
    /// [`settings_choose_root`](Self::settings_choose_root).
    pub fn settings_set_permissions(
        &self,
        request: PermissionsChangeRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let wanted = permission_set(request.permissions);
        let (workspace_id, confirmation) = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let workspace = store
                .registered_workspace(request.workspace_id)
                .ok_or_else(DesktopError::unknown_workspace)?;
            let folder = folder_of_directory(workspace.directory_path());
            (
                workspace.id(),
                raise_confirmation(&folder, &workspace.permissions(), &wanted),
            )
        };
        if let Some(confirmation) = confirmation {
            confirm(prompt, &confirmation)?;
        }
        self.apply(|store| store.set_permissions(workspace_id, wanted, request.revision))
    }

    /// `settings` only. Ask the operator for a folder and, after a native
    /// confirmation, make it one workspace's package cache root.
    ///
    /// # Errors
    ///
    /// `unknown_workspace`; otherwise as
    /// [`settings_choose_root`](Self::settings_choose_root).
    pub fn settings_choose_package_cache(
        &self,
        request: WorkspaceRevisionRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
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
            .pick_folder(FolderPurpose::PackageCache, None)
            .ok_or_else(DesktopError::cancelled)?;
        confirm(
            prompt,
            &Confirmation {
                title: "Set a package cache",
                message: format!(
                    "Use this folder as the package cache for the workspace\n{workspace_folder}?\n\n{}\n\nTrusted package fetches write to it, and runs in the workspace can read it.",
                    shown(&folder)
                ),
            },
        )?;
        self.apply(|store| store.set_package_cache_root(workspace_id, &folder, request.revision))
    }

    /// `settings` only. Clear one workspace's package cache root. Cached files
    /// stay in place. This narrows access, so no dialog is shown.
    ///
    /// # Errors
    ///
    /// `unknown_workspace`, `stale_revision`, `settings_change_running`,
    /// `busy`, `control`, `runs_not_stopped`.
    pub fn settings_clear_package_cache(
        &self,
        request: WorkspaceRevisionRequest,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        self.apply(|store| {
            let workspace_id = store
                .registered_workspace(request.workspace_id)
                .map(|workspace| workspace.id())
                .ok_or(ControlError::UnknownWorkspace)?;
            store.clear_package_cache_root(workspace_id, request.revision)
        })
    }

    /// Apply one change to a freshly opened store, release it, stop the runs
    /// that lost access, and return the new overview.
    fn apply(
        &self,
        change: impl FnOnce(&mut ControlStore) -> Result<(), ControlError>,
    ) -> Result<SettingsOverview, DesktopError> {
        {
            let mut store = self.inner.open_or_create_control()?;
            change(&mut store).map_err(|error| DesktopError::from_control(&error))?;
        }
        let withdrawn = run::stop_withdrawn_runs(&self.inner.data_root, REVOCATION_WAIT, &[])
            .map_err(|_| DesktopError::runs_unchecked())?;
        if !withdrawn.unconfirmed.is_empty() {
            return Err(DesktopError::runs_not_stopped(withdrawn.unconfirmed.len()));
        }
        let store = self.inner.open_or_create_control()?;
        Ok(overview_of(&self.inner, store))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(flags: [bool; 6]) -> PermissionSet {
        let [
            model_tools,
            read,
            write,
            execute,
            task_network,
            host_services,
        ] = flags;
        PermissionSet {
            read,
            write,
            execute,
            task_network,
            model_tools,
            host_services,
        }
    }

    #[test]
    fn turning_preferences_off_needs_no_confirmation() {
        let current = set([true, true, true, true, true, true]);
        let wanted = set([false, true, false, false, false, false]);
        assert_eq!(raise_confirmation("folder", &current, &wanted), None);
        assert_eq!(raise_confirmation("folder", &current, &current), None);
    }

    #[test]
    fn each_preference_turned_on_is_named_with_its_effect() {
        let current = set([false, true, true, false, false, false]);
        let wanted = set([true, true, false, true, false, true]);
        let confirmation = raise_confirmation("C:\\code\\app", &current, &wanted).expect("a raise");
        assert_eq!(confirmation.title, "Allow more access");
        assert!(
            confirmation
                .message
                .starts_with("Turn on model tools, execute, host services for this workspace?"),
            "{}",
            confirmation.message
        );
        assert!(confirmation.message.contains("C:\\code\\app"));
        assert!(
            confirmation
                .message
                .contains("- execute: the trusted runner")
        );
        // A preference turned off, or left on, is not listed.
        assert!(!confirmation.message.contains("- write"));
        assert!(!confirmation.message.contains("- read"));
        assert!(!confirmation.message.contains("- task network"));
    }

    #[test]
    fn the_guard_admits_one_change_and_clears_on_drop() {
        let flag = AtomicBool::new(false);
        let first = ChangeGuard::take(&flag).expect("first change");
        let second = ChangeGuard::take(&flag).err().expect("refused");
        assert_eq!(second.code(), "settings_change_running");
        drop(first);
        assert!(ChangeGuard::take(&flag).is_ok());
        assert!(!flag.load(Ordering::Acquire));
    }

    #[test]
    fn folder_titles_are_fixed() {
        assert_eq!(
            FolderPurpose::DevelopmentRoot.title(),
            "Choose the development root"
        );
        assert_eq!(
            FolderPurpose::Workspace.title(),
            "Choose a workspace folder"
        );
        assert_eq!(
            FolderPurpose::PackageCache.title(),
            "Choose a package cache folder"
        );
    }
}
