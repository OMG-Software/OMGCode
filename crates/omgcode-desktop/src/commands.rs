//! Command handlers exposed to the webviews.
//!
//! Every handler:
//! 1. takes the calling [`tauri::Webview`] and calls [`authorize`] first, with
//!    the runtime-supplied label of that webview, before it reads any state or
//!    argument-dependent data;
//! 2. returns only fixed-shape data or a [`CommandError`].
//!
//! Scheduling (Tauri 2, "Calling Rust from the Frontend"): a command without
//! `async` runs on the main (UI) thread unless declared
//! `#[tauri::command(async)]`; an `async` command runs on the async runtime's
//! worker threads. The service blocks on files, `SQLite`, and the settings
//! store (and `approvals_summary` for up to two seconds), so every service
//! handler is `async` and moves the call into
//! [`tauri::async_runtime::spawn_blocking`] through [`blocking`]. The service
//! is managed state of type `Arc<DesktopService>`; `State<'_, T>` is borrowed
//! for the handler, so a handler clones the `Arc` into the blocking closure.
//! The two handlers below that touch no service (`app_info` and
//! `approvals_show_window`) only read constants or send a window message, so
//! they stay synchronous.
//!
//! Arguments are matched by name: Tauri deserializes each parameter from the
//! key of the invoke argument object with the same name (`rename_all =
//! "snake_case"` keeps the Rust names; the default would expect camelCase
//! keys). Each parameter list therefore equals the keys the frontend passes,
//! which `tests/layers.rs` checks mechanically.

use std::sync::Arc;

use omgcode_app::desktop::{DesktopError, DesktopService, SessionRequest};
use serde::Serialize;
use tauri::{Manager, State, Webview};
use uuid::Uuid;

use crate::error::CommandError;
use crate::policy::{Command, WindowLabel, authorize};

// Handlers are referenced by their full module path in `generate_handler!`:
// the macro each `#[tauri::command]` generates is not re-exported by `pub use`.
pub mod approvals;
pub mod chat;
pub mod sessions;
pub mod settings;

/// The service as Tauri state.
pub type Service<'a> = State<'a, Arc<DesktopService>>;

/// Runs one service call on the blocking pool and maps its failures.
///
/// The caller has already passed [`authorize`]. A [`DesktopError`] becomes the
/// same `code` and `message`; a task that panicked or was cancelled becomes
/// `internal` with a fixed message.
pub(crate) async fn blocking<T, F>(state: &Service<'_>, work: F) -> Result<T, CommandError>
where
    T: Send + 'static,
    F: FnOnce(&DesktopService) -> Result<T, DesktopError> + Send + 'static,
{
    let service = Arc::clone(state.inner());
    match tauri::async_runtime::spawn_blocking(move || work(&service)).await {
        Ok(result) => result.map_err(CommandError::from),
        Err(_) => Err(CommandError::internal()),
    }
}

/// The reply to `app_info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppInfo {
    /// This crate's package version.
    pub version: String,
    /// `"windows"`, `"linux"`, or `"other"`.
    pub platform: &'static str,
    /// Whether the build supports model tools at all (not whether any is enabled).
    pub model_tools_supported: bool,
}

/// The platform name reported to the frontend.
const fn platform_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "other"
    }
}

/// Builds the `app_info` reply. Separate from the handler so tests need no webview.
#[must_use]
pub fn app_info_value() -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        platform: platform_name(),
        model_tools_supported: omgcode_app::MODEL_TOOLS_SUPPORTED,
    }
}

/// `main` only. Returns the version, platform, and model-tool support.
///
/// # Errors
///
/// `forbidden_window` when the caller is not `main`.
#[tauri::command(rename_all = "snake_case")]
pub fn app_info(webview: Webview) -> Result<AppInfo, CommandError> {
    authorize(Command::AppInfo, webview.label())?;
    Ok(app_info_value())
}

/// `main` only. Shows, unminimizes, and focuses the existing `approvals` window.
/// It creates no window and navigates nothing.
///
/// # Errors
///
/// `forbidden_window` when the caller is not `main`; `window_unavailable` when
/// the approvals window is missing or the platform refuses a step.
#[tauri::command(rename_all = "snake_case")]
pub fn approvals_show_window(webview: Webview) -> Result<(), CommandError> {
    authorize(Command::ApprovalsShowWindow, webview.label())?;
    show_window(&webview, WindowLabel::Approvals)
}

/// `main` only. Shows, unminimizes, and focuses the existing `settings` window
/// (ADR-0059). It creates no window and navigates nothing. It first clears the
/// window's workspace scope, so the window shows every workspace (ADR-0067).
///
/// # Errors
///
/// `forbidden_window` when the caller is not `main`; `internal` when the scope
/// state is unavailable; `window_unavailable` when the settings window is
/// missing or the platform refuses a step.
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_show_window(
    webview: Webview,
    state: Service<'_>,
) -> Result<(), CommandError> {
    authorize(Command::SettingsShowWindow, webview.label())?;
    blocking(&state, |service| service.settings_scope_clear()).await?;
    show_window(&webview, WindowLabel::Settings)
}

/// `main` only. Scopes the `settings` window to the workspace the named session
/// was recorded in, then shows, unminimizes, and focuses it (ADR-0067). The
/// workspace is resolved by Rust from the session index and the settings
/// store; the webview names only the session. The scope is display state: it
/// grants and removes no access.
///
/// # Errors
///
/// `forbidden_window` when the caller is not `main`; `not_found` for an unknown
/// session; `unknown_workspace` when its workspace is not registered;
/// `window_unavailable` when the settings window is missing or the platform
/// refuses a step.
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_show_workspace(
    webview: Webview,
    state: Service<'_>,
    session_id: Uuid,
) -> Result<(), CommandError> {
    authorize(Command::SettingsShowWorkspace, webview.label())?;
    let request = SessionRequest { session_id };
    blocking(&state, move |service| {
        service.settings_scope_workspace(request)
    })
    .await?;
    show_window(&webview, WindowLabel::Settings)
}

/// Show, unminimize, and focus one existing window. The caller has already
/// passed [`authorize`].
fn show_window(webview: &Webview, label: WindowLabel) -> Result<(), CommandError> {
    let window = webview
        .app_handle()
        .get_webview_window(label.as_str())
        .ok_or_else(CommandError::window_unavailable)?;
    window
        .unminimize()
        .map_err(|_| CommandError::window_unavailable())?;
    window
        .show()
        .map_err(|_| CommandError::window_unavailable())?;
    window
        .set_focus()
        .map_err(|_| CommandError::window_unavailable())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_info_reports_package_version_platform_and_tool_support() {
        // When the reply is built.
        let info = app_info_value();

        // Then it carries this crate's version and the app crate's constant.
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            info.model_tools_supported,
            omgcode_app::MODEL_TOOLS_SUPPORTED
        );
        assert!(["windows", "linux", "other"].contains(&info.platform));
    }

    #[test]
    fn app_info_serializes_with_snake_case_fields() {
        let value = serde_json::to_value(app_info_value()).expect("serialize");
        let object = value.as_object().expect("object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["model_tools_supported", "platform", "version"]);
    }
}
