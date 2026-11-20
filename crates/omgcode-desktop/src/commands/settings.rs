//! `settings`-window handlers (ADR-0059, ADR-0060).
//!
//! The window reads the trusted configuration and, from phase S2, changes the
//! development root, workspace registrations, workspace preferences, and
//! package cache roots. It also records model connections, sets an Ollama Cloud key, and chooses
//! the active one (ADR-0064). No handler takes a path: a folder comes from a native
//! dialog that Rust opens ([`NativePrompt`]), and every change that widens
//! access waits for the operator's answer in a native confirmation. Each
//! handler runs on the blocking pool because the dialogs and the settings
//! store block.

use omgcode_app::desktop::{
    ApiKeySetRequest, ApiKeyText, ConnectionAddRequest, ConnectionRemoveRequest,
    ConnectionSelectRequest, NewConnectionKind, PermissionFlags, PermissionsChangeRequest,
    SettingsOverview, SettingsRevisionRequest, WorkspaceRevisionRequest,
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

/// `settings` only. Records a connection (ADR-0064, ADR-0069): a local LM
/// Studio or Ollama connection with a loopback `endpoint`, or `ollama_cloud`
/// with an `api_key` and no endpoint. Recording it grants nothing, so no
/// dialog is shown. The key is moved into a zeroing type on arrival and is
/// never returned, logged, or echoed.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`invalid_request`, `control`,
/// `credential_unavailable`, `credential_locked`).
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

/// `settings` only. Replaces the stored API key of an `ollama_cloud`
/// profile. The key goes to the same fixed host, so no dialog is shown. It is
/// never returned, logged, or echoed.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`invalid_request`, `control`,
/// `credential_unavailable`, `credential_locked`).
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
/// turns go (ADR-0064, ADR-0069).
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

/// `settings` only. Clears the window's workspace scope and returns the
/// unscoped overview (ADR-0069). The scope is display state; clearing it
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
