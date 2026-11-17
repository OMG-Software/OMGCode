//! The chat event hub: one subscriber and the live preview of each running turn
//! (ADR-0049).
//!
//! A turn thread feeds the hub with the provisional text of a streamed model
//! request. The hub keeps, for each running turn, the preview of the current
//! request (at most [`MAX_PREVIEW_BYTES`]) and pushes events to the one
//! subscriber. The subscriber is a [`ChatEventSink`], so this crate stays free
//! of any user-interface framework.
//!
//! # Authority
//!
//! Preview text is provisional and untrusted. It grants nothing and is never
//! written to the journal. The journal and `chat_status` stay the record. Neither
//! preview text nor an event is ever logged.
//!
//! # Locking
//!
//! The hub lives behind `Inner::events`. Events are sent **while that lock is
//! held**, so a replay and a live event cannot reach the subscriber out of
//! order. That is why a sink must not block and must not call back into the
//! service: it would stall every turn thread, or deadlock on this lock.
//!
//! Lock order: the `turns` lock and the `events` lock are never held together.
//! Where both are needed, `turns` is taken and released first. The `events` lock
//! is never held while the `index` or `reviews` lock is taken.
//!
//! A poisoned `events` lock never panics. A turn thread drops the event and
//! carries on (streaming stops; the turn itself is unaffected). `chat_subscribe`
//! returns `internal`.

use std::collections::HashMap;
use std::sync::MutexGuard;

use uuid::Uuid;

use super::Inner;
use super::error::DesktopError;
use super::wire::ChatEvent;
use crate::ChatStreamEvent;

/// Most preview text kept, and sent, for one model request.
pub(super) const MAX_PREVIEW_BYTES: usize = 16 * 1024;
/// Most text bytes in one `delta` event.
pub(super) const MAX_DELTA_BYTES: usize = 1024;

/// Delivers one event to the subscriber. Returns `false` when the subscriber
/// is gone; the hub then drops it. It must not block and must not call back
/// into the service.
pub type ChatEventSink = Box<dyn Fn(&ChatEvent) -> bool + Send + Sync>;

/// The live preview of one running turn's current request.
struct Preview {
    /// The request number, from 1. `0` means no request has started yet.
    request: u32,
    /// The text accepted for the current request, at most [`MAX_PREVIEW_BYTES`].
    text: String,
    /// Some text of the current request did not fit.
    truncated: bool,
}

/// The subscriber and the running turns' previews.
#[derive(Default)]
pub(super) struct EventHub {
    /// At most one subscriber; a new one replaces it.
    sink: Option<ChatEventSink>,
    /// One entry per running turn, removed by [`finish`](Self::finish).
    live: HashMap<Uuid, Preview>,
}

