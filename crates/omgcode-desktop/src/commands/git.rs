//! `main`-window handler for the Git status tab (ADR-0085).

use omgcode_app::desktop::{GitStatusView, SessionRequest};
use tauri::Webview;
use uuid::Uuid;

use super::{Service, blocking};
use crate::error::CommandError;
use crate::policy::{Command, authorize};

/// `main` only. The Git branch and a bounded list of changed paths for the
/// session's registered workspace, read in process with no program run
/// (ADR-0085). The workspace comes from the session's record, never from the
/// webview.
///
/// # Errors
///
/// `forbidden_window`, or the service's error, including `unknown_workspace`
/// when the session's workspace is not registered here.
#[tauri::command(rename_all = "snake_case")]
pub async fn workspace_git_status(
    webview: Webview,
    state: Service<'_>,
    session_id: Uuid,
) -> Result<GitStatusView, CommandError> {
    authorize(Command::WorkspaceGitStatus, webview.label())?;
    let request = SessionRequest { session_id };
    blocking(&state, move |service| service.workspace_git_status(request)).await
}
