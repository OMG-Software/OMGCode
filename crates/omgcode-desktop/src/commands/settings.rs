//! `settings`-window handlers (ADR-0059, ADR-0060).
//!
//! The window reads the trusted configuration and, from phase S2, changes the
//! development root, workspace registrations, workspace preferences, and
//! package cache roots. It also records model connections, sets a hosted provider's key, and
//! chooses the active one (ADR-0064, ADR-0092). No handler takes a path: a folder comes from a native
//! dialog that Rust opens ([`NativePrompt`]), and every change that widens
//! access waits for the operator's answer in a native confirmation. Each
//! handler runs on the blocking pool because the dialogs and the settings
//! store block.

use omgcode_app::desktop::{
    ApiKeySetRequest, ApiKeyText, ChatBudgetsDto, ChatBudgetsSaveRequest, ChatGptRequest,
    ChatGptSignOutResult, ConnectionAddRequest, ConnectionRemoveRequest, ConnectionSelectRequest,
    ContainerAccessChoice, ContainerGrantRequest, ContainerRevokeRequest, DiscoveredGrantsRequest,
    DiscoveredProfilesRequest, EnvironmentEntry, NewConnectionKind, PackageProfileAddRequest,
    PermissionFlags, PermissionsChangeRequest, ProfileRemoveRequest, SettingsOverview,
    SettingsRevisionRequest, SkillChoice, SkillRemoveRequest, SkillStateRequest,
    ToolchainDiscovery, ToolchainDiscoveryRequest, ToolchainProfileAddRequest,
    WebHostChangeRequest, WorkspaceInstructionsRequest, WorkspaceRevisionRequest, discovery_host,
};
use tauri::Webview;
use uuid::Uuid;

use super::{Service, blocking};
use crate::dialogs::NativePrompt;
use crate::error::CommandError;
use crate::policy::{Command, authorize};

/// `settings` only. Returns the trusted configuration, creating an empty
/// settings store when none exists (ADR-0060). It waits up to two seconds for
/// the settings store.
///
/// # Errors
///
/// `forbidden_window`, or the service's error.
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_overview(
    webview: Webview,
    state: Service<'_>,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsOverview, webview.label())?;
    blocking(&state, |service| service.settings_overview()).await
}

/// `settings` only. Opens a native folder dialog and, after a native
/// confirmation, sets the development root.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`cancelled` when the operator
/// closes a dialog or declines).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_choose_root(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsChooseRoot, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = SettingsRevisionRequest { revision };
    blocking(&state, move |service| {
        service.settings_choose_root(request, &prompt)
    })
    .await
}

/// `settings` only. Opens a native folder dialog at the development root and,
/// after a native confirmation, registers the folder as a workspace.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`no_development_root`,
/// `cancelled`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_register_workspace(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsRegisterWorkspace, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = SettingsRevisionRequest { revision };
    blocking(&state, move |service| {
        service.settings_register_workspace(request, &prompt)
    })
    .await
}

/// `settings` only. Sets one workspace's complete preference set. Turning any
/// preference on first needs a native confirmation.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`unknown_workspace`,
/// `cancelled`, `runs_not_stopped`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_set_permissions(
    webview: Webview,
    state: Service<'_>,
    workspace_id: Uuid,
    revision: u64,
    permissions: PermissionFlags,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsSetPermissions, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = PermissionsChangeRequest {
        workspace_id,
        revision,
        permissions,
    };
    blocking(&state, move |service| {
        service.settings_set_permissions(request, &prompt)
    })
    .await
}

/// `settings` only. Opens a native folder dialog and, after a native
/// confirmation, sets one workspace's package cache root.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`unknown_workspace`,
/// `cancelled`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_choose_package_cache(
    webview: Webview,
    state: Service<'_>,
    workspace_id: Uuid,
    revision: u64,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsChoosePackageCache, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = WorkspaceRevisionRequest {
        workspace_id,
        revision,
    };
    blocking(&state, move |service| {
        service.settings_choose_package_cache(request, &prompt)
    })
    .await
}

