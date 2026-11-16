//! Strict reader for one streamed (server-sent events) Chat Completions reply.
//!
//! The reader is a pure function over [`Read`], so the stream rules can be
//! tested without a socket. It refuses anything it does not understand and
//! repairs nothing. Text reaches the caller only from a payload that passed
//! every check, so a later failure can discard text but never deliver text
//! from a payload that was itself refused (ADR-0049).

use super::{
    MAX_RESPONSE_BYTES, MAX_RESPONSE_TOOL_CALLS, ProviderError, Usage, parse_usage, reject_actions,
};
use crate::storage::parse_strict_json;
use serde_json::{Map, Value};
use std::io::Read;

/// Whether the request advertised tools, which decides how a reply is judged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StreamMode {
    /// A text-only request: any tool call or tool finish reason is refused.
    Text,
    /// A tool-capable request: tool calls are assembled from their pieces.
    Tools,
}

/// A fully validated streamed reply.
#[derive(Debug, PartialEq)]
pub(super) struct StreamOutcome {
    /// The concatenated assistant text. It is untrusted model output.
    pub(super) content: String,
    /// The one finish reason the stream reported.
    pub(super) finish_reason: String,
    /// The last usage object the server sent, when any.
    pub(super) usage: Option<Usage>,
    /// Assembled raw tool calls holding only the members that were received.
    pub(super) tool_calls: Vec<Value>,
}

