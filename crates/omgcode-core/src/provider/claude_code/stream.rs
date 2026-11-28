//! Bounded line reading and strict parsing of the Claude Code `stream-json`
//! output, plus the launch lockdown check.
//!
//! Every line is untrusted. Lines are bounded before they are parsed, parsed
//! with duplicate keys refused, and reduced to the few fields OMGCode uses.
//! Every string and collection kept is bounded; a value beyond a limit is an
//! error, never a truncation.

use crate::storage::parse_strict_json;
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::io::{ErrorKind, Read};
use thiserror::Error;

/// The longest accepted output line. A line holds one whole event, and a
/// streamed 32 KiB tool-call argument repeats its envelope on every piece.
pub const MAX_STREAM_LINE_BYTES: usize = 8 * 1024 * 1024;
/// The longest kept name, id, status, or version, in bytes.
pub const MAX_NAME_BYTES: usize = 256;
/// The most entries kept from one list in the init report.
pub const MAX_LIST_ITEMS: usize = 256;
/// The most content blocks kept from one assistant message.
pub const MAX_BLOCKS: usize = 256;

/// Bytes requested from the reader at a time.
const READ_CHUNK_BYTES: usize = 8 * 1024;

/// Why a line could not be read or understood.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum StreamError {
    /// The reader failed.
    #[error("reading the Claude Code stream failed ({0:?})")]
    Io(ErrorKind),
    /// A line was longer than the limit.
    #[error("a Claude Code stream line is longer than {limit} bytes")]
    LineTooLong {
        /// The limit that was exceeded.
        limit: usize,
    },
    /// The stream ended inside a line.
    #[error("the Claude Code stream ended inside a line")]
    PartialLine,
    /// The line is not one JSON value, or repeats an object key.
    #[error("a Claude Code stream line is not strict JSON")]
    InvalidJson,
    /// A known message does not have the expected shape.
    #[error("a Claude Code stream message has an unexpected shape ({0})")]
    Shape(&'static str),
    /// A kept string is longer than its limit.
    #[error("a Claude Code stream field is too long ({0})")]
    TooLong(&'static str),
    /// A kept list is longer than its limit.
    #[error("a Claude Code stream list is too long ({0})")]
    TooMany(&'static str),
}

/// Splits a byte stream into lines without buffering more than a limit.
///
/// A line ends at `\n`; one trailing `\r` is stripped. A line longer than the
/// limit is an error, never a truncation, and the reader is not read further
/// once that is known. An end of stream inside a line is an error. After any
/// error the iterator ends. Blank lines are returned as empty lines.
pub struct BoundedLines<R: Read> {
    reader: R,
    limit: usize,
    chunk: Vec<u8>,
    start: usize,
    end: usize,
    finished: bool,
}

impl<R: Read> BoundedLines<R> {
    /// Read lines of at most [`MAX_STREAM_LINE_BYTES`].
    pub fn new(reader: R) -> Self {
        Self::with_limit(reader, MAX_STREAM_LINE_BYTES)
    }

    /// Read lines of at most `limit` bytes, counting a trailing `\r`.
    pub fn with_limit(reader: R, limit: usize) -> Self {
        Self {
            reader,
            limit,
            chunk: vec![0; READ_CHUNK_BYTES],
            start: 0,
            end: 0,
            finished: false,
        }
    }

    fn fail(&mut self, error: StreamError) -> Option<Result<Vec<u8>, StreamError>> {
        self.finished = true;
        Some(Err(error))
    }
}

impl<R: Read> Iterator for BoundedLines<R> {
    type Item = Result<Vec<u8>, StreamError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let mut line: Vec<u8> = Vec::new();
        loop {
            if self.start == self.end {
                match self.reader.read(&mut self.chunk) {
                    Ok(0) => {
                        self.finished = true;
                        return if line.is_empty() {
                            None
                        } else {
                            Some(Err(StreamError::PartialLine))
                        };
                    }
                    Ok(count) => {
                        self.start = 0;
                        self.end = count.min(self.chunk.len());
                    }
                    Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                    Err(error) => return self.fail(StreamError::Io(error.kind())),
                }
            }
            let available = &self.chunk[self.start..self.end];
            let newline = available.iter().position(|byte| *byte == b'\n');
            let taken = newline.unwrap_or(available.len());
            // The check precedes the copy, so a line never grows past the
            // limit by more than the checked amount.
            if line.len().saturating_add(taken) > self.limit {
                return self.fail(StreamError::LineTooLong { limit: self.limit });
            }
            line.extend_from_slice(&available[..taken]);
            if newline.is_some() {
                self.start += taken + 1;
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Some(Ok(line));
            }
            self.start = self.end;
        }
    }
}

/// One MCP server entry of the init report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpServerStatus {
    /// The server name Claude Code reports.
    pub name: String,
    /// The connection status, such as `connected`.
    pub status: String,
}

