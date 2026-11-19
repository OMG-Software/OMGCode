//! `main`-window handlers for chat settings, turns, status, and cancellation,
//! and for the status bar's workspace branch and model connection (ADR-0064).

use omgcode_app::desktop::{
    ChatDefaults, ChatEvent, ChatEventSink, ChatModels, ChatPreferencesDto,
    ChatPreferencesSaveRequest, ChatSendRequest, ChatSettingsDto, ChatStatus,
    ConnectionChooseRequest, ConnectionStatus, SessionContext, SessionRequest, SessionSkills,
};
use tauri::Webview;
use tauri::ipc::Channel;
use uuid::Uuid;

use super::{Service, blocking};
use crate::dialogs::NativePrompt;
use crate::error::CommandError;
use crate::policy::{Command, authorize};

/// `main` only. Returns the chat defaults: the fixed endpoint the process was
/// started with, and the saved chat preferences or the built-in values.
///
/// # Errors
///
/// `forbidden_window`, or `internal` when the blocking task fails.
#[tauri::command(rename_all = "snake_case")]
pub async fn chat_defaults(
    webview: Webview,
    state: Service<'_>,
) -> Result<ChatDefaults, CommandError> {
    authorize(Command::ChatDefaults, webview.label())?;
    blocking(&state, |service| Ok(service.chat_defaults())).await
}

/// `main` only. Saves the chat preferences the composer starts from
/// (ADR-0059 decision 8). They grant nothing and carry no endpoint.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`invalid_request`,
/// `preferences`).
#[tauri::command(rename_all = "snake_case")]
pub async fn chat_preferences_save(
    webview: Webview,
    state: Service<'_>,
    preferences: ChatPreferencesDto,
) -> Result<ChatDefaults, CommandError> {
    authorize(Command::ChatPreferencesSave, webview.label())?;
    let request = ChatPreferencesSaveRequest { preferences };
    blocking(&state, move |service| {
        service.chat_preferences_save(request)
    })
    .await
}

/// `main` only. Lists the skills enabled for a session's workspace, which the
/// operator may select for one turn. Names only.
///
/// # Errors
///
/// `forbidden_window`, or the service's error.
#[tauri::command(rename_all = "snake_case")]
pub async fn session_skills(
    webview: Webview,
    state: Service<'_>,
    session_id: Uuid,
) -> Result<SessionSkills, CommandError> {
    authorize(Command::SessionSkills, webview.label())?;
    let request = SessionRequest { session_id };
    blocking(&state, move |service| service.session_skills(request)).await
}

/// `main` only. Whether the session's workspace is registered here, and the
/// Git branch it has checked out, read from `HEAD` for display (ADR-0064).
///
/// # Errors
///
/// `forbidden_window`, or the service's error.
#[tauri::command(rename_all = "snake_case")]
pub async fn session_context(
    webview: Webview,
    state: Service<'_>,
    session_id: Uuid,
) -> Result<SessionContext, CommandError> {
    authorize(Command::SessionContext, webview.label())?;
    let request = SessionRequest { session_id };
    blocking(&state, move |service| service.session_context(request)).await
}

/// `main` only. The connection the next turn uses and the local connections
/// the status bar may offer. Names and endpoints only, never a credential.
///
/// # Errors
///
/// `forbidden_window`, or the service's error.
#[tauri::command(rename_all = "snake_case")]
pub async fn connection_status(
    webview: Webview,
    state: Service<'_>,
) -> Result<ConnectionStatus, CommandError> {
    authorize(Command::ConnectionStatus, webview.label())?;
    blocking(&state, |service| service.connection_status()).await
}

/// `main` only. Asks to make a recorded local profile (or the default, for a
/// `null` profile) the model connection. Rust shows a native Yes/No dialog,
/// modal to the main window, naming the provider and endpoint, and records the
/// choice only on Yes (ADR-0064).
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`connection_fixed`,
/// `cancelled`, `control`, `stale_revision`).
#[tauri::command(rename_all = "snake_case")]
pub async fn connection_select(
    webview: Webview,
    state: Service<'_>,
    profile: Option<String>,
) -> Result<ConnectionStatus, CommandError> {
    authorize(Command::ConnectionSelect, webview.label())?;
    let prompt = NativePrompt::new(webview.window());
    let request = ConnectionChooseRequest { profile };
    blocking(&state, move |service| {
        service.connection_select(request, &prompt)
    })
    .await
}

/// `main` only. Lists the models the next turn's connection reports, for the
/// composer's model choice (ADR-0066). Ids only; the list grants nothing.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`provider`, `configuration`,
/// `credential`).
#[tauri::command(rename_all = "snake_case")]
pub async fn chat_models(webview: Webview, state: Service<'_>) -> Result<ChatModels, CommandError> {
    authorize(Command::ChatModels, webview.label())?;
    blocking(&state, |service| service.chat_models()).await
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
    skills: Vec<String>,
) -> Result<ChatStatus, CommandError> {
    authorize(Command::ChatSend, webview.label())?;
    let request = ChatSendRequest {
        session_id,
        message,
        settings,
        skills,
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
