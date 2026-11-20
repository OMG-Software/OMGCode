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
//! - A change of where chat turns are sent is also confirmed (ADR-0064):
//!   choosing the active connection, from `settings` or the `main` status
//!   bar, and removing the active connection, which returns turns to the
//!   default endpoint.
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
    ControlError, ControlStore, CredentialStore, MAX_PROVIDER_ENDPOINT_BYTES, ProviderAuth,
    ProviderKind, ProviderProfileDeclaration, delete_unbound_credential,
};
use omgcode_core::policy::{Access, PermissionSet, WriteTool};
use omgcode_core::provider::OLLAMA_CLOUD_ENDPOINT;
use omgcode_core::run::{self, REVOCATION_WAIT};
use omgcode_core::turn::display_safe_line;

use super::DesktopService;
use super::error::DesktopError;
use super::session_connections::{ChosenConnection, SessionRecord, check_model, recorded_endpoint};
use super::settings::overview_of;
use super::state::folder_of_directory;
use super::wire::{
    ApiKeySetRequest, ConnectionAddRequest, ConnectionChooseRequest, ConnectionRemoveRequest,
    ConnectionSelectRequest, ConnectionStatus, ConnectionStatusRequest, NewConnectionKind,
    PermissionFlags, PermissionsChangeRequest, SessionModelSelectRequest, SettingsOverview,
    SettingsRevisionRequest, WorkspaceRevisionRequest,
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

/// How the confirmation names a provider.
const fn provider_label(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::OllamaLocal => "Ollama",
        ProviderKind::OllamaCloud => "Ollama Cloud",
        _ => "LM Studio",
    }
}

/// What the confirmation adds for the hosted service. Prompts, tool results,
/// and command output are the model's whole view of the workspace, so the
/// operator is told before a turn can send them off this computer (ADR-0067).
const CLOUD_NOTICE: &str = "This is Ollama's hosted service. Prompts, workspace text read by tools, and command output leave this computer for ollama.com.";

/// The confirmation for choosing a recorded profile. `subject` says whose chat
/// turns are sent there. Every word is fixed text or a display-safe value.
fn profile_confirmation(
    subject: &str,
    kind: ProviderKind,
    name: &str,
    endpoint: &str,
    credential_bound: bool,
) -> Confirmation {
    let hosted = if kind == ProviderKind::OllamaCloud {
        format!("\n\n{CLOUD_NOTICE}")
    } else {
        String::new()
    };
    let credential = if credential_bound {
        "\n\nIts stored credential is sent with each request."
    } else {
        ""
    };
    Confirmation {
        title: "Choose the model connection",
        message: format!(
            "Send {subject} to {} through the connection {}?\n\n{}\n\nEach turn's messages, instructions, selected skills, and tool results go to this server.{hosted}{credential}",
            provider_label(kind),
            display_safe_line(name),
            display_safe_line(endpoint),
        ),
    }
}

/// The confirmation for choosing the global default connection, composed from
/// fixed text and core's display-safe values. A choice core would refuse is
/// refused here first, with core's own error, so no dialog is shown for it.
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
                "Send chat turns of sessions that have no connection of their own to the default connection?\n\n{}\n\nThe OMGCODE_LM_TOKEN environment variable is sent to it when set.",
                display_safe_line(default_endpoint)
            ),
        });
    };
    let status = store.check_provider_selectable(name)?;
    let profile = status.profile();
    let endpoint = profile.endpoint().ok_or(ControlError::ProviderRefused(
        "only a profile with a recorded endpoint can be the active connection",
    ))?;
    if current == Some(profile.id()) {
        return Err(ControlError::ProviderRefused(
            "this is already the active connection",
        ));
    }
    Ok(profile_confirmation(
        "chat turns of sessions that have no connection of their own",
        profile.kind(),
        profile.name(),
        endpoint.as_str(),
        status.credential().is_some(),
    ))
}

/// A connection chosen for one session, as it is recorded after Yes.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Choice {
    /// The launch default endpoint with `OMGCODE_LM_TOKEN`.
    Default,
    /// A recorded profile and the endpoint shown in the confirmation.
    Profile { name: String, endpoint: String },
}

impl Choice {
    fn record(&self, default_endpoint: &str) -> ChosenConnection {
        match self {
            Self::Default => ChosenConnection::Default {
                endpoint: default_endpoint.to_owned(),
            },
            Self::Profile { name, endpoint } => ChosenConnection::Profile {
                name: name.clone(),
                endpoint: endpoint.clone(),
            },
        }
    }
}

