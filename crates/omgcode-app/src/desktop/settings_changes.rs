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

use omgcode_core::control::{
    ControlError, ControlStore, MAX_PROVIDER_ENDPOINT_BYTES, ProviderAuth, ProviderKind,
    ProviderProfileDeclaration,
};
use omgcode_core::policy::{Access, PermissionSet, WriteTool};
use omgcode_core::run::{self, REVOCATION_WAIT};
use omgcode_core::turn::display_safe_line;

use super::DesktopService;
use super::error::DesktopError;
use super::settings::overview_of;
use super::state::folder_of_directory;
use super::wire::{
    ConnectionRemoveRequest, ConnectionSelectRequest, LocalConnectionAddRequest, LocalProviderKind,
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

/// One preference as the confirmation names it: its label, what permitting
/// it lets happen, and its state. `model_tools` is shown as `deny` or `allow`.
struct Preference {
    name: &'static str,
    effect: &'static str,
    state: Access,
}

/// Every preference in display order, including the per-tool write limits.
fn preferences_of(set: &PermissionSet) -> [Preference; 11] {
    let tool = |name, effect, tool| Preference {
        name,
        effect,
        state: set.write_tools.get(tool),
    };
    [
        Preference {
            name: "model tools",
            effect: "chat offers the local model the workspace tools",
            state: if set.model_tools {
                Access::Allow
            } else {
                Access::Deny
            },
        },
        Preference {
            name: "read",
            effect: "tools may read files in the workspace",
            state: set.read,
        },
        Preference {
            name: "write",
            effect: "model tools and trusted commands may change files in the workspace",
            state: set.write,
        },
        Preference {
            name: "execute",
            effect: "the trusted runner and the model command tool may start programs in the workspace",
            state: set.execute,
        },
        Preference {
            name: "task network",
            effect: "a trusted run may select network access",
            state: set.task_network,
        },
        Preference {
            name: "host services",
            effect: "a trusted run may select host-service socket access (Linux)",
            state: set.host_services,
        },
        tool(
            "edit tool",
            "the model may edit existing files",
            WriteTool::Edit,
        ),
        tool(
            "multi-edit tool",
            "the model may make several edits to one file",
            WriteTool::MultiEdit,
        ),
        tool(
            "create tool",
            "the model may create new files",
            WriteTool::Create,
        ),
        tool(
            "rename tool",
            "the model may rename files",
            WriteTool::Rename,
        ),
        tool(
            "delete tool",
            "the model may delete files (each is checkpointed first)",
            WriteTool::Delete,
        ),
    ]
}

/// What a widened state adds, for the confirmation text.
const fn widened_meaning(state: Access) -> &'static str {
    match state {
        Access::Deny => "",
        Access::Prompt => "each model call asks you first",
        Access::Allow => "model calls run without asking you",
    }
}

const fn permission_set(flags: PermissionFlags) -> PermissionSet {
    PermissionSet {
        read: flags.read,
        write: flags.write,
        execute: flags.execute,
        task_network: flags.task_network,
        model_tools: flags.model_tools,
        host_services: flags.host_services,
        write_tools: flags.write_tools,
    }
}

/// The confirmation for widening preferences, or `None` when nothing is
/// widened. A preference widens when its state moves towards `allow`, from
/// `deny` to `prompt` or `allow`, or from `prompt` to `allow`.
fn raise_confirmation(
    folder: &str,
    current: &PermissionSet,
    wanted: &PermissionSet,
) -> Option<Confirmation> {
    let raised: Vec<(Preference, Access)> = preferences_of(current)
        .into_iter()
        .zip(preferences_of(wanted))
        .filter(|(was, now)| now.state > was.state)
        .map(|(was, now)| (now, was.state))
        .collect();
    if raised.is_empty() {
        return None;
    }
    let names: Vec<&str> = raised.iter().map(|(now, _)| now.name).collect();
    let mut message = format!(
        "Widen {} for this workspace?\n\n{folder}\n",
        names.join(", ")
    );
    for (now, was) in &raised {
        message.push_str(&format!(
            "\n- {}: {} to {}. {}",
            now.name,
            was.as_str(),
            now.state.as_str(),
            now.effect
        ));
        let meaning = widened_meaning(now.state);
        if now.name != "model tools" && !meaning.is_empty() {
            message.push_str(&format!("; {meaning}"));
        }
        message.push('.');
    }
    Some(Confirmation {
        title: "Allow more access",
        message,
    })
}

/// Largest profile name accepted before core applies its own, smaller bound.
const MAX_NAME_INPUT_BYTES: usize = 256;

/// How the confirmation names a local provider.
const fn provider_label(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::OllamaLocal => "Ollama",
        _ => "LM Studio",
    }
}

