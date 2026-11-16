//! Approval handlers. `approvals_summary` belongs to `main`; the other three
//! belong to the `approvals` window only (ADR-0046 decisions 1 and 2).

use omgcode_app::desktop::{
    ApprovalDecideRequest, ApprovalDetail, ApprovalList, ApprovalRequest, ApprovalView,
    ApprovalsSummary, Decision,
};
use tauri::Webview;
use uuid::Uuid;

use super::{Service, blocking};
use crate::error::CommandError;
use crate::policy::{Command, authorize};

/// `main` only. Counts pending approvals for the badge. It waits up to two
/// seconds for the settings store, which is why it runs on the blocking pool.
///
/// # Errors
///
/// `forbidden_window`, or the service's error.
#[tauri::command(rename_all = "snake_case")]
pub async fn approvals_summary(
    webview: Webview,
    state: Service<'_>,
) -> Result<ApprovalsSummary, CommandError> {
    authorize(Command::ApprovalsSummary, webview.label())?;
    blocking(&state, |service| service.approvals_summary()).await
}

/// `approvals` only. Lists approval records.
///
/// # Errors
///
/// `forbidden_window`, or the service's error.
#[tauri::command(rename_all = "snake_case")]
pub async fn approvals_list(
    webview: Webview,
    state: Service<'_>,
) -> Result<ApprovalList, CommandError> {
    authorize(Command::ApprovalsList, webview.label())?;
    blocking(&state, |service| service.approvals_list()).await
}

/// `approvals` only. Returns the full review of one approval and a single-use
/// review token.
///
/// # Errors
///
/// `forbidden_window`, or the service's error.
#[tauri::command(rename_all = "snake_case")]
pub async fn approval_detail(
    webview: Webview,
    state: Service<'_>,
    id: Uuid,
) -> Result<ApprovalDetail, CommandError> {
    authorize(Command::ApprovalDetail, webview.label())?;
    let request = ApprovalRequest { id };
    blocking(&state, move |service| service.approval_detail(request)).await
}

/// `approvals` only. Approves or denies one approval.
///
/// # Errors
///
/// `forbidden_window`, or the service's error.
#[tauri::command(rename_all = "snake_case")]
pub async fn approval_decide(
    webview: Webview,
    state: Service<'_>,
    id: Uuid,
    decision: Decision,
    writers_stopped: bool,
    review_token: Option<String>,
) -> Result<ApprovalView, CommandError> {
    authorize(Command::ApprovalDecide, webview.label())?;
    let request = ApprovalDecideRequest {
        id,
        decision,
        writers_stopped,
        review_token,
    };
    blocking(&state, move |service| service.approval_decide(request)).await
}