/// `settings` only. Clears one workspace's package cache root. It narrows
/// access, so no dialog is shown.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`unknown_workspace`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_clear_package_cache(
    webview: Webview,
    state: Service<'_>,
    workspace_id: Uuid,
    revision: u64,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsClearPackageCache, webview.label())?;
    let request = WorkspaceRevisionRequest {
        workspace_id,
        revision,
    };
    blocking(&state, move |service| {
        service.settings_clear_package_cache(request)
    })
    .await
}

/// `settings` only. After a native confirmation that names the folder and what
/// is removed, unregisters one workspace (ADR-0081). The folder and its files
/// are not touched.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`unknown_workspace`,
/// `turn_running`, `cancelled`, `stale_revision`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_unregister_workspace(
    webview: Webview,
    state: Service<'_>,
    workspace_id: Uuid,
    revision: u64,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsUnregisterWorkspace, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = WorkspaceRevisionRequest {
        workspace_id,
        revision,
    };
    blocking(&state, move |service| {
        service.settings_unregister_workspace(request, &prompt)
    })
    .await
}

/// `settings` only. After a native confirmation that names the provider and
/// endpoint, makes a recorded local profile (or the default endpoint, for a
/// `null` profile) the active model connection (ADR-0064).
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`connection_fixed`,
/// `cancelled`, `control`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_select_connection(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    profile: Option<String>,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsSelectConnection, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = ConnectionSelectRequest { revision, profile };
    blocking(&state, move |service| {
        service.settings_select_connection(request, &prompt)
    })
    .await
}

/// `settings` only. Records a connection (ADR-0064, ADR-0071, ADR-0092): a
/// local LM Studio or Ollama connection with a loopback `endpoint`, a named
/// hosted provider with an `api_key` and no endpoint, or `openai_compatible`
/// with an HTTPS `endpoint` and an `api_key`. Recording it grants nothing, so no
/// dialog is shown. The key is moved into a zeroing type on arrival and is
/// never returned, logged, or echoed.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`invalid_request`, `control`,
/// `credential_unavailable`, `credential_storage_not_private`,
/// `credential_locked`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_add_connection(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    name: String,
    kind: NewConnectionKind,
    endpoint: Option<String>,
    api_key: Option<ApiKeyText>,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsAddConnection, webview.label())?;
    let request = ConnectionAddRequest {
        revision,
        name,
        kind,
        endpoint,
        api_key,
    };
    blocking(&state, move |service| {
        service.settings_add_connection(request)
    })
    .await
}

/// `settings` only. Replaces the stored API key of a hosted profile. The key
/// goes to the same recorded endpoint, so no dialog is shown. It is
/// never returned, logged, or echoed.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`invalid_request`, `control`,
/// `credential_unavailable`, `credential_storage_not_private`,
/// `credential_locked`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_set_api_key(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    name: String,
    api_key: ApiKeyText,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsSetApiKey, webview.label())?;
    let request = ApiKeySetRequest {
        revision,
        name,
        api_key,
    };
    blocking(&state, move |service| service.settings_set_api_key(request)).await
}

/// `settings` only. Removes a recorded provider profile. A profile with a
/// stored API key first asks a native Yes/No saying the connection and its
/// key will be deleted; removing the active connection also says where new
/// turns go (ADR-0064, ADR-0071).
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`control`, `cancelled`,
/// `credential_unavailable`, `credential_orphaned`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_remove_connection(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    name: String,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsRemoveConnection, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = ConnectionRemoveRequest { revision, name };
    blocking(&state, move |service| {
        service.settings_remove_connection(request, &prompt)
    })
    .await
}

/// `settings` only. After a native confirmation that names the browser step
/// and what goes to OpenAI, signs a connection in with ChatGPT (ADR-0093),
/// recording an `openai` subscription profile first when `name` names none.
/// Rust opens the system browser and waits for the loopback callback for at
/// most five minutes; the webview never sees the address, the callback, or a
/// token. `settings_cancel_chatgpt_sign_in` ends the wait.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`cancelled`,
/// `stale_revision`, `settings_change_running`, `control`, `credential`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_sign_in_chatgpt(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    name: String,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsSignInChatGpt, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = ChatGptRequest { revision, name };
    blocking(&state, move |service| {
        service.settings_sign_in_chatgpt(request, &prompt)
    })
    .await
}