/// The fields of the `system` / `init` event that the lockdown check needs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitReport {
    /// `claude_code_version`.
    pub version: String,
    /// The model the session reports.
    pub model: String,
    /// `apiKeySource`; `none` means no API key is in use.
    pub api_key_source: String,
    /// Every tool the model can call, built-in or MCP.
    pub tools: Vec<String>,
    /// The MCP servers Claude Code connected to.
    pub mcp_servers: Vec<McpServerStatus>,
    /// Plugin names.
    pub plugins: Vec<String>,
    /// Skill names.
    pub skills: Vec<String>,
    /// Slash command names.
    pub slash_commands: Vec<String>,
}

/// One content block of an assistant message.
#[derive(Clone, Debug, PartialEq)]
pub enum AssistantBlock {
    /// Assistant text. It is untrusted model output.
    Text(String),
    /// A tool call the model made. The input is untrusted.
    ToolUse {
        /// The tool-use id.
        id: String,
        /// The tool name as Claude Code reports it, such as
        /// `mcp__omgcode__workspace_read_file`.
        name: String,
        /// The raw input value.
        input: Value,
    },
    /// Any other block, such as thinking. Its content is dropped.
    Other,
}

/// An `assistant` event.
#[derive(Clone, Debug, PartialEq)]
pub struct AssistantMessage {
    /// The message id, when present.
    pub message_id: Option<String>,
    /// The parent tool-use id; present for a sub-agent's message.
    pub parent_tool_use_id: Option<String>,
    /// The content blocks, in order.
    pub blocks: Vec<AssistantBlock>,
    /// The top-level `error` code, such as `authentication_failed`.
    pub error: Option<String>,
}

/// The final `result` event.
///
/// Success is decided by `is_error`, never by `subtype`: a failed sign-in
/// reports subtype `success` with `is_error` true.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResultReport {
    /// Whether the run failed.
    pub is_error: bool,
    /// The result subtype. Do not use it to decide success.
    pub subtype: String,
    /// The final text, or the error text when `is_error` is true.
    pub result: Option<String>,
    /// Why the run ended, such as `completed` or `api_error`.
    pub terminal_reason: Option<String>,
    /// The model's stop reason.
    pub stop_reason: Option<String>,
}

/// One parsed output line.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    /// The init event.
    Init(InitReport),
    /// An assistant message.
    Assistant(AssistantMessage),
    /// A piece of provisional reply text from the main agent. Only a
    /// `text_delta` whose `parent_tool_use_id` is null is reported.
    TextDelta(String),
    /// The name of a `tool_use` block from its streamed
    /// `content_block_start`, from any agent, before the block is complete.
    /// The caller checks it against the offered tools at once.
    ToolUseStart(String),
    /// The final result.
    Result(ResultReport),
    /// A well-formed line that OMGCode does not use, including unknown
    /// top-level types (forward compatibility) and empty lines.
    Ignored,
}

