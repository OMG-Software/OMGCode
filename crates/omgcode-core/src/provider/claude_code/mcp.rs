//! An I/O-free MCP server state machine (JSON-RPC 2.0, one message per line).
//!
//! The caller reads lines from Claude Code, feeds each to
//! [`McpServer::handle_line`], writes the returned reply lines back, runs each
//! [`McpOutput::Call`] through its own authorisation, and answers it with
//! [`McpServer::answer`]. The server never sends a request of its own: no
//! sampling, elicitation, or roots. Everything received is untrusted.
//!
//! Supported protocol versions: only `2025-11-25`, the version Claude Code
//! 2.1.325 negotiated in an observed session. Any `initialize` is answered
//! with it, and the client decides whether to continue. The stateless
//! `server/discover` of 2026-07-28 is answered `-32601`, which makes Claude
//! Code fall back to `initialize`. This was not checked against the
//! specification text beyond that observed exchange.

use crate::storage::parse_strict_json;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use thiserror::Error;

/// The longest accepted input line. A `tools/call` carries up to 32 KiB of
/// arguments and escaping can double them.
pub const MAX_MCP_LINE_BYTES: usize = 64 * 1024;
/// The most calls that may be unanswered at once, cancelled ones included
/// until their answer is dropped.
pub const MAX_PENDING_CALLS: usize = 16;
/// The most request ids one session records as used. MCP forbids reusing an
/// id within a session, so a full record refuses further requests instead of
/// forgetting an id.
pub const MAX_USED_REQUEST_IDS: usize = 4096;
/// The longest accepted string request id, in bytes.
pub const MAX_ID_BYTES: usize = 256;
/// The longest accepted tool-use id, in bytes.
pub const MAX_TOOL_USE_ID_BYTES: usize = 256;
/// The largest text [`McpServer::answer`] will encode, in bytes.
pub const MAX_ANSWER_TEXT_BYTES: usize = 1024 * 1024;
/// The one protocol version this server answers with.
pub const PROTOCOL_VERSION: &str = "2025-11-25";

const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_REQUEST: i64 = -32600;
const INVALID_PARAMS: i64 = -32602;
const NOT_INITIALIZED: i64 = -32002;

/// A fatal protocol failure. The caller ends the turn.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum McpError {
    /// A line was longer than [`MAX_MCP_LINE_BYTES`].
    #[error("an MCP line is longer than {MAX_MCP_LINE_BYTES} bytes")]
    LineTooLong,
    /// A line is not strict JSON.
    #[error("an MCP line is not strict JSON")]
    InvalidJson,
    /// A JSON-RPC batch (an array) was sent.
    #[error("a JSON-RPC batch is not accepted")]
    BatchRejected,
    /// The message is not an object.
    #[error("an MCP message is not an object")]
    NotAnObject,
    /// `jsonrpc` is missing or not `2.0`.
    #[error("the jsonrpc member is missing or not 2.0")]
    InvalidVersion,
    /// An id is not an integer or a bounded string.
    #[error("a request id is not an integer or a bounded string")]
    InvalidId,
    /// `method` is present but not a string.
    #[error("the method is not a string")]
    InvalidMethod,
    /// The client sent a response, but the server sends no requests.
    #[error("a response arrived but no request was sent")]
    UnexpectedResponse,
    /// A call reused the id of an unanswered call.
    #[error("a call reused the id of an unanswered call")]
    DuplicatePending,
    /// A request reused the id of an earlier request in this session.
    #[error("a request reused an id already used in this session")]
    RequestIdReused,
    /// The record of used request ids is full.
    #[error("more than {MAX_USED_REQUEST_IDS} request ids were used in this session")]
    TooManyRequestIds,
    /// Too many calls are unanswered.
    #[error("more than {MAX_PENDING_CALLS} calls are unanswered")]
    TooManyPending,
    /// The key is not an unanswered call.
    #[error("the call is unknown or already answered")]
    UnknownRequest,
    /// The answer text is longer than [`MAX_ANSWER_TEXT_BYTES`].
    #[error("the answer text is longer than {MAX_ANSWER_TEXT_BYTES} bytes")]
    AnswerTooLarge,
    /// A message could not be encoded.
    #[error("a message could not be encoded")]
    Encode,
}

/// A JSON-RPC request id: an integer or a string.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum RequestKey {
    /// An integer id.
    Number(i64),
    /// A string id of at most [`MAX_ID_BYTES`] bytes.
    Text(String),
}

impl RequestKey {
    fn from_value(value: &Value) -> Result<Self, McpError> {
        match value {
            Value::Number(number) => number.as_i64().map(Self::Number).ok_or(McpError::InvalidId),
            Value::String(text) if text.len() <= MAX_ID_BYTES => Ok(Self::Text(text.clone())),
            _ => Err(McpError::InvalidId),
        }
    }