/// The choice naming `name` and the confirmation for it, from an open store.
/// Core refuses an unknown or unselectable profile, with its own error.
fn requested_choice(
    store: Option<&ControlStore>,
    name: Option<&str>,
    default_endpoint: &str,
) -> Result<(Choice, Confirmation), ControlError> {
    let Some(name) = name else {
        return Ok((
            Choice::Default,
            Confirmation {
                title: "Choose the model connection",
                message: format!(
                    "Send this session's chat turns to the default connection?\n\n{}\n\nThe OMGCODE_LM_TOKEN environment variable is sent to it when set.",
                    display_safe_line(default_endpoint)
                ),
            },
        ));
    };
    let store = store.ok_or(ControlError::UnknownProvider)?;
    let status = store.check_provider_selectable(name)?;
    let endpoint = recorded_endpoint(store, name).ok_or(ControlError::ProviderRefused(
        "the recorded endpoint cannot be used",
    ))?;
    let confirmation = profile_confirmation(
        "this session's chat turns",
        status.profile().kind(),
        status.profile().name(),
        &endpoint,
        status.credential().is_some(),
    );
    Ok((
        Choice::Profile {
            name: status.profile().name().to_owned(),
            endpoint,
        },
        confirmation,
    ))
}

/// The choice a session currently has, or `None` when what is recorded no
/// longer holds, so choosing it again is a change that is confirmed.
fn current_choice(
    store: Option<&ControlStore>,
    record: Option<&SessionRecord>,
    default_endpoint: &str,
) -> Option<Choice> {
    match record.map(|record| &record.connection) {
        Some(ChosenConnection::Default { endpoint }) => {
            (endpoint == default_endpoint).then_some(Choice::Default)
        }
        Some(ChosenConnection::Profile { name, endpoint }) => {
            let store = store?;
            store.check_provider_selectable(name).ok()?;
            (recorded_endpoint(store, name).as_deref() == Some(endpoint.as_str())).then(|| {
                Choice::Profile {
                    name: name.clone(),
                    endpoint: endpoint.clone(),
                }
            })
        }
        None | Some(ChosenConnection::Follow {}) => {
            let name = store?
                .active_provider_profile()
                .map(|status| status.profile().name().to_owned());
            match name {
                None => Some(Choice::Default),
                Some(name) => {
                    let endpoint = recorded_endpoint(store?, &name)?;
                    Some(Choice::Profile { name, endpoint })
                }
            }
        }
    }
}

