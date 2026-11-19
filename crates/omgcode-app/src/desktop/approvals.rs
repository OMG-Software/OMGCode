//! Approval commands (ADR-0046).
//!
//! The service decides nothing about authority. Core owns the approval
//! records, the exact-binding review, and the refusals. This module lists
//! records, asks core to review the recorded request an approval is bound to,
//! issues a single-use review token for an exact review, and refuses an
//! approve whose token is missing, expired, or for another approval.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::time::Instant;

use omgcode_core::control::{ApprovalOperation, ApprovalState, ApprovalStatus, now_unix};
use omgcode_core::session::{SessionInspection, TranscriptEntryKind};
use omgcode_core::turn::{ApprovalReview, ReviewCandidate, display_safe_line, review_approval};
use uuid::Uuid;

use super::error::DesktopError;
use super::review_tokens::RedeemError;
use super::wire::{
    ApprovalDecideRequest, ApprovalDetail, ApprovalField, ApprovalList, ApprovalRequest,
    ApprovalView, ApprovalsSummary, Binding, Decision,
};
use super::{DesktopService, Inner};

/// Most non-pending approvals `approvals_list` returns, the most recent ones.
const MAX_LISTED_RESOLVED: usize = 50;

impl DesktopService {
    /// How many approvals are pending.
    ///
    /// Without a settings store the count is 0. When the store stays busy past
    /// the bounded wait, the last count read is returned: this is an advisory
    /// badge and grants nothing.
    ///
    /// # Errors
    ///
    /// `control` when the settings store is invalid.
    pub fn approvals_summary(&self) -> Result<ApprovalsSummary, DesktopError> {
        let inner = &self.inner;
        let pending = match inner.open_control() {
            Ok(Some(store)) => {
                let statuses = store.approval_statuses(now_unix());
                drop(store);
                inner.note_pending(&statuses)
            }
            Ok(None) => {
                inner.pending_cache.store(0, Ordering::Relaxed);
                0
            }
            Err(error) if error.code() == "busy" => inner.pending_cache.load(Ordering::Relaxed),
            Err(error) => return Err(error),
        };
        Ok(ApprovalsSummary { pending })
    }

    /// Every pending approval, then the 50 most recent others, each joined with
    /// its session's title and folder when the index knows the session.
    ///
    /// # Errors
    ///
    /// `busy` or `control` for the settings store, `internal` for service state.
    pub fn approvals_list(&self) -> Result<ApprovalList, DesktopError> {
        let inner = &self.inner;
        let now = now_unix();
        // Statuses first; the store is released before the index is locked.
        let statuses = match inner.open_control()? {
            Some(store) => {
                let statuses = store.approval_statuses(now);
                drop(store);
                statuses
            }
            None => Vec::new(),
        };
        inner.note_pending(&statuses);
        let (pending, resolved): (Vec<ApprovalStatus>, Vec<ApprovalStatus>) = statuses
            .into_iter()
            .partition(|status| status.state == ApprovalState::Pending);
        let skip = resolved.len().saturating_sub(MAX_LISTED_RESOLVED);
        let chosen: Vec<ApprovalStatus> = pending
            .into_iter()
            .chain(resolved.into_iter().skip(skip))
            .collect();
        let approvals = inner.views_of(&chosen)?;
        Ok(ApprovalList {
            now_unix: now,
            approvals,
        })
    }

