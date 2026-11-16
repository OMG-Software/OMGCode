//! `main`-window handlers for chat settings, turns, status, and cancellation.

use omgcode_app::desktop::{
    ChatEvent, ChatEventSink, ChatSendRequest, ChatSettingsDto, ChatStatus, SessionRequest,
};
use tauri::Webview;
use tauri::ipc::Channel;
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

/// `main` only. Registers the one chat event subscriber, replacing any earlier
/// one, and replays the preview of every running turn (ADR-0049).
///
/// The caller creates the [`tauri::ipc::Channel`] and passes it as `on_event`.
/// A channel needs no capability beyond this command's own permission. A Tauri
/// event (`emit` and `listen`) would need a `core:event` permission, which both
/// windows are kept without (ADR-0045). The sink sends each event on the
/// channel and reports whether that worked; the service drops a sink that
/// reports failure, so a closed webview ends its own subscription.
///
/// `Channel::send` serializes the event and queues one script evaluation for the
/// webview; it does not wait for the page, so the sink does not block the turn
/// thread that calls it while the service's event lock is held.
///
/// # Errors
///
/// `forbidden_window`, or `internal` when the service's event state is
/// unavailable or the blocking task fails.
#[tauri::command(rename_all = "snake_case")]
pub async fn chat_subscribe(
    webview: Webview,
    state: Service<'_>,
    on_event: Channel<ChatEvent>,
) -> Result<(), CommandError> {
    authorize(Command::ChatSubscribe, webview.label())?;
    let sink: ChatEventSink =
        Box::new(move |event: &ChatEvent| on_event.send(event.clone()).is_ok());
    blocking(&state, move |service| service.chat_subscribe(sink)).await
}
