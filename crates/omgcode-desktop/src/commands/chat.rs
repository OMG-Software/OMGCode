//! `main`-window handlers for chat settings, turns, status, and cancellation.

use omgcode_app::desktop::{ChatSendRequest, ChatSettingsDto, ChatStatus, SessionRequest};
use tauri::Webview;
use uuid::Uuid;

use super::{Service, blocking};
use crate::error::CommandError;
use crate::policy::{Command, authorize};

/// `main` only. Returns the process-wide chat defaults, including the fixed
/// endpoint the process was started with.
///
/// # Errors
///
/// `forbidden_window`, or `internal` when the blocking task fails.
#[tauri::command(rename_all = "snake_case")]
pub async fn chat_defaults(
    webview: Webview,
    state: Service<'_>,
) -> Result<ChatSettingsDto, CommandError> {
    authorize(Command::ChatDefaults, webview.label())?;
    blocking(&state, |service| Ok(service.chat_defaults())).await
}

/// `main` only. Starts one chat turn on a service thread and returns at once.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`invalid_request` when the
/// endpoint differs from the process endpoint).
#[tauri::command(rename_all = "snake_case")]
pub async fn chat_send(
    webview: Webview,
    state: Service<'_>,
    session_id: Uuid,
    message: String,
    settings: ChatSettingsDto,
) -> Result<ChatStatus, CommandError> {
    authorize(Command::ChatSend, webview.label())?;
    let request = ChatSendRequest {
        session_id,
        message,
        settings,
    };
    blocking(&state, move |service| service.chat_send(request)).await
}

/// `main` only. Reports a session's chat turn state.
///
/// # Errors
///
/// `forbidden_window`, or the service's error.
#[tauri::command(rename_all = "snake_case")]
pub async fn chat_status(
    webview: Webview,
    state: Service<'_>,
    session_id: Uuid,
) -> Result<ChatStatus, CommandError> {
    authorize(Command::ChatStatus, webview.label())?;
    let request = SessionRequest { session_id };
    blocking(&state, move |service| service.chat_status(request)).await
}

/// `main` only. Requests cooperative cancellation of a running turn.
///
/// # Errors
///
/// `forbidden_window`, or the service's error.
#[tauri::command(rename_all = "snake_case")]
pub async fn chat_cancel(
    webview: Webview,
    state: Service<'_>,
    session_id: Uuid,
) -> Result<ChatStatus, CommandError> {
    authorize(Command::ChatCancel, webview.label())?;
    let request = SessionRequest { session_id };
    blocking(&state, move |service| service.chat_cancel(request)).await
}