/// `settings` only. Cancels the running ChatGPT sign-in, if any, including
/// one still waiting for its native confirmation. With no sign-in running it
/// does nothing. It takes nothing and grants nothing.
///
/// # Errors
///
/// `forbidden_window`.
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_cancel_chatgpt_sign_in(
    webview: Webview,
    state: Service<'_>,
) -> Result<(), CommandError> {
    authorize(Command::SettingsCancelChatGptSignIn, webview.label())?;
    blocking(&state, move |service| {
        service.settings_cancel_chatgpt_sign_in();
        Ok(())
    })
    .await
}

/// `settings` only. After a native confirmation, revokes a ChatGPT session
/// once, records the sign-out, and deletes its tokens. `revoked` says
/// whether OpenAI confirmed the revocation.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`cancelled`,
/// `stale_revision`, `settings_change_running`, `credential`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_sign_out_chatgpt(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    name: String,
) -> Result<ChatGptSignOutResult, CommandError> {
    authorize(Command::SettingsSignOutChatGpt, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = ChatGptRequest { revision, name };
    blocking(&state, move |service| {
        service.settings_sign_out_chatgpt(request, &prompt)
    })
    .await
}

/// `settings` only. Lists the extra grants on the application data folder
/// and, after a native confirmation that names the folder and each account,
/// removes exactly those grants (ADR-0073). The request carries nothing: Rust
/// reads the folder and the list itself.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`settings_change_running`,
/// `cancelled`, `data_folder_unreadable`, `data_folder_not_repairable`,
/// `data_folder_changed`, `data_folder_repair_failed`, or
/// `data_folder_repaired` when the grants were removed but the settings could
/// not be read again).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_repair_data_folder(
    webview: Webview,
    state: Service<'_>,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsRepairDataFolder, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    blocking(&state, move |service| {
        service.settings_repair_data_folder(&prompt)
    })
    .await
}

/// `settings` only. Clears the window's workspace scope and returns the
/// unscoped overview (ADR-0071). The scope is display state; clearing it
/// changes no setting.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`internal`, `busy`, `control`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_clear_scope(
    webview: Webview,
    state: Service<'_>,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsClearScope, webview.label())?;
    blocking(&state, |service| {
        service.settings_scope_clear()?;
        service.settings_overview()
    })
    .await
}

/// `settings` only. Saves the chat budgets and the global custom instructions
/// (ADR-0080). They grant nothing, so no native confirmation is shown.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`invalid_request`,
/// `preferences`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_save_chat_defaults(
    webview: Webview,
    state: Service<'_>,
    defaults: ChatBudgetsDto,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsSaveChatDefaults, webview.label())?;
    let request = ChatBudgetsSaveRequest { defaults };
    blocking(&state, move |service| {
        service.settings_save_chat_defaults(request)
    })
    .await
}

/// `settings` only. Replaces one registered workspace's custom instructions
/// (ADR-0080); empty text removes them. They grant nothing, so no native
/// confirmation is shown.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`unknown_workspace`,
/// `invalid_request`, `preferences`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_set_workspace_instructions(
    webview: Webview,
    state: Service<'_>,
    workspace_id: Uuid,
    instructions: String,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsSetWorkspaceInstructions, webview.label())?;
    let request = WorkspaceInstructionsRequest {
        workspace_id,
        instructions,
    };
    blocking(&state, move |service| {
        service.settings_set_workspace_instructions(request)
    })
    .await
}

/// `settings` only. Opens a native folder dialog and imports the chosen skill
/// folder through the same core path as `omgcode skills add`. The skill is
/// registered disabled and nothing in it runs (ADR-0082). The request carries
/// only the revision; no path comes from the webview.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`cancelled`, `stale_revision`,
/// and core's skill codes such as `skill_name_taken` or `skill_md_missing`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_import_skill(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsImportSkill, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = SettingsRevisionRequest { revision };
    blocking(&state, move |service| {
        service.settings_import_skill(request, &prompt)
    })
    .await
}

/// `settings` only. Sets a skill's application setting (`workspace_id` null)
/// or one workspace's override (`enabled`, `disabled`, or `inherit`). A change
/// that leaves the skill enabled there first needs a native confirmation that
/// names it; disabling needs none (ADR-0082).
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`skill_not_found`,
/// `skill_workspace_unknown`, `skill_inherit_needs_workspace`, `cancelled`,
/// `stale_revision`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_set_skill_state(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    name: String,
    workspace_id: Option<Uuid>,
    skill_state: SkillChoice,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsSetSkillState, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = SkillStateRequest {
        revision,
        name,
        workspace_id,
        state: skill_state,
    };
    blocking(&state, move |service| {
        service.settings_set_skill_state(request, &prompt)
    })
    .await
}