    /// Review the recorded request that one approval is bound to.
    ///
    /// A non-pending approval is `stale` and nothing is read. For a pending
    /// one, the session's journal is inspected without the writer lock and core
    /// finds the recorded request whose digest the approval holds. Only an
    /// `exact` result carries the tool, the fields, and a fresh single-use
    /// review token, which replaces any earlier token for this approval.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown approval, `busy` when a turn runs here for the
    /// session or a store or journal is held, `invalid_journal`, `control`.
    pub fn approval_detail(
        &self,
        request: ApprovalRequest,
    ) -> Result<ApprovalDetail, DesktopError> {
        let inner = &self.inner;
        let status = inner.status_of_approval(request.id)?;
        let not_reviewed = |binding: Binding| -> Result<ApprovalDetail, DesktopError> {
            Ok(ApprovalDetail {
                approval: inner.view_of(&status)?,
                binding,
                tool: None,
                fields: Vec::new(),
                review_token: None,
                waiting: false,
            })
        };
        if status.state != ApprovalState::Pending {
            return not_reviewed(Binding::Stale);
        }
        // A running turn holds the session journal. When it is waiting on this
        // approval (ADR-0062), the call it published is the only candidate;
        // core still recomputes the digest from it and the live workspace.
        // A turn running for any other reason makes the review busy.
        let awaited = if inner.is_running(status.session_id)? {
            match inner.awaited_call(status.session_id)? {
                Some(call) if call.approval_id == status.id => Some(call),
                _ => return Err(DesktopError::busy()),
            }
        } else {
            None
        };
        let inspection;
        let candidates: Vec<ReviewCandidate<'_>> = if let Some(call) = &awaited {
            vec![ReviewCandidate {
                tool: &call.tool,
                arguments: &call.arguments,
            }]
        } else {
            let path = match inner.resolve_path(status.session_id) {
                Ok(path) => path,
                Err(error) if error.code() == "not_found" => {
                    return not_reviewed(Binding::NotFound);
                }
                Err(error) => return Err(error),
            };
            inspection = SessionInspection::inspect(&path)
                .map_err(|error| DesktopError::from_session(&error))?;
            inspection
                .entries
                .iter()
                .filter_map(|entry| match &entry.kind {
                    TranscriptEntryKind::ToolRequested {
                        tool, arguments, ..
                    } => Some(ReviewCandidate { tool, arguments }),
                    _ => None,
                })
                .collect()
        };
        // No service lock and no control store is held; core opens its own.
        let review = review_approval(&inner.data_root, &status, &candidates)
            .map_err(|error| DesktopError::from_review(&error))?;
        match review {
            ApprovalReview::Exact(reviewed) => {
                let token =
                    inner
                        .lock_reviews()?
                        .issue(status.id, &status.request_sha256, Instant::now());
                Ok(ApprovalDetail {
                    approval: inner.view_of(&status)?,
                    binding: Binding::Exact,
                    tool: Some(reviewed.tool),
                    fields: reviewed
                        .fields
                        .into_iter()
                        .map(|field| ApprovalField {
                            label: field.label.to_owned(),
                            value: field.value,
                            multiline: field.multiline,
                        })
                        .collect(),
                    review_token: Some(token),
                    waiting: awaited.is_some(),
                })
            }
            ApprovalReview::Stale(_) => not_reviewed(Binding::Stale),
            ApprovalReview::NotFound => not_reviewed(Binding::NotFound),
        }
    }

    /// Approve or deny one pending approval.
    ///
    /// Deny needs no token. Approve first redeems the review token against the
    /// approval's current digest, read from the store in the same hold as the
    /// decision. The token is consumed by every attempt, so a refused approve
    /// needs a fresh `approval_detail`. A `file_write` approve also needs
    /// `writers_stopped` and calls core's acknowledgement path; other
    /// operations ignore the field. Core still refuses the wrong combination.
    ///
    /// # Errors
    ///
    /// `not_found`, `not_pending`, `review_expired`, `review_mismatch`,
    /// `writers_stopped_required`, `busy`, or `control` with core's refusal.
    pub fn approval_decide(
        &self,
        request: ApprovalDecideRequest,
    ) -> Result<ApprovalView, DesktopError> {
        let inner = &self.inner;
        let mut store = inner
            .open_control()?
            .ok_or_else(|| DesktopError::not_found("approval"))?;
        let now = now_unix();
        let status = store
            .approval_status(request.id, now)
            .ok_or_else(|| DesktopError::not_found("approval"))?;
        if status.state != ApprovalState::Pending {
            return Err(DesktopError::not_pending());
        }
        let decided = match request.decision {
            Decision::Deny => store.decide_approval(request.id, false, now),
            Decision::Approve => {
                // Only the redeem takes the reviews lock; the store stays open
                // so the record cannot change between the check and the decision.
                inner.redeem(
                    request.id,
                    request.review_token.as_deref(),
                    &status.request_sha256,
                )?;
                if status.operation == ApprovalOperation::FileWrite {
                    if !request.writers_stopped {
                        return Err(DesktopError::writers_stopped_required());
                    }
                    store.approve_with_writers_stopped(request.id, now)
                } else {
                    store.decide_approval(request.id, true, now)
                }
            }
        };
        decided.map_err(|error| DesktopError::from_control(&error))?;
        let updated = store
            .approval_status(request.id, now)
            .ok_or_else(|| DesktopError::not_found("approval"))?;
        let statuses = store.approval_statuses(now);
        drop(store);
        inner.note_pending(&statuses);
        inner.view_of(&updated)
    }
}

