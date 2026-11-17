//! Delivery of provisional streamed text to a front end.
//!
//! One sink serves a whole turn. Each model request gets a fresh redactor, so
//! the known server token is removed across piece boundaries before any text
//! leaves the application layer. The journal never sees this path.

use super::ChatStreamEvent;
use crate::secrets::StreamRedactor;

/// Forwards redacted text pieces of the current request to the front end.
pub(super) struct StreamSink<'s> {
    on_stream: &'s mut dyn FnMut(ChatStreamEvent<'_>),
    token: Option<&'s str>,
    redactor: StreamRedactor<'s>,
}

impl<'s> StreamSink<'s> {
    /// Build a sink for one turn. `token` is the known server token, if any.
    pub(super) fn new(
        on_stream: &'s mut dyn FnMut(ChatStreamEvent<'_>),
        token: Option<&'s str>,
    ) -> Self {
        Self {
            on_stream,
            token,
            redactor: StreamRedactor::new(token),
        }
    }

    /// Announce a model request and start a fresh redactor for it.
    pub(super) fn start_request(&mut self) {
        self.redactor = StreamRedactor::new(self.token);
        (self.on_stream)(ChatStreamEvent::RequestStarted);
    }

    /// Redact one piece of the current request and forward what is safe.
    pub(super) fn text(&mut self, piece: &str) {
        let safe = self.redactor.push(piece);
        self.emit(&safe);
    }

    /// End a request that succeeded and forward the redactor's withheld tail.
    ///
    /// A failed request calls nothing: its withheld tail is dropped with the
    /// redactor, because the stream is not a reply.
    pub(super) fn finish_request(&mut self) {
        let redactor = std::mem::replace(&mut self.redactor, StreamRedactor::new(self.token));
        let tail = redactor.finish();
        self.emit(&tail);
    }

    /// Never emit an empty `Text`.
    fn emit(&mut self, text: &str) {
        if !text.is_empty() {
            (self.on_stream)(ChatStreamEvent::Text(text));
        }
    }
}