/// Undo a profile recorded for a key that could not be stored: remove it, and
/// delete a binding it gained if the binding was recorded after all. `true`
/// when the profile is gone and no bound item remains.
fn roll_back_profile(store: &mut ControlStore, credentials: &CredentialStore, name: &str) -> bool {
    let revision = store.revision();
    match store.remove_provider_profile(name, revision) {
        Ok(removal) => removal
            .credential
            .as_ref()
            .is_none_or(|binding| delete_unbound_credential(credentials, binding).is_none()),
        Err(_) => false,
    }
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

    /// `settings` only. Choose the global default model connection: a
    /// selectable recorded profile by name (local, or `ollama_cloud` with a
    /// bound credential), or `None` for the default endpoint (ADR-0064,
    /// ADR-0067). It applies to sessions that have no connection of their own. A
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

    /// `main` only: the composer's and status bar's choice of model connection
    /// for one session (ADR-0064, ADR-0067).
    ///
    /// The window names a session and a profile, or none for the default, and
    /// nothing else. Every refusal comes before any dialog: a launch argument
    /// that fixed the connection, an unknown session, a profile core would
    /// not accept, and a choice the session already has. Rust then shows the
    /// native Yes/No confirmation naming the provider and endpoint (and, for
    /// Ollama Cloud, that prompts and tool output leave this computer), which
    /// no webview can answer. Only on Yes is the choice recorded for the
    /// session, and its model is cleared because model ids belong to one
    /// server. The settings store is read again after Yes, so a profile
    /// removed, or given another endpoint, while the dialog was open records
    /// nothing. No settings revision is involved: the global selection is not
    /// changed.
    ///
    /// # Errors
    ///
    /// `connection_fixed`; `not_found`; `control` for an unknown, unselectable,
    /// or unchanged choice; `cancelled`; `stale_revision` when the profile
    /// changed during the dialog; `settings_change_running`; `busy`;
    /// `session_choices` when the choice cannot be saved.
    pub fn connection_select(
        &self,
        request: ConnectionChooseRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<ConnectionStatus, DesktopError> {
        let inner = &self.inner;
        let _guard = ChangeGuard::take(&inner.settings_change)?;
        if inner.token.is_fixed() {
            return Err(DesktopError::connection_fixed());
        }
        let session_id = request.session_id;
        inner.resolve_path(session_id)?;
        let (records, _) = inner.session_records();
        let (choice, confirmation) = {
            let store = inner.open_control()?;
            let requested =
                requested_choice(store.as_ref(), request.profile.as_deref(), &inner.endpoint)
                    .map_err(|error| DesktopError::from_control(&error))?;
            let current = current_choice(store.as_ref(), records.get(&session_id), &inner.endpoint);
            if current.as_ref() == Some(&requested.0) {
                return Err(DesktopError::from_control(&ControlError::ProviderRefused(
                    "this is already the connection of this session",
                )));
            }
            requested
        };
        confirm(prompt, &confirmation)?;
        // What was confirmed must still be what is recorded.
        let again = {
            let store = inner.open_control()?;
            requested_choice(store.as_ref(), request.profile.as_deref(), &inner.endpoint)
                .map_err(|error| DesktopError::from_control(&error))?
        };
        if again.0 != choice {
            return Err(DesktopError::stale_revision());
        }
        inner.update_session_record(session_id, |_| SessionRecord {
            connection: choice.record(&inner.endpoint),
            model: String::new(),
        })?;
        self.connection_status(ConnectionStatusRequest {
            session_id: Some(session_id),
        })
    }

    /// `main` only: record the model chosen for one session (ADR-0067). No
    /// dialog is shown: a model grants nothing, and the server still decides
    /// whether it exists. The model id passes the rule `chat_send` applies (at
    /// most 256 bytes, no control character); an empty id clears the choice. A
    /// session with no record gets one that follows the default connection, so
    /// choosing a model never pins a provider the operator did not confirm.
    ///
    /// # Errors
    ///
    /// `invalid_request` for a model name that is not valid; `not_found`;
    /// `session_choices` when the choice cannot be saved.
    pub fn session_model_select(
        &self,
        request: SessionModelSelectRequest,
    ) -> Result<ConnectionStatus, DesktopError> {
        check_model(&request.model)?;
        self.inner.resolve_path(request.session_id)?;
        self.inner
            .update_session_record(request.session_id, |existing| SessionRecord {
                connection: existing.map_or(ChosenConnection::Follow {}, |record| {
                    record.connection.clone()
                }),
                model: request.model,
            })?;
        self.connection_status(ConnectionStatusRequest {
            session_id: Some(request.session_id),
        })
    }

    /// `settings` only. Record a connection through the same core call as the
    /// CLI's `provider-profile-add`: a local LM Studio or Ollama connection
    /// with no credential and a typed loopback endpoint, or Ollama Cloud with
    /// its fixed endpoint and an API key (ADR-0067 decision 5). Recording it
    /// grants nothing and sends nothing; only selecting it does, after a
    /// confirmation. No dialog is shown.
    ///
    /// A local kind needs `endpoint` and no `api_key`; `ollama_cloud` needs an
    /// `api_key` and no `endpoint`. Either mismatch is refused as
    /// `invalid_request` before anything is read or recorded.
    ///
    /// For Ollama Cloud the key is moved into core's zeroing `Secret`
    /// immediately and checked as the CLI checks it (non-empty, at most 16
    /// KiB). The native credential store is opened first, so an unavailable
    /// or locked store refuses the request before any profile exists. A
    /// profile cannot be bound before it exists, so the profile is recorded
    /// first, then the key is stored and bound through
    /// `ControlStore::set_provider_credential`. If that fails, the profile is
    /// removed again in the same locked store (and any binding it gained is
    /// deleted), so a failure leaves no profile without its key. The
    /// settings revision still advances by the two recorded events. If the
    /// profile cannot be removed either, the error is `credential_orphaned`.
    /// The key, its length, and a digest of it appear in no response or error.
    ///
    /// # Errors
    ///
    /// `invalid_request` for an oversized value, a mismatched field, or a key
    /// that is empty, too long, or has a control character; `control` with
    /// core's reason (a bad name, a duplicate, or an endpoint that is not
    /// `http://<loopback IP>:<port>/v1`); `credential_unavailable`,
    /// `credential_locked`, `credential_io`, `credential_orphaned`;
    /// `stale_revision`; `settings_change_running`; `busy`.
    pub fn settings_add_connection(
        &self,
        request: ConnectionAddRequest,
    ) -> Result<SettingsOverview, DesktopError> {
        let ConnectionAddRequest {
            revision,
            name,
            kind,
            endpoint,
            api_key,
        } = request;
        if name.len() > MAX_NAME_INPUT_BYTES
            || endpoint
                .as_ref()
                .is_some_and(|text| text.len() > MAX_PROVIDER_ENDPOINT_BYTES)
        {
            return Err(DesktopError::invalid_request("A value is too long."));
        }
        let (kind, endpoint, auth, secret) = match (kind, endpoint, api_key) {
            (NewConnectionKind::OllamaCloud, None, Some(key)) => {
                let secret = key
                    .into_secret()
                    .ok_or_else(DesktopError::api_key_invalid)?;
                (
                    ProviderKind::OllamaCloud,
                    OLLAMA_CLOUD_ENDPOINT.to_owned(),
                    ProviderAuth::ApiKey,
                    Some(secret),
                )
            }
            (NewConnectionKind::OllamaCloud, _, _) => {
                return Err(DesktopError::invalid_request(
                    "Ollama Cloud takes an API key and no endpoint.",
                ));
            }
            (local, Some(endpoint), None) => {
                let kind = match local {
                    NewConnectionKind::LmStudio => ProviderKind::LmStudio,
                    _ => ProviderKind::OllamaLocal,
                };
                (kind, endpoint, ProviderAuth::None, None)
            }
            (_, _, _) => {
                return Err(DesktopError::invalid_request(
                    "A local connection takes an endpoint and no API key.",
                ));
            }
        };
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let credentials = if secret.is_some() {
            Some(
                CredentialStore::open(&self.inner.data_root)
                    .map_err(|error| DesktopError::from_credential(&error))?,
            )
        } else {
            None
        };
        let declaration = ProviderProfileDeclaration {
            name,
            kind: kind.as_str().to_owned(),
            auth: auth.as_str().to_owned(),
            endpoint: Some(endpoint),
        };
        self.apply_checked(|store| {
            store
                .add_provider_profile(&declaration, revision)
                .map_err(|error| DesktopError::from_control(&error))?;
            let (Some(credentials), Some(secret)) = (credentials, secret) else {
                return Ok(());
            };
            let current = store.revision();
            match store.set_provider_credential(&credentials, &declaration.name, &secret, current) {
                Ok(set) if set.orphan.is_none() => Ok(()),
                Ok(_) => Err(DesktopError::credential_orphaned()),
                Err(error) => {
                    let clean = roll_back_profile(store, &credentials, &declaration.name);
                    Err(if clean {
                        DesktopError::from_provider_credential(&error)
                    } else {
                        DesktopError::credential_orphaned()
                    })
                }
            }
        })
    }

    /// `settings` only. Replace the stored API key of a recorded
    /// `ollama_cloud` profile through `ControlStore::set_provider_credential`,
    /// as the CLI's `provider-credential-set` does: the new item is stored,
    /// then bound, then the replaced item is deleted. The key is handled as in
    /// [`settings_add_connection`](Self::settings_add_connection).
    ///
    /// No dialog is shown (ADR-0059 decision 6 asks for none). The key goes
    /// only to the same fixed host the connection already names, so the
    /// audience of a turn is unchanged; the profile's endpoint is not
    /// editable, and choosing the connection stays confirmed separately.
    ///
    /// # Errors
    ///
    /// `invalid_request` for a profile that is not an `ollama_cloud` profile
    /// or a key that is empty, too long, or has a control character; `control`
    /// for an unknown profile; `credential_unavailable`, `credential_locked`,
    /// `credential_io`, `credential_orphaned` (the new key is bound but the
    /// replaced item could not be deleted); `stale_revision`;
    /// `settings_change_running`; `busy`.
    pub fn settings_set_api_key(
        &self,
        request: ApiKeySetRequest,
    ) -> Result<SettingsOverview, DesktopError> {
        let ApiKeySetRequest {
            revision,
            name,
            api_key,
        } = request;
        if name.len() > MAX_NAME_INPUT_BYTES {
            return Err(DesktopError::invalid_request("A value is too long."));
        }
        let secret = api_key
            .into_secret()
            .ok_or_else(DesktopError::api_key_invalid)?;
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let credentials = CredentialStore::open(&self.inner.data_root)
            .map_err(|error| DesktopError::from_credential(&error))?;
        self.apply_checked(|store| {
            let status = store
                .provider_profile(&name)
                .ok_or_else(|| DesktopError::from_control(&ControlError::UnknownProvider))?;
            if status.profile().kind() != ProviderKind::OllamaCloud {
                return Err(DesktopError::invalid_request(
                    "Only an Ollama Cloud connection has an API key set here.",
                ));
            }
            match store.set_provider_credential(&credentials, &name, &secret, revision) {
                Ok(set) if set.orphan.is_none() => Ok(()),
                Ok(_) => Err(DesktopError::credential_orphaned()),
                Err(error) => Err(DesktopError::from_provider_credential(&error)),
            }
        })
    }

    /// `settings` only. Remove a recorded provider profile. A profile with a
    /// stored credential is removed together with it, as the CLI's
    /// `provider-profile-remove` does: core records the removal and the end of
    /// the binding in one event, then the native item is deleted.
    ///
    /// A profile with a stored key first asks a native Yes/No that says the
    /// connection and its stored API key will be deleted, and, when it is the
    /// active selection and a launch argument did not fix the connection,
    /// that new turns for sessions without their own connection go to the
    /// default endpoint. Removing an active profile without a key asks the
    /// ADR-0064 Yes/No about the default endpoint. Removing any other profile
    /// changes where no turn goes, so no dialog is shown.
    ///
    /// A session that chose the removed profile refuses its next turn with
    /// `session_connection_stale` (ADR-0067), because core no longer knows the
    /// profile.
    ///
    /// # Errors
    ///
    /// `control` for an unknown profile; `cancelled`; `stale_revision`;
    /// `credential_unavailable` or `credential_locked` (the native store is
    /// checked before anything is removed); `credential_orphaned` (the
    /// profile is removed but its native item was not deleted);
    /// `settings_change_running`; `busy`.
    pub fn settings_remove_connection(
        &self,
        request: ConnectionRemoveRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let (confirmation, bound) = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let status = store
                .provider_profile(&request.name)
                .ok_or_else(|| DesktopError::from_control(&ControlError::UnknownProvider))?;
            let bound = status.credential().is_some();
            let active = store
                .active_provider_profile()
                .is_some_and(|active| active.profile().id() == status.profile().id());
            let default_note = if active && !self.inner.token.is_fixed() {
                Some(format!(
                    "New chat turns for sessions without their own connection go to the default connection {}, which receives OMGCODE_LM_TOKEN when it is set.",
                    display_safe_line(&self.inner.endpoint)
                ))
            } else {
                None
            };
            let confirmation = match (bound, default_note) {
                (true, note) => Some(Confirmation {
                    title: "Remove the model connection and its API key",
                    message: format!(
                        "Remove the connection {} and delete its stored API key?\n\nThe key is deleted from this computer's credential store and cannot be recovered here.{}",
                        display_safe_line(&request.name),
                        note.map(|text| format!("\n\n{text}")).unwrap_or_default(),
                    ),
                }),
                (false, Some(note)) => Some(Confirmation {
                    title: "Remove the active model connection",
                    message: format!(
                        "Remove the connection {}?\n\n{note}",
                        display_safe_line(&request.name),
                    ),
                }),
                (false, None) => None,
            };
            (confirmation, bound)
        };
        if let Some(confirmation) = confirmation {
            confirm(prompt, &confirmation)?;
        }
        // Open the native store before anything is removed, so an unavailable
        // or locked store refuses the removal instead of stranding the item.
        let credentials = if bound {
            Some(
                CredentialStore::open(&self.inner.data_root)
                    .map_err(|error| DesktopError::from_credential(&error))?,
            )
        } else {
            None
        };
        // Core checks the same revision again, so a credential bound after the
        // check above makes the removal a revision conflict, not a removal
        // that strands the native item.
        self.apply_checked(|store| {
            let removal = store
                .remove_provider_profile(&request.name, request.revision)
                .map_err(|error| DesktopError::from_control(&error))?;
            match (&removal.credential, &credentials) {
                (Some(binding), Some(credentials)) => {
                    match delete_unbound_credential(credentials, binding) {
                        None => Ok(()),
                        Some(_) => Err(DesktopError::credential_orphaned()),
                    }
                }
                (Some(_), None) => Err(DesktopError::credential_orphaned()),
                (None, _) => Ok(()),
            }
        })
    }

    /// As [`apply_checked`](Self::apply_checked) for a change that fails only
    /// with a core settings error.
    fn apply(
        &self,
        change: impl FnOnce(&mut ControlStore) -> Result<(), ControlError>,
    ) -> Result<SettingsOverview, DesktopError> {
        self.apply_checked(|store| {
            change(store).map_err(|error| DesktopError::from_control(&error))
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
    fn apply_checked(
        &self,
        change: impl FnOnce(&mut ControlStore) -> Result<(), DesktopError>,
    ) -> Result<SettingsOverview, DesktopError> {
        let listed = {
            let mut store = self.inner.open_or_create_control()?;
            change(&mut store)?;
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