/// Parse one output line.
///
/// # Errors
///
/// Returns an error for invalid or duplicate-key JSON, and for a known
/// message type whose shape is wrong or whose values exceed a limit. An
/// unknown top-level `type` is [`StreamEvent::Ignored`].
pub fn parse_line(line: &[u8]) -> Result<StreamEvent, StreamError> {
    if line.is_empty() {
        return Ok(StreamEvent::Ignored);
    }
    let value = parse_strict_json(line).map_err(|_| StreamError::InvalidJson)?;
    let top = as_object(&value, "line")?;
    let Some(kind) = top.get("type").and_then(Value::as_str) else {
        return Err(StreamError::Shape("type"));
    };
    match kind {
        "system" => match top.get("subtype").and_then(Value::as_str) {
            Some("init") => parse_init(top).map(StreamEvent::Init),
            _ => Ok(StreamEvent::Ignored),
        },
        "stream_event" => parse_stream_event(top),
        "assistant" => parse_assistant(top).map(StreamEvent::Assistant),
        "result" => parse_result(top).map(StreamEvent::Result),
        _ => Ok(StreamEvent::Ignored),
    }
}

fn as_object<'a>(
    value: &'a Value,
    field: &'static str,
) -> Result<&'a Map<String, Value>, StreamError> {
    value.as_object().ok_or(StreamError::Shape(field))
}

fn bounded(text: &str, field: &'static str) -> Result<String, StreamError> {
    if text.len() > MAX_NAME_BYTES {
        return Err(StreamError::TooLong(field));
    }
    Ok(text.to_owned())
}

fn required_name(map: &Map<String, Value>, field: &'static str) -> Result<String, StreamError> {
    match map.get(field).and_then(Value::as_str) {
        Some(text) => bounded(text, field),
        None => Err(StreamError::Shape(field)),
    }
}

/// An optional bounded string: absent and `null` are `None`.
fn optional_name(
    map: &Map<String, Value>,
    field: &'static str,
) -> Result<Option<String>, StreamError> {
    match map.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => bounded(text, field).map(Some),
        Some(_) => Err(StreamError::Shape(field)),
    }
}

/// An optional string bounded only by the line limit.
fn optional_text(
    map: &Map<String, Value>,
    field: &'static str,
) -> Result<Option<String>, StreamError> {
    match map.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(StreamError::Shape(field)),
    }
}

fn array<'a>(
    map: &'a Map<String, Value>,
    field: &'static str,
    limit: usize,
) -> Result<&'a [Value], StreamError> {
    let items = map
        .get(field)
        .and_then(Value::as_array)
        .ok_or(StreamError::Shape(field))?;
    if items.len() > limit {
        return Err(StreamError::TooMany(field));
    }
    Ok(items)
}

/// A list entry that is a string or an object with a string `name`.
fn entry_name(value: &Value, field: &'static str) -> Result<String, StreamError> {
    match value {
        Value::String(text) => bounded(text, field),
        Value::Object(map) => required_name(map, "name"),
        _ => Err(StreamError::Shape(field)),
    }
}

fn names(map: &Map<String, Value>, field: &'static str) -> Result<Vec<String>, StreamError> {
    array(map, field, MAX_LIST_ITEMS)?
        .iter()
        .map(|item| entry_name(item, field))
        .collect()
}

fn parse_init(top: &Map<String, Value>) -> Result<InitReport, StreamError> {
    let mcp_servers = array(top, "mcp_servers", MAX_LIST_ITEMS)?
        .iter()
        .map(|item| {
            let map = as_object(item, "mcp_servers")?;
            Ok(McpServerStatus {
                name: required_name(map, "name")?,
                status: required_name(map, "status")?,
            })
        })
        .collect::<Result<Vec<_>, StreamError>>()?;
    Ok(InitReport {
        version: required_name(top, "claude_code_version")?,
        model: required_name(top, "model")?,
        api_key_source: required_name(top, "apiKeySource")?,
        tools: names(top, "tools")?,
        mcp_servers,
        plugins: names(top, "plugins")?,
        skills: names(top, "skills")?,
        slash_commands: names(top, "slash_commands")?,
    })
}

