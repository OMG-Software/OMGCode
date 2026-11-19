//! The call a turn is waiting on while the operator decides its approval
//! (ADR-0063).
//!
//! A turn that waits in-turn holds its session journal, so a reviewer cannot
//! read the recorded request from it. The waiting turn publishes the exact
//! call here instead. The call is only a review candidate: the review still
//! recomputes the approval digest from it and the live workspace, so a call
//! that does not match the record is never shown as exact.

use std::sync::{Arc, Mutex, PoisonError};

use uuid::Uuid;

/// One model call that waits for the operator to decide an approval.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AwaitedCall {
    /// The approval record the turn waits on.
    pub approval_id: Uuid,
    /// The requested tool name.
    pub tool: String,
    /// The exact arguments JSON string the journal recorded for the call.
    pub arguments: String,
}

/// A shared slot holding the call a turn currently waits on, if any.
///
/// Enabling an in-turn wait requires one. The turn sets the slot before each
/// wait and clears it afterwards; a front end reads it to review the call.
///
/// Ownership: the turn runtime holds one clone for the turn's lifetime and
/// the front end holds another, such as the desktop turn table's slot for the
/// session. The value is freed when the last clone drops. It holds only plain
/// data, so no reference cycle can form. The lock is held only to copy or
/// replace the value. A poisoned lock is recovered, because the value is a
/// whole `Option` with no invariant a panic could leave half-updated.
#[derive(Clone, Debug, Default)]
pub struct ApprovalWaitSlot(Arc<Mutex<Option<AwaitedCall>>>);

impl ApprovalWaitSlot {
    /// An empty slot.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The call the turn waits on now, or `None` when it is not waiting.
    #[must_use]
    pub fn current(&self) -> Option<AwaitedCall> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Publish `call` until the returned guard is dropped.
    pub(super) fn publish(&self, call: AwaitedCall) -> PublishedCall<'_> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(call);
        PublishedCall { slot: self }
    }
}

/// Clears the slot when the wait ends, including on unwind.
pub(super) struct PublishedCall<'a> {
    slot: &'a ApprovalWaitSlot,
}

impl Drop for PublishedCall<'_> {
    fn drop(&mut self) {
        *self.slot.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_published_call_is_visible_until_its_guard_drops() {
        let slot = ApprovalWaitSlot::new();
        let reader = slot.clone();
        assert_eq!(reader.current(), None);
        let call = AwaitedCall {
            approval_id: Uuid::new_v4(),
            tool: "workspace_create_file".to_owned(),
            arguments: "{}".to_owned(),
        };
        let guard = slot.publish(call.clone());
        assert_eq!(reader.current(), Some(call));
        drop(guard);
        assert_eq!(reader.current(), None);
    }
}