    fn to_value(&self) -> Value {
        match self {
            Self::Number(number) => json!(number),
            Self::Text(text) => json!(text),
        }
    }
}

/// One tool the server lists.
#[derive(Clone, Debug, PartialEq)]
pub struct McpTool {
    /// The tool name.
    pub name: String,
    /// The description shown to the model.
    pub description: String,
    /// The JSON Schema of the arguments.
    pub input_schema: Value,
}

/// A `tools/call` the caller must answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingCall {
    /// The key to pass to [`McpServer::answer`].
    pub key: RequestKey,
    /// The requested tool name, untrusted and not checked against the list.
    pub name: String,
    /// The arguments re-serialised compactly. A non-object value is carried
    /// through as is for the caller's runtime to refuse; absent arguments
    /// become `{}`.
    pub arguments_json: String,
    /// `_meta["claudecode/toolUseId"]` when it is a short string without
    /// control characters.
    pub tool_use_id: Option<String>,
}

/// What [`McpServer::handle_line`] asks the caller to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum McpOutput {
    /// Write this line (it ends with `\n`) to Claude Code.
    Reply(Vec<u8>),
    /// A tool call to run and answer.
    Call(PendingCall),
    /// Claude Code cancelled this unanswered call. Stop it if possible; its
    /// eventual answer is dropped by [`McpServer::answer`].
    Cancelled(RequestKey),
}

/// The result of [`McpServer::answer`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Answer {
    /// Write this `tools/call` response line.
    Line(Vec<u8>),
    /// The call was cancelled; write nothing.
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CallState {
    Active,
    Cancelled,
}

/// The server state: its identity, its tools, and the unanswered calls.
pub struct McpServer {
    name: String,
    version: String,
    tools: Vec<McpTool>,
    initialized: bool,
    pending: HashMap<RequestKey, CallState>,
    used_ids: HashSet<RequestKey>,
}

