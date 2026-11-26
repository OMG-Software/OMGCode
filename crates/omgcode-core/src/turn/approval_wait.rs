//! The call a turn is waiting on while the operator decides its approval
//! (ADR-0063).
//!
//! A turn that waits in-turn holds its session journal, so a reviewer cannot
//! read the recorded request from it. The waiting turn publishes the exact
//! call here instead. The call is only a review candidate: the review still
//! recomputes the approval digest from it and the live workspace, so a call
//! that does not match the record is never shown as exact.
//!
//! A front end may also attach a [`WaitObserver`], which the turn calls each
//! time it starts waiting, so the front end can show its review surface at
//! once instead of when it next polls (ADR-0105). The observer only learns
//! that a wait began; it decides nothing and receives nothing it could
//! approve with.

use std::fmt;
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
///
/// An attached [`WaitObserver`] is shared by every clone made after it was
/// attached. It must not own a clone of the slot it is attached to, or the
/// slot would keep itself alive. A holder that only reads the call, and may
/// outlive the turn, keeps [`ApprovalWaitSlot::reader`] instead, so it does
/// not keep the observer alive.
#[derive(Clone, Default)]
pub struct ApprovalWaitSlot {
    call: Arc<Mutex<Option<AwaitedCall>>>,
    observer: Option<WaitObserver>,
}

/// Called on the turn's thread each time the turn starts waiting for a
/// decision, after the call is published and before the wait begins.
///
/// It runs with no lock of the slot held and must return promptly: the wait,
/// and with it the operator's decision, starts only when it returns. It must
/// not panic, because a panic would end the turn. It receives nothing: the
/// front end reads [`ApprovalWaitSlot::current`] or its own approval list if
/// it needs the call.
pub type WaitObserver = Arc<dyn Fn() + Send + Sync>;

impl fmt::Debug for ApprovalWaitSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApprovalWaitSlot")
            .field("call", &self.current())
            .field("observer", &self.observer.is_some())
            .finish()
    }
}

impl ApprovalWaitSlot {
    /// An empty slot.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// This slot with `observer` called whenever the turn starts waiting.
    #[must_use]
    pub fn with_observer(mut self, observer: WaitObserver) -> Self {
        self.observer = Some(observer);
        self
    }

    /// A handle that shares this slot's call but holds no observer. A wait
    /// published through it tells no observer.
    #[must_use]
    pub fn reader(&self) -> Self {
        Self {
            call: Arc::clone(&self.call),
            observer: None,
        }
    }

    /// The call the turn waits on now, or `None` when it is not waiting.
    #[must_use]
    pub fn current(&self) -> Option<AwaitedCall> {
        self.call
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Publish `call` until the returned guard is dropped, then tell the
    /// observer, if any, that a wait began.
    pub(super) fn publish(&self, call: AwaitedCall) -> PublishedCall<'_> {
        *self.call.lock().unwrap_or_else(PoisonError::into_inner) = Some(call);
        // The lock is released above, so the observer may read the slot.
        if let Some(observer) = &self.observer {
            observer();
        }
        PublishedCall { slot: self }
    }
}

/// Clears the slot when the wait ends, including on unwind.
pub(super) struct PublishedCall<'a> {
    slot: &'a ApprovalWaitSlot,
}

impl Drop for PublishedCall<'_> {
    fn drop(&mut self) {
        *self
            .slot
            .call
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
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

    fn call() -> AwaitedCall {
        AwaitedCall {
            approval_id: Uuid::new_v4(),
            tool: "workspace_run_command".to_owned(),
            arguments: "{}".to_owned(),
        }
    }

    #[test]
    fn the_observer_runs_once_per_wait_after_the_call_is_published() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // The observer reads through a clone made before it was attached, so
        // it owns no clone of the slot that holds it.
        let reader = ApprovalWaitSlot::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let slot = reader.clone().with_observer({
            let reader = reader.clone();
            let seen = Arc::clone(&seen);
            let calls = Arc::clone(&calls);
            Arc::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                // The slot's lock is free and the call is already visible.
                seen.lock().expect("seen").push(reader.current());
            })
        });
        let shared = slot.clone();

        let first = call();
        let guard = shared.publish(first.clone());
        drop(guard);
        let second = call();
        drop(slot.publish(second.clone()));

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(*seen.lock().expect("seen"), vec![Some(first), Some(second)]);
        assert_eq!(reader.current(), None);
    }

    #[test]
    fn a_reader_shares_the_call_but_not_the_observer() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let observer: WaitObserver = {
            let calls = Arc::clone(&calls);
            Arc::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
            })
        };
        let weak = Arc::downgrade(&observer);
        let slot = ApprovalWaitSlot::new().with_observer(observer);
        let reader = slot.reader();
        assert!(format!("{reader:?}").contains("observer: false"));

        let published = call();
        let guard = slot.publish(published.clone());
        assert_eq!(reader.current(), Some(published));
        drop(guard);
        assert_eq!(reader.current(), None);
        drop(reader.publish(call()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Only the reader is left, and it does not keep the observer alive.
        drop(slot);
        assert!(weak.upgrade().is_none());
        let late = call();
        let shared = reader.clone();
        let guard = reader.publish(late.clone());
        assert_eq!(shared.current(), Some(late));
        drop(guard);
    }

    #[test]
    fn a_slot_without_an_observer_still_publishes_and_debug_shows_no_closure() {
        let slot = ApprovalWaitSlot::new();
        let guard = slot.publish(call());
        assert!(slot.current().is_some());
        drop(guard);
        let text = format!("{slot:?}");
        assert!(text.contains("observer: false"), "{text}");
    }
}
