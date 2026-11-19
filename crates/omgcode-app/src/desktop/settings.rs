//! Read-only views of the trusted configuration (ADR-0058, phase S1).
//!
//! [`DesktopService::settings_overview`] serves the `settings` window. It
//! reads the settings store and shapes what the trusted CLI's `workspace
//! status` and `skills list` show, with every path and recorded name passed
//! through core's display-safe functions. It changes nothing, reads no stored
//! secret, and returns no host path that could be reopened.
//!
//! [`DesktopService::session_skills`] serves the `main` window's per-turn skill
//! choice. It returns only the names of skills enabled for the session's
//! workspace; enabling a skill grants no permission (ADR-0057).

use omgcode_core::control::{ContainerGrantAccess, ContainerGrantState, ProfilePlatform};
use omgcode_core::turn::display_safe_line;

use super::error::DesktopError;
use super::state::folder_of_directory;
use super::wire::{
    ConnectionSourceView, ConnectionView, ContainerGrantView, PackageProfileView,
    ProviderProfileView, SessionRequest, SessionSkills, SettingsOverview, SkillOverrideView,
    SkillSettingView, ToolchainProfileView, WorkspaceSettingsView,
};
use super::{DesktopService, Inner, TokenResolver};
use crate::TokenSource;

impl DesktopService {
    /// A read-only view of the trusted configuration, for the `settings`
    /// window only.
    ///
    /// Without a settings store, every list is empty and `store_initialized`
    /// is false; the connection is still reported. The store is opened and
    /// released inside this call.
    ///
    /// # Errors
    ///
    /// `busy` when the settings store stays held past a two-second wait,
    /// `control` when it is invalid.
    pub fn settings_overview(&self) -> Result<SettingsOverview, DesktopError> {
        let inner = &self.inner;
        let connection = connection_view(inner);
        let Some(store) = inner.open_control()? else {
            return Ok(SettingsOverview {
                store_initialized: false,
                revision: 0,
                development_root: None,
                workspaces: Vec::new(),
                connection,
                provider_profiles: Vec::new(),
                toolchain_profiles: Vec::new(),
                package_profiles: Vec::new(),
                container_grants: Vec::new(),
                skills: Vec::new(),
            });
        };
        let workspaces = store
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
                    package_cache_root: store.package_cache_root_display(workspace.id()),
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
            store_initialized: true,
            revision: store.revision(),
            development_root: store.development_root_display(),
            workspaces,
            connection,
            provider_profiles,
            toolchain_profiles,
            package_profiles,
            container_grants,
            skills,
        };
        drop(store);
        Ok(overview)
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
        let none = SessionSkills {
            workspace_registered: false,
            skills: Vec::new(),
        };
        let Some(recorded) = recorded else {
            return Ok(none);
        };
        let Some(store) = inner.open_control()? else {
            return Ok(none);
        };
        let Some(workspace_id) = store
            .registered_workspace(recorded)
            .map(|workspace| workspace.id())
        else {
            return Ok(none);
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
}

/// The connection this process uses, as the `settings` window shows it.
fn connection_view(inner: &Inner) -> ConnectionView {
    let (source, profile) = match &inner.token {
        TokenResolver::Source(TokenSource::Environment) => {
            (ConnectionSourceView::Environment, None)
        }
        TokenResolver::Source(TokenSource::Profile(name)) => {
            (ConnectionSourceView::Profile, Some(display_safe_line(name)))
        }
        TokenResolver::Injected(_) => (ConnectionSourceView::Injected, None),
    };
    ConnectionView {
        endpoint: display_safe_line(&inner.endpoint),
        source,
        profile,
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