/// The longest prefix of `text` that is at most `max` bytes and ends on a
/// character boundary.
fn prefix_len(text: &str, max: usize) -> usize {
    let mut end = max.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Send one event. A sink that returns `false` is dropped at once.
///
/// Returns whether a subscriber is still attached afterwards.
fn send(sink: &mut Option<ChatEventSink>, event: &ChatEvent) -> bool {
    let kept = sink.as_ref().is_some_and(|deliver| deliver(event));
    if !kept {
        *sink = None;
    }
    kept
}

/// Send `text` as `delta` events of at most [`MAX_DELTA_BYTES`] bytes, split
/// only at character boundaries. Stops when the subscriber is gone.
fn send_deltas(sink: &mut Option<ChatEventSink>, session_id: Uuid, request: u32, text: &str) {
    let mut rest = text;
    while !rest.is_empty() {
        let end = prefix_len(rest, MAX_DELTA_BYTES);
        // `end` is zero only for a bound smaller than one character, which the
        // constant is not; stop rather than loop if that ever changed.
        let (Some(piece), Some(tail)) = (rest.get(..end), rest.get(end..)) else {
            return;
        };
        if piece.is_empty() {
            return;
        }
        let event = ChatEvent::Delta {
            session_id,
            request,
            text: piece.to_owned(),
        };
        if !send(sink, &event) {
            return;
        }
        rest = tail;
    }
}

impl EventHub {
    /// Register a running turn with no request yet.
    pub(super) fn begin(&mut self, session_id: Uuid) {
        self.live.insert(
            session_id,
            Preview {
                request: 0,
                text: String::new(),
                truncated: false,
            },
        );
    }

    /// A model request of the turn started: supersede the preview.
    ///
    /// Does nothing for a session that is not live.
    pub(super) fn request_started(&mut self, session_id: Uuid) {
        let Some(preview) = self.live.get_mut(&session_id) else {
            return;
        };
        preview.request = preview.request.saturating_add(1);
        preview.text.clear();
        preview.truncated = false;
        let event = ChatEvent::RequestStarted {
            session_id,
            request: preview.request,
        };
        send(&mut self.sink, &event);
    }

    /// Provisional text of the current request.
    ///
    /// Text that arrives before any request started behaves as if
    /// `request_started` had come first. Only what fits under
    /// [`MAX_PREVIEW_BYTES`] is kept and sent; the first time something does not
    /// fit, one `preview_truncated` event is sent and the rest of the request's
    /// text is dropped without notice.
    pub(super) fn text(&mut self, session_id: Uuid, text: &str) {
        if self
            .live
            .get(&session_id)
            .is_some_and(|preview| preview.request == 0)
        {
            self.request_started(session_id);
        }
        let Some(preview) = self.live.get_mut(&session_id) else {
            return;
        };
        if preview.truncated || text.is_empty() {
            return;
        }
        let room = MAX_PREVIEW_BYTES.saturating_sub(preview.text.len());
        let fit = prefix_len(text, room);
        let request = preview.request;
        if let Some(part) = text.get(..fit)
            && !part.is_empty()
        {
            preview.text.push_str(part);
            send_deltas(&mut self.sink, session_id, request, part);
        }
        if fit < text.len() {
            preview.truncated = true;
            send(
                &mut self.sink,
                &ChatEvent::PreviewTruncated {
                    session_id,
                    request,
                },
            );
        }
    }

    /// The turn left `running`: forget its preview and send `finished`.
    ///
    /// Sent even when no request ever started.
    pub(super) fn finish(&mut self, session_id: Uuid) {
        self.live.remove(&session_id);
        send(&mut self.sink, &ChatEvent::Finished { session_id });
    }

    /// Replace the subscriber, then replay the preview of every running turn
    /// that has started a request, in ascending session order.
    ///
    /// The replay for a session is `request_started`, its text as `delta`
    /// events, and `preview_truncated` when flagged. A sink that refuses an
    /// event is dropped and the replay stops.
    pub(super) fn subscribe(&mut self, sink: ChatEventSink) {
        self.sink = Some(sink);
        let mut ids: Vec<Uuid> = self.live.keys().copied().collect();
        ids.sort_unstable();
        for session_id in ids {
            let Some(preview) = self.live.get(&session_id) else {
                continue;
            };
            if preview.request == 0 {
                continue;
            }
            let request = preview.request;
            let started = ChatEvent::RequestStarted {
                session_id,
                request,
            };
            if !send(&mut self.sink, &started) {
                return;
            }
            send_deltas(&mut self.sink, session_id, request, &preview.text);
            if preview.truncated
                && !send(
                    &mut self.sink,
                    &ChatEvent::PreviewTruncated {
                        session_id,
                        request,
                    },
                )
            {
                return;
            }
            if self.sink.is_none() {
                return;
            }
        }
    }
}

impl Inner {
    /// Register a running turn with the hub. A poisoned lock drops the call.
    pub(super) fn events_begin(&self, session_id: Uuid) {
        if let Ok(mut hub) = self.events.lock() {
            hub.begin(session_id);
        }
    }

    /// Forward one stream event of a turn to the hub. A poisoned lock drops it.
    pub(super) fn events_stream(&self, session_id: Uuid, event: ChatStreamEvent<'_>) {
        if let Ok(mut hub) = self.events.lock() {
            match event {
                ChatStreamEvent::RequestStarted => hub.request_started(session_id),
                ChatStreamEvent::Text(text) => hub.text(session_id, text),
            }
        }
    }

    /// Tell the hub the turn left `running`. A poisoned lock drops the call.
    ///
    /// The caller records the result in the turn table first and has released
    /// the `turns` lock.
    pub(super) fn events_finish(&self, session_id: Uuid) {
        if let Ok(mut hub) = self.events.lock() {
            hub.finish(session_id);
        }
    }

    /// Lock the hub for a subscription; `internal` when it is poisoned.
    pub(super) fn lock_events(&self) -> Result<MutexGuard<'_, EventHub>, DesktopError> {
        self.events.lock().map_err(|_| DesktopError::poisoned())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    type Log = Arc<Mutex<Vec<ChatEvent>>>;

    fn collector() -> (Log, ChatEventSink) {
        let log: Log = Arc::default();
        let writer = Arc::clone(&log);
        let sink: ChatEventSink = Box::new(move |event| {
            writer.lock().expect("log").push(event.clone());
            true
        });
        (log, sink)
    }

    fn events(log: &Log) -> Vec<ChatEvent> {
        log.lock().expect("log").clone()
    }

    fn id(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }

    fn delta_text(events: &[ChatEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                ChatEvent::Delta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn text_before_any_request_starts_one_first() {
        // Given a live turn and a subscriber.
        let mut hub = EventHub::default();
        let (log, sink) = collector();
        hub.begin(id(1));
        hub.subscribe(sink);

        // When text arrives with no RequestStarted.
        hub.text(id(1), "hi");

        // Then request 1 starts first and the text follows.
        assert_eq!(
            events(&log),
            [
                ChatEvent::RequestStarted {
                    session_id: id(1),
                    request: 1
                },
                ChatEvent::Delta {
                    session_id: id(1),
                    request: 1,
                    text: "hi".to_owned()
                },
            ]
        );
    }

    #[test]
    fn deltas_split_only_at_character_boundaries() {
        // Given text of three-byte characters, 2,100 bytes in all.
        let mut hub = EventHub::default();
        let (log, sink) = collector();
        hub.begin(id(1));
        hub.subscribe(sink);
        let text = "\u{20ac}".repeat(700);

        // When it is sent.
        hub.text(id(1), &text);

        // Then each delta is at most 1,024 bytes and ends on a boundary, the
        // first is 1,023 bytes, and the pieces rebuild the text.
        let seen = events(&log);
        let lengths: Vec<usize> = seen
            .iter()
            .filter_map(|event| match event {
                ChatEvent::Delta { text, .. } => Some(text.len()),
                _ => None,
            })
            .collect();
        assert_eq!(lengths, [1023, 1023, 54]);
        assert_eq!(delta_text(&seen), text);
    }

    #[test]
    fn the_preview_stops_at_the_bound_on_a_character_boundary() {
        // Given a reply of three-byte characters longer than the bound.
        let mut hub = EventHub::default();
        let (log, sink) = collector();
        hub.begin(id(1));
        hub.subscribe(sink);
        let text = "\u{20ac}".repeat(6000);

        // When it is sent, and more text follows for the same request.
        hub.text(id(1), &text);
        hub.text(id(1), "later");

        // Then 16,383 bytes were sent (the largest whole-character fit), one
        // truncation event followed, and nothing came after it.
        let seen = events(&log);
        assert_eq!(delta_text(&seen).len(), 16_383);
        let truncations = seen
            .iter()
            .filter(|event| matches!(event, ChatEvent::PreviewTruncated { .. }))
            .count();
        assert_eq!(truncations, 1);
        assert!(matches!(
            seen.last(),
            Some(ChatEvent::PreviewTruncated { .. })
        ));

        // And a new request starts a fresh preview.
        hub.request_started(id(1));
        hub.text(id(1), "again");
        let seen = events(&log);
        assert_eq!(
            seen.last(),
            Some(&ChatEvent::Delta {
                session_id: id(1),
                request: 2,
                text: "again".to_owned()
            })
        );
    }

    #[test]
    fn text_that_exactly_fills_the_bound_is_not_truncated() {
        // Given text of exactly the preview bound.
        let mut hub = EventHub::default();
        let (log, sink) = collector();
        hub.begin(id(1));
        hub.subscribe(sink);
        hub.text(id(1), &"a".repeat(MAX_PREVIEW_BYTES));
        assert!(
            !events(&log)
                .iter()
                .any(|event| matches!(event, ChatEvent::PreviewTruncated { .. }))
        );

        // When one more byte arrives.
        hub.text(id(1), "b");

        // Then the truncation is reported once and the byte is not sent.
        let seen = events(&log);
        assert_eq!(delta_text(&seen).len(), MAX_PREVIEW_BYTES);
        assert!(matches!(
            seen.last(),
            Some(ChatEvent::PreviewTruncated { request: 1, .. })
        ));
    }

    #[test]
    fn a_subscription_replays_running_turns_in_ascending_session_order() {
        // Given three turns, started in a different order than their ids.
        let mut hub = EventHub::default();
        hub.begin(id(9));
        hub.begin(id(2));
        hub.begin(id(5));
        hub.text(id(9), "nine");
        hub.text(id(2), &"x".repeat(MAX_PREVIEW_BYTES + 1));
        // The turn with no request yet has nothing to replay.

        // When a subscriber arrives.
        let (log, sink) = collector();
        hub.subscribe(sink);

        // Then session 2 comes first with its text and truncation, then 9.
        let seen = events(&log);
        assert_eq!(
            seen.first(),
            Some(&ChatEvent::RequestStarted {
                session_id: id(2),
                request: 1
            })
        );
        let truncated_at = seen
            .iter()
            .position(|event| matches!(event, ChatEvent::PreviewTruncated { .. }))
            .expect("truncation replayed");
        assert_eq!(
            seen.get(truncated_at.saturating_add(1)),
            Some(&ChatEvent::RequestStarted {
                session_id: id(9),
                request: 1
            })
        );
        assert_eq!(
            seen.last(),
            Some(&ChatEvent::Delta {
                session_id: id(9),
                request: 1,
                text: "nine".to_owned()
            })
        );
        assert!(
            !seen.iter().any(|event| matches!(
                event,
                ChatEvent::RequestStarted { session_id, .. } if *session_id == id(5)
            )),
            "a turn with no request is not replayed"
        );
    }

    #[test]
    fn a_replay_holds_only_the_current_request() {
        // Given a turn on its second request.
        let mut hub = EventHub::default();
        hub.begin(id(1));
        hub.text(id(1), "first request");
        hub.request_started(id(1));
        hub.text(id(1), "second");

        // When a subscriber arrives.
        let (log, sink) = collector();
        hub.subscribe(sink);

        // Then only request 2 is replayed.
        assert_eq!(
            events(&log),
            [
                ChatEvent::RequestStarted {
                    session_id: id(1),
                    request: 2
                },
                ChatEvent::Delta {
                    session_id: id(1),
                    request: 2,
                    text: "second".to_owned()
                },
            ]
        );
    }

    #[test]
    fn finish_without_any_request_is_sent_and_clears_the_turn() {
        // Given a turn that never streamed.
        let mut hub = EventHub::default();
        let (log, sink) = collector();
        hub.begin(id(1));
        hub.subscribe(sink);

        // When it finishes.
        hub.finish(id(1));

        // Then only `finished` was sent, and a later subscriber replays nothing.
        assert_eq!(events(&log), [ChatEvent::Finished { session_id: id(1) }]);
        let (later, sink) = collector();
        hub.subscribe(sink);
        assert!(events(&later).is_empty());
    }

    #[test]
    fn a_sink_that_returns_false_is_dropped_and_later_events_go_nowhere() {
        // Given a sink that accepts one event and then reports it is gone.
        let mut hub = EventHub::default();
        let calls = Arc::new(Mutex::new(0_u32));
        let counter = Arc::clone(&calls);
        hub.begin(id(1));
        hub.subscribe(Box::new(move |_| {
            let mut count = counter.lock().expect("count");
            *count += 1;
            *count < 2
        }));

        // When more events follow.
        hub.text(id(1), "one");
        hub.text(id(1), "two");
        hub.finish(id(1));

        // Then the sink saw exactly the two calls up to its refusal.
        assert_eq!(*calls.lock().expect("count"), 2);
        assert!(hub.sink.is_none());
    }

    #[test]
    fn a_second_subscriber_replaces_the_first() {
        // Given a subscriber.
        let mut hub = EventHub::default();
        let (first, sink) = collector();
        hub.begin(id(1));
        hub.subscribe(sink);
        hub.text(id(1), "a");
        let before = events(&first).len();

        // When another subscribes and more text arrives.
        let (second, sink) = collector();
        hub.subscribe(sink);
        hub.text(id(1), "b");

        // Then the first sees nothing more, and the second sees the replay and
        // the new text.
        assert_eq!(events(&first).len(), before);
        assert_eq!(delta_text(&events(&second)), "ab");
    }
}