fn parse_stream_event(top: &Map<String, Value>) -> Result<StreamEvent, StreamError> {
    let event = as_object(
        top.get("event").ok_or(StreamError::Shape("event"))?,
        "event",
    )?;
    match event.get("type").and_then(Value::as_str) {
        Some("content_block_start") => return parse_block_start(event),
        Some("content_block_delta") => {}
        _ => return Ok(StreamEvent::Ignored),
    }
    if optional_name(top, "parent_tool_use_id")?.is_some() {
        return Ok(StreamEvent::Ignored);
    }
    let delta = as_object(
        event.get("delta").ok_or(StreamError::Shape("delta"))?,
        "delta",
    )?;
    if delta.get("type").and_then(Value::as_str) != Some("text_delta") {
        return Ok(StreamEvent::Ignored);
    }
    match delta.get("text") {
        Some(Value::String(text)) => Ok(StreamEvent::TextDelta(text.clone())),
        _ => Err(StreamError::Shape("text")),
    }
}

/// A `content_block_start` reports a `tool_use` block's name; any other
/// block start is ignored.
fn parse_block_start(event: &Map<String, Value>) -> Result<StreamEvent, StreamError> {
    let block = as_object(
        event
            .get("content_block")
            .ok_or(StreamError::Shape("content_block"))?,
        "content_block",
    )?;
    if block.get("type").and_then(Value::as_str) != Some("tool_use") {
        return Ok(StreamEvent::Ignored);
    }
    let name = required_name(block, "name")?;
    if name.is_empty() {
        return Err(StreamError::Shape("tool_use"));
    }
    Ok(StreamEvent::ToolUseStart(name))
}

fn parse_assistant(top: &Map<String, Value>) -> Result<AssistantMessage, StreamError> {
    let message = as_object(
        top.get("message").ok_or(StreamError::Shape("message"))?,
        "message",
    )?;
    let blocks = array(message, "content", MAX_BLOCKS)?
        .iter()
        .map(parse_block)
        .collect::<Result<Vec<_>, StreamError>>()?;
    Ok(AssistantMessage {
        message_id: optional_name(message, "id")?,
        parent_tool_use_id: optional_name(top, "parent_tool_use_id")?,
        blocks,
        error: optional_name(top, "error")?,
    })
}

fn parse_block(value: &Value) -> Result<AssistantBlock, StreamError> {
    let map = as_object(value, "content")?;
    match map.get("type").and_then(Value::as_str) {
        Some("text") => match map.get("text") {
            Some(Value::String(text)) => Ok(AssistantBlock::Text(text.clone())),
            _ => Err(StreamError::Shape("text")),
        },
        Some("tool_use") => {
            let id = required_name(map, "id")?;
            let name = required_name(map, "name")?;
            if id.is_empty() || name.is_empty() {
                return Err(StreamError::Shape("tool_use"));
            }
            let input = map.get("input").ok_or(StreamError::Shape("input"))?.clone();
            Ok(AssistantBlock::ToolUse { id, name, input })
        }
        Some(_) => Ok(AssistantBlock::Other),
        None => Err(StreamError::Shape("content type")),
    }
}

fn parse_result(top: &Map<String, Value>) -> Result<ResultReport, StreamError> {
    let Some(is_error) = top.get("is_error").and_then(Value::as_bool) else {
        return Err(StreamError::Shape("is_error"));
    };
    Ok(ResultReport {
        is_error,
        subtype: required_name(top, "subtype")?,
        result: optional_text(top, "result")?,
        terminal_reason: optional_name(top, "terminal_reason")?,
        stop_reason: optional_name(top, "stop_reason")?,
    })
}

