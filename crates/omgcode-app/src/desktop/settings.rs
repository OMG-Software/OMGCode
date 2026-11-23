//! Views of the trusted configuration (ADR-0059).
//!
//! [`DesktopService::settings_overview`] serves the `settings` window. It
//! reads the settings store, creating an empty one when none exists
//! (ADR-0060), and shapes what the trusted CLI's `workspace status` and
//! `skills list` show, with every path and recorded name passed through
//! core's display-safe functions. It reads no stored secret and returns no
//! host path that could be reopened. Changes are in `settings_changes`.
//!
//! [`DesktopService::session_skills`] serves the `main` window's per-turn skill
//! choice. It returns only the names of skills enabled for the session's
//! workspace; enabling a skill grants no permission (ADR-0057).
//!
//! [`DesktopService::connection_status`] and
//! [`DesktopService::session_context`] serve the `main` window's status bar
//! (ADR-0064): the connection the next turn uses with the local connections
//! that may be chosen, and the Git branch of a session's workspace, read from
//! `HEAD` for display only.

use omgcode_core::control::{
    ContainerGrantAccess, ContainerGrantState, ControlStore, ProfilePlatform,
};
use omgcode_core::git_head::{WorkspaceHead, workspace_head};
use omgcode_core::policy::WorkspaceId;
use omgcode_core::turn::display_safe_line;

use super::error::DesktopError;
use super::instructions;
use super::preferences;
use super::session_connections::{ChosenConnection, recorded_endpoint};
use super::settings_chat::{
    chat_defaults_view, global_for_room, longest_for_room, workspace_instructions_view,
};
use super::state::folder_of_directory;
use super::wire::{
    BranchState, BranchView, ConnectionChoice, ConnectionOrigin, ConnectionSourceView,
    ConnectionStatus, ConnectionStatusRequest, ConnectionView, ContainerGrantView,
    PackageProfileView, ProviderProfileView, SessionContext, SessionRequest, SessionSkills,
    SettingsOverview, SkillOverrideView, SkillSettingView, ToolchainProfileView,
    WorkspaceSettingsView,
};
use super::{DesktopService, Inner, TokenResolver};
use crate::TokenSource;

impl DesktopService {
    /// A view of the trusted configuration, for the `settings` window only.
    ///
    /// When no settings store exists, an empty one is created first
    /// (ADR-0060): it records no root, workspace, or permission. The store is
    /// opened and released inside this call.
    ///
    /// # Errors
    ///
    /// `busy` when the settings store stays held past a two-second wait,
    /// `control` when it is invalid or cannot be created.
    pub fn settings_overview(&self) -> Result<SettingsOverview, DesktopError> {
        let store = self.inner.open_or_create_control()?;
        Ok(overview_of(&self.inner, store))
    }
}