/// Read one event stream to its `[DONE]` marker.
///
/// `on_text` receives each non-empty piece of assistant text, in order, after
/// the payload that carried it passed validation. The whole stream is bounded
/// by the response byte limit. A read error or timeout is `Transport`; a stream
/// that ends before `[DONE]` is `IncompleteStream`.
pub(super) fn read_event_stream(
    mut reader: impl Read,
    mode: StreamMode,
    on_text: &mut dyn FnMut(&str),
) -> Result<StreamOutcome, ProviderError> {
    let mut state = StreamState::new(mode);
    let mut line: Vec<u8> = Vec::new();
    let mut total: u64 = 0;
    let mut chunk = [0_u8; 8192];
    loop {
        let count = reader
            .read(&mut chunk)
            .map_err(|_| ProviderError::Transport)?;
        if count == 0 {
            // A partial last line is discarded: it was never a complete event.
            return Err(ProviderError::IncompleteStream);
        }
        for &byte in chunk.iter().take(count) {
            // Counting per byte stops exactly at the limit and reads no byte
            // beyond a `[DONE]` that arrived in the same chunk.
            total = total
                .checked_add(1)
                .filter(|total| *total <= MAX_RESPONSE_BYTES)
                .ok_or(ProviderError::ResponseTooLarge)?;
            if byte != b'\n' {
                line.push(byte);
                continue;
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let flow = state.handle_line(&line, on_text)?;
            line.clear();
            if flow == Flow::Done {
                return state.finish();
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flow {
    Continue,
    Done,
}

/// One tool call being assembled. Fields stay unset until a piece carries them.
#[derive(Default)]
struct PartialCall {
    id: Option<String>,
    kind_seen: bool,
    function_seen: bool,
    name: Option<String>,
    arguments: Option<String>,
}

impl PartialCall {
    /// Merge one call piece. Only the keys the adapter understands are allowed.
    ///
    /// A null member is treated as absent: it carries no data, and servers
    /// send null for the members a later piece does not repeat.
    fn apply(&mut self, entry: &Map<String, Value>) -> Result<(), ProviderError> {
        if let Some(id) = present(entry.get("id")) {
            let id = id.as_str().ok_or(ProviderError::MalformedResponse)?;
            match &self.id {
                Some(existing) if existing != id => {
                    return Err(ProviderError::MalformedResponse);
                }
                Some(_) => {}
                None => self.id = Some(id.to_owned()),
            }
        }
        if let Some(kind) = present(entry.get("type")) {
            if kind.as_str() != Some("function") {
                return Err(ProviderError::MalformedResponse);
            }
            self.kind_seen = true;
        }
        if let Some(function) = present(entry.get("function")) {
            let function = function
                .as_object()
                .ok_or(ProviderError::MalformedResponse)?;
            if function
                .keys()
                .any(|key| !matches!(key.as_str(), "name" | "arguments"))
            {
                return Err(ProviderError::MalformedResponse);
            }
            self.function_seen = true;
            append_piece(&mut self.name, function.get("name"))?;
            append_piece(&mut self.arguments, function.get("arguments"))?;
        }
        Ok(())
    }

    /// Build the raw call value from the members that were actually received.
    fn into_value(self) -> Value {
        let mut call = Map::new();
        if let Some(id) = self.id {
            call.insert("id".to_owned(), Value::String(id));
        }
        if self.kind_seen {
            call.insert("type".to_owned(), Value::String("function".to_owned()));
        }
        if self.function_seen {
            let mut function = Map::new();
            if let Some(name) = self.name {
                function.insert("name".to_owned(), Value::String(name));
            }
            if let Some(arguments) = self.arguments {
                function.insert("arguments".to_owned(), Value::String(arguments));
            }
            call.insert("function".to_owned(), Value::Object(function));
        }
        Value::Object(call)
    }
}

/// A member that is present and not null.
fn present(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| !value.is_null())
}

/// Append one string piece; absent or null pieces change nothing, other types fail.
fn append_piece(slot: &mut Option<String>, piece: Option<&Value>) -> Result<(), ProviderError> {
    match piece {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(text)) => {
            slot.get_or_insert_with(String::new).push_str(text);
            Ok(())
        }
        Some(_) => Err(ProviderError::MalformedResponse),
    }
}

struct StreamState {
    mode: StreamMode,
    content: String,
    finish_reason: Option<String>,
    usage: Option<Usage>,
    calls: Vec<PartialCall>,
}

impl StreamState {
    const fn new(mode: StreamMode) -> Self {
        Self {
            mode,
            content: String::new(),
            finish_reason: None,
            usage: None,
            calls: Vec::new(),
        }
    }

    /// Judge one complete line (without its line ending).
    fn handle_line(
        &mut self,
        raw: &[u8],
        on_text: &mut dyn FnMut(&str),
    ) -> Result<Flow, ProviderError> {
        let line = std::str::from_utf8(raw).map_err(|_| ProviderError::MalformedResponse)?;
        if line.is_empty() || line.starts_with(':') {
            return Ok(Flow::Continue);
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => {}
            "event" | "id" | "retry" => return Ok(Flow::Continue),
            _ => return Err(ProviderError::MalformedResponse),
        }
        if value == "[DONE]" {
            return if self.finish_reason.is_some() {
                Ok(Flow::Done)
            } else {
                Err(ProviderError::MalformedResponse)
            };
        }
        self.handle_payload(value.as_bytes(), on_text)?;
        Ok(Flow::Continue)
    }

    /// Validate one JSON payload completely, then deliver its text.
    fn handle_payload(
        &mut self,
        bytes: &[u8],
        on_text: &mut dyn FnMut(&str),
    ) -> Result<(), ProviderError> {
        let value = parse_strict_json(bytes).map_err(|_| ProviderError::MalformedResponse)?;
        let object = value.as_object().ok_or(ProviderError::MalformedResponse)?;
        // The error body is never inspected or reported.
        if object.get("error").is_some_and(|error| !error.is_null()) {
            return Err(ProviderError::MalformedResponse);
        }
        let usage = match object.get("usage") {
            None | Some(Value::Null) => None,
            Some(usage) => Some(parse_usage(usage)?),
        };
        let choices = object
            .get("choices")
            .and_then(Value::as_array)
            .ok_or(ProviderError::MalformedResponse)?;
        let piece = match choices.as_slice() {
            [] => None,
            [choice] => self.handle_choice(choice)?,
            _ => return Err(ProviderError::MalformedResponse),
        };
        if usage.is_some() {
            self.usage = usage;
        }
        if let Some(text) = piece.filter(|text| !text.is_empty()) {
            self.content.push_str(text);
            on_text(text);
        }
        Ok(())
    }

    /// Validate the one choice of a payload and return its text piece.
    fn handle_choice<'v>(&mut self, choice: &'v Value) -> Result<Option<&'v str>, ProviderError> {
        if self.finish_reason.is_some() {
            return Err(ProviderError::MalformedResponse);
        }
        let choice = choice.as_object().ok_or(ProviderError::MalformedResponse)?;
        if let Some(index) = choice.get("index")
            && index.as_u64() != Some(0)
        {
            return Err(ProviderError::MalformedResponse);
        }
        reject_actions(choice.get("tool_calls"), choice.get("function_call"))?;
        let delta = choice
            .get("delta")
            .and_then(Value::as_object)
            .ok_or(ProviderError::MalformedResponse)?;
        if let Some(role) = present(delta.get("role"))
            && role.as_str() != Some("assistant")
        {
            return Err(ProviderError::MalformedResponse);
        }
        reject_actions(None, delta.get("function_call"))?;
        match (self.mode, delta.get("tool_calls")) {
            (_, None | Some(Value::Null)) => {}
            (StreamMode::Text, Some(Value::Array(calls))) if calls.is_empty() => {}
            (StreamMode::Text, Some(Value::Array(_))) => {
                return Err(ProviderError::UnsupportedToolCall);
            }
            (StreamMode::Tools, Some(Value::Array(calls))) => self.assemble(calls)?,
            (_, Some(_)) => return Err(ProviderError::MalformedResponse),
        }
        let piece = match delta.get("content") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(text.as_str()),
            Some(_) => return Err(ProviderError::MalformedResponse),
        };
        match choice.get("finish_reason") {
            None | Some(Value::Null) => {}
            Some(Value::String(reason)) => self.accept_finish(reason)?,
            Some(_) => return Err(ProviderError::MalformedResponse),
        }
        Ok(piece)
    }

    /// Merge the call pieces of one delta, in order, by `index`.
    fn assemble(&mut self, entries: &[Value]) -> Result<(), ProviderError> {
        for entry in entries {
            let entry = entry.as_object().ok_or(ProviderError::MalformedResponse)?;
            if entry
                .keys()
                .any(|key| !matches!(key.as_str(), "index" | "id" | "type" | "function"))
            {
                return Err(ProviderError::MalformedResponse);
            }
            let index = entry
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|index| usize::try_from(index).ok())
                .ok_or(ProviderError::MalformedResponse)?;
            if index == self.calls.len() && index < MAX_RESPONSE_TOOL_CALLS {
                self.calls.push(PartialCall::default());
            }
            self.calls
                .get_mut(index)
                .ok_or(ProviderError::MalformedResponse)?
                .apply(entry)?;
        }
        Ok(())
    }

    /// Accept the one finish reason, judged like the non-streaming parsers.
    fn accept_finish(&mut self, reason: &str) -> Result<(), ProviderError> {
        match self.mode {
            StreamMode::Text => {
                if matches!(reason, "tool_calls" | "function_call") {
                    return Err(ProviderError::UnsupportedToolCall);
                }
                if !matches!(reason, "stop" | "length") {
                    return Err(ProviderError::MalformedResponse);
                }
            }
            StreamMode::Tools => {
                let accepted = if self.calls.is_empty() {
                    matches!(reason, "stop" | "length")
                } else {
                    matches!(reason, "tool_calls" | "stop")
                };
                if !accepted {
                    return Err(ProviderError::MalformedResponse);
                }
            }
        }
        self.finish_reason = Some(reason.to_owned());
        Ok(())
    }

    fn finish(self) -> Result<StreamOutcome, ProviderError> {
        let finish_reason = self.finish_reason.ok_or(ProviderError::MalformedResponse)?;
        Ok(StreamOutcome {
            content: self.content,
            finish_reason,
            usage: self.usage,
            tool_calls: self
                .calls
                .into_iter()
                .map(PartialCall::into_value)
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Run the reader over a fixed input and collect the delivered pieces.
    fn run(mode: StreamMode, input: &[u8]) -> (Result<StreamOutcome, ProviderError>, Vec<String>) {
        let mut pieces = Vec::new();
        let result = read_event_stream(Cursor::new(input.to_vec()), mode, &mut |text: &str| {
            pieces.push(text.to_owned());
        });
        (result, pieces)
    }

    fn delta(content: &str) -> String {
        format!(
            "data: {}\n\n",
            serde_json::json!({"choices":[{"index":0,"delta":{"role":"assistant","content":content},"finish_reason":null}]})
        )
    }

    fn finish(reason: &str) -> String {
        format!(
            "data: {}\n\n",
            serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":reason}]})
        )
    }

    const DONE: &str = "data: [DONE]\n\n";

    fn text_stream(parts: &[&str]) -> String {
        let mut stream = String::new();
        for part in parts {
            stream.push_str(&delta(part));
        }
        stream.push_str(&finish("stop"));
        stream.push_str(DONE);
        stream
    }

    #[test]
    fn text_pieces_arrive_in_order_and_build_the_reply() {
        // Given a stream of three text pieces and a finish reason.
        let input = text_stream(&["Hel", "lo ", "there"]);

        // When the reader runs.
        let (result, pieces) = run(StreamMode::Text, input.as_bytes());

        // Then the reply is the concatenation and the callback saw each piece.
        let outcome = result.expect("valid stream");
        assert_eq!(outcome.content, "Hello there");
        assert_eq!(outcome.finish_reason, "stop");
        assert_eq!(outcome.usage, None);
        assert!(outcome.tool_calls.is_empty());
        assert_eq!(pieces, ["Hel", "lo ", "there"]);
    }

    #[test]
    fn crlf_comments_and_ignored_fields_are_accepted() {
        // Given CRLF line ends, comments, ignored fields, and `data:` without a space.
        let first = delta("a").trim_end().replace('\n', "\r\n");
        let input = format!(
            ": keep-alive\r\nevent: message\r\nid: 7\r\nretry: 100\r\n{first}\r\n\r\ndata:{}\r\ndata: [DONE]\r\n",
            serde_json::json!({"choices":[{"index":0,"delta":{"content":"b"},"finish_reason":"stop"}]})
        );

        // When the reader runs.
        let (result, pieces) = run(StreamMode::Text, input.as_bytes());

        // Then every piece is delivered and the ignored lines changed nothing.
        assert_eq!(result.expect("valid stream").content, "ab");
        assert_eq!(pieces, ["a", "b"]);
    }

    #[test]
    fn an_unknown_field_line_is_refused() {
        let input = format!("{}bogus: 1\n\n{}", delta("a"), DONE);
        let (result, _) = run(StreamMode::Text, input.as_bytes());
        assert_eq!(result, Err(ProviderError::MalformedResponse));
    }

    #[test]
    fn usage_on_a_payload_without_choices_is_taken_and_absent_usage_stays_absent() {
        // Given a usage payload with empty choices after the finish reason.
        let mut input = delta("hi");
        input.push_str(&finish("stop"));
        input.push_str(
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1,\"total_tokens\":4}}\n\n",
        );
        input.push_str(DONE);

        // When the reader runs.
        let (result, _) = run(StreamMode::Text, input.as_bytes());

        // Then the usage is kept; a stream without usage has none.
        assert_eq!(
            result.expect("valid stream").usage,
            Some(Usage {
                prompt_tokens: Some(3),
                completion_tokens: Some(1),
                total_tokens: Some(4),
            })
        );
        let (plain, _) = run(StreamMode::Text, text_stream(&["x"]).as_bytes());
        assert_eq!(plain.expect("valid stream").usage, None);
    }

    #[test]
    fn end_of_input_before_done_is_incomplete() {
        let mut input = delta("hi");
        input.push_str(&finish("stop"));
        let (result, pieces) = run(StreamMode::Text, input.as_bytes());
        assert_eq!(result, Err(ProviderError::IncompleteStream));
        assert_eq!(pieces, ["hi"]);

        // A final line without its line feed was never a complete event.
        let (result, _) = run(StreamMode::Text, format!("{input}data: [DONE]").as_bytes());
        assert_eq!(result, Err(ProviderError::IncompleteStream));
    }

    #[test]
    fn a_refused_stream_shape_fails_without_repair() {
        let cases: Vec<(&str, String)> = vec![
            ("done before finish", format!("{}{DONE}", delta("a"))),
            (
                "content after finish",
                format!("{}{}{DONE}", finish("stop"), delta("late")),
            ),
            (
                "second finish",
                format!("{}{}{DONE}", finish("stop"), finish("stop")),
            ),
            (
                "duplicate key",
                format!(
                    "data: {{\"choices\":[{{\"delta\":{{\"content\":\"a\",\"content\":\"b\"}}}}]}}\n\n{DONE}"
                ),
            ),
            ("non-object payload", format!("data: [1]\n\n{DONE}")),
            (
                "two choices",
                format!("data: {{\"choices\":[{{\"delta\":{{}}}},{{\"delta\":{{}}}}]}}\n\n{DONE}"),
            ),
            (
                "choice index 1",
                format!(
                    "data: {{\"choices\":[{{\"index\":1,\"delta\":{{\"content\":\"a\"}}}}]}}\n\n{DONE}"
                ),
            ),
            (
                "role user",
                format!(
                    "data: {{\"choices\":[{{\"delta\":{{\"role\":\"user\",\"content\":\"a\"}}}}]}}\n\n{DONE}"
                ),
            ),
            (
                "error member",
                format!("data: {{\"error\":{{\"message\":\"boom\"}},\"choices\":[]}}\n\n{DONE}"),
            ),
            (
                "numeric content",
                format!("data: {{\"choices\":[{{\"delta\":{{\"content\":5}}}}]}}\n\n{DONE}"),
            ),
            (
                "missing delta",
                format!("data: {{\"choices\":[{{\"index\":0}}]}}\n\n{DONE}"),
            ),
            (
                "unknown finish reason",
                format!("{}{DONE}", finish("content_filter")),
            ),
            ("empty payload", format!("data:\n\n{DONE}")),
        ];
        for (name, input) in cases {
            let (result, _) = run(StreamMode::Text, input.as_bytes());
            assert_eq!(result, Err(ProviderError::MalformedResponse), "{name}");
        }
    }

    #[test]
    fn text_delivered_before_a_malformed_payload_stays_delivered() {
        let input = format!("{}{}data: not json\n\n{DONE}", delta("one"), delta("two"));
        let (result, pieces) = run(StreamMode::Text, input.as_bytes());
        assert_eq!(result, Err(ProviderError::MalformedResponse));
        assert_eq!(pieces, ["one", "two"]);
    }

    #[test]
    fn a_refused_payload_delivers_none_of_its_own_text() {
        // Given a payload whose content is fine but whose role is refused.
        let input =
            "data: {\"choices\":[{\"delta\":{\"role\":\"tool\",\"content\":\"leak\"}}]}\n\n";
        let (result, pieces) = run(StreamMode::Text, input.as_bytes());
        assert_eq!(result, Err(ProviderError::MalformedResponse));
        assert!(pieces.is_empty());
    }

    #[test]
    fn empty_content_is_never_delivered() {
        let input = text_stream(&["", "a", ""]);
        let (result, pieces) = run(StreamMode::Text, input.as_bytes());
        assert_eq!(result.expect("valid stream").content, "a");
        assert_eq!(pieces, ["a"]);
    }

    #[test]
    fn a_stream_over_one_mebibyte_is_refused() {
        let mut input = String::new();
        while input.len() <= 1024 * 1024 {
            input.push_str(": padding padding padding padding padding padding padding\n");
        }
        let (result, _) = run(StreamMode::Text, input.as_bytes());
        assert_eq!(result, Err(ProviderError::ResponseTooLarge));
    }

    #[test]
    fn nothing_after_done_is_read() {
        let input = format!("{}garbage that is not an event", text_stream(&["a"]));
        let (result, _) = run(StreamMode::Text, input.as_bytes());
        assert_eq!(result.expect("valid stream").content, "a");
    }

    /// A reader that fails after its data, like a connection that times out.
    struct FailingReader(Cursor<Vec<u8>>);

    impl Read for FailingReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            match self.0.read(buffer)? {
                0 => Err(std::io::Error::from(std::io::ErrorKind::TimedOut)),
                count => Ok(count),
            }
        }
    }

    #[test]
    fn a_read_error_is_a_transport_failure_and_keeps_delivered_text() {
        let mut pieces = Vec::new();
        let reader = FailingReader(Cursor::new(delta("kept").into_bytes()));
        let result = read_event_stream(reader, StreamMode::Text, &mut |text: &str| {
            pieces.push(text.to_owned());
        });
        assert_eq!(result, Err(ProviderError::Transport));
        assert_eq!(pieces, ["kept"]);
    }

    #[test]
    fn the_text_mode_refuses_tool_calls_and_tool_finish_reasons() {
        let call_delta = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"type\":\"function\",\"function\":{\"name\":\"n\",\"arguments\":\"\"}}]}}]}\n\n";
        let (result, pieces) = run(StreamMode::Text, call_delta.as_bytes());
        assert_eq!(result, Err(ProviderError::UnsupportedToolCall));
        assert!(pieces.is_empty());

        for reason in ["tool_calls", "function_call"] {
            let input = format!("{}{DONE}", finish(reason));
            let (result, _) = run(StreamMode::Text, input.as_bytes());
            assert_eq!(result, Err(ProviderError::UnsupportedToolCall), "{reason}");
        }
        let legacy = "data: {\"choices\":[{\"delta\":{\"function_call\":{\"name\":\"n\"}}}]}\n\n";
        let (result, _) = run(StreamMode::Text, legacy.as_bytes());
        assert_eq!(result, Err(ProviderError::UnsupportedToolCall));

        // An empty array is LM Studio's ordinary "no action" form.
        let empty = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\",\"tool_calls\":[]},\"finish_reason\":\"stop\"}]}\n\n";
        let (result, _) = run(StreamMode::Text, format!("{empty}{DONE}").as_bytes());
        assert_eq!(result.expect("empty list is no action").content, "ok");
    }

    fn call_payload(entries: &str, finish_reason: Option<&str>) -> String {
        let finish_reason =
            finish_reason.map_or_else(|| "null".to_owned(), |reason| format!("\"{reason}\""));
        format!(
            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{entries}]}},\"finish_reason\":{finish_reason}}}]}}\n\n"
        )
    }

    fn tool_run(parts: &[String], finish_reason: &str) -> Result<StreamOutcome, ProviderError> {
        let mut input = parts.concat();
        input.push_str(&finish(finish_reason));
        input.push_str(DONE);
        run(StreamMode::Tools, input.as_bytes()).0
    }

    #[test]
    fn documented_tool_call_pieces_assemble_into_one_raw_call() {
        // Given the first piece with id, type, name and empty arguments, then fragments.
        let parts = vec![
            call_payload(
                r#"{"index":0,"id":"call_1","type":"function","function":{"name":"workspace_read_file","arguments":""}}"#,
                None,
            ),
            call_payload(r#"{"index":0,"function":{"arguments":"{\"pa"}}"#, None),
            call_payload(
                r#"{"index":0,"function":{"arguments":"th\":\"a.txt\"}"}}"#,
                None,
            ),
        ];

        // When the stream finishes with tool_calls.
        let outcome = tool_run(&parts, "tool_calls").expect("valid tool stream");

        // Then one call holds only the received members and parses.
        assert_eq!(
            outcome.tool_calls,
            vec![serde_json::json!({
                "id": "call_1",
                "type": "function",
                "function": {"name": "workspace_read_file", "arguments": "{\"path\":\"a.txt\"}"}
            })]
        );
        let call = outcome.tool_calls.first().expect("one call");
        assert!(crate::turn::ToolCall::parse(call).is_ok());
        assert_eq!(outcome.content, "");
        assert_eq!(outcome.finish_reason, "tool_calls");
    }

    #[test]
    fn a_tool_name_sent_in_pieces_is_concatenated_and_a_missing_id_is_absent() {
        let parts = vec![
            call_payload(r#"{"index":0,"function":{"name":"workspace_"}}"#, None),
            call_payload(
                r#"{"index":0,"function":{"name":"read_file","arguments":"{}"}}"#,
                None,
            ),
        ];
        let outcome = tool_run(&parts, "stop").expect("finish stop with calls is accepted");
        assert_eq!(
            outcome.tool_calls,
            vec![serde_json::json!({
                "function": {"name": "workspace_read_file", "arguments": "{}"}
            })]
        );
    }

    #[test]
    fn null_members_of_later_pieces_are_treated_as_absent() {
        // Given a first piece with every member and later pieces that send
        // null for the members they do not repeat.
        let parts = vec![
            call_payload(
                r#"{"index":0,"id":"a","type":"function","function":{"name":"workspace_read_file","arguments":""}}"#,
                None,
            ),
            call_payload(
                r#"{"index":0,"id":null,"type":null,"function":{"name":null,"arguments":"{}"}}"#,
                None,
            ),
            call_payload(r#"{"index":0,"function":null}"#, None),
        ];

        // When the stream is read.
        let outcome = tool_run(&parts, "tool_calls").expect("null members are absent");

        // Then the nulls changed nothing.
        assert_eq!(
            outcome.tool_calls,
            vec![serde_json::json!({
                "id": "a",
                "type": "function",
                "function": {"name": "workspace_read_file", "arguments": "{}"}
            })]
        );
    }

    #[test]
    fn a_null_role_is_treated_as_absent() {
        let input = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"role\":null,\"content\":\"a\"}}}}]}}\n\n{}{DONE}",
            finish("stop")
        );
        let (result, pieces) = run(StreamMode::Text, input.as_bytes());
        assert_eq!(result.expect("a null role is absent").content, "a");
        assert_eq!(pieces, ["a"]);
    }

    #[test]
    fn a_repeated_equal_id_is_accepted_and_a_different_id_is_refused() {
        let same = vec![
            call_payload(r#"{"index":0,"id":"a"}"#, None),
            call_payload(r#"{"index":0,"id":"a"}"#, None),
        ];
        assert!(tool_run(&same, "tool_calls").is_ok());

        let different = vec![
            call_payload(r#"{"index":0,"id":"a"}"#, None),
            call_payload(r#"{"index":0,"id":"b"}"#, None),
        ];
        assert_eq!(
            tool_run(&different, "tool_calls"),
            Err(ProviderError::MalformedResponse)
        );
    }

    #[test]
    fn malformed_call_pieces_are_refused() {
        let cases = [
            ("index gap", r#"{"index":1,"id":"a"}"#),
            ("no index", r#"{"id":"a"}"#),
            ("string index", r#"{"index":"0","id":"a"}"#),
            ("unknown call key", r#"{"index":0,"extra":1}"#),
            ("wrong type", r#"{"index":0,"type":"other"}"#),
            (
                "unknown function key",
                r#"{"index":0,"function":{"other":"x"}}"#,
            ),
            ("numeric name", r#"{"index":0,"function":{"name":5}}"#),
            ("numeric id", r#"{"index":0,"id":5}"#),
            ("non-object function", r#"{"index":0,"function":"x"}"#),
            ("non-object call", "5"),
        ];
        for (name, entry) in cases {
            let parts = vec![call_payload(entry, None)];
            assert_eq!(
                tool_run(&parts, "tool_calls"),
                Err(ProviderError::MalformedResponse),
                "{name}"
            );
        }
    }

    #[test]
    fn the_call_limit_is_enforced_at_index_sixty_four() {
        let ids: Vec<String> = (0..64)
            .map(|index| call_payload(&format!(r#"{{"index":{index},"id":"c{index}"}}"#), None))
            .collect();
        assert_eq!(
            tool_run(&ids, "tool_calls")
                .expect("sixty-four calls")
                .tool_calls
                .len(),
            64
        );
        let mut over = ids;
        over.push(call_payload(r#"{"index":64,"id":"c64"}"#, None));
        assert_eq!(
            tool_run(&over, "tool_calls"),
            Err(ProviderError::MalformedResponse)
        );
    }

    #[test]
    fn tool_finish_reasons_follow_the_non_streaming_rules() {
        let call = vec![call_payload(r#"{"index":0,"id":"a"}"#, None)];
        assert_eq!(
            tool_run(&call, "length"),
            Err(ProviderError::MalformedResponse)
        );
        assert_eq!(
            tool_run(&call, "function_call"),
            Err(ProviderError::MalformedResponse)
        );
        assert!(tool_run(&call, "stop").is_ok());
        // `tool_calls` with no call, and `length` text, mirror the buffered parser.
        assert_eq!(
            tool_run(&[delta("text")], "tool_calls"),
            Err(ProviderError::MalformedResponse)
        );
        assert!(tool_run(&[delta("text")], "length").is_ok());
    }

    #[test]
    fn the_finish_reason_may_share_a_payload_with_the_last_call_piece() {
        let mut input = call_payload(
            r#"{"index":0,"id":"a","type":"function","function":{"name":"workspace_list_directory","arguments":"{}"}}"#,
            Some("tool_calls"),
        );
        input.push_str(DONE);
        let (result, _) = run(StreamMode::Tools, input.as_bytes());
        assert_eq!(result.expect("valid").tool_calls.len(), 1);
    }

    #[test]
    fn tool_mode_text_alongside_calls_is_kept() {
        let parts = vec![
            delta("Looking."),
            call_payload(r#"{"index":0,"id":"a"}"#, None),
        ];
        let outcome = tool_run(&parts, "tool_calls").expect("valid");
        assert_eq!(outcome.content, "Looking.");
    }
}
