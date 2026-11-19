//! `settings`-window handlers (ADR-0058). In phase S1 the window can only read
//! the trusted configuration; every change is still a trusted CLI operation.

use omgcode_app::desktop::SettingsOverview;
use tauri::Webview;

use super::{Service, blocking};
use crate::error::CommandError;
use crate::policy::{Command, authorize};

/// `settings` only. Returns a read-only view of the trusted configuration. It
/// waits up to two seconds for the settings store, which is why it runs on the
/// blocking pool.
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