/// Shape the overview from an open store, then release the store. Every path
/// and recorded name passes through core's display-safe functions.
pub(super) fn overview_of(inner: &Inner, store: ControlStore) -> SettingsOverview {
    let connection = connection_view(inner, Some(&store));
    let active = store
        .active_provider_profile()
        .map(|status| status.profile().id());
    let mut chat = chat_defaults_view(&inner.data_root);
    // The rooms follow the saves' rule (ADR-0080): `None` when the text they
    // must fit beside cannot be read now. This read is not under the
    // preferences lock, so the saves check again.
    let workspace_room =
        global_for_room(&inner.data_root).map(|global| instructions::workspace_room(&global));
    chat.instructions_max_bytes = longest_for_room(
        &inner.data_root,
        store
            .workspaces()
            .map(|workspace| *workspace.id().as_uuid()),
    )
    .map(instructions::global_room);
    let workspaces: Vec<WorkspaceSettingsView> = store
        .workspaces()
        .map(|workspace| {
            let permissions = workspace.permissions();
            WorkspaceSettingsView {
                workspace_id: *workspace.id().as_uuid(),
                folder_display: folder_of_directory(workspace.directory_path()),
                model_tools: permissions.model_tools,
                read: permissions.read,
                write: permissions.write,
                execute: permissions.execute,
                task_network: permissions.task_network,
                host_services: permissions.host_services,
                write_tools: permissions.write_tools,
                package_cache_root: store.package_cache_root_display(workspace.id()),
                instructions: workspace_instructions_view(
                    &inner.data_root,
                    *workspace.id().as_uuid(),
                ),
                instructions_max_bytes: workspace_room,
            }
        })
        .collect();
    let provider_profiles = store
        .provider_profiles()
        .iter()
        .map(|status| {
            let profile = status.profile();
            ProviderProfileView {
                name: display_safe_line(profile.name()),
                kind: profile.kind().as_str(),
                auth: profile.auth().as_str(),
                endpoint: profile
                    .endpoint()
                    .map(|endpoint| display_safe_line(endpoint.as_str())),
                credential_bound: status.credential().is_some(),
                selectable: store.check_provider_selectable(profile.name()).is_ok(),
                active: active == Some(profile.id()),
            }
        })
        .collect();
    let toolchain_profiles = store
        .profiles()
        .iter()
        .map(|status| {
            let profile = status.profile();
            ToolchainProfileView {
                name: display_safe_line(profile.name()),
                platform: platform_name(profile.platform()),
                read_only_roots: profile.read_only_root_displays(),
                path_entries: profile.path_entry_displays(),
                environment_names: profile
                    .environment()
                    .iter()
                    .map(|variable| display_safe_line(variable.name()))
                    .collect(),
            }
        })
        .collect();
    let package_profiles = store
        .package_profiles()
        .iter()
        .map(|status| PackageProfileView {
            name: display_safe_line(status.profile().name()),
            origins: status
                .profile()
                .origins()
                .iter()
                .map(|origin| display_safe_line(&origin.render()))
                .collect(),
            credentials_bound: u64::try_from(status.credentials().len()).unwrap_or(u64::MAX),
        })
        .collect();
    let container_grants = store
        .container_grants()
        .iter()
        .map(|status| {
            let grant = status.grant();
            ContainerGrantView {
                grant_id: grant.id(),
                workspace_id: *grant.workspace_id().as_uuid(),
                directory: grant.directory_display(),
                access: access_name(grant.access()),
                state: grant_state_name(status.state()),
            }
        })
        .collect();
    let skills = store
        .skill_statuses(None)
        .into_iter()
        .map(|status| SkillSettingView {
            name: status.skill.name().as_str().to_owned(),
            application: status.application,
            workspace_overrides: status
                .workspace_overrides
                .iter()
                .map(|entry| SkillOverrideView {
                    workspace_id: *entry.workspace_id.as_uuid(),
                    enabled: entry.enabled,
                })
                .collect(),
        })
        .collect();
    let overview = SettingsOverview {
        revision: store.revision(),
        development_root: store.development_root_display(),
        workspaces,
        connection,
        provider_profiles,
        toolchain_profiles,
        package_profiles,
        container_grants,
        skills,
        chat,
        scope: inner.settings_scope(),
    };
    drop(store);
    overview
}

impl DesktopService {
    /// Scope the `settings` window to the registered workspace a session was
    /// recorded in (ADR-0071). The identifier comes from the session index and
    /// the settings store, never from the caller. The scope is display state:
    /// it grants and removes no access.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown session, `unknown_workspace` when the
    /// session's workspace is not registered here or there is no settings
    /// store, `busy`, `control`, or `internal`. The scope is unchanged on error.
    pub fn settings_scope_workspace(&self, request: SessionRequest) -> Result<(), DesktopError> {
        let Some((store, workspace_id)) = self.session_workspace(request)? else {
            return Err(DesktopError::unknown_workspace());
        };
        drop(store);
        self.inner.set_settings_scope(Some(*workspace_id.as_uuid()))
    }

    /// Clear the `settings` window's scope so it shows every workspace.
    ///
    /// # Errors
    ///
    /// `internal` when the scope state is unavailable (a poisoned lock).
    pub fn settings_scope_clear(&self) -> Result<(), DesktopError> {
        self.inner.set_settings_scope(None)
    }

    /// The skills the operator may select for one turn in this session: those
    /// effectively enabled for the session's recorded workspace.
    ///
    /// A session whose workspace is not registered here, or with no settings
    /// store, has none.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown session, `busy`, `control`, or `internal`.
    pub fn session_skills(&self, request: SessionRequest) -> Result<SessionSkills, DesktopError> {
        let Some((store, workspace_id)) = self.session_workspace(request)? else {
            return Ok(SessionSkills {
                workspace_registered: false,
                skills: Vec::new(),
            });
        };
        let skills = store
            .skill_statuses(Some(workspace_id))
            .into_iter()
            .filter(|status| status.effective == Some(true))
            .map(|status| status.skill.name().as_str().to_owned())
            .collect();
        drop(store);
        Ok(SessionSkills {
            workspace_registered: true,
            skills,
        })
    }