/// `settings` only. After a native confirmation that names the skill and says
/// its stored copy is deleted, removes it as `omgcode skills remove` does
/// (ADR-0082). The folder it was imported from is never touched.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`skill_not_found`, `cancelled`,
/// `stale_revision`, `package_orphaned` when the removal is recorded but the
/// stored copy was not fully deleted).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_remove_skill(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    name: String,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsRemoveSkill, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = SkillRemoveRequest { revision, name };
    blocking(&state, move |service| {
        service.settings_remove_skill(request, &prompt)
    })
    .await
}

/// `settings` only. Opens a native folder dialog for each read-only root and
/// each `PATH` entry (`root_count` and `path_entry_count` say how many; no
/// path is carried), then, after a native confirmation that lists them and
/// the variable names, records a toolchain profile as `omgcode workspace
/// profile-add` does (ADR-0088).
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`invalid_request`,
/// `cancelled`, `stale_revision`, or `control` with core's reason).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_add_toolchain_profile(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    name: String,
    environment: Vec<EnvironmentEntry>,
    root_count: u8,
    path_entry_count: u8,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsAddToolchainProfile, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = ToolchainProfileAddRequest {
        revision,
        name,
        environment,
        root_count,
        path_entry_count,
    };
    blocking(&state, move |service| {
        service.settings_add_toolchain_profile(request, &prompt)
    })
    .await
}

/// `settings` only. After a native confirmation, removes a toolchain profile
/// as `omgcode workspace profile-remove` does and stops the runs the settings
/// then withdraw (ADR-0088).
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`cancelled`,
/// `stale_revision`, `runs_not_stopped`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_remove_toolchain_profile(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    name: String,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsRemoveToolchainProfile, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = ProfileRemoveRequest { revision, name };
    blocking(&state, move |service| {
        service.settings_remove_toolchain_profile(request, &prompt)
    })
    .await
}

/// `settings` only. After a native confirmation that lists the origins core
/// parsed, records a package-network profile (configuration only) as `omgcode
/// workspace package-profile-add` does (ADR-0088).
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`invalid_request`,
/// `cancelled`, `stale_revision`, or `control` with core's reason).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_add_package_profile(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    name: String,
    origins: Vec<String>,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsAddPackageProfile, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = PackageProfileAddRequest {
        revision,
        name,
        origins,
    };
    blocking(&state, move |service| {
        service.settings_add_package_profile(request, &prompt)
    })
    .await
}

/// `settings` only. After a native confirmation, removes a package-network
/// profile as `omgcode workspace package-profile-remove` does, which also
/// unbinds and deletes the registry credentials bound to its origins
/// (ADR-0088).
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`cancelled`,
/// `stale_revision`, `credential_orphaned`, `runs_not_stopped`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_remove_package_profile(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    name: String,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsRemovePackageProfile, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = ProfileRemoveRequest { revision, name };
    blocking(&state, move |service| {
        service.settings_remove_package_profile(request, &prompt)
    })
    .await
}

/// `settings` only, Windows only. Opens a native folder dialog and, after a
/// native confirmation that names the workspace, the folder and the access and
/// says the folder's access-control list changes, grants the folder to the
/// workspace's container as `omgcode workspace container-grant` does
/// (ADR-0088).
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`unsupported_platform`,
/// `unknown_workspace`, `cancelled`, `stale_revision`,
/// `container_access_failed`, or `control` with core's reason).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_grant_container_folder(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    workspace_id: Uuid,
    access: ContainerAccessChoice,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsGrantContainerFolder, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = ContainerGrantRequest {
        revision,
        workspace_id,
        access,
    };
    blocking(&state, move |service| {
        service.settings_grant_container_folder(request, &prompt)
    })
    .await
}