/// The confirmation for choosing the active connection, composed from fixed
/// text and core's display-safe values. A choice core would refuse is refused
/// here first, with core's own error, so no dialog is shown for it.
fn selection_confirmation(
    store: &ControlStore,
    name: Option<&str>,
    default_endpoint: &str,
) -> Result<Confirmation, ControlError> {
    let current = store
        .active_provider_profile()
        .map(|status| status.profile().id());
    let Some(name) = name else {
        if current.is_none() {
            return Err(ControlError::ProviderRefused(
                "this is already the active connection",
            ));
        }
        return Ok(Confirmation {
            title: "Choose the model connection",
            message: format!(
                "Send new chat turns to the default connection?\n\n{}\n\nThe OMGCODE_LM_TOKEN environment variable is sent to it when set.",
                display_safe_line(default_endpoint)
            ),
        });
    };
    let status = store
        .provider_profile(name)
        .ok_or(ControlError::UnknownProvider)?;
    let profile = status.profile();
    let Some(endpoint) = profile.endpoint().filter(|_| profile.kind().is_local()) else {
        return Err(ControlError::ProviderRefused(
            "only an lm_studio or ollama_local profile with a recorded endpoint can be the active connection",
        ));
    };
    if current == Some(profile.id()) {
        return Err(ControlError::ProviderRefused(
            "this is already the active connection",
        ));
    }
    let credential = if status.credential().is_some() {
        "\n\nIts stored credential is sent with each request."
    } else {
        ""
    };
    Ok(Confirmation {
        title: "Choose the model connection",
        message: format!(
            "Send new chat turns to {} through the connection {}?\n\n{}\n\nEach turn's messages, instructions, selected skills, and tool results go to this server.{credential}",
            provider_label(profile.kind()),
            display_safe_line(profile.name()),
            display_safe_line(endpoint.as_str()),
        ),
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
        let on: Vec<String> = preferences_of(&defaults)
            .into_iter()
            .take(6)
            .filter(|preference| preference.state.permits())
            .map(|preference| format!("{} ({})", preference.name, preference.state.as_str()))
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

    /// `settings` only. Choose the active model connection: a recorded local
    /// profile by name, or `None` for the default endpoint (ADR-0064). A
    /// native confirmation names the provider and endpoint first, because the
    /// choice decides where chat turns send their messages.
    ///
    /// It applies to turns that start after it is recorded. A `main` window
    /// that still shows the earlier endpoint has its next send refused with
    /// `connection_changed`.
    ///
    /// # Errors
    ///
    /// `connection_fixed` when a launch argument fixed the connection;
    /// `control` with core's reason for an unknown, unselectable, or
    /// unchanged choice; otherwise as
    /// [`settings_choose_root`](Self::settings_choose_root).
    pub fn settings_select_connection(
        &self,
        request: ConnectionSelectRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        if self.inner.token.is_fixed() {
            return Err(DesktopError::connection_fixed());
        }
        let confirmation = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            selection_confirmation(&store, request.profile.as_deref(), &self.inner.endpoint)
                .map_err(|error| DesktopError::from_control(&error))?
        };
        confirm(prompt, &confirmation)?;
        self.apply(|store| {
            store.select_active_provider(request.profile.as_deref(), request.revision)
        })
    }

    /// `settings` only. Record a local LM Studio or Ollama connection with no
    /// credential, through the same core call as the CLI's
    /// `provider-profile-add --auth none --endpoint`. Recording it grants
    /// nothing and sends nothing; only selecting it does, after a
    /// confirmation.
    ///
    /// # Errors
    ///
    /// `invalid_request` for an oversized value; `control` with core's reason
    /// (a bad name, a duplicate, or an endpoint that is not
    /// `http://<loopback IP>:<port>/v1`); `stale_revision`;
    /// `settings_change_running`; `busy`.
    pub fn settings_add_local_connection(
        &self,
        request: LocalConnectionAddRequest,
    ) -> Result<SettingsOverview, DesktopError> {
        if request.name.len() > MAX_NAME_INPUT_BYTES
            || request.endpoint.len() > MAX_PROVIDER_ENDPOINT_BYTES
        {
            return Err(DesktopError::invalid_request("A value is too long."));
        }
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let kind = match request.kind {
            LocalProviderKind::LmStudio => ProviderKind::LmStudio,
            LocalProviderKind::OllamaLocal => ProviderKind::OllamaLocal,
        };
        let declaration = ProviderProfileDeclaration {
            name: request.name,
            kind: kind.as_str().to_owned(),
            auth: ProviderAuth::None.as_str().to_owned(),
            endpoint: Some(request.endpoint),
        };
        self.apply(|store| {
            store
                .add_provider_profile(&declaration, request.revision)
                .map(drop)
        })
    }

    /// `settings` only. Remove a recorded provider profile that has no stored
    /// credential. Removing the active connection returns to the default
    /// endpoint in the same recorded event. This narrows access, so no native
    /// dialog is shown. A profile with a bound credential stays with the
    /// trusted CLI, which also deletes the native item.
    ///
    /// # Errors
    ///
    /// `credential_bound`; `control` for an unknown profile;
    /// `stale_revision`; `settings_change_running`; `busy`.
    pub fn settings_remove_connection(
        &self,
        request: ConnectionRemoveRequest,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let status = store
                .provider_profile(&request.name)
                .ok_or_else(|| DesktopError::from_control(&ControlError::UnknownProvider))?;
            if status.credential().is_some() {
                return Err(DesktopError::credential_bound());
            }
        }
        // Core checks the same revision again, so a credential bound after the
        // check above makes the removal a revision conflict, not a removal
        // that strands the native item.
        self.apply(|store| {
            store
                .remove_provider_profile(&request.name, request.revision)
                .map(drop)
        })
    }

    /// Apply one change to a freshly opened store, list the runs it withdrew
    /// while the store is still held, release it, wait for those runs and any
    /// others the settings withdraw to stop, and return the new overview.
    ///
    /// The runs are listed under the lock because runs start only under it,
    /// and a change that restores access after the release cannot then hide
    /// them from the wait. Their watchers stop them even then, because a
    /// watcher checks every settings state recorded since it last read them
    /// (ADR-0061).
    fn apply(
        &self,
        change: impl FnOnce(&mut ControlStore) -> Result<(), ControlError>,
    ) -> Result<SettingsOverview, DesktopError> {
        let listed = {
            let mut store = self.inner.open_or_create_control()?;
            change(&mut store).map_err(|error| DesktopError::from_control(&error))?;
            run::withdrawn_runs(&store)
        };
        // An unreadable run journal never undoes the recorded change, and the
        // watchers still stop the runs: wait for what the settings now
        // withdraw, then report that the runs were not all checked.
        let named = listed.as_deref().unwrap_or(&[]);
        let withdrawn = run::stop_withdrawn_runs(&self.inner.data_root, REVOCATION_WAIT, named)
            .map_err(|_| DesktopError::runs_unchecked())?;
        if !withdrawn.unconfirmed.is_empty() {
            return Err(DesktopError::runs_not_stopped(withdrawn.unconfirmed.len()));
        }
        if listed.is_err() {
            return Err(DesktopError::runs_unchecked());
        }
        let store = self.inner.open_or_create_control()?;
        Ok(overview_of(&self.inner, store))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(model_tools: bool, states: [Access; 5]) -> PermissionSet {
        let [read, write, execute, task_network, host_services] = states;
        PermissionSet {
            read,
            write,
            execute,
            task_network,
            model_tools,
            host_services,
            write_tools: WriteToolAccess::default(),
        }
    }

    use Access::{Allow, Deny, Prompt};
    use omgcode_core::policy::WriteToolAccess;

    #[test]
    fn narrowing_preferences_needs_no_confirmation() {
        let current = set(true, [Allow, Allow, Allow, Allow, Allow]);
        let wanted = set(false, [Allow, Prompt, Deny, Deny, Prompt]);
        assert_eq!(raise_confirmation("folder", &current, &wanted), None);
        assert_eq!(raise_confirmation("folder", &current, &current), None);
        let mut limited = current;
        limited.write_tools.set(WriteTool::Delete, Prompt);
        assert_eq!(raise_confirmation("folder", &current, &limited), None);
    }

    #[test]
    fn each_widened_preference_is_named_with_both_states() {
        let current = set(false, [Allow, Prompt, Deny, Deny, Deny]);
        let wanted = set(true, [Allow, Allow, Prompt, Deny, Allow]);
        let confirmation = raise_confirmation("C:\\code\\app", &current, &wanted).expect("a raise");
        assert_eq!(confirmation.title, "Allow more access");
        assert!(
            confirmation.message.starts_with(
                "Widen model tools, write, execute, host services for this workspace?"
            ),
            "{}",
            confirmation.message
        );
        assert!(confirmation.message.contains("C:\\code\\app"));
        assert!(
            confirmation
                .message
                .contains("- write: prompt to allow. model tools and trusted commands may change files in the workspace; model calls run without asking you."),
            "{}",
            confirmation.message
        );
        assert!(
            confirmation
                .message
                .contains("- execute: deny to prompt. the trusted runner"),
            "{}",
            confirmation.message
        );
        // A preference narrowed, or left alone, is not listed.
        assert!(!confirmation.message.contains("- read"));
        assert!(!confirmation.message.contains("- task network"));
    }

    #[test]
    fn widening_a_tool_limit_is_confirmed() {
        let mut current = set(true, [Allow, Allow, Deny, Deny, Deny]);
        current.write_tools.set(WriteTool::Delete, Prompt);
        let mut wanted = current;
        wanted.write_tools.set(WriteTool::Delete, Allow);
        let confirmation = raise_confirmation("folder", &current, &wanted).expect("a raise");
        assert!(
            confirmation
                .message
                .contains("- delete tool: prompt to allow.")
        );
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