impl McpServer {
    /// Create a server that lists `tools`.
    pub fn new(name: impl Into<String>, version: impl Into<String>, tools: Vec<McpTool>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            tools,
            initialized: false,
            pending: HashMap::new(),
            used_ids: HashSet::new(),
        }
    }

    /// Whether `initialize` has been answered.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// The number of calls not yet answered, cancelled ones included.
    pub fn pending_calls(&self) -> usize {
        self.pending.len()
    }

    /// Handle one input line (without its newline).
    ///
    /// # Errors
    ///
    /// Returns a fatal [`McpError`] for an over-long, invalid, batched, or
    /// malformed message, a response, a reused request id, a full record of
    /// used ids, or too many pending calls. Recoverable client mistakes (unknown method, request
    /// before `initialize`, bad parameters) are JSON-RPC error replies.
    pub fn handle_line(&mut self, line: &[u8]) -> Result<Vec<McpOutput>, McpError> {
        if line.len() > MAX_MCP_LINE_BYTES {
            return Err(McpError::LineTooLong);
        }
        let value = parse_strict_json(line).map_err(|_| McpError::InvalidJson)?;
        let message = match &value {
            Value::Array(_) => return Err(McpError::BatchRejected),
            Value::Object(map) => map,
            _ => return Err(McpError::NotAnObject),
        };
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(McpError::InvalidVersion);
        }
        let Some(method) = message.get("method") else {
            return Err(McpError::UnexpectedResponse);
        };
        let Some(method) = method.as_str() else {
            return Err(McpError::InvalidMethod);
        };
        match message.get("id") {
            None => self.notification(method, message.get("params")),
            Some(id) => {
                let key = RequestKey::from_value(id)?;
                self.request(key, method, message.get("params"))
            }
        }
    }

    fn notification(
        &mut self,
        method: &str,
        params: Option<&Value>,
    ) -> Result<Vec<McpOutput>, McpError> {
        if method != "notifications/cancelled" {
            return Ok(Vec::new());
        }
        let Some(request_id) = params.and_then(|p| p.get("requestId")) else {
            return Ok(Vec::new());
        };
        let Ok(key) = RequestKey::from_value(request_id) else {
            return Ok(Vec::new());
        };
        match self.pending.get_mut(&key) {
            Some(state @ CallState::Active) => {
                *state = CallState::Cancelled;
                Ok(vec![McpOutput::Cancelled(key)])
            }
            _ => Ok(Vec::new()),
        }
    }

    fn request(
        &mut self,
        key: RequestKey,
        method: &str,
        params: Option<&Value>,
    ) -> Result<Vec<McpOutput>, McpError> {
        self.record_id(&key)?;
        let reply = match method {
            "server/discover" => error_reply(&key, METHOD_NOT_FOUND, "method not found")?,
            "ping" => result_reply(&key, json!({}))?,
            "initialize" => self.initialize(&key, params)?,
            _ if !self.initialized => error_reply(&key, NOT_INITIALIZED, "not initialized")?,
            "tools/list" => result_reply(&key, self.tools_value())?,
            "tools/call" => return self.tools_call(key, params),
            _ => error_reply(&key, METHOD_NOT_FOUND, "method not found")?,
        };
        Ok(vec![McpOutput::Reply(reply)])
    }

    /// Record `key` as used, before any method dispatch or validation.
    fn record_id(&mut self, key: &RequestKey) -> Result<(), McpError> {
        if self.pending.contains_key(key) {
            return Err(McpError::DuplicatePending);
        }
        if self.used_ids.contains(key) {
            return Err(McpError::RequestIdReused);
        }
        if self.used_ids.len() >= MAX_USED_REQUEST_IDS {
            return Err(McpError::TooManyRequestIds);
        }
        self.used_ids.insert(key.clone());
        Ok(())
    }

    fn initialize(
        &mut self,
        key: &RequestKey,
        params: Option<&Value>,
    ) -> Result<Vec<u8>, McpError> {
        if self.initialized {
            return error_reply(key, INVALID_REQUEST, "already initialized");
        }
        let valid = params
            .and_then(Value::as_object)
            .is_some_and(|p| p.get("protocolVersion").is_some_and(Value::is_string));
        if !valid {
            return error_reply(key, INVALID_PARAMS, "invalid initialize parameters");
        }
        self.initialized = true;
        result_reply(
            key,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": self.name, "version": self.version },
            }),
        )
    }

    fn tools_value(&self) -> Value {
        let tools: Vec<Value> = self
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.input_schema,
                })
            })
            .collect();
        json!({ "tools": tools })
    }

    fn tools_call(
        &mut self,
        key: RequestKey,
        params: Option<&Value>,
    ) -> Result<Vec<McpOutput>, McpError> {
        let Some(params) = params.and_then(Value::as_object) else {
            return Ok(vec![McpOutput::Reply(error_reply(
                &key,
                INVALID_PARAMS,
                "invalid tools/call parameters",
            )?)]);
        };
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return Ok(vec![McpOutput::Reply(error_reply(
                &key,
                INVALID_PARAMS,
                "invalid tools/call parameters",
            )?)]);
        };
        if self.pending.len() >= MAX_PENDING_CALLS {
            return Err(McpError::TooManyPending);
        }
        let arguments_json = match params.get("arguments") {
            Some(arguments) => serde_json::to_string(arguments).map_err(|_| McpError::Encode)?,
            None => "{}".to_owned(),
        };
        let call = PendingCall {
            key: key.clone(),
            name: name.to_owned(),
            arguments_json,
            tool_use_id: tool_use_id(params),
        };
        self.pending.insert(key, CallState::Active);
        Ok(vec![McpOutput::Call(call)])
    }

    /// Build the `tools/call` response for an unanswered call.
    ///
    /// A call cancelled by the client is removed and reported as
    /// [`Answer::Cancelled`] so its late answer can be dropped.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::UnknownRequest`] for an unknown or already
    /// answered key and [`McpError::AnswerTooLarge`] for text over
    /// [`MAX_ANSWER_TEXT_BYTES`]; the call stays pending in the second case.
    pub fn answer(
        &mut self,
        key: &RequestKey,
        text: &str,
        is_error: bool,
    ) -> Result<Answer, McpError> {
        let Some(state) = self.pending.get(key).copied() else {
            return Err(McpError::UnknownRequest);
        };
        if state == CallState::Cancelled {
            self.pending.remove(key);
            return Ok(Answer::Cancelled);
        }
        if text.len() > MAX_ANSWER_TEXT_BYTES {
            return Err(McpError::AnswerTooLarge);
        }
        let line = result_reply(
            key,
            json!({
                "content": [{ "type": "text", "text": text }],
                "isError": is_error,
            }),
        )?;
        self.pending.remove(key);
        Ok(Answer::Line(line))
    }
}

/// `_meta["claudecode/toolUseId"]` when it is a short string without control
/// characters.
fn tool_use_id(params: &Map<String, Value>) -> Option<String> {
    let id = params.get("_meta")?.get("claudecode/toolUseId")?.as_str()?;
    (!id.is_empty() && id.len() <= MAX_TOOL_USE_ID_BYTES && !id.chars().any(char::is_control))
        .then(|| id.to_owned())
}

fn encode(value: &Value) -> Result<Vec<u8>, McpError> {
    let mut line = serde_json::to_vec(value).map_err(|_| McpError::Encode)?;
    line.push(b'\n');
    Ok(line)
}

fn result_reply(key: &RequestKey, result: Value) -> Result<Vec<u8>, McpError> {
    encode(&json!({ "jsonrpc": "2.0", "id": key.to_value(), "result": result }))
}

fn error_reply(key: &RequestKey, code: i64, message: &str) -> Result<Vec<u8>, McpError> {
    encode(&json!({
        "jsonrpc": "2.0",
        "id": key.to_value(),
        "error": { "code": code, "message": message },
    }))
}

#[cfg(test)]
mod tests;