/// The check that failed. Values are bounded by [`MAX_NAME_BYTES`].
#[derive(Debug, Error, Eq, PartialEq)]
pub enum LockdownViolation {
    /// A tool outside the expected set is available, such as a built-in.
    #[error("an unexpected tool is available: {0}")]
    UnexpectedTool(String),
    /// An expected tool is not available.
    #[error("an expected tool is missing: {0}")]
    MissingTool(String),
    /// The tool list repeats a name.
    #[error("the tool list repeats a name")]
    DuplicateTool,
    /// The number of MCP servers is not one.
    #[error("{0} MCP servers were reported, expected exactly one")]
    McpServerCount(usize),
    /// The one MCP server has another name.
    #[error("the MCP server is named {0}")]
    McpServerName(String),
    /// The MCP server is not `connected`.
    #[error("the MCP server status is {0}")]
    McpServerStatus(String),
    /// Plugins are loaded.
    #[error("{0} plugins are loaded")]
    PluginsPresent(usize),
    /// Skills are loaded.
    #[error("{0} skills are loaded")]
    SkillsPresent(usize),
    /// Slash commands are available.
    #[error("{0} slash commands are available")]
    SlashCommandsPresent(usize),
    /// An API key may be in use.
    #[error("the API key source is {0}, expected none")]
    ApiKeySource(String),
}

/// Check that the session offers exactly the OMGCode MCP tools and nothing
/// that bills an API key or runs other code.
///
/// `tools` must equal, as a set, `mcp__<server>__<tool>` for each expected
/// tool; `mcp_servers` must be exactly `server` with status `connected`;
/// plugins, skills, and slash commands must be empty; and `apiKeySource` must
/// be `none`, so an API key in the environment cannot bill silently.
///
/// # Errors
///
/// Returns the first failed check.
pub fn check_lockdown(
    init: &InitReport,
    server: &str,
    expected_tools: &[&str],
) -> Result<(), LockdownViolation> {
    if init.api_key_source != "none" {
        return Err(LockdownViolation::ApiKeySource(init.api_key_source.clone()));
    }
    let expected: HashSet<String> = expected_tools
        .iter()
        .map(|tool| format!("mcp__{server}__{tool}"))
        .collect();
    let actual: HashSet<&str> = init.tools.iter().map(String::as_str).collect();
    if actual.len() != init.tools.len() {
        return Err(LockdownViolation::DuplicateTool);
    }
    if let Some(extra) = init.tools.iter().find(|tool| !expected.contains(*tool)) {
        return Err(LockdownViolation::UnexpectedTool(extra.clone()));
    }
    let mut missing: Vec<&String> = expected
        .iter()
        .filter(|tool| !actual.contains(tool.as_str()))
        .collect();
    missing.sort();
    if let Some(tool) = missing.first() {
        return Err(LockdownViolation::MissingTool(bounded_copy(tool)));
    }
    match init.mcp_servers.as_slice() {
        [only] => {
            if only.name != server {
                return Err(LockdownViolation::McpServerName(only.name.clone()));
            }
            if only.status != "connected" {
                return Err(LockdownViolation::McpServerStatus(only.status.clone()));
            }
        }
        other => return Err(LockdownViolation::McpServerCount(other.len())),
    }
    if !init.plugins.is_empty() {
        return Err(LockdownViolation::PluginsPresent(init.plugins.len()));
    }
    if !init.skills.is_empty() {
        return Err(LockdownViolation::SkillsPresent(init.skills.len()));
    }
    if !init.slash_commands.is_empty() {
        return Err(LockdownViolation::SlashCommandsPresent(
            init.slash_commands.len(),
        ));
    }
    Ok(())
}

/// A copy cut at a character boundary within [`MAX_NAME_BYTES`], for a name
/// the caller (not Claude) supplied.
fn bounded_copy(text: &str) -> String {
    text.chars()
        .scan(0usize, |used, ch| {
            *used += ch.len_utf8();
            (*used <= MAX_NAME_BYTES).then_some(ch)
        })
        .collect()
}

#[cfg(test)]
mod tests;