impl Inner {
    /// Cache and return how many of `statuses` are pending.
    fn note_pending(&self, statuses: &[ApprovalStatus]) -> u64 {
        let pending = statuses
            .iter()
            .filter(|status| status.state == ApprovalState::Pending)
            .count();
        let pending = u64::try_from(pending).unwrap_or(u64::MAX);
        self.pending_cache.store(pending, Ordering::Relaxed);
        pending
    }

    /// The status of one approval; the store is released before returning.
    fn status_of_approval(&self, id: Uuid) -> Result<ApprovalStatus, DesktopError> {
        let store = self
            .open_control()?
            .ok_or_else(|| DesktopError::not_found("approval"))?;
        let status = store.approval_status(id, now_unix());
        drop(store);
        status.ok_or_else(|| DesktopError::not_found("approval"))
    }

    /// Use the review token for an approval.
    fn redeem(
        &self,
        approval: Uuid,
        presented: Option<&str>,
        digest: &str,
    ) -> Result<(), DesktopError> {
        let outcome = self
            .lock_reviews()?
            .redeem(approval, presented, digest, Instant::now());
        outcome.map_err(|error| match error {
            RedeemError::Expired => DesktopError::review_expired(),
            RedeemError::Mismatch => DesktopError::review_mismatch(),
        })
    }

    fn view_of(&self, status: &ApprovalStatus) -> Result<ApprovalView, DesktopError> {
        let mut views = self.views_of(std::slice::from_ref(status))?;
        views
            .pop()
            .ok_or_else(|| DesktopError::internal("The approval could not be shown."))
    }

    /// Join statuses with session labels from the index, which is locked once.
    ///
    /// A label the index cannot supply is `null`: it is cosmetic, and the
    /// record itself comes from the settings store.
    fn views_of(&self, statuses: &[ApprovalStatus]) -> Result<Vec<ApprovalView>, DesktopError> {
        let labels: HashMap<Uuid, (String, String)> = self.with_index(|state| {
            let mut labels = HashMap::new();
            for status in statuses {
                if labels.contains_key(&status.session_id) {
                    continue;
                }
                if let Ok(Some(session)) = state.index.session(status.session_id) {
                    let title = super::state::summary_of(&session, &HashSet::new()).title;
                    labels.insert(
                        status.session_id,
                        (title, super::state::folder_of(&session)),
                    );
                }
            }
            Ok(labels)
        })?;
        Ok(statuses
            .iter()
            .map(|status| {
                let label = labels.get(&status.session_id);
                ApprovalView {
                    id: status.id,
                    session_id: status.session_id,
                    session_title: label.map(|(title, _)| title.clone()),
                    folder_display: label.map(|(_, folder)| folder.clone()),
                    operation: status.operation.into(),
                    summary: display_safe_line(&status.summary),
                    state: status.state.into(),
                    created_unix: status.created_unix,
                    expires_unix: status.expires_unix,
                    request_sha256: status.request_sha256.clone(),
                    writers_stopped: status.writers_stopped,
                }
            })
            .collect())
    }
}
