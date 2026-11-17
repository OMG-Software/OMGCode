//! Provisional streamed chat text on standard error (`chat --stream`).
//!
//! Streamed text is untrusted model output, so every piece goes through core's
//! streaming display-safe escaping before it reaches a terminal. Standard
//! output is never used here: it carries the one JSON result of the command.
//!
//! The writer is `io::Stderr`, which locks only for the span of one write. No
//! lock is held while a model request is in flight.

use std::io::Write;

use omgcode_app::ChatStreamEvent;
use omgcode_core::turn::DisplaySafeStream;

/// Writes escaped streamed text, one line per model request that produced text.
pub(crate) struct StreamPrinter<W: Write> {
    out: W,
    safe: DisplaySafeStream,
    /// Whether the current request has delivered any text.
    wrote_text: bool,
    /// Set by the first failed write; later text is dropped.
    failed: bool,
}

impl<W: Write> StreamPrinter<W> {
    /// Print to `out`, normally standard error.
    pub(crate) fn new(out: W) -> Self {
        Self {
            out,
            safe: DisplaySafeStream::new(),
            wrote_text: false,
            failed: false,
        }
    }

    /// Handle one event of the turn.
    pub(crate) fn handle(&mut self, event: ChatStreamEvent<'_>) {
        match event {
            ChatStreamEvent::RequestStarted => self.end_request(),
            ChatStreamEvent::Text(text) => {
                self.wrote_text = true;
                let escaped = self.safe.push(text);
                self.write(&escaped);
            }
        }
    }

    /// End the turn: complete the last request's line, if it printed text.
    pub(crate) fn finish(&mut self) {
        self.end_request();
    }

    /// Flush the held tail of a request that wrote text and end its line.
    fn end_request(&mut self) {
        let safe = std::mem::take(&mut self.safe);
        if std::mem::take(&mut self.wrote_text) {
            let tail = safe.finish();
            self.write(&tail);
            self.write("\n");
        }
    }

    fn write(&mut self, text: &str) {
        if self.failed || text.is_empty() {
            return;
        }
        if self.out.write_all(text.as_bytes()).is_err() {
            // Provisional text is a convenience on a side channel. A closed or
            // full standard error must not fail a turn whose result is still
            // recorded and printed, so stop writing and discard the error.
            self.failed = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    fn run(events: &[ChatStreamEvent<'_>]) -> String {
        let mut bytes = Vec::new();
        let mut printer = StreamPrinter::new(&mut bytes);
        for event in events {
            printer.handle(*event);
        }
        printer.finish();
        String::from_utf8(bytes).expect("escaped text is UTF-8")
    }

    #[test]
    fn each_request_with_text_ends_in_one_line_feed() {
        let output = run(&[
            ChatStreamEvent::RequestStarted,
            ChatStreamEvent::Text("Hel"),
            ChatStreamEvent::Text("lo"),
            ChatStreamEvent::RequestStarted,
            ChatStreamEvent::Text("again"),
        ]);
        assert_eq!(output, "Hello\nagain\n");
    }

    #[test]
    fn a_request_without_text_prints_nothing() {
        let output = run(&[
            ChatStreamEvent::RequestStarted,
            ChatStreamEvent::RequestStarted,
        ]);
        assert_eq!(output, "");
    }

    #[test]
    fn control_and_bidi_characters_are_escaped_across_pieces() {
        let output = run(&[
            ChatStreamEvent::RequestStarted,
            ChatStreamEvent::Text("a\u{1b}[31m \u{202e}b \\"),
            ChatStreamEvent::Text("u{41} end\\"),
        ]);
        assert_eq!(output, "a\\u{001b}[31m \\u{202e}b \\u{5c}u{41} end\\\n");
    }

    /// A writer that accepts `limit` writes and then fails every write.
    struct FailAfter {
        limit: usize,
        accepted: Vec<u8>,
        attempts: usize,
    }

    impl Write for FailAfter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.attempts += 1;
            if self.attempts > self.limit {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe));
            }
            self.accepted.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_failed_write_stops_all_later_writes_without_panicking() {
        let mut writer = FailAfter {
            limit: 1,
            accepted: Vec::new(),
            attempts: 0,
        };
        {
            let mut printer = StreamPrinter::new(&mut writer);
            printer.handle(ChatStreamEvent::RequestStarted);
            printer.handle(ChatStreamEvent::Text("kept"));
            printer.handle(ChatStreamEvent::Text("lost"));
            printer.handle(ChatStreamEvent::RequestStarted);
            printer.handle(ChatStreamEvent::Text("also lost"));
            printer.finish();
        }
        assert_eq!(writer.accepted, b"kept");
        assert_eq!(writer.attempts, 2, "no write is attempted after a failure");
    }
}
