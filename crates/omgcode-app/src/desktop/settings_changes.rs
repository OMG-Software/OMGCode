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
    AssignmentFailure, CLAUDE_CODE_TURN_STOP_WAIT, CONTAINER_GRANTS_SUPPORTED, ControlError,
    ControlStore, CredentialId, CredentialStore, FolderGrant, MAX_PROVIDER_ENDPOINT_BYTES,
    ProviderAuth, ProviderCredentialError, ProviderKind, ProviderProfileDeclaration, Revocations,
    SkippedGrant, delete_unbound_credential, wait_for_claude_code_turns,
};
use omgcode_core::policy::{Access, PermissionSet, WorkspaceId, WriteTool};
use omgcode_core::provider::ModelEndpoint;
use omgcode_core::run::{self, REVOCATION_WAIT};
use omgcode_core::turn::display_safe_line;

use super::DesktopService;
use super::error::DesktopError;
use super::instructions;
use super::session_connections::{ChosenConnection, SessionRecord, check_model, recorded_endpoint};
use super::settings::overview_of;
use super::state::folder_of_directory;
use super::wire::{
    ApiKeySetRequest, ConnectionAddRequest, ConnectionChooseRequest, ConnectionRemoveRequest,
    ConnectionSelectRequest, ConnectionStatus, ConnectionStatusRequest, PermissionFlags,
    PermissionsChangeRequest, SessionEffortSelectRequest, SessionModelSelectRequest,
    SettingsOverview, SettingsRevisionRequest, WorkspaceRevisionRequest,
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
    /// A skill folder to import.
    Skill,
    /// A read-only root of a toolchain profile.
    ToolchainRoot,
    /// A `PATH` entry of a toolchain profile, inside one of its roots.
    ToolchainPathEntry,
    /// A folder to grant read and execute to a workspace's container. A
    /// read-write grant opens no dialog (ADR-0104).
    ContainerGrant,
}

impl FolderPurpose {
    /// The fixed dialog title.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::DevelopmentRoot => "Choose the development root",
            Self::Workspace => "Choose a workspace folder",
            Self::PackageCache => "Choose a package cache folder",
            Self::Skill => "Choose a skill folder",
            Self::ToolchainRoot => "Choose a read-only root for the toolchain profile",
            Self::ToolchainPathEntry => "Choose a PATH entry inside a root of the profile",
            // The access goes to the workspace's AppContainer, not to the
            // operator or the folder (ADR-0104).
            Self::ContainerGrant => "Choose a folder the workspace's sandbox may read and execute",
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
pub(super) struct ChangeGuard<'a>(&'a AtomicBool);