/// `settings` only, Windows only. After a native confirmation, revokes one
/// recorded container grant as `omgcode workspace container-revoke` does and
/// stops the workspace's runs that started before it (ADR-0088).
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`unsupported_platform`,
/// `cancelled`, `stale_revision`, `container_access_failed`,
/// `runs_not_stopped`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_revoke_container_grant(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    grant_id: Uuid,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsRevokeContainerGrant, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = ContainerRevokeRequest { revision, grant_id };
    blocking(&state, move |service| {
        service.settings_revoke_container_grant(request, &prompt)
    })
    .await
}

/// `settings` only. Reads this computer's standard install locations and the
/// operator's `PATH` and returns a toolchain profile proposal for each
/// toolchain found, checked against the settings (ADR-0103). No program runs,
/// no workspace file is read, and nothing is recorded. With `workspace_id`,
/// on Windows, each root says whether that workspace's container can read it.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`unsupported_platform`,
/// `unknown_workspace`, `busy`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_discover_toolchains(
    webview: Webview,
    state: Service<'_>,
    workspace_id: Option<Uuid>,
) -> Result<ToolchainDiscovery, CommandError> {
    authorize(Command::SettingsDiscoverToolchains, webview.label())?;
    let request = ToolchainDiscoveryRequest { workspace_id };
    blocking(&state, move |service| {
        service.settings_discover_toolchains(request, &discovery_host()?)
    })
    .await
}

/// `settings` only. Discovers again, and after one native confirmation that
/// lists each selected proposal's roots, `PATH` entries, and variables,
/// records the proposals whose digests the window sent as `omgcode workspace
/// profile-add` does (ADR-0103). No path is carried.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`invalid_request`,
/// `discovery_changed`, `cancelled`, `stale_revision`, or `control` with
/// core's reason).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_add_discovered_profiles(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    digests: Vec<String>,
    workspace_id: Option<Uuid>,
) -> Result<ToolchainDiscovery, CommandError> {
    authorize(Command::SettingsAddDiscoveredProfiles, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = DiscoveredProfilesRequest {
        revision,
        digests,
        workspace_id,
    };
    blocking(&state, move |service| {
        service.settings_add_discovered_profiles(request, &discovery_host()?, &prompt)
    })
    .await
}

/// `settings` only, Windows only. Discovers again, and after one native
/// confirmation that names the workspace and every folder, grants the
/// workspace's container read-execute access to the selected proposals' roots
/// that it cannot read and this account can grant, as `omgcode workspace
/// container-grant` does (ADR-0103).
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`unsupported_platform`,
/// `invalid_request`, `discovery_changed`, `unknown_workspace`,
/// `nothing_to_grant`, `cancelled`, `stale_revision`,
/// `container_access_failed`, or `control` with core's reason).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_grant_discovered_roots(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    workspace_id: Uuid,
    digests: Vec<String>,
) -> Result<ToolchainDiscovery, CommandError> {
    authorize(Command::SettingsGrantDiscoveredRoots, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = DiscoveredGrantsRequest {
        revision,
        workspace_id,
        digests,
    };
    blocking(&state, move |service| {
        service.settings_grant_discovered_roots(request, &discovery_host()?, &prompt)
    })
    .await
}

/// `settings` only. After a native confirmation that names the workspace
/// folder and the canonical host pattern (and, for a wildcard, says it covers
/// every subdomain), adds one pattern to the workspace's `web_fetch`
/// allowlist. The pattern is parsed first; an invalid one is refused without
/// a dialog.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (a `web_host_*` code,
/// `unknown_workspace`, `cancelled`, `stale_revision`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_add_web_fetch_host(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    workspace_id: Uuid,
    pattern: String,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsAddWebFetchHost, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = WebHostChangeRequest {
        revision,
        workspace_id,
        pattern,
    };
    blocking(&state, move |service| {
        service.settings_add_web_fetch_host(request, &prompt)
    })
    .await
}

/// `settings` only. After a native confirmation that names the workspace
/// folder and the canonical host pattern, removes one pattern from the
/// workspace's `web_fetch` allowlist.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (a `web_host_*` code,
/// `unknown_workspace`, `cancelled`, `stale_revision`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_remove_web_fetch_host(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    workspace_id: Uuid,
    pattern: String,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsRemoveWebFetchHost, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = WebHostChangeRequest {
        revision,
        workspace_id,
        pattern,
    };
    blocking(&state, move |service| {
        service.settings_remove_web_fetch_host(request, &prompt)
    })
    .await
}