    /// What the `main` window's status bar shows about a session: whether its
    /// workspace is registered here, and the Git branch that workspace has
    /// checked out (ADR-0064).
    ///
    /// The branch is read from `HEAD` through the anchored broker, with no Git
    /// program, configuration, or link followed. It is display text only.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown session, `busy`, `control`, or `internal`.
    pub fn session_context(&self, request: SessionRequest) -> Result<SessionContext, DesktopError> {
        let Some((store, workspace_id)) = self.session_workspace(request)? else {
            return Ok(SessionContext {
                workspace_registered: false,
                branch: branch_view(None),
            });
        };
        let head = workspace_head(&store, workspace_id);
        drop(store);
        Ok(SessionContext {
            workspace_registered: true,
            branch: branch_view(Some(head.unwrap_or(WorkspaceHead::Unreadable(
                "the workspace record cannot be used",
            )))),
        })
    }

    /// The connection a session's next turn uses, where that choice comes from,
    /// the session's model, and the connections that may be chosen, for the
    /// `main` window's composer and status bar (ADR-0064, ADR-0071). With no
    /// session it describes the default: the global selection, or the launch
    /// endpoint. It names profiles, kinds, and endpoints only, never a
    /// credential, and reads no native credential store.
    ///
    /// A session whose recorded choice no longer holds is reported with
    /// `stale` set and still shows what was recorded.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown session, `busy`, `control`, or `internal`.
    pub fn connection_status(
        &self,
        request: ConnectionStatusRequest,
    ) -> Result<ConnectionStatus, DesktopError> {
        let inner = &self.inner;
        if let Some(session_id) = request.session_id {
            inner.resolve_path(session_id)?;
        }
        let store = inner.open_control()?;
        let (records, unreadable) = inner.session_records();
        let record = request
            .session_id
            .and_then(|session_id| records.get(&session_id));
        let saved_model = || {
            preferences::load(&inner.data_root)
                .ok()
                .flatten()
                .map(|saved| saved.model)
                .unwrap_or_default()
        };
        let model = record.map_or_else(saved_model, |record| record.model.clone());
        let fixed = inner.token.is_fixed();
        let (connection, origin, stale) = if fixed {
            (
                connection_view(inner, store.as_ref()),
                ConnectionOrigin::Launch,
                false,
            )
        } else {
            match record.map(|record| &record.connection) {
                None | Some(ChosenConnection::Follow {}) => (
                    connection_view(inner, store.as_ref()),
                    ConnectionOrigin::Default,
                    false,
                ),
                Some(ChosenConnection::Default { endpoint }) => (
                    ConnectionView {
                        endpoint: display_safe_line(endpoint),
                        source: ConnectionSourceView::Active,
                        profile: None,
                        kind: None,
                        selectable: true,
                    },
                    ConnectionOrigin::Session,
                    *endpoint != inner.endpoint,
                ),
                Some(ChosenConnection::Profile { name, endpoint }) => {
                    let current = store.as_ref().and_then(|store| {
                        store
                            .check_provider_selectable(name)
                            .ok()
                            .and(recorded_endpoint(store, name))
                    });
                    let kind = store
                        .as_ref()
                        .and_then(|store| store.provider_profile(name))
                        .map(|status| status.profile().kind().as_str());
                    (
                        ConnectionView {
                            endpoint: display_safe_line(endpoint),
                            source: ConnectionSourceView::Active,
                            profile: Some(display_safe_line(name)),
                            kind,
                            selectable: true,
                        },
                        ConnectionOrigin::Session,
                        current.as_deref() != Some(endpoint.as_str()),
                    )
                }
            }
        };
        let choices = match (&store, fixed) {
            (Some(store), false) => store
                .provider_profiles()
                .iter()
                .map(|status| status.profile())
                .filter_map(|profile| {
                    let endpoint = profile.endpoint()?;
                    store.check_provider_selectable(profile.name()).ok()?;
                    Some(ConnectionChoice {
                        name: display_safe_line(profile.name()),
                        kind: profile.kind().as_str(),
                        endpoint: display_safe_line(endpoint.as_str()),
                        active: !stale
                            && connection.profile.as_deref()
                                == Some(display_safe_line(profile.name()).as_str()),
                    })
                })
                .collect(),
            _ => Vec::new(),
        };
        drop(store);
        Ok(ConnectionStatus {
            connection,
            origin,
            model,
            stale,
            session_choices_unreadable: unreadable,
            choices,
        })
    }