impl<'a> ChangeGuard<'a> {
    pub(super) fn take(flag: &'a AtomicBool) -> Result<Self, DesktopError> {
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
fn preferences_of(set: &PermissionSet) -> [Preference; 13] {
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
        Preference {
            name: "web fetch",
            effect: "the model may fetch public HTTPS pages; under prompt each fetch asks you, and under allow pages on this workspace's allowed hosts are fetched without asking while other hosts still ask",
            state: set.web_fetch,
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
        tool(
            "create directory tool",
            "the model may create new directories",
            WriteTool::CreateDirectory,
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

/// The preference set a window asked for.
const fn permission_set(flags: PermissionFlags) -> PermissionSet {
    PermissionSet {
        read: flags.read,
        write: flags.write,
        execute: flags.execute,
        task_network: flags.task_network,
        model_tools: flags.model_tools,
        host_services: flags.host_services,
        web_fetch: flags.web_fetch,
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
        // `model tools` has no per-call meaning, and `web fetch` states its own
        // (allowlisted hosts skip approval; other hosts still ask).
        if now.name != "model tools" && now.name != "web fetch" && !meaning.is_empty() {
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
        ProviderKind::LmStudio => "LM Studio",
        ProviderKind::OllamaLocal => "Ollama",
        ProviderKind::OllamaCloud => "Ollama Cloud",
        ProviderKind::Anthropic => "Anthropic (Claude)",
        ProviderKind::OpenAi => "OpenAI",
        ProviderKind::Google => "Google (Gemini)",
        ProviderKind::Meta => "Meta (Llama API)",
        ProviderKind::Moonshot => "Moonshot AI (Kimi)",
        ProviderKind::Tencent => "Tencent (Hunyuan)",
        ProviderKind::ThinkingMachines => "Thinking Machines Lab (Tinker)",
        ProviderKind::MiniMax => "MiniMax",
        ProviderKind::OpenRouter => "OpenRouter",
        ProviderKind::ZAi => "Z.ai (GLM)",
        ProviderKind::DeepSeek => "DeepSeek",
        ProviderKind::XAi => "xAI (Grok)",
        ProviderKind::OpenAiCompatible => "an OpenAI-compatible service",
        ProviderKind::ClaudeCode => "Claude Code",
    }
}

/// What the confirmation adds for a hosted service. Prompts, tool results,
/// and command output are the model's whole view of the workspace, so the
/// operator is told before a turn can send them off this computer, and to
/// which host (ADR-0071, ADR-0092). An operator-typed address is called out as
/// such, because no rule in OMGCode vouches for it.
fn hosted_notice(kind: ProviderKind, endpoint: &str) -> String {
    let host = ModelEndpoint::parse(endpoint)
        .ok()
        .and_then(|parsed| parsed.host().map(|host| display_safe_line(&host)))
        .unwrap_or_else(|| display_safe_line(endpoint));
    let typed = if kind == ProviderKind::OpenAiCompatible {
        " This address was typed when the connection was added; check that it is the service you mean."
    } else {
        ""
    };
    format!(
        "This is a hosted service. Prompts, workspace text read by tools, and command output leave this computer for {host}.{typed}"
    )
}

/// The confirmation for choosing a recorded profile. `subject` says whose chat
/// turns are sent there. Every word is fixed text or a display-safe value.
fn profile_confirmation(
    subject: &str,
    kind: ProviderKind,
    name: &str,
    endpoint: &str,
    credential_bound: bool,
) -> Confirmation {
    let hosted = if kind.is_hosted() {
        format!("\n\n{}", hosted_notice(kind, endpoint))
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
/// delete a binding it gained if the binding was recorded after all. `Ok`
/// when the profile is gone and no bound item remains; otherwise the
/// identifier of an item that could not be deleted, or `None` when the
/// profile itself could not be removed.
fn roll_back_profile(
    store: &mut ControlStore,
    credentials: &CredentialStore,
    name: &str,
) -> Result<(), Option<CredentialId>> {
    let revision = store.revision();
    let removal = store
        .remove_provider_profile(name, revision)
        .map_err(|_| None)?;
    match removal
        .credential
        .as_ref()
        .and_then(|binding| delete_unbound_credential(credentials, binding))
    {
        None => Ok(()),
        Some(orphan) => Err(Some(orphan.id)),
    }
}

pub(super) fn check_revision(store: &ControlStore, expected: u64) -> Result<(), DesktopError> {
    if store.revision() == expected {
        Ok(())
    } else {
        Err(DesktopError::stale_revision())
    }
}

pub(super) fn confirm(
    prompt: &dyn SettingsPrompt,
    confirmation: &Confirmation,
) -> Result<(), DesktopError> {
    if prompt.confirm(confirmation) {
        Ok(())
    } else {
        Err(DesktopError::cancelled())
    }
}

/// The display-safe text of a folder the operator chose.
pub(super) fn shown(folder: &Path) -> String {
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
    /// the default preferences, as `omgcode workspace register` does. On
    /// Windows the same consent grants the workspace's container read and
    /// write on the folder when this account can change its access list
    /// (ADR-0107). Registering a folder again reuses its identifier and
    /// grants the folder only if that grant is missing.
    ///
    /// # Errors
    ///
    /// `no_development_root` before any dialog when no root is set;
    /// `workspace_grant_failed` when the workspace was registered but its
    /// folder's grant failed; `workspace_grant_skipped` when it was
    /// registered but only an administrator can grant its folder, or this
    /// account's access to the folder's access list could not be read;
    /// otherwise as
    /// [`settings_choose_root`](Self::settings_choose_root).
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
            &register_confirmation(&shown(&folder), &on.join(" and ")),
        )?;
        self.apply_reporting(|store| {
            let registered = store
                .register_workspace_with_access(&folder, request.revision)
                .map_err(|error| DesktopError::from_control(&error))?;
            let unfinished = match registered.grant {
                Ok(FolderGrant::Granted(_) | FolderGrant::NotNeeded) => None,
                Ok(FolderGrant::Skipped(reason)) => {
                    Some(DesktopError::workspace_grant_skipped(reason))
                }
                Err(error) => Some(DesktopError::workspace_grant_failed(&error)),
            };
            Ok((AlsoWithdrawn::default(), unfinished))
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
        let (workspace_id, wanted, confirmation) = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let workspace = store
                .registered_workspace(request.workspace_id)
                .ok_or_else(DesktopError::unknown_workspace)?;
            let folder = folder_of_directory(workspace.directory_path());
            let wanted = permission_set(request.permissions);
            (
                workspace.id(),
                wanted,
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

    /// `settings` only. Remove one workspace's registration after a native
    /// confirmation that names its folder and lists what is removed
    /// (ADR-0081).
    ///
    /// Every container grant of the workspace is revoked first, as `omgcode
    /// workspace unregister` does (ADR-0107). When one cannot be revoked, the
    /// workspace stays registered with the grants not yet revoked, and the
    /// error says so. Then one settings event drops the registration, its
    /// preferences, its profile assignments, its package cache root
    /// selection, its skill overrides, and its checkpoint folder record. The
    /// workspace's instructions file is then deleted under the preferences
    /// lock, so no later turn sends its text. The folder, its `.OMGCode`
    /// folder, cached packages, and the sessions recorded in the workspace are
    /// not touched. Registering the folder again assigns a new identifier, so
    /// those sessions stay in the journals and chat without tools.
    ///
    /// # Errors
    ///
    /// `unknown_workspace`; `turn_running` while a chat turn runs in a session
    /// of the workspace (checked before the dialog and again before the
    /// change); `container_access_failed` or `revocation_incomplete` when a
    /// grant was not revoked, so the workspace stays registered;
    /// `instructions_orphaned` when the workspace was removed and its
    /// withdrawn runs were stopped, but its instructions file could not be
    /// deleted; otherwise as
    /// [`settings_choose_root`](Self::settings_choose_root).
    pub fn settings_unregister_workspace(
        &self,
        request: WorkspaceRevisionRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let (workspace_id, workspace_folder, grants) = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let workspace = store
                .registered_workspace(request.workspace_id)
                .ok_or_else(DesktopError::unknown_workspace)?;
            let grants = store
                .container_grants()
                .iter()
                .filter(|status| status.grant().workspace_id() == workspace.id())
                .map(|status| display_safe_line(&status.grant().directory_display()))
                .collect::<Vec<_>>();
            (
                workspace.id(),
                folder_of_directory(workspace.directory_path()),
                grants,
            )
        };
        self.refuse_running_turns(request.workspace_id)?;
        confirm(prompt, &unregister_confirmation(&workspace_folder, &grants))?;
        // The dialog can stay open for a long time, so a turn started meanwhile
        // is found here. A turn that starts after this check stops using tools,
        // because every tool call checks the live settings.
        self.refuse_running_turns(request.workspace_id)?;
        // The preferences lock comes first, as in every other save that also
        // reads settings, so the removal of the instructions file never
        // overlaps a save or a turn's read of it.
        let (listed, before, unregistration, removed) = {
            let _lock = self
                .inner
                .preferences
                .lock()
                .map_err(|_| DesktopError::poisoned())?;
            let mut store = self.inner.open_or_create_control()?;
            // A revocation withdraws every run of the workspace, which the
            // settings no longer show once it is unregistered, so its runs
            // are listed before the change.
            let before = run::pending_runs(&store);
            let unregistration = store
                .unregister_workspace_with_access(workspace_id, request.revision)
                .map_err(|error| DesktopError::from_control(&error))?;
            // The change is recorded, so the runs it withdraws are listed
            // before anything else can fail, and are stopped and waited for
            // even when the file below cannot be deleted.
            let listed = run::withdrawn_runs(&store);
            let removed = unregistration
                .unregistered()
                .then(|| instructions::remove(&self.inner.data_root, *workspace_id.as_uuid()));
            (listed, before, unregistration, removed)
        };
        // A grant that was not revoked, or a removal that was not recorded,
        // then a run that was not stopped, is reported first; a remaining
        // instructions file is never sent, because the workspace is no longer
        // registered.
        let (also, deferred) = revocation_outcome(
            before,
            &unregistration.revocations,
            RevokingChange::Unregistration,
        );
        let deferred = deferred.or_else(|| {
            unregistration
                .removal
                .as_ref()
                .map(DesktopError::removal_not_recorded)
        });
        let overview = self.finish_reporting(listed, also, deferred)?;
        if let Some(Err(_)) = removed {
            return Err(DesktopError::removed_instructions_orphaned());
        }
        Ok(overview)
    }

    /// Refuse while a chat turn runs in any session recorded in this workspace.
    fn refuse_running_turns(&self, workspace: uuid::Uuid) -> Result<(), DesktopError> {
        for session_id in self.inner.running_sessions()? {
            let recorded = self.inner.with_index(|state| {
                state
                    .index
                    .session(session_id)
                    .map_err(|error| DesktopError::from_index(&error))
            })?;
            if recorded.and_then(|session| session.workspace_id) == Some(workspace) {
                return Err(DesktopError::workspace_turn_running());
            }
        }
        Ok(())
    }

    /// `settings` only. Choose the global default model connection: a
    /// selectable recorded profile by name (local, or hosted with a bound
    /// credential), or `None` for the default endpoint (ADR-0064, ADR-0071,
    /// ADR-0092). It applies to sessions that have no connection of their own. A
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
    /// for one session (ADR-0064, ADR-0071).
    ///
    /// The window names a session and a profile, or none for the default, and
    /// nothing else. Every refusal comes before any dialog: a launch argument
    /// that fixed the connection, an unknown session, a profile core would
    /// not accept, and a choice the session already has. Rust then shows the
    /// native Yes/No confirmation naming the provider and endpoint (and, for
    /// a hosted service, that prompts and tool output leave this computer for
    /// its host), which
    /// no webview can answer. Only on Yes is the choice recorded for the
    /// session, and its model and reasoning effort are cleared because model
    /// ids belong to one server. The settings store is read again after Yes, so a profile
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
        // What was confirmed must still be what is recorded: the choice and
        // every detail the dialog named, so a profile replaced under the same
        // name and endpoint by another provider kind, or one whose key was
        // bound or removed meanwhile, records nothing.
        let again = {
            let store = inner.open_control()?;
            requested_choice(store.as_ref(), request.profile.as_deref(), &inner.endpoint)
                .map_err(|error| DesktopError::from_control(&error))?
        };
        if again != (choice.clone(), confirmation) {
            return Err(DesktopError::stale_revision());
        }
        // The model and effort are cleared: model ids belong to one server,
        // and the efforts a model accepts depend on it (ADR-0102).
        inner.update_session_record(session_id, |_| SessionRecord {
            connection: choice.record(&inner.endpoint),
            model: String::new(),
            reasoning_effort: None,
        })?;
        self.connection_status(ConnectionStatusRequest {
            session_id: Some(session_id),
        })
    }

    /// `main` only: record the model chosen for one session (ADR-0071). No
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
                reasoning_effort: existing.and_then(|record| record.reasoning_effort),
            })?;
        self.connection_status(ConnectionStatusRequest {
            session_id: Some(request.session_id),
        })
    }

    /// `main` only: record the reasoning effort chosen for one session
    /// (ADR-0102), or `None` to send none. No dialog is shown: an effort
    /// grants nothing, and the server still decides whether its model takes
    /// the value. An effort is refused unless the session's connection, as
    /// `connection_status` reports it now, takes one. The session's provider
    /// and model are kept. Like a model choice, a session with no record gets
    /// one that follows the default connection; it records the model the
    /// status showed (the saved preference's), so the model shown does not
    /// change.
    ///
    /// It holds the one-change-at-a-time guard, as a provider choice does,
    /// so the connection it checks the effort against is the one the record
    /// keeps: a provider choice cannot land between the check and the save.
    ///
    /// # Errors
    ///
    /// `invalid_request` for an effort the session's connection does not
    /// take; `settings_change_running` while another settings change, such
    /// as a provider choice awaiting its confirmation, runs; `not_found`;
    /// `busy`, `control`, or `internal` while reading the connection;
    /// `session_choices` when the choice cannot be saved.
    pub fn session_effort_select(
        &self,
        request: SessionEffortSelectRequest,
    ) -> Result<ConnectionStatus, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let session_id = request.session_id;
        let status = self.connection_status(ConnectionStatusRequest {
            session_id: Some(session_id),
        })?;
        if request
            .reasoning_effort
            .is_some_and(|effort| !status.reasoning_efforts.contains(&effort))
        {
            return Err(DesktopError::invalid_request(
                "This connection does not take a reasoning effort.",
            ));
        }
        self.inner
            .update_session_record(session_id, |existing| match existing {
                Some(record) => SessionRecord {
                    reasoning_effort: request.reasoning_effort,
                    ..record.clone()
                },
                None => SessionRecord {
                    connection: ChosenConnection::Follow {},
                    model: status.model,
                    reasoning_effort: request.reasoning_effort,
                },
            })?;
        self.connection_status(ConnectionStatusRequest {
            session_id: Some(session_id),
        })
    }

    /// `settings` only. Record a connection through the same core call as the
    /// CLI's `provider-profile-add`: a local LM Studio or Ollama connection
    /// with no credential and a typed loopback endpoint, a named hosted
    /// provider with its fixed endpoint and an API key (ADR-0071 decision 5,
    /// ADR-0092), or an OpenAI-compatible service with a typed HTTPS base URL
    /// and an API key. Recording it grants nothing and sends nothing; only
    /// selecting it does, after a confirmation that names the host. No dialog
    /// is shown.
    ///
    /// A local kind needs `endpoint` and no `api_key`; a named hosted kind
    /// needs an `api_key` and no `endpoint`; `openai_compatible` needs both.
    /// Any mismatch is refused as `invalid_request` before anything is read or
    /// recorded.
    ///
    /// For a hosted kind the key is moved into core's zeroing `Secret`
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
    /// The exception is a binding write that failed but may be recorded: the
    /// new item may then be the one the profile names, so neither is removed
    /// and the error is `credential_binding_uncertain` with its identifier.
    /// The key, its length, and a digest of it appear in no response or error.
    ///
    /// # Errors
    ///
    /// `invalid_request` for an oversized value, a mismatched field, a
    /// `claude_code` kind, which only the trusted CLI records (ADR-0109), or
    /// a key that is empty, too long, or has a control character; `control` with
    /// core's reason (a bad name, a duplicate, or an endpoint its kind's rule
    /// refuses); `credential_unavailable`,
    /// `credential_locked`, `credential_io`, `credential_orphaned`,
    /// `credential_binding_uncertain`; `stale_revision`;
    /// `settings_change_running`; `busy`.
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
        // Only the trusted CLI records a program connection (ADR-0109), so
        // the request is refused before its key is read.
        if kind.runs_program() {
            return Err(DesktopError::invalid_request(
                "A Claude Code connection is added with the trusted CLI.",
            ));
        }
        let fixed = kind.fixed_endpoint();
        let (endpoint, auth, secret) = match (kind.is_local(), fixed, endpoint, api_key) {
            (true, _, Some(endpoint), None) => (endpoint, ProviderAuth::None, None),
            (true, _, _, _) => {
                return Err(DesktopError::invalid_request(
                    "A local connection takes an endpoint and no API key.",
                ));
            }
            // Rust supplies a named provider's one endpoint; it is never typed.
            (false, Some(fixed), None, Some(key)) => (
                fixed.to_owned(),
                ProviderAuth::ApiKey,
                Some(
                    key.into_secret()
                        .ok_or_else(DesktopError::malformed_key_input)?,
                ),
            ),
            (false, Some(_), _, _) => {
                return Err(DesktopError::invalid_request(
                    "This provider takes an API key and no endpoint.",
                ));
            }
            (false, None, Some(endpoint), Some(key)) => (
                endpoint,
                ProviderAuth::ApiKey,
                Some(
                    key.into_secret()
                        .ok_or_else(DesktopError::malformed_key_input)?,
                ),
            ),
            (false, None, _, _) => {
                return Err(DesktopError::invalid_request(
                    "An OpenAI-compatible connection takes an endpoint and an API key.",
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
                Ok(set) => match set.orphan {
                    None => Ok(()),
                    Some(orphan) => Err(DesktopError::credential_item_orphaned(&orphan.id)),
                },
                // The binding may be recorded, so the new item may be the one the profile names.
                // Removing the profile would delete it; both are kept and the identifier is
                // reported (security coding rules).
                Err(error @ ProviderCredentialError::BindingUncertain { .. }) => {
                    Err(DesktopError::from_provider_credential(&error))
                }
                Err(error) => Err(
                    match roll_back_profile(store, &credentials, &declaration.name) {
                        Ok(()) => DesktopError::from_provider_credential(&error),
                        Err(Some(left)) => DesktopError::credential_item_orphaned(&left),
                        Err(None) => DesktopError::connection_rollback_failed(&error),
                    },
                ),
            }
        })
    }

    /// `settings` only. Replace the stored API key of a recorded hosted
    /// profile through `ControlStore::set_provider_credential`,
    /// as the CLI's `provider-credential-set` does: the new item is stored,
    /// then bound, then the replaced item is deleted. The key is handled as in
    /// [`settings_add_connection`](Self::settings_add_connection).
    ///
    /// No dialog is shown (ADR-0059 decision 6 asks for none). The key goes
    /// only to the same recorded endpoint the connection already names, so the
    /// audience of a turn is unchanged; the profile's endpoint is not
    /// editable, and choosing the connection stays confirmed separately.
    ///
    /// # Errors
    ///
    /// `invalid_request` for a profile that is not a hosted `api_key` profile
    /// with a recorded endpoint, or a key that is empty, too long, or has a control character; `control`
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
            .ok_or_else(DesktopError::malformed_key_input)?;
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let credentials = CredentialStore::open(&self.inner.data_root)
            .map_err(|error| DesktopError::from_credential(&error))?;
        self.apply_checked(|store| {
            let status = store
                .provider_profile(&name)
                .ok_or_else(|| DesktopError::from_control(&ControlError::UnknownProvider))?;
            // A subscription profile can never hold a key (core's
            // `require_api_key`), so refuse it here with the same message.
            if !status.profile().kind().is_hosted()
                || status.profile().auth() != ProviderAuth::ApiKey
            {
                return Err(DesktopError::invalid_request(
                    "Only a hosted connection that uses an API key has a key set here.",
                ));
            }
            // A profile recorded without an endpoint can never be used, and its endpoint cannot
            // be added, so a key bound to it would serve nothing. Core now records a named kind's
            // endpoint at add time (ADR-0092), so only a journal from 0.58.0 or earlier holds one.
            if status.profile().endpoint().is_none() {
                return Err(DesktopError::invalid_request(
                    "This connection has no endpoint, so it cannot be used. Remove it and add it again.",
                ));
            }
            match store.set_provider_credential(&credentials, &name, &secret, revision) {
                Ok(set) => match set.orphan {
                    None => Ok(()),
                    // The profile now names the new key, so only this identifier finds the old one.
                    Some(orphan) => Err(DesktopError::replaced_key_orphaned(&orphan.id)),
                },
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
    /// `session_connection_stale` (ADR-0071), because core no longer knows the
    /// profile.
    ///
    /// # Errors
    ///
    /// `control` for an unknown profile; `cancelled`; `stale_revision`;
    /// `credential_unavailable` or `credential_locked` (the native store is
    /// checked before anything is removed); `credential_orphaned` (the
    /// profile is removed but its native item was not deleted);
    /// `settings_change_running`; `busy`; and `control` when a removed Claude
    /// Code connection's running turn did not end within 30 seconds.
    pub fn settings_remove_connection(
        &self,
        request: ConnectionRemoveRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let (confirmation, bound, claude_code) = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let status = store
                .provider_profile(&request.name)
                .ok_or_else(|| DesktopError::from_control(&ControlError::UnknownProvider))?;
            let bound = status.credential().is_some();
            let claude_code = status
                .profile()
                .claude_code_setup()
                .map(|_| status.profile().id());
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
            (confirmation, bound, claude_code)
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
        let overview = self.apply_checked(|store| {
            let removal = store
                .remove_provider_profile(&request.name, request.revision)
                .map_err(|error| DesktopError::from_control(&error))?;
            match (&removal.credential, &credentials) {
                (Some(binding), Some(credentials)) => {
                    match delete_unbound_credential(credentials, binding) {
                        None => Ok(()),
                        Some(orphan) => Err(DesktopError::removed_key_orphaned(&orphan.id)),
                    }
                }
                (Some(binding), None) => Err(DesktopError::removed_key_orphaned(binding.id())),
                (None, _) => Ok(()),
            }
        })?;
        // A removed Claude Code connection's running turns end once their
        // watchers read the released settings; the removal reports success
        // only after each of their processes was observed ended (ADR-0109).
        if let Some(profile_id) = claude_code {
            wait_for_claude_code_turns(
                &self.inner.data_root,
                profile_id,
                CLAUDE_CODE_TURN_STOP_WAIT,
            )
            .map_err(|error| DesktopError::from_control(&error))?;
        }
        Ok(overview)
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
    pub(super) fn apply_checked(
        &self,
        change: impl FnOnce(&mut ControlStore) -> Result<(), DesktopError>,
    ) -> Result<SettingsOverview, DesktopError> {
        self.apply_checked_also(|store| change(store).map(|()| AlsoWithdrawn::default()))
    }

    /// As [`apply_checked`](Self::apply_checked), for a change that also names
    /// runs to stop that the settings alone do not show as withdrawn, such as
    /// the runs of a workspace whose container grant was revoked.
    pub(super) fn apply_checked_also(
        &self,
        change: impl FnOnce(&mut ControlStore) -> Result<AlsoWithdrawn, DesktopError>,
    ) -> Result<SettingsOverview, DesktopError> {
        self.apply_reporting(|store| change(store).map(|also| (also, None)))
    }

    /// As [`apply_checked_also`](Self::apply_checked_also), for a change that
    /// was recorded but did not finish everything it set out to do, such as
    /// an unassignment whose grant could not be revoked (ADR-0107). The
    /// runs are waited for first, because the recorded part of the change
    /// can have withdrawn them; the deferred error is returned afterwards,
    /// with any wait failure added to its message.
    pub(super) fn apply_reporting(
        &self,
        change: impl FnOnce(
            &mut ControlStore,
        ) -> Result<(AlsoWithdrawn, Option<DesktopError>), DesktopError>,
    ) -> Result<SettingsOverview, DesktopError> {
        let (listed, also, deferred) = {
            let mut store = self.inner.open_or_create_control()?;
            let (also, deferred) = change(&mut store)?;
            (run::withdrawn_runs(&store), also, deferred)
        };
        self.finish_reporting(listed, also, deferred)
    }

    /// [`finish_change`](Self::finish_change), then `deferred`, the error of
    /// a change that was recorded but not finished, with any wait failure
    /// added to its message.
    fn finish_reporting(
        &self,
        listed: Result<Vec<uuid::Uuid>, run::RunError>,
        also: AlsoWithdrawn,
        deferred: Option<DesktopError>,
    ) -> Result<SettingsOverview, DesktopError> {
        match deferred {
            None => self.finish_change(listed, also),
            Some(error) => Err(self.failed_after_wait(listed, also, error)),
        }
    }

    /// `error`, the failure of a change that still withdrew runs, such as an
    /// assignment whose rollback revoked a grant (ADR-0107), returned after
    /// the wait in [`finish_change`](Self::finish_change) with any wait
    /// failure added to its message. The store must be closed.
    pub(super) fn failed_after_wait(
        &self,
        listed: Result<Vec<uuid::Uuid>, run::RunError>,
        also: AlsoWithdrawn,
        error: DesktopError,
    ) -> DesktopError {
        match self.wait_for_withdrawn(listed, also) {
            Ok(()) => error,
            Err(wait) => DesktopError::execution(
                error.code(),
                format!("{} {}", error.message(), wait.message()),
            ),
        }
    }

    /// After a recorded change: wait for the runs it withdrew, the runs in
    /// `also`, and any others the settings withdraw to stop, then return the
    /// new overview.
    fn finish_change(
        &self,
        listed: Result<Vec<uuid::Uuid>, run::RunError>,
        also: AlsoWithdrawn,
    ) -> Result<SettingsOverview, DesktopError> {
        self.wait_for_withdrawn(listed, also)?;
        let store = self.inner.open_or_create_control()?;
        Ok(overview_of(&self.inner, store))
    }

    /// Wait for the runs listed as withdrawn, the runs in `also`, and any
    /// others the settings now withdraw to stop.
    fn wait_for_withdrawn(
        &self,
        listed: Result<Vec<uuid::Uuid>, run::RunError>,
        also: AlsoWithdrawn,
    ) -> Result<(), DesktopError> {
        // An unreadable run journal never undoes the recorded change, and the
        // watchers still stop the runs: wait for what the settings now
        // withdraw, then report that the runs were not all checked.
        let mut named = listed.as_deref().unwrap_or(&[]).to_vec();
        named.extend(also.runs);
        let withdrawn = run::stop_withdrawn_runs(&self.inner.data_root, REVOCATION_WAIT, &named)
            .map_err(|_| DesktopError::runs_unchecked())?;
        if !withdrawn.unconfirmed.is_empty() {
            return Err(DesktopError::runs_not_stopped(withdrawn.unconfirmed.len()));
        }
        if listed.is_err() || also.unchecked {
            return Err(DesktopError::runs_unchecked());
        }
        Ok(())
    }
}

/// Runs a change stops in addition to those the settings withdraw.
#[derive(Debug, Default)]
pub(super) struct AlsoWithdrawn {
    /// Pending runs to wait for.
    pub runs: Vec<uuid::Uuid>,
    /// The run journal could not be read, so the list may be incomplete.
    pub unchecked: bool,
}

/// Which operator change revoked grants, which decides what a failed
/// revocation left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RevokingChange {
    /// An unassignment or a profile removal, recorded before its revocations.
    Recorded,
    /// An unregistration, recorded only after every grant was revoked, so a
    /// failed revocation leaves the workspace registered.
    Unregistration,
}

/// What a change that revoked grants leaves to wait for and report.
///
/// Revoking any grant withdraws every run of its workspace. `before` holds
/// every pending run, listed by `run::pending_runs` under the lock before the
/// change; only the runs of a workspace whose grant the change tried to revoke
/// are waited for, because a run that is not withdrawn would be reported as
/// not stopped. A revocation that failed is reported after the wait, because
/// the revocations before it stand.
pub(super) fn revocation_outcome(
    before: Result<Vec<(WorkspaceId, uuid::Uuid)>, run::RunError>,
    revocations: &Revocations,
    change: RevokingChange,
) -> (AlsoWithdrawn, Option<DesktopError>) {
    let attempted = !revocations.workspaces.is_empty();
    let also = match (attempted, before) {
        (false, _) => AlsoWithdrawn::default(),
        (true, Ok(runs)) => AlsoWithdrawn {
            runs: revocations.withdrawn_runs(&runs),
            unchecked: false,
        },
        (true, Err(_)) => AlsoWithdrawn {
            runs: Vec::new(),
            unchecked: true,
        },
    };
    let deferred = (!revocations.failures.is_empty())
        .then(|| DesktopError::revocation_incomplete(&revocations.failures, change));
    (also, deferred)
}

/// The runs to wait for after a failed assignment, or `None` when its
/// rollback attempted no revocation. Revoking a grant withdraws every run of
/// its workspace that started before, so a rollback that revoked one, or
/// tried to, stops those runs as an unassignment does (ADR-0107). `before`
/// holds every pending run, listed under the lock before the assignment.
pub(super) fn rollback_withdrawn(
    before: Result<Vec<(WorkspaceId, uuid::Uuid)>, run::RunError>,
    failure: &AssignmentFailure,
) -> Option<AlsoWithdrawn> {
    (!failure.released.workspaces.is_empty())
        .then(|| revocation_outcome(before, &failure.released, RevokingChange::Recorded).0)
}

/// The native confirmation for registering `folder`. On Windows it also asks
/// for the read-write grant registration applies (ADR-0107).
fn register_confirmation(folder: &str, on: &str) -> Confirmation {
    let sandbox = if CONTAINER_GRANTS_SUPPORTED {
        "\n\nOMGCode runs the workspace's commands in its own AppContainer, a Windows sandbox that starts with no access to your files. Registering also lets that sandbox read and write this folder and everything beneath it, by adding it to the folder's access-control list on disk, when this account can change that list. The access stays until you revoke it or remove the workspace."
    } else {
        ""
    };
    Confirmation {
        title: "Register a workspace",
        message: format!(
            "Register this folder as a workspace?\n\n{folder}\n\nA new workspace starts with {on} on and everything else off.{sandbox}"
        ),
    }
}

/// The native confirmation for removing a workspace whose container grants
/// are on `grants`, display-safe.
fn unregister_confirmation(workspace_folder: &str, grants: &[String]) -> Confirmation {
    let revoked = if grants.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nFirst, the workspace's AppContainer, the Windows sandbox its commands run in, is removed from the access-control list of each folder it was granted, and its runs are stopped:\n{}\nIf a folder's list cannot be changed, the workspace stays registered.",
            grants
                .iter()
                .map(|grant| format!("  {grant}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    Confirmation {
        title: "Remove a workspace",
        message: format!(
            "Remove this workspace?\n\n{workspace_folder}{revoked}\n\nIts preferences, toolchain profile assignments, instructions, package cache selection, skill overrides, and checkpoint record are removed from OMGCode. The folder and its files are not touched, and its checkpoints stay in its .OMGCode folder. Its sessions stay, and can chat without tools only. Registering the folder again makes a new workspace with the default preferences."
        ),
    }
}

impl DesktopError {
    /// The workspace was registered, but its container was not granted its
    /// folder. The operating system's own text is never shown.
    fn workspace_grant_failed(error: &ControlError) -> Self {
        let reason = match error {
            ControlError::Container(_) => {
                "its folder's access list could not be changed or read back".to_owned()
            }
            other => Self::from_control(other).message().to_owned(),
        };
        Self::execution(
            "workspace_grant_failed",
            format!(
                "The workspace was registered, but its sandbox was not granted its folder: {reason}. Under Container grants, first revoke a grant of the folder that is pending or gives only read and execute, if there is one, then grant the folder read and write."
            ),
        )
    }

    /// The workspace was registered, but its folder needs a grant that was
    /// not attempted (ADR-0107). A run in it is refused until it has one.
    fn workspace_grant_skipped(reason: SkippedGrant) -> Self {
        let (reason, recovery) = match reason {
            SkippedGrant::NeedsAdministrator => (
                "only an administrator can change its folder's access list",
                "Its runs are refused until an administrator grants the folder.",
            ),
            SkippedGrant::AccessUnknown => (
                "whether this account can change its folder's access list could not be read",
                "Its runs are refused until it has that grant: grant the folder read and write under Container grants.",
            ),
        };
        Self::execution(
            "workspace_grant_skipped",
            format!(
                "The workspace was registered, but its sandbox was not granted its folder: {reason}. {recovery}"
            ),
        )
    }

    /// Revocations the change made did not all complete; every grant was
    /// attempted. After a recorded change the change stands; after an
    /// unregistration the workspace stays registered. A grant revoked whose
    /// container profile could not be deleted is not listed for **Revoke**,
    /// because it is gone, and when no other grant failed the message says
    /// every grant was revoked. The operating system's own text is never
    /// shown.
    pub(super) fn revocation_incomplete(
        failures: &[(uuid::Uuid, ControlError)],
        change: RevokingChange,
    ) -> Self {
        let unrevoked: Vec<&ControlError> = failures
            .iter()
            .map(|(_, error)| error)
            .filter(|error| !matches!(error, ControlError::ProfileRemoval(_)))
            .collect();
        let mut message = match (change, unrevoked.is_empty()) {
            (RevokingChange::Recorded, false) => {
                "The change was recorded, but not every container grant it releases was revoked."
            }
            (RevokingChange::Recorded, true) => {
                "The change was recorded, and every container grant it releases was revoked."
            }
            (RevokingChange::Unregistration, false) => {
                "The workspace stays registered, because not every container grant of it was revoked."
            }
            (RevokingChange::Unregistration, true) => {
                "The workspace stays registered: every container grant of it was revoked, but its removal was not attempted."
            }
        }
        .to_owned();
        // Each kind of failure is counted under its own reason: an
        // access-list failure leaves its grant `revoking`, and any other
        // failure is counted under the message it maps to.
        let access = unrevoked
            .iter()
            .filter(|error| matches!(error, ControlError::Container(_)))
            .count();
        if access > 0 {
            message.push_str(&format!(
                " {access} grant(s) could not be removed from their folders' access lists and stay revoking."
            ));
        }
        let mut reasons: Vec<(String, usize)> = Vec::new();
        for error in unrevoked
            .iter()
            .filter(|error| !matches!(error, ControlError::Container(_)))
        {
            let reason = Self::from_control(error)
                .message()
                .trim_end_matches('.')
                .to_owned();
            match reasons.iter_mut().find(|(seen, _)| *seen == reason) {
                Some((_, count)) => *count += 1,
                None => reasons.push((reason, 1)),
            }
        }
        for (reason, count) in &reasons {
            message.push_str(&format!(" {count} grant(s) were not revoked: {reason}."));
        }
        if !unrevoked.is_empty() {
            message.push_str(" The settings list each one, and Revoke tries again.");
        }
        if unrevoked.len() < failures.len() {
            message.push_str(if unrevoked.is_empty() {
                " The workspace's AppContainer profile could not be deleted afterwards; it holds no access now."
            } else {
                " A grant was revoked, but the workspace's AppContainer profile could not be deleted; it holds no access now."
            });
        }
        if change == RevokingChange::Unregistration {
            message.push_str(if unrevoked.is_empty() {
                " Remove the workspace again."
            } else {
                " Then remove the workspace again."
            });
        }
        let code = if access > 0 {
            "container_access_failed"
        } else {
            "revocation_incomplete"
        };
        Self::execution(code, message)
    }

    /// Every container grant of the workspace was revoked, but its removal
    /// could not be recorded, so it stays registered.
    fn removal_not_recorded(error: &ControlError) -> Self {
        Self::execution(
            "revocation_incomplete",
            format!(
                "Every container grant of the workspace was revoked, but its removal could not be recorded: {}. The workspace stays registered; remove it again.",
                Self::from_control(error).message().trim_end_matches('.')
            ),
        )
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
            web_fetch: Access::Deny,
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

    fn removal_failed() -> ControlError {
        ControlError::ProfileRemoval(std::io::Error::other("operating system text"))
    }

    /// A failed assignment waits only when its rollback attempted a
    /// revocation, and then for that workspace's pending runs alone; a run
    /// list that could not be read is reported as unchecked.
    #[test]
    fn a_failed_assignment_waits_for_the_runs_its_rollback_withdrew() {
        let id = || {
            uuid::Uuid::new_v4()
                .to_string()
                .parse::<WorkspaceId>()
                .expect("workspace id")
        };
        let (app, other) = (id(), id());
        let (run_app, run_other) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let before = || Ok(vec![(app, run_app), (other, run_other)]);
        let failure = |workspaces| AssignmentFailure {
            error: ControlError::LimitReached,
            released: Revocations {
                workspaces,
                ..Revocations::default()
            },
            kept: Vec::new(),
        };

        assert!(rollback_withdrawn(before(), &failure(vec![])).is_none());
        let also = rollback_withdrawn(before(), &failure(vec![app])).expect("a wait");
        assert_eq!(also.runs, [run_app]);
        assert!(!also.unchecked);
        let unread = rollback_withdrawn(
            Err(run::RunError::Refused("the run journal is unreadable")),
            &failure(vec![app]),
        )
        .expect("a wait");
        assert!(unread.runs.is_empty());
        assert!(unread.unchecked);
    }

    /// A grant revoked whose AppContainer profile could not be deleted is
    /// not reported as a grant left to revoke.
    #[test]
    fn a_failed_profile_deletion_alone_says_every_grant_was_revoked() {
        let failures = [(uuid::Uuid::new_v4(), removal_failed())];
        for (change, lead) in [
            (
                RevokingChange::Recorded,
                "The change was recorded, and every container grant it releases was revoked.",
            ),
            (
                RevokingChange::Unregistration,
                "The workspace stays registered: every container grant of it was revoked",
            ),
        ] {
            let error = DesktopError::revocation_incomplete(&failures, change);
            let message = error.message();
            assert_eq!(error.code(), "revocation_incomplete");
            assert!(message.starts_with(lead), "{message}");
            assert!(message.contains("could not be deleted"), "{message}");
            assert!(!message.contains("not revoked"), "{message}");
            assert!(!message.contains("Revoke tries again"), "{message}");
            assert!(!message.contains("operating system text"), "{message}");
            assert_eq!(
                message.ends_with(" Remove the workspace again."),
                change == RevokingChange::Unregistration,
                "{message}"
            );
        }
    }

    /// A folder grant registration did not attempt is reported with its
    /// reason, so the operator does not learn of it from a refused run.
    #[test]
    fn a_skipped_folder_grant_is_reported_with_its_reason() {
        let error = DesktopError::workspace_grant_skipped(SkippedGrant::NeedsAdministrator);
        assert_eq!(error.code(), "workspace_grant_skipped");
        assert_eq!(
            error.message(),
            "The workspace was registered, but its sandbox was not granted its folder: only an administrator can change its folder's access list. Its runs are refused until an administrator grants the folder."
        );
        let error = DesktopError::workspace_grant_skipped(SkippedGrant::AccessUnknown);
        assert!(
            error
                .message()
                .contains("could not be read. Its runs are refused until it has that grant"),
            "{}",
            error.message()
        );
        assert!(
            error.message().ends_with("under Container grants."),
            "{}",
            error.message()
        );
    }

    /// A grant left revoking is still reported beside one whose profile
    /// could not be deleted.
    #[test]
    fn a_grant_left_revoking_is_reported_beside_a_failed_profile_deletion() {
        let failures = [
            (uuid::Uuid::new_v4(), removal_failed()),
            (
                uuid::Uuid::new_v4(),
                ControlError::Container(std::io::Error::other("operating system text")),
            ),
        ];
        let error = DesktopError::revocation_incomplete(&failures, RevokingChange::Unregistration);
        let message = error.message();
        assert_eq!(error.code(), "container_access_failed");
        assert!(
            message.starts_with(
                "The workspace stays registered, because not every container grant of it was revoked."
            ),
            "{message}"
        );
        assert!(
            message.contains(" 1 grant(s) could not be removed"),
            "{message}"
        );
        assert!(message.contains("A grant was revoked, but"), "{message}");
        assert!(
            message.ends_with(" Then remove the workspace again."),
            "{message}"
        );
        assert!(!message.contains("operating system text"), "{message}");
    }

    /// When one grant fails its access-list change and others fail for
    /// other reasons, each kind is counted under its own reason, and the
    /// access-list one is still said to stay revoking.
    #[test]
    fn mixed_revocation_failures_are_counted_by_kind() {
        let failures = [
            (
                uuid::Uuid::new_v4(),
                ControlError::Container(std::io::Error::other("operating system text")),
            ),
            (uuid::Uuid::new_v4(), ControlError::UnknownGrant),
            (uuid::Uuid::new_v4(), ControlError::UnknownGrant),
            (uuid::Uuid::new_v4(), ControlError::LimitReached),
        ];
        let error = DesktopError::revocation_incomplete(&failures, RevokingChange::Recorded);
        let message = error.message();
        assert_eq!(error.code(), "container_access_failed");
        let unknown = DesktopError::from_control(&ControlError::UnknownGrant);
        let limit = DesktopError::from_control(&ControlError::LimitReached);
        assert_eq!(
            message,
            format!(
                "The change was recorded, but not every container grant it releases was revoked. 1 grant(s) could not be removed from their folders' access lists and stay revoking. 2 grant(s) were not revoked: {}. 1 grant(s) were not revoked: {}. The settings list each one, and Revoke tries again.",
                unknown.message().trim_end_matches('.'),
                limit.message().trim_end_matches('.')
            )
        );
        assert!(!message.contains("operating system text"), "{message}");

        let others = DesktopError::revocation_incomplete(&failures[1..], RevokingChange::Recorded);
        assert_eq!(others.code(), "revocation_incomplete");
        assert!(
            !others.message().contains("access lists"),
            "{}",
            others.message()
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
        // The grant dialog names who receives the access (ADR-0104).
        assert_eq!(
            FolderPurpose::ContainerGrant.title(),
            "Choose a folder the workspace's sandbox may read and execute"
        );
    }
}
