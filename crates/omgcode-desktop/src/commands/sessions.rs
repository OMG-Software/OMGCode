//! `main`-window handlers for the session tree, search, transcripts, the
//! session lifecycle, workspaces, and session creation.

use omgcode_app::desktop::{
    SearchRequest, SearchResult, SessionCreateRequest, SessionProcesses, SessionRequest,
    SessionSummary, SessionTree, TranscriptPage, TranscriptRequest, TreeRequest, TreeSort,
    TreeView, WorkspaceList,
};
use tauri::Webview;
use uuid::Uuid;

use super::{Service, blocking};
use crate::error::CommandError;
use crate::policy::{Command, authorize};

/// `main` only. Lists sessions grouped by workspace folder.
///
/// # Errors
///
/// `forbidden_window`, or the service's error.
#[tauri::command(rename_all = "snake_case")]
pub async fn session_tree(
    webview: Webview,
    state: Service<'_>,
    view: TreeView,
    sort: TreeSort,
    descending: bool,
) -> Result<SessionTree, CommandError> {
    authorize(Command::SessionTree, webview.label())?;
    let request = TreeRequest {
        view,
        sort,
        descending,
    };
    blocking(&state, move |service| service.session_tree(request)).await
}

/// `main` only. Searches session text.
///
/// # Errors
///
/// `forbidden_window`, or the service's error.
#[tauri::command(rename_all = "snake_case")]
pub async fn session_search(
    webview: Webview,
    state: Service<'_>,
    query: String,
    view: TreeView,
    limit: u32,
) -> Result<SearchResult, CommandError> {
    authorize(Command::SessionSearch, webview.label())?;
    let request = SearchRequest { query, view, limit };
    blocking(&state, move |service| service.session_search(request)).await
}

/// `main` only. Reads one page of a session transcript.
///
/// # Errors
///
/// `forbidden_window`, or the service's error.
#[tauri::command(rename_all = "snake_case")]
pub async fn session_transcript(
    webview: Webview,
    state: Service<'_>,
    session_id: Uuid,
    after_seq: Option<u64>,
    limit: u32,
) -> Result<TranscriptPage, CommandError> {
    authorize(Command::SessionTranscript, webview.label())?;
    let request = TranscriptRequest {
        session_id,
        after_seq,
        limit,
    };
    blocking(&state, move |service| service.session_transcript(request)).await
}

/// `main` only. Archives a session.
///
/// # Errors
///
/// `forbidden_window`, or the service's error.
#[tauri::command(rename_all = "snake_case")]
pub async fn session_archive(
    webview: Webview,
    state: Service<'_>,
    session_id: Uuid,
) -> Result<SessionSummary, CommandError> {
    authorize(Command::SessionArchive, webview.label())?;
    let request = SessionRequest { session_id };
    blocking(&state, move |service| service.session_archive(request)).await
}

/// `main` only. Restores an archived session.
///
/// # Errors
///
/// `forbidden_window`, or the service's error.
#[tauri::command(rename_all = "snake_case")]
pub async fn session_restore(
    webview: Webview,
    state: Service<'_>,
    session_id: Uuid,
) -> Result<SessionSummary, CommandError> {
    authorize(Command::SessionRestore, webview.label())?;
    let request = SessionRequest { session_id };
    blocking(&state, move |service| service.session_restore(request)).await
}

/// `main` only. Closes a session's unfinished turn as operator-interrupted.
///
/// # Errors
///
/// `forbidden_window`, or the service's error.
#[tauri::command(rename_all = "snake_case")]
pub async fn session_interrupt(
    webview: Webview,
    state: Service<'_>,
    session_id: Uuid,
) -> Result<SessionSummary, CommandError> {
    authorize(Command::SessionInterrupt, webview.label())?;
    let request = SessionRequest { session_id };
    blocking(&state, move |service| service.session_interrupt(request)).await
}

/// `main` only. Lists the workspaces the operator registered.
///
/// # Errors
///
/// `forbidden_window`, or the service's error.
#[tauri::command(rename_all = "snake_case")]
pub async fn workspace_list(
    webview: Webview,
    state: Service<'_>,
) -> Result<WorkspaceList, CommandError> {
    authorize(Command::WorkspaceList, webview.label())?;
    blocking(&state, |service| service.workspace_list()).await
}

/// `main` only. Creates a session in a registered workspace.
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`unknown_workspace` for an
/// identifier the operator did not register).
#[tauri::command(rename_all = "snake_case")]
pub async fn session_create(
    webview: Webview,
    state: Service<'_>,
    workspace_id: Uuid,
    title: String,
) -> Result<SessionSummary, CommandError> {
    authorize(Command::SessionCreate, webview.label())?;
    let request = SessionCreateRequest {
        workspace_id,
        title,
    };
    blocking(&state, move |service| service.session_create(request)).await
}

/// `main` only. Lists the session's command-tool calls, newest first, from the
/// model action journal, the run journal, and the session journal. Read-only:
/// it starts, stops, and re-runs nothing, and it returns no captured output,
/// environment value, or credential (ADR-0086).
///
/// # Errors
///
/// `forbidden_window`, or the service's error (`not_found` for an unknown
/// session).
#[tauri::command(rename_all = "snake_case")]
pub async fn session_processes(
    webview: Webview,
    state: Service<'_>,
    session_id: Uuid,
) -> Result<SessionProcesses, CommandError> {
    authorize(Command::SessionProcesses, webview.label())?;
    let request = SessionRequest { session_id };
    blocking(&state, move |service| service.session_processes(request)).await
}