    /// The settings store, with the registered workspace a session was
    /// recorded in, or `None` when there is no store or the workspace is not
    /// registered here.
    pub(super) fn session_workspace(
        &self,
        request: SessionRequest,
    ) -> Result<Option<(ControlStore, WorkspaceId)>, DesktopError> {
        let inner = &self.inner;
        let recorded = inner.with_index(|state| {
            let lookup = |state: &super::state::IndexState| {
                state
                    .index
                    .session(request.session_id)
                    .map_err(|error| DesktopError::from_index(&error))
            };
            if let Some(session) = lookup(state)? {
                return Ok(session.workspace_id);
            }
            state.refresh_bounded(true)?;
            lookup(state)?
                .map(|session| session.workspace_id)
                .ok_or_else(|| DesktopError::not_found("session"))
        })?;
        let Some(recorded) = recorded else {
            return Ok(None);
        };
        let Some(store) = inner.open_control()? else {
            return Ok(None);
        };
        let Some(workspace_id) = store
            .registered_workspace(recorded)
            .map(|workspace| workspace.id())
        else {
            return Ok(None);
        };
        Ok(Some((store, workspace_id)))
    }
}

/// The status bar's view of a workspace's `HEAD`. `None` means no registered
/// workspace.
fn branch_view(head: Option<WorkspaceHead>) -> BranchView {
    let (state, name, note) = match head {
        None | Some(WorkspaceHead::NotRepository) => (BranchState::None, None, None),
        Some(WorkspaceHead::Branch(name)) => (BranchState::Branch, Some(name), None),
        Some(WorkspaceHead::Detached(commit)) => (BranchState::Detached, Some(commit), None),
        Some(WorkspaceHead::Unreadable(reason)) => {
            (BranchState::Unavailable, None, Some(reason.to_owned()))
        }
    };
    BranchView {
        state,
        name: name.map(|name| display_safe_line(&name)),
        note,
    }
}

/// The connection the next turn uses. The active connection is read from
/// `store`, the store the caller holds; with no store, none is active.
fn connection_view(inner: &Inner, store: Option<&ControlStore>) -> ConnectionView {
    let launch = |source, profile: Option<&String>| ConnectionView {
        endpoint: display_safe_line(&inner.endpoint),
        source,
        profile: profile.map(|name| display_safe_line(name)),
        kind: profile
            .and_then(|name| store?.provider_profile(name))
            .map(|status| status.profile().kind().as_str()),
        selectable: false,
    };
    match &inner.token {
        TokenResolver::Source(TokenSource::Environment) => {
            launch(ConnectionSourceView::Environment, None)
        }
        TokenResolver::Source(TokenSource::Profile(name)) => {
            launch(ConnectionSourceView::Profile, Some(name))
        }
        TokenResolver::Injected(_) => launch(ConnectionSourceView::Injected, None),
        TokenResolver::Source(TokenSource::Active) => {
            let active = store
                .and_then(ControlStore::active_provider_profile)
                .map(|status| status.profile());
            ConnectionView {
                endpoint: display_safe_line(
                    active
                        .and_then(|profile| profile.endpoint())
                        .map_or(inner.endpoint.as_str(), |endpoint| endpoint.as_str()),
                ),
                source: ConnectionSourceView::Active,
                profile: active.map(|profile| display_safe_line(profile.name())),
                kind: active.map(|profile| profile.kind().as_str()),
                selectable: true,
            }
        }
    }
}

const fn platform_name(platform: ProfilePlatform) -> &'static str {
    match platform {
        ProfilePlatform::Linux => "linux",
        ProfilePlatform::Windows => "windows",
    }
}

const fn access_name(access: ContainerGrantAccess) -> &'static str {
    match access {
        ContainerGrantAccess::ReadExecute => "read_execute",
        ContainerGrantAccess::ReadWrite => "read_write",
    }
}

const fn grant_state_name(state: ContainerGrantState) -> &'static str {
    match state {
        ContainerGrantState::Pending => "pending",
        ContainerGrantState::Active => "active",
        ContainerGrantState::Revoking => "revoking",
    }
}
