//! `settings`-window handlers (ADR-0059, ADR-0060).
//!
//! The window reads the trusted configuration and, from phase S2, changes the
//! development root, workspace registrations, workspace preferences, and
//! package cache roots. It also records local model connections and chooses
//! the active one (ADR-0062). No handler takes a path: a folder comes from a native
//! dialog that Rust opens ([`NativePrompt`]), and every change that widens
//! access waits for the operator's answer in a native confirmation. Each
//! handler runs on the blocking pool because the dialogs and the settings
//! store block.

use omgcode_app::desktop::{
    ConnectionRemoveRequest, ConnectionSelectRequest, LocalConnectionAddRequest, LocalProviderKind,
    PermissionFlags, PermissionsChangeRequest, SettingsOverview, SettingsRevisionRequest,
    WorkspaceRevisionRequest,
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
/// `null` profile) the active model connection (ADR-0062).
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

/// `settings` only. Records a local LM Studio or Ollama connection with no
/// credential. Recording it grants nothing, so no dialog is shown.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`invalid_request`, `control`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_add_local_connection(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    name: String,
    kind: LocalProviderKind,
    endpoint: String,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsAddLocalConnection, webview.label())?;
    let request = LocalConnectionAddRequest {
        revision,
        name,
        kind,
        endpoint,
    };
    blocking(&state, move |service| {
        service.settings_add_local_connection(request)
    })
    .await
}

/// `settings` only. Removes a recorded provider profile that has no stored
/// credential. It narrows access, so no dialog is shown.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`credential_bound`,
/// `control`).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_remove_connection(
    webview: Webview,
    state: Service<'_>,
    revision: u64,
    name: String,
) -> Result<SettingsOverview, CommandError> {
    authorize(Command::SettingsRemoveConnection, webview.label())?;
    let request = ConnectionRemoveRequest { revision, name };
    blocking(&state, move |service| {
        service.settings_remove_connection(request)
    })
    .await
}
