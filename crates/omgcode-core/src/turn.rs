//! Session-owned, bounded request and tool-call lifecycle.
//!
//! The runtime owns one open session journal and opens one control-store
//! writer for each tool operation. It refreshes a live session binding before
//! every broker call. It dispatches the read and directory-list broker calls,
//! one approval-gated exact edit through checkpointed apply (ADR-0043), and one
//! approval-gated command through the trusted runner (ADR-0040). Settings
//! never authorize an edit or a command by themselves; each needs a trusted
//! single-use approval bound to the live request. No network tool exists.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

use crate::control::{
    ApprovalBinding, ApprovalDeclaration, ApprovalOperation, ApprovalState, ControlError,
    ControlStore, now_unix, valid_profile_name,
};
use crate::files::{FileBroker, FileError, FileListing, FileTarget};
use crate::policy::{DecisionReason, PermissionSet, WorkspaceId};
use crate::prompt::{
    self, BASE_PROMPT, BASE_PROMPT_VERSION, ConversationMessage, ConversationRole, ModelMessage,
    PromptError,
};
use crate::replacement::{CheckpointError, RecordedArtifact};
use crate::session::{
    MAX_MESSAGE_BYTES, MessageRole, NativePath, RecordedMessage, RequestManifest, SessionError,
    SessionJournal, ToolOutcome, sha256_hex,
};

/// Revision of the tool schemas advertised by this runtime.
///
/// Version `0.3.0` adds `workspace_edit_file`. Journals record the version
/// that was advertised; earlier versions replay unchanged.
pub const TOOL_SCHEMA_VERSION: &str = "0.3.0";

/// Maximum model requests in one turn, including the text-only final request.
pub const MAX_TOOL_ROUNDS: usize = 4;
/// Maximum tool calls accepted from one model response.
pub const MAX_TOOL_CALLS_PER_ROUND: usize = 8;
/// Maximum tool calls dispatched across one turn.
pub const MAX_TOOL_CALLS: usize = 16;
/// Maximum bytes in one source-labelled tool message.
pub const MAX_TOOL_RESULT_BYTES: usize = MAX_MESSAGE_BYTES;
/// Maximum file size accepted by one read call.
pub const MAX_READ_BYTES: usize = 15 * 1024;
/// Default file-size limit for one read call.
pub const DEFAULT_READ_BYTES: usize = 8 * 1024;
/// Maximum aggregate tool-message bytes across one turn.
pub const MAX_TOTAL_TOOL_BYTES: u64 = 256 * 1024;
/// Maximum call-identifier bytes accepted from a model response.
pub const MAX_TOOL_CALL_ID_BYTES: usize = 128;
/// Maximum argument JSON bytes accepted from a model response.
pub const MAX_TOOL_ARGUMENT_BYTES: usize = 4 * 1024;
/// Maximum listing entries requested by a model call.
pub const MAX_LIST_ENTRIES: usize = 512;
/// Default number of listing entries returned to the model.
pub const DEFAULT_LIST_ENTRIES: usize = 128;
/// Hard upper bound for a caller-selected turn deadline.
pub const MAX_TURN_DEADLINE: Duration = Duration::from_secs(300);
/// Most arguments accepted by one model command call.
pub const MAX_COMMAND_ARGUMENTS: usize = 64;
/// Longest one model command argument, in bytes.
pub const MAX_COMMAND_ARGUMENT_BYTES: usize = 1024;
/// Longest total model command arguments, in bytes.
pub const MAX_COMMAND_TOTAL_BYTES: usize = 16 * 1024;
/// Longest model command program, in bytes.
pub const MAX_COMMAND_PROGRAM_BYTES: usize = 1024;
/// Deadline for one model command run.
pub const COMMAND_DEADLINE: Duration = Duration::from_secs(20);
/// Retained bytes from each model command output stream.
pub const COMMAND_OUTPUT_BYTES: usize = 4 * 1024;
/// Approval lifetime requested by a model command call.
pub const COMMAND_APPROVAL_TTL_SECS: u64 = 900;
/// Largest current or proposed file accepted by one model edit call. It
/// matches the broker's read bound and the checkpointed apply bound.
pub const MAX_EDIT_FILE_BYTES: usize = 1024 * 1024;
/// Approval lifetime requested by a model edit call.
pub const EDIT_APPROVAL_TTL_SECS: u64 = 900;
/// Longest operator-facing edit approval summary, in bytes.
pub const MAX_EDIT_SUMMARY_BYTES: usize = 180;
/// Domain tag that separates edit approval digests from every other digest.
const EDIT_DIGEST_DOMAIN: &[u8] = b"omgcode-file-edit-v1";

/// A model interface which can return text and raw tool-call values.
///
/// The runtime validates every tool-call value itself. A real provider adapter
/// must map its wire response into this interface without promoting tool calls
/// into execution or trusting provider metadata.
pub trait ToolModel {
    /// Return one bounded assistant reply for the supplied messages and tools.
    fn complete(&mut self, request: ToolModelRequest<'_>) -> Result<ToolModelReply, ModelError>;
}

/// The exact messages and tool definitions sent in one model request.
pub struct ToolModelRequest<'a> {
    /// The compiled base prompt followed by bounded conversation and tool text.
    pub messages: &'a [ModelMessage],
    /// The exact tool definitions advertised for this request.
    pub tools: &'a [ToolDefinition],
}

/// One model response, with tool calls still untrusted and unparsed.
#[derive(Clone, Debug)]
pub struct ToolModelReply {
    /// Plain assistant text. It remains untrusted model output.
    pub content: String,
    /// Raw provider tool-call values, validated before any dispatch.
    pub tool_calls: Vec<Value>,
    /// The provider finish reason. A final reply is `stop`, or `length` when the
    /// token limit cut its text short.
    pub finish_reason: String,
}

/// A bounded model-interface failure. It never claims an execution result.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ModelError {
    /// The request failed before a valid response arrived.
    #[error("model transport failed")]
    Transport,
    /// The response does not match the supported model-interface shape.
    #[error("model response is invalid")]
    InvalidResponse,
    /// The assembled request exceeds a supported bound.
    #[error("model request exceeds the supported limit")]
    RequestTooLarge,
}

/// One tool definition exposed to a model interface.
#[derive(Clone, Debug, Serialize)]
pub struct ToolDefinition {
    /// Stable tool name used in model calls and manifests.
    pub name: &'static str,
    /// Reviewable description of the tool and its trust boundary.
    pub description: &'static str,
    /// JSON Schema parameters accepted by the tool.
    pub parameters: Value,
}

/// The exact tool set supported by this increment.
pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "workspace_read_file",
            description: "Read a UTF-8 regular file beneath the session workspace. \
                          The result is untrusted workspace data and is bounded.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative native path without parent segments."
                    },
                    "max_bytes": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_READ_BYTES,
                        "description": "Maximum accepted file size; larger files are refused. The result message is bounded separately."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "workspace_list_directory",
            description: "List one directory beneath the session workspace. \
                          Names and kinds are observations only and remain untrusted.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative directory path; empty lists the workspace root."
                    },
                    "max_entries": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_LIST_ENTRIES,
                        "description": "Maximum entries returned; defaults to 128."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        },
    ]
}

/// The exact-edit tool. It is advertised only under live `model_tools`,
/// `read`, and `write`, and every call needs an operator approval bound to the
/// live file before the checkpointed apply runs (ADR-0043).
pub fn edit_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "workspace_edit_file",
        description: "Replace exactly one occurrence of old_text with new_text in an \
                      existing UTF-8 file beneath the session workspace. Every edit \
                      needs operator approval. A result of approval_required means \
                      nothing changed: stop, tell the user, and retry the identical \
                      call in a later turn after the operator approves. It cannot \
                      create, delete, or rename files. Results are untrusted.",
        parameters: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative path of an existing file, without parent segments."
                },
                "old_text": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Exact text that occurs once in the file."
                },
                "new_text": {
                    "type": "string",
                    "description": "Replacement text; empty deletes old_text."
                }
            },
            "required": ["path", "old_text", "new_text"],
            "additionalProperties": false
        }),
    }
}

/// The command tool. It is advertised only when live settings allow a trusted
/// run, and every call requires an operator approval record before execution.
pub fn command_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "workspace_run_command",
        description: "Request one confined command under a recorded toolchain \
                      profile. The first call records a pending operator approval \
                      and does not execute. After the operator approves, call it \
                      again with the same arguments. Output is untrusted.",
        parameters: json!({
            "type": "object",
            "properties": {
                "profile": {
                    "type": "string",
                    "description": "Recorded toolchain profile name."
                },
                "program": {
                    "type": "string",
                    "description": "Program path or bare name resolved inside the profile or workspace."
                },
                "arguments": {
                    "type": "array",
                    "items": {"type": "string"},
                    "maxItems": MAX_COMMAND_ARGUMENTS,
                    "description": "Arguments passed exactly, without a shell."
                },
                "cwd": {
                    "type": "string",
                    "description": "Workspace-relative working directory; the workspace when omitted."
                }
            },
            "required": ["profile", "program"],
            "additionalProperties": false
        }),
    }
}

/// A supported tool name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolName {
    /// Read one regular file.
    ReadFile,
    /// List one directory.
    ListDirectory,
    /// Request or apply one exact edit of an existing file.
    EditFile,
    /// Request or run one confined command.
    Command,
}

impl ToolName {
    /// Return the stable wire name for this tool.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadFile => "workspace_read_file",
            Self::ListDirectory => "workspace_list_directory",
            Self::EditFile => "workspace_edit_file",
            Self::Command => "workspace_run_command",
        }
    }
}

/// Validated arguments for one supported tool call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolArguments {
    /// Read a workspace-relative file with a bounded byte limit.
    ReadFile {
        /// Validated workspace-relative path.
        path: String,
        /// Positive byte limit.
        max_bytes: usize,
    },
    /// List a workspace-relative directory with a bounded entry count.
    ListDirectory {
        /// Validated workspace-relative path; empty denotes the workspace root.
        path: String,
        /// Positive entry limit.
        max_entries: usize,
    },
    /// Replace exactly one occurrence of `old_text` in an existing file.
    EditFile {
        /// Validated workspace-relative path.
        path: String,
        /// Non-empty text that must occur exactly once. Untrusted model data.
        old_text: String,
        /// Replacement text, possibly empty. Untrusted model data.
        new_text: String,
    },
    /// Request or run one confined command.
    Command {
        /// Recorded toolchain profile name.
        profile: String,
        /// Program path or bare name.
        program: String,
        /// Exact arguments.
        arguments: Vec<String>,
        /// Workspace-relative working directory.
        cwd: Option<String>,
    },
}

/// One strictly validated model tool call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolCall {
    /// Provider call identifier, unique within the turn.
    pub call_id: String,
    /// Supported tool name.
    pub name: ToolName,
    /// Validated typed arguments.
    pub arguments: ToolArguments,
    arguments_json: String,
}

impl ToolCall {
    /// Parse one provider tool-call value without granting any authority.
    ///
    /// The accepted shape is an OpenAI-style function call with exactly `id`,
    /// `type`, and `function`, where `function` has exactly `name` and a
    /// JSON-object `arguments` string. Unknown fields, duplicate keys in the
    /// arguments string, unsupported tools, invalid paths, and out-of-range
    /// limits are rejected. Duplicate outer keys cannot be detected after a
    /// caller converts the call to `Value`; adapters must reject them before
    /// conversion.
    pub fn parse(value: &Value) -> Result<Self, ToolCallError> {
        let object = value.as_object().ok_or(ToolCallError::NotObject)?;
        if object.len() != 3
            || !object.contains_key("id")
            || !object.contains_key("type")
            || !object.contains_key("function")
        {
            return Err(ToolCallError::UnsupportedShape);
        }
        let call_id = object
            .get("id")
            .and_then(Value::as_str)
            .ok_or(ToolCallError::InvalidCallId)?;
        if call_id.is_empty()
            || call_id.len() > MAX_TOOL_CALL_ID_BYTES
            || call_id.chars().any(char::is_control)
        {
            return Err(ToolCallError::InvalidCallId);
        }
        if object.get("type").and_then(Value::as_str) != Some("function") {
            return Err(ToolCallError::UnsupportedShape);
        }
        let function = object
            .get("function")
            .and_then(Value::as_object)
            .ok_or(ToolCallError::UnsupportedShape)?;
        if function.len() != 2
            || !function.contains_key("name")
            || !function.contains_key("arguments")
        {
            return Err(ToolCallError::UnsupportedShape);
        }
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .ok_or(ToolCallError::UnsupportedTool)?;
        let name = match name {
            "workspace_read_file" => ToolName::ReadFile,
            "workspace_list_directory" => ToolName::ListDirectory,
            "workspace_edit_file" => ToolName::EditFile,
            "workspace_run_command" => ToolName::Command,
            _ => return Err(ToolCallError::UnsupportedTool),
        };
        let arguments_json = function
            .get("arguments")
            .and_then(Value::as_str)
            .ok_or(ToolCallError::InvalidArguments)?;
        if arguments_json.is_empty() || arguments_json.len() > MAX_TOOL_ARGUMENT_BYTES {
            return Err(ToolCallError::InvalidArguments);
        }
        let arguments_value = crate::storage::parse_strict_json(arguments_json.as_bytes())
            .map_err(|_| ToolCallError::InvalidArguments)?;
        let arguments_object = arguments_value
            .as_object()
            .ok_or(ToolCallError::InvalidArguments)?;
        let arguments = match name {
            ToolName::ReadFile => {
                let mut path = None;
                let mut max_bytes = DEFAULT_READ_BYTES;
                for (key, value) in arguments_object {
                    match key.as_str() {
                        "path" => {
                            path = Some(value.as_str().ok_or(ToolCallError::InvalidArguments)?);
                        }
                        "max_bytes" => {
                            let number = value.as_u64().ok_or(ToolCallError::InvalidArguments)?;
                            let number = usize::try_from(number)
                                .map_err(|_| ToolCallError::InvalidArguments)?;
                            if !(1..=MAX_READ_BYTES).contains(&number) {
                                return Err(ToolCallError::InvalidArguments);
                            }
                            max_bytes = number;
                        }
                        _ => return Err(ToolCallError::InvalidArguments),
                    }
                }
                let path = path.ok_or(ToolCallError::InvalidArguments)?;
                validate_relative_path(path, false)?;
                ToolArguments::ReadFile {
                    path: path.to_owned(),
                    max_bytes,
                }
            }
            ToolName::ListDirectory => {
                let mut path = None;
                let mut max_entries = DEFAULT_LIST_ENTRIES;
                for (key, value) in arguments_object {
                    match key.as_str() {
                        "path" => {
                            path = Some(value.as_str().ok_or(ToolCallError::InvalidArguments)?);
                        }
                        "max_entries" => {
                            let number = value.as_u64().ok_or(ToolCallError::InvalidArguments)?;
                            let number = usize::try_from(number)
                                .map_err(|_| ToolCallError::InvalidArguments)?;
                            if !(1..=MAX_LIST_ENTRIES).contains(&number) {
                                return Err(ToolCallError::InvalidArguments);
                            }
                            max_entries = number;
                        }
                        _ => return Err(ToolCallError::InvalidArguments),
                    }
                }
                let path = path.ok_or(ToolCallError::InvalidArguments)?;
                validate_relative_path(path, true)?;
                ToolArguments::ListDirectory {
                    path: path.to_owned(),
                    max_entries,
                }
            }
            ToolName::EditFile => {
                let mut path = None;
                let mut old_text = None;
                let mut new_text = None;
                for (key, value) in arguments_object {
                    let text = value.as_str().ok_or(ToolCallError::InvalidArguments)?;
                    match key.as_str() {
                        "path" => path = Some(text),
                        "old_text" => old_text = Some(text),
                        "new_text" => new_text = Some(text),
                        _ => return Err(ToolCallError::InvalidArguments),
                    }
                }
                let path = path.ok_or(ToolCallError::InvalidArguments)?;
                let old_text = old_text.ok_or(ToolCallError::InvalidArguments)?;
                let new_text = new_text.ok_or(ToolCallError::InvalidArguments)?;
                if old_text.is_empty() {
                    return Err(ToolCallError::InvalidArguments);
                }
                validate_relative_path(path, false)?;
                ToolArguments::EditFile {
                    path: path.to_owned(),
                    old_text: old_text.to_owned(),
                    new_text: new_text.to_owned(),
                }
            }
            ToolName::Command => {
                let mut profile = None;
                let mut program = None;
                let mut arguments = Vec::new();
                let mut cwd = None;
                for (key, value) in arguments_object {
                    match key.as_str() {
                        "profile" => {
                            profile = Some(value.as_str().ok_or(ToolCallError::InvalidArguments)?);
                        }
                        "program" => {
                            program = Some(value.as_str().ok_or(ToolCallError::InvalidArguments)?);
                        }
                        "arguments" => {
                            let values = value.as_array().ok_or(ToolCallError::InvalidArguments)?;
                            if values.len() > MAX_COMMAND_ARGUMENTS {
                                return Err(ToolCallError::InvalidArguments);
                            }
                            let mut total = 0usize;
                            for argument in values {
                                let argument =
                                    argument.as_str().ok_or(ToolCallError::InvalidArguments)?;
                                if argument.len() > MAX_COMMAND_ARGUMENT_BYTES
                                    || argument.contains('\0')
                                {
                                    return Err(ToolCallError::InvalidArguments);
                                }
                                total = total
                                    .checked_add(argument.len())
                                    .filter(|total| *total <= MAX_COMMAND_TOTAL_BYTES)
                                    .ok_or(ToolCallError::InvalidArguments)?;
                                arguments.push(argument.to_owned());
                            }
                        }
                        "cwd" => {
                            cwd = Some(value.as_str().ok_or(ToolCallError::InvalidArguments)?);
                        }
                        _ => return Err(ToolCallError::InvalidArguments),
                    }
                }
                let profile = profile.ok_or(ToolCallError::InvalidArguments)?;
                let program = program.ok_or(ToolCallError::InvalidArguments)?;
                if !valid_profile_name(profile)
                    || program.is_empty()
                    || program.len() > MAX_COMMAND_PROGRAM_BYTES
                    || program.contains('\0')
                    || program.chars().any(char::is_control)
                {
                    return Err(ToolCallError::InvalidArguments);
                }
                let cwd = match cwd {
                    Some("") => None,
                    Some(path) => {
                        validate_relative_path(path, true)?;
                        Some(path.to_owned())
                    }
                    None => None,
                };
                ToolArguments::Command {
                    profile: profile.to_owned(),
                    program: program.to_owned(),
                    arguments,
                    cwd,
                }
            }
        };
        Ok(Self {
            call_id: call_id.to_owned(),
            name,
            arguments,
            arguments_json: arguments_json.to_owned(),
        })
    }

    /// Return the validated workspace-relative path used for dispatch.
    ///
    /// A command call has no single path; it returns an empty string and the
    /// dispatcher uses its typed arguments.
    pub fn path(&self) -> &str {
        match &self.arguments {
            ToolArguments::ReadFile { path, .. }
            | ToolArguments::ListDirectory { path, .. }
            | ToolArguments::EditFile { path, .. } => path,
            ToolArguments::Command { .. } => "",
        }
    }

    /// Return the exact arguments JSON recorded with the request intent.
    pub fn arguments_json(&self) -> &str {
        &self.arguments_json
    }
}

/// A rejected tool-call value. Messages contain no caller-supplied text.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ToolCallError {
    /// The call value is not a JSON object.
    #[error("tool call is not a JSON object")]
    NotObject,
    /// The call does not have the supported function-call shape.
    #[error("tool call has an unsupported shape")]
    UnsupportedShape,
    /// The call identifier is missing, empty, or too large.
    #[error("tool call identifier is invalid")]
    InvalidCallId,
    /// The tool name is not supported by this runtime.
    #[error("tool name is unsupported")]
    UnsupportedTool,
    /// Two calls in one response used the same identifier.
    #[error("tool call identifier is duplicated")]
    DuplicateCallId,
    /// The arguments are not a bounded strict JSON object.
    #[error("tool arguments are invalid")]
    InvalidArguments,
    /// The target path is absolute, empty where required, or contains a parent segment.
    #[error("tool path is invalid")]
    InvalidPath,
}

/// Cooperative cancellation for one turn.
///
/// Cancellation is checked between bounded operations. It cannot preempt a
/// broker call already in progress; that operation may still complete and is
/// recorded in the file-operation journal.
#[derive(Clone, Debug)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
}

impl CancelToken {
    /// Create an uncancelled token.
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Request cancellation at the next runtime checkpoint.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Return whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Bounded limits for one turn. Values are validated when the runtime is built.
#[derive(Clone, Copy, Debug)]
pub struct TurnLimits {
    /// Maximum model requests, including the final text-only request.
    pub max_rounds: usize,
    /// Maximum tool calls in one model response.
    pub max_calls_per_round: usize,
    /// Maximum tool calls across the turn.
    pub max_tool_calls: usize,
    /// Maximum bytes in one source-labelled tool message.
    pub max_result_bytes: usize,
    /// Maximum aggregate tool-message bytes across the turn.
    pub max_total_tool_bytes: u64,
    /// Cooperative turn deadline checked between operations.
    pub deadline: Duration,
}

impl Default for TurnLimits {
    fn default() -> Self {
        Self {
            max_rounds: MAX_TOOL_ROUNDS,
            max_calls_per_round: MAX_TOOL_CALLS_PER_ROUND,
            max_tool_calls: MAX_TOOL_CALLS,
            max_result_bytes: MAX_TOOL_RESULT_BYTES,
            max_total_tool_bytes: MAX_TOTAL_TOOL_BYTES,
            deadline: Duration::from_secs(30),
        }
    }
}

impl TurnLimits {
    /// Reject impossible or unsupported limit combinations.
    fn validate(self) -> Result<Self, TurnError> {
        if !(1..=MAX_TOOL_ROUNDS).contains(&self.max_rounds)
            || !(1..=MAX_TOOL_CALLS_PER_ROUND).contains(&self.max_calls_per_round)
            || !(1..=MAX_TOOL_CALLS).contains(&self.max_tool_calls)
            || !(1..=MAX_TOOL_RESULT_BYTES).contains(&self.max_result_bytes)
            || self.max_total_tool_bytes < u64::try_from(self.max_result_bytes).unwrap_or(u64::MAX)
            || self.max_total_tool_bytes > MAX_TOTAL_TOOL_BYTES
            || self.deadline > MAX_TURN_DEADLINE
        {
            return Err(TurnError::InvalidLimits);
        }
        Ok(self)
    }
}

/// A bounded turn limit that stopped the request.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum TurnLimit {
    /// The model-round limit was reached.
    #[error("model round limit")]
    Rounds,
    /// A model response contained too many tool calls.
    #[error("tool calls per round limit")]
    CallsPerRound,
    /// The turn's total tool-call limit was reached.
    #[error("total tool call limit")]
    ToolCalls,
    /// The aggregate tool-output limit was reached.
    #[error("total tool output limit")]
    TotalToolBytes,
    /// One rendered tool message exceeded the selected limit.
    #[error("tool result message limit")]
    ResultBytes,
}

/// A controlled turn failure. No variant claims an action succeeded.
#[derive(Debug, Error)]
pub enum TurnError {
    /// The session journal rejected a read or append.
    #[error("session error: {0}")]
    Session(#[from] SessionError),
    /// Current workspace policy or native identity checks failed.
    #[error("workspace policy error: {0}")]
    Control(#[from] ControlError),
    /// Prompt assembly rejected the bounded request.
    #[error("prompt assembly failed: {0}")]
    Prompt(#[from] PromptError),
    /// The model interface failed without producing a usable reply.
    #[error("model request failed: {0}")]
    Model(#[from] ModelError),
    /// A model tool call was malformed or unsupported.
    #[error("invalid tool call: {0}")]
    InvalidToolCall(#[from] ToolCallError),
    /// A bounded turn limit stopped the request.
    #[error("turn limit exceeded: {0}")]
    Limit(TurnLimit),
    /// Cooperative cancellation was requested.
    #[error("turn was cancelled")]
    Cancelled,
    /// The cooperative turn deadline elapsed.
    #[error("turn deadline elapsed")]
    Timeout,
    /// The session is archived and cannot start a turn.
    #[error("session is archived")]
    Archived,
    /// The session already has an active turn.
    #[error("session has an active turn")]
    ActiveTurn,
    /// Tool calls require a schema-version-2 journal.
    #[error("session schema version 2 is required for tool calls")]
    SchemaV2Required,
    /// The selected workspace does not match the journal's descriptive metadata.
    #[error("session workspace metadata does not match the selected workspace")]
    WorkspaceMismatch,
    /// The supplied manifest does not describe the compiled prompt actually sent.
    #[error("request manifest does not match the compiled base prompt")]
    ManifestMismatch,
    /// Current policy or native checks denied the tool operation.
    #[error("tool operation denied: {0}")]
    ToolDenied(#[source] Box<FileError>),
    /// A terminal tool failure stopped the turn without retry.
    #[error("tool operation failed: {0}")]
    ToolFailed(#[source] Box<FileError>),
    /// The turn failed, and its terminal event could not be recorded.
    #[error("turn failure could not be recorded: {record}; original failure: {original}")]
    FailureNotRecorded {
        /// The failed terminal append.
        #[source]
        record: Box<SessionError>,
        /// The failure which the terminal event was intended to record.
        original: Box<TurnError>,
    },
    /// The caller supplied unsupported turn limits.
    #[error("turn limits are invalid")]
    InvalidLimits,
}

/// The durable result of one completed turn.
#[derive(Clone, Debug, Serialize)]
pub struct TurnReport {
    /// Request identifier recorded in the session journal.
    pub request_id: Uuid,
    /// Final assistant text, also recorded as the last assistant message.
    pub reply: String,
    /// Number of model requests made.
    pub rounds: usize,
    /// Number of tool calls dispatched.
    pub tool_calls: usize,
    /// Aggregate source-labelled tool-message bytes.
    pub tool_bytes: u64,
    /// The exact manifest recorded for the request, including tool metadata.
    pub manifest: RequestManifest,
    /// The final reply's provider finish reason: `stop`, or `length` when the
    /// token limit cut the reply short. The turn is still recorded as complete.
    pub finish_reason: String,
    /// Saved tool results replaced by [`WITHHELD_TOOL_RESULT`] because live
    /// settings did not allow model file reads when the turn started.
    pub withheld_tool_results: usize,
}

/// Text sent in place of a saved tool result whose replay is not authorized.
pub const WITHHELD_TOOL_RESULT: &str = "Tool result withheld [source omgcode_runtime]: current workspace settings do not allow model file reads, so this earlier workspace data is not sent again.";

/// Text sent in place of a compaction summary built from saved tool results
/// when live settings do not authorize replaying those results.
pub const WITHHELD_COMPACTION_SUMMARY: &str = "Compacted earlier context withheld [source omgcode_runtime]: the summary was built from earlier workspace tool results, and current workspace settings do not allow model file reads, so it is not sent again.";

/// Map a journal's active context (summary plus retained messages) to history.
///
/// This is the only replay path for a turn. A summary whose input included
/// saved tool results is workspace data under the same rule as those results:
/// without live authority it becomes [`WITHHELD_COMPACTION_SUMMARY`] and counts
/// as withheld. The returned history has one entry per active message.
#[must_use]
pub fn replay_active(
    journal: &SessionJournal,
    tool_results_allowed: bool,
) -> (Vec<ConversationMessage>, usize) {
    let mut active = journal.active_messages();
    let mut withheld = 0_usize;
    let summary_withheld = !tool_results_allowed
        && journal
            .snapshot()
            .compaction
            .as_ref()
            .is_some_and(|record| record.includes_tool_results);
    if summary_withheld && let Some(summary) = active.first_mut() {
        summary.text = WITHHELD_COMPACTION_SUMMARY.to_owned();
        withheld = 1;
    }
    let (history, tool_withheld) = replay_history(&active, tool_results_allowed);
    (history, withheld.saturating_add(tool_withheld))
}

/// Map saved session messages to conversation history.
///
/// A saved tool result is workspace data that was disclosed under an earlier
/// decision. It is replayed only when `tool_results_allowed` reflects live
/// authority; otherwise it becomes [`WITHHELD_TOOL_RESULT`]. User and assistant
/// messages are always replayed. Returns the history and the withheld count.
#[must_use]
pub fn replay_history(
    messages: &[RecordedMessage],
    tool_results_allowed: bool,
) -> (Vec<ConversationMessage>, usize) {
    let mut withheld = 0_usize;
    let history = messages
        .iter()
        .map(|message| match message.role {
            MessageRole::User => ConversationMessage {
                role: ConversationRole::User,
                content: message.text.clone(),
            },
            MessageRole::Assistant => ConversationMessage {
                role: ConversationRole::Assistant,
                content: message.text.clone(),
            },
            // Stored tool text is already source-labelled untrusted data.
            MessageRole::Tool if tool_results_allowed => ConversationMessage {
                role: ConversationRole::User,
                content: message.text.clone(),
            },
            MessageRole::Tool => {
                withheld = withheld.saturating_add(1);
                ConversationMessage {
                    role: ConversationRole::User,
                    content: WITHHELD_TOOL_RESULT.to_owned(),
                }
            }
        })
        .collect();
    (history, withheld)
}

/// A session-owned turn runtime.
///
/// The runtime borrows an exclusively locked journal and the trusted
/// application-data directory. It opens the control store only for the duration
/// of each tool operation, creates a fresh binding, and releases the settings
/// lock before the next model request. It never derives authority from journal
/// metadata or a previous decision.
pub struct TurnRuntime<'a> {
    journal: &'a mut SessionJournal,
    data_directory: &'a Path,
    workspace_id: WorkspaceId,
    limits: TurnLimits,
    cancel: CancelToken,
    tool_text_filter: Option<&'a dyn Fn(&str) -> String>,
}

impl<'a> TurnRuntime<'a> {
    /// Build a runtime for one open journal and selected workspace.
    ///
    /// The caller supplies the application-data directory and workspace ID from
    /// trusted current settings. The journal's recorded ID is checked only to
    /// keep the transcript association coherent; it is not an access grant.
    pub fn new(
        journal: &'a mut SessionJournal,
        data_directory: &'a Path,
        workspace_id: WorkspaceId,
        limits: TurnLimits,
    ) -> Result<Self, TurnError> {
        let limits = limits.validate()?;
        if journal.schema_version() < 2 {
            return Err(TurnError::SchemaV2Required);
        }
        if journal.snapshot().workspace_id != *workspace_id.as_uuid() {
            return Err(TurnError::WorkspaceMismatch);
        }
        Ok(Self {
            journal,
            data_directory,
            workspace_id,
            limits,
            cancel: CancelToken::new(),
            tool_text_filter: None,
        })
    }

    /// Transform each tool result before it is limited, recorded, or sent.
    ///
    /// The runtime itself redacts nothing. A caller that knows a secret, such
    /// as a provider token, supplies a filter which removes it. The filtered
    /// text is the only form that reaches the journal and the model; its byte
    /// count and digest are taken after filtering.
    #[must_use]
    pub fn with_tool_text_filter(mut self, filter: &'a dyn Fn(&str) -> String) -> Self {
        self.tool_text_filter = Some(filter);
        self
    }

    /// Return a token which requests cancellation at the next checkpoint.
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Run one bounded model/tool turn and record its terminal state.
    ///
    /// Deterministic failures before the turn marker return without mutation.
    /// Once the turn starts, every failure attempts to append a terminal event;
    /// if that append also fails, the caller receives `FailureNotRecorded`.
    pub fn run<M: ToolModel>(
        &mut self,
        model: &mut M,
        user_text: &str,
        custom_instructions: &str,
        mut manifest: RequestManifest,
    ) -> Result<TurnReport, TurnError> {
        if manifest.base_prompt != BASE_PROMPT
            || manifest.prompt_version != BASE_PROMPT_VERSION
            || manifest.prompt_sha256 != prompt::base_prompt_sha256()
        {
            return Err(TurnError::ManifestMismatch);
        }
        self.ensure_runnable()?;
        let deadline = Instant::now()
            .checked_add(self.limits.deadline)
            .ok_or(TurnError::Timeout)?;
        self.check_checkpoints(deadline)?;

        // One settings snapshot selects the advertised tools; every call
        // rechecks the live preferences before dispatch.
        let permissions = self.workspace_permissions();
        let mut tools = tool_definitions();
        if permissions.is_some_and(edit_tool_allowed) {
            tools.push(edit_tool_definition());
        }
        if permissions.is_some_and(command_tool_allowed) {
            tools.push(command_tool_definition());
        }
        // The runtime owns the exact instruction and tool metadata it sends.
        manifest.custom_instructions = custom_instructions.to_owned();
        manifest.tool_schema_version = Some(TOOL_SCHEMA_VERSION.to_owned());
        manifest.tools = Some(
            tools
                .iter()
                .map(|definition| definition.name.to_owned())
                .collect(),
        );

        // Active context is the compaction summary plus retained messages.
        // Saved tool results, and summaries built from them, are replayed only
        // under live read authority.
        let (mut history, withheld_tool_results) =
            replay_active(self.journal, self.tool_replay_allowed());
        history.push(ConversationMessage {
            role: ConversationRole::User,
            content: user_text.to_owned(),
        });

        // Catch deterministic prompt-size failures before creating a turn.
        prompt::assemble(custom_instructions, &history)?;
        self.journal.preflight_request(user_text, &manifest)?;
        let max_result_bytes =
            u64::try_from(self.limits.max_result_bytes).map_err(|_| TurnError::InvalidLimits)?;
        let request_id = self.journal.start_turn()?;
        if let Err(error) = self.journal.record_message(MessageRole::User, user_text) {
            return Err(self.abort(request_id, "persistence", error.into()));
        }
        if let Err(error) = self.journal.record_manifest(request_id, manifest.clone()) {
            return Err(self.abort(request_id, "persistence", error.into()));
        }

        let mut rounds = 0_usize;
        let mut total_calls = 0_usize;
        let mut tool_bytes = 0_u64;
        let mut turn_ids = HashSet::new();

        loop {
            if let Err(error) = self.check_checkpoints(deadline) {
                return Err(self.abort_for(request_id, error));
            }
            if rounds >= self.limits.max_rounds {
                let error = TurnError::Limit(TurnLimit::Rounds);
                return Err(self.abort_for(request_id, error));
            }
            rounds = match rounds.checked_add(1) {
                Some(value) => value,
                None => return Err(self.abort_for(request_id, TurnError::InvalidLimits)),
            };

            let messages = match prompt::assemble(custom_instructions, &history) {
                Ok(messages) => messages,
                Err(error) => {
                    return Err(self.abort(request_id, "prompt_limit", error.into()));
                }
            };
            let reply = match model.complete(ToolModelRequest {
                messages: &messages,
                tools: &tools,
            }) {
                Ok(reply) => reply,
                Err(error) => {
                    return Err(self.abort(request_id, "model_error", error.into()));
                }
            };
            if let Err(error) = self.check_checkpoints(deadline) {
                return Err(self.abort_for(request_id, error));
            }
            if reply.content.len() > MAX_MESSAGE_BYTES {
                let error = TurnError::Session(SessionError::InvalidValue(
                    "assistant reply exceeds 16 KiB",
                ));
                return Err(self.abort(request_id, "response_too_large", error));
            }

            if reply.tool_calls.is_empty() {
                if let Err(error) = self
                    .journal
                    .record_message(MessageRole::Assistant, &reply.content)
                {
                    return Err(self.abort(request_id, "persistence", error.into()));
                }
                if let Err(error) = self.journal.complete_turn(request_id) {
                    return Err(self.abort(request_id, "persistence", error.into()));
                }
                return Ok(TurnReport {
                    request_id,
                    reply: reply.content,
                    rounds,
                    tool_calls: total_calls,
                    tool_bytes,
                    manifest,
                    finish_reason: reply.finish_reason,
                    withheld_tool_results,
                });
            }

            if reply.tool_calls.len() > self.limits.max_calls_per_round {
                let error = TurnError::Limit(TurnLimit::CallsPerRound);
                return Err(self.abort_for(request_id, error));
            }
            total_calls = match total_calls.checked_add(reply.tool_calls.len()) {
                Some(total) => total,
                None => return Err(self.abort_for(request_id, TurnError::InvalidLimits)),
            };
            if total_calls > self.limits.max_tool_calls {
                let error = TurnError::Limit(TurnLimit::ToolCalls);
                return Err(self.abort_for(request_id, error));
            }

            if !reply.content.is_empty() {
                if let Err(error) = self
                    .journal
                    .record_message(MessageRole::Assistant, &reply.content)
                {
                    return Err(self.abort(request_id, "persistence", error.into()));
                }
                history.push(ConversationMessage {
                    role: ConversationRole::Assistant,
                    content: reply.content.clone(),
                });
            }

            let mut calls = Vec::with_capacity(reply.tool_calls.len());
            for value in &reply.tool_calls {
                let call = match ToolCall::parse(value) {
                    Ok(call) => call,
                    Err(error) => {
                        return Err(self.abort(
                            request_id,
                            "invalid_tool_call",
                            TurnError::InvalidToolCall(error),
                        ));
                    }
                };
                if !turn_ids.insert(call.call_id.clone()) {
                    return Err(self.abort(
                        request_id,
                        "invalid_tool_call",
                        TurnError::InvalidToolCall(ToolCallError::DuplicateCallId),
                    ));
                }
                calls.push(call);
            }

            if rounds >= self.limits.max_rounds {
                let error = TurnError::Limit(TurnLimit::Rounds);
                return Err(self.abort_for(request_id, error));
            }

            for call in &calls {
                if let Err(error) = self.check_checkpoints(deadline) {
                    return Err(self.abort_for(request_id, error));
                }
                // Reserve the largest possible tool message against the
                // assembled-prompt budget before recording an intent or
                // dispatching the operation.
                history.push(ConversationMessage {
                    role: ConversationRole::User,
                    content: "x".repeat(self.limits.max_result_bytes),
                });
                let prompt_check = prompt::assemble(custom_instructions, &history);
                history.pop();
                if let Err(error) = prompt_check {
                    return Err(self.abort_for(request_id, TurnError::Prompt(error)));
                }
                if tool_bytes
                    .checked_add(max_result_bytes)
                    .is_none_or(|projected| projected > self.limits.max_total_tool_bytes)
                {
                    let error = TurnError::Limit(TurnLimit::TotalToolBytes);
                    return Err(self.abort_for(request_id, error));
                }
                if let Err(error) = self.journal.record_tool_request(
                    request_id,
                    &call.call_id,
                    call.name.as_str(),
                    call.arguments_json(),
                ) {
                    return Err(self.abort(request_id, "persistence", error.into()));
                }
                let mut execution = WorkspaceToolDispatcher {
                    data_directory: self.data_directory,
                    session_id: self.journal.snapshot().session_id,
                    workspace_id: self.workspace_id,
                    cancel: self.cancel.clone(),
                }
                .dispatch(call, self.limits.max_result_bytes);
                if let Some(filter) = self.tool_text_filter {
                    execution.text = filter(&execution.text);
                }
                let message_limit = self.limits.max_result_bytes.min(MAX_MESSAGE_BYTES);
                if execution.text.len() > message_limit {
                    let error = TurnError::Limit(TurnLimit::ResultBytes);
                    return Err(self.abort_for(request_id, error));
                }
                let text_bytes = match u64::try_from(execution.text.len()) {
                    Ok(bytes) => bytes,
                    Err(_) => return Err(self.abort_for(request_id, TurnError::InvalidLimits)),
                };
                let digest = sha256_hex(execution.text.as_bytes());
                if let Err(error) = self.journal.record_tool_result(
                    request_id,
                    &call.call_id,
                    execution.outcome,
                    execution.code,
                    text_bytes,
                    &digest,
                ) {
                    return Err(self.abort(request_id, "persistence", error.into()));
                }
                if let Err(error) = self
                    .journal
                    .record_tool_message(&call.call_id, &execution.text)
                {
                    return Err(self.abort(request_id, "persistence", error.into()));
                }
                history.push(ConversationMessage {
                    role: ConversationRole::User,
                    content: execution.text.clone(),
                });
                tool_bytes = match tool_bytes.checked_add(text_bytes) {
                    Some(total) => total,
                    None => return Err(self.abort_for(request_id, TurnError::InvalidLimits)),
                };
                if tool_bytes > self.limits.max_total_tool_bytes {
                    let error = TurnError::Limit(TurnLimit::TotalToolBytes);
                    return Err(self.abort_for(request_id, error));
                }
                match execution.terminal {
                    Some(TerminalToolError::Denied(error)) => {
                        return Err(self.abort(
                            request_id,
                            "tool_denied",
                            TurnError::ToolDenied(Box::new(error)),
                        ));
                    }
                    Some(TerminalToolError::Failed(error)) => {
                        return Err(self.abort(
                            request_id,
                            "tool_failed",
                            TurnError::ToolFailed(Box::new(error)),
                        ));
                    }
                    None => {}
                }
            }
        }
    }

    /// Refuse a turn before any mutation when the session cannot accept one.
    fn ensure_runnable(&self) -> Result<(), TurnError> {
        if self.journal.schema_version() < 2 {
            return Err(TurnError::SchemaV2Required);
        }
        if self.journal.snapshot().workspace_id != *self.workspace_id.as_uuid() {
            return Err(TurnError::WorkspaceMismatch);
        }
        if self.journal.snapshot().archived {
            return Err(TurnError::Archived);
        }
        if self.journal.snapshot().active_turn.is_some() {
            return Err(TurnError::ActiveTurn);
        }
        Ok(())
    }

    /// Report whether live settings let the model see saved tool results.
    ///
    /// It needs the session workspace's `model_tools` and `read` preferences.
    /// Any settings failure withholds them. The settings lock is released
    /// before the first model request.
    fn tool_replay_allowed(&self) -> bool {
        self.workspace_permissions()
            .is_some_and(|permissions| permissions.model_tools && permissions.read)
    }

    /// Read the workspace's current preferences, releasing the settings lock.
    fn workspace_permissions(&self) -> Option<PermissionSet> {
        match ControlStore::open_existing(self.data_directory) {
            Ok(Some(control)) => control
                .workspace(self.workspace_id)
                .map(|workspace| workspace.permissions()),
            Ok(None) | Err(_) => None,
        }
    }

    /// Check cooperative cancellation and the turn deadline.
    fn check_checkpoints(&self, deadline: Instant) -> Result<(), TurnError> {
        if self.cancel.is_cancelled() {
            return Err(TurnError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(TurnError::Timeout);
        }
        Ok(())
    }

    /// Append a terminal failure event and preserve a recording failure.
    fn abort(&mut self, request_id: Uuid, code: &'static str, error: TurnError) -> TurnError {
        match self.journal.fail_turn(request_id, code) {
            Ok(()) => error,
            Err(record_error) => TurnError::FailureNotRecorded {
                record: Box::new(record_error),
                original: Box::new(error),
            },
        }
    }

    /// Append a terminal event using the stable code for a runtime failure.
    fn abort_for(&mut self, request_id: Uuid, error: TurnError) -> TurnError {
        let code = match &error {
            TurnError::Cancelled => "cancelled",
            TurnError::Timeout => "timeout",
            TurnError::Limit(_) => "tool_limit",
            TurnError::InvalidToolCall(_) => "invalid_tool_call",
            TurnError::Model(_) => "model_error",
            TurnError::Prompt(_) => "prompt_limit",
            TurnError::ToolDenied(_) => "tool_denied",
            TurnError::ToolFailed(_) => "tool_failed",
            TurnError::Session(_) => "persistence",
            _ => "turn_failed",
        };
        self.abort(request_id, code, error)
    }
}

/// Report whether the edit tool may be offered: live `model_tools`, `read`,
/// and `write`. Each call rechecks them, and every edit still needs approval.
const fn edit_tool_allowed(permissions: PermissionSet) -> bool {
    permissions.model_tools && permissions.read && permissions.write
}

/// Report whether the command tool may be offered: live `model_tools`,
/// `execute`, `read`, and `write`. Each call rechecks them.
const fn command_tool_allowed(permissions: PermissionSet) -> bool {
    permissions.model_tools && permissions.execute && permissions.read && permissions.write
}

/// A terminal tool failure retained until its audit records are durable.
enum TerminalToolError {
    Denied(FileError),
    Failed(FileError),
}

/// One bounded tool execution result, including the exact text shown to the model.
struct ToolExecution {
    outcome: ToolOutcome,
    code: Option<&'static str>,
    text: String,
    terminal: Option<TerminalToolError>,
}

impl ToolExecution {
    fn success(text: String) -> Self {
        Self {
            outcome: ToolOutcome::Succeeded,
            code: None,
            text,
            terminal: None,
        }
    }

    fn recoverable(code: &'static str, call: &ToolCall) -> Self {
        Self {
            outcome: ToolOutcome::Failed,
            code: Some(code),
            text: labelled_error(call, code),
            terminal: None,
        }
    }

    fn denied(code: &'static str, call: &ToolCall, error: FileError) -> Self {
        Self {
            outcome: ToolOutcome::Denied,
            code: Some(code),
            text: labelled_error(call, code),
            terminal: Some(TerminalToolError::Denied(error)),
        }
    }

    fn failed(code: &'static str, call: &ToolCall, error: FileError) -> Self {
        Self {
            outcome: ToolOutcome::Failed,
            code: Some(code),
            text: labelled_error(call, code),
            terminal: Some(TerminalToolError::Failed(error)),
        }
    }
}

/// Dispatch one validated call through a freshly opened store and the native broker.
struct WorkspaceToolDispatcher<'a> {
    data_directory: &'a Path,
    session_id: Uuid,
    workspace_id: WorkspaceId,
    cancel: CancelToken,
}

impl WorkspaceToolDispatcher<'_> {
    fn dispatch(&mut self, call: &ToolCall, max_result_bytes: usize) -> ToolExecution {
        // Hold the settings lock only for this operation, never across model I/O.
        // A permission change can therefore take effect before the next call.
        let mut control = match ControlStore::open_existing(self.data_directory) {
            Ok(Some(control)) => control,
            Ok(None) => {
                return ToolExecution::denied(
                    "policy_unavailable",
                    call,
                    FileError::Control(ControlError::Invalid("control store is absent")),
                );
            }
            Err(error) => {
                return ToolExecution::denied(
                    "policy_unavailable",
                    call,
                    FileError::Control(error),
                );
            }
        };
        // The persisted model-tool preference is rechecked on every call, so a
        // change made during model I/O stops the next operation.
        let permissions = match control.workspace(self.workspace_id) {
            Some(workspace) => workspace.permissions(),
            None => {
                return ToolExecution::denied(
                    "policy_unavailable",
                    call,
                    FileError::Denied(DecisionReason::PermissionDisabled),
                );
            }
        };
        if !permissions.model_tools {
            return ToolExecution::denied(
                "model_tools_disabled",
                call,
                FileError::Denied(DecisionReason::PermissionDisabled),
            );
        }
        if matches!(call.arguments, ToolArguments::Command { .. }) {
            if !(permissions.execute && permissions.read && permissions.write) {
                return ToolExecution::denied(
                    "command_disabled",
                    call,
                    FileError::Denied(DecisionReason::PermissionDisabled),
                );
            }
            return self.dispatch_command(control, call, max_result_bytes);
        }
        if matches!(call.arguments, ToolArguments::EditFile { .. }) {
            if !edit_tool_allowed(permissions) {
                return ToolExecution::denied(
                    "edit_disabled",
                    call,
                    FileError::Denied(DecisionReason::PermissionDisabled),
                );
            }
            return self.dispatch_edit(control, call, max_result_bytes);
        }
        let binding = match control.bind_session(self.session_id, self.workspace_id) {
            Ok(binding) => binding,
            Err(error) => {
                return ToolExecution::denied(
                    "policy_unavailable",
                    call,
                    FileError::Control(error),
                );
            }
        };
        let target = FileTarget {
            session_id: self.session_id,
            workspace_id: self.workspace_id,
            path: PathBuf::from(call.path()),
        };
        let mut broker = FileBroker::new(&mut control);
        match &call.arguments {
            ToolArguments::ReadFile { max_bytes, .. } => {
                let limit = (*max_bytes).min(max_result_bytes);
                match broker.read(&binding, &target, limit) {
                    Ok(read) => match std::str::from_utf8(&read.bytes) {
                        Ok(text) => read_result(call, text, max_result_bytes),
                        Err(_) => ToolExecution::recoverable("non_utf8", call),
                    },
                    Err(error) => classify_file_error(call, error),
                }
            }
            ToolArguments::ListDirectory { max_entries, .. } => {
                match broker.list(&binding, &target, (*max_entries).min(MAX_LIST_ENTRIES)) {
                    Ok(listing) => list_result(call, listing, max_result_bytes),
                    Err(error) => classify_file_error(call, error),
                }
            }
            ToolArguments::EditFile { .. } => ToolExecution::recoverable("invalid_request", call),
            ToolArguments::Command { .. } => ToolExecution::recoverable("invalid_command", call),
        }
    }

    /// Request or apply one exact edit under one control-store writer.
    ///
    /// The writer is held for the whole bounded call, and the broker read, the
    /// approval append, and the checkpointed apply each open at most one
    /// subordinate journal at a time, keeping the documented lock order. The
    /// expected current value comes from this live audited read, never from the
    /// model or the conversation. An approval is consumed before the apply, so
    /// a failed apply spends it.
    fn dispatch_edit(
        &self,
        mut control: ControlStore,
        call: &ToolCall,
        max_result_bytes: usize,
    ) -> ToolExecution {
        let ToolArguments::EditFile {
            path,
            old_text,
            new_text,
        } = &call.arguments
        else {
            return ToolExecution::recoverable("invalid_request", call);
        };
        let binding = match control.bind_session(self.session_id, self.workspace_id) {
            Ok(binding) => binding,
            Err(error) => {
                return ToolExecution::denied(
                    "policy_unavailable",
                    call,
                    FileError::Control(error),
                );
            }
        };
        let target = FileTarget {
            session_id: self.session_id,
            workspace_id: self.workspace_id,
            path: PathBuf::from(path),
        };
        let current =
            match FileBroker::new(&mut control).read(&binding, &target, MAX_EDIT_FILE_BYTES) {
                Ok(read) => read.bytes,
                Err(error) => return classify_edit_error(call, error),
            };
        let Ok(text) = std::str::from_utf8(&current) else {
            return ToolExecution::recoverable("non_utf8", call);
        };
        let start = match unique_match(text, old_text) {
            TextMatch::None => return ToolExecution::recoverable("edit_no_match", call),
            TextMatch::Many => return ToolExecution::recoverable("edit_ambiguous", call),
            TextMatch::Once(start) => start,
        };
        if old_text == new_text {
            return ToolExecution::recoverable("edit_unchanged", call);
        }
        let Some(proposed) = replace_once(text, start, old_text.len(), new_text) else {
            return ToolExecution::recoverable("edit_too_large", call);
        };
        let (Some(current_artifact), Some(proposed_artifact)) =
            (artifact(&current), artifact(proposed.as_bytes()))
        else {
            return ToolExecution::recoverable("edit_too_large", call);
        };
        let normalized = normalized_path(path);
        let digest = edit_digest(
            self.session_id,
            self.workspace_id,
            &normalized,
            &current_artifact,
            &proposed_artifact,
        );
        let now = now_unix();
        let revision = control.revision();
        let statuses = control.approval_statuses(now);
        let matching = |state| {
            statuses.iter().find(|status| {
                status.session_id == self.session_id
                    && status.workspace_id == self.workspace_id
                    && status.operation == ApprovalOperation::FileWrite
                    && status.request_sha256 == digest
                    && status.settings_revision == revision
                    && status.state == state
            })
        };
        if let Some(approved) = matching(ApprovalState::Approved) {
            let approval = ApprovalBinding {
                session_id: self.session_id,
                workspace_id: self.workspace_id,
                operation: ApprovalOperation::FileWrite,
                request_sha256: digest.clone(),
            };
            if let Err(error) = control.consume_approval(approved.id, &approval, now) {
                return status_result(
                    call,
                    ToolOutcome::Failed,
                    "approval_failed",
                    json!({"error": error.to_string()}),
                    max_result_bytes,
                );
            }
            // The record is spent before the write, so no crash or retry can
            // reuse it. The apply rechecks live read/write, the binding, the
            // qualified filesystem, and that the target still has the exact
            // captured size and digest.
            let applied = FileBroker::new(&mut control).apply_replacement(
                &binding,
                &target,
                &current_artifact,
                proposed.as_bytes(),
            );
            drop(control);
            return match applied {
                Ok(receipt) if receipt.applied => {
                    let body = json!({
                        "tool": call.name.as_str(),
                        "status": "applied",
                        "path": normalized,
                        "bytes": receipt.proposed.bytes,
                        "sha256": receipt.proposed.sha256,
                        "operation_id": receipt.operation_id,
                    });
                    let message = labelled_result(call, &body.to_string());
                    if message.len() > max_result_bytes.min(MAX_MESSAGE_BYTES) {
                        return ToolExecution::recoverable("output_too_large", call);
                    }
                    ToolExecution::success(message)
                }
                // Different proposed bytes always publish or fail; an
                // unapplied receipt can only mean the bytes were identical.
                Ok(_) => ToolExecution::recoverable("edit_unchanged", call),
                Err(error) => classify_edit_error(call, error),
            };
        }
        if let Some(pending) = matching(ApprovalState::Pending) {
            return edit_approval_required(call, pending.id, max_result_bytes);
        }
        if matching(ApprovalState::Denied).is_some() {
            return status_result(
                call,
                ToolOutcome::Denied,
                "approval_denied",
                json!({"message": "The operator denied this edit. Do not retry it."}),
                max_result_bytes,
            );
        }
        let status = match control.request_approval(
            &ApprovalDeclaration {
                session_id: self.session_id,
                workspace_id: self.workspace_id,
                operation: ApprovalOperation::FileWrite,
                request_sha256: digest,
                summary: edit_summary(&normalized, &current_artifact, &proposed_artifact),
                ttl_secs: EDIT_APPROVAL_TTL_SECS,
            },
            now,
        ) {
            Ok(status) => status,
            Err(error) => {
                return status_result(
                    call,
                    ToolOutcome::Failed,
                    "approval_failed",
                    json!({"error": error.to_string()}),
                    max_result_bytes,
                );
            }
        };
        drop(control);
        edit_approval_required(call, status.id, max_result_bytes)
    }

    /// Request or execute one command. The control store is dropped before the
    /// run starts so the runner can take its own settings lock.
    fn dispatch_command(
        &self,
        mut control: ControlStore,
        call: &ToolCall,
        max_result_bytes: usize,
    ) -> ToolExecution {
        let ToolArguments::Command {
            profile,
            program,
            arguments,
            cwd,
        } = &call.arguments
        else {
            return ToolExecution::recoverable("invalid_command", call);
        };
        let request = crate::run::RunRequest {
            workspace_id: self.workspace_id,
            profile: profile.clone(),
            program: PathBuf::from(program),
            arguments: arguments.iter().map(OsString::from).collect(),
            working_directory: cwd.as_ref().map(PathBuf::from),
            host_services: false,
            network: false,
            model_tools: true,
            limits: command_limits(),
        };
        let digest = command_digest(self.session_id, &request);
        let now = now_unix();
        let revision = control.revision();
        let statuses = control.approval_statuses(now);
        let matching = |state| {
            statuses.iter().find(|status| {
                status.session_id == self.session_id
                    && status.workspace_id == self.workspace_id
                    && status.operation == ApprovalOperation::Command
                    && status.request_sha256 == digest
                    && status.settings_revision == revision
                    && status.state == state
            })
        };
        if let Some(approved) = matching(ApprovalState::Approved) {
            let binding = ApprovalBinding {
                session_id: self.session_id,
                workspace_id: self.workspace_id,
                operation: ApprovalOperation::Command,
                request_sha256: digest,
            };
            if let Err(error) = control.consume_approval(approved.id, &binding, now) {
                return status_result(
                    call,
                    ToolOutcome::Failed,
                    "approval_failed",
                    json!({"error": error.to_string()}),
                    max_result_bytes,
                );
            }
            drop(control);
            return self.execute_command(call, &request, max_result_bytes);
        }
        if let Some(pending) = matching(ApprovalState::Pending) {
            return status_result(
                call,
                ToolOutcome::Failed,
                "approval_required",
                json!({
                    "approval_id": pending.id,
                    "message": "The operator must approve this command before it can run.",
                }),
                max_result_bytes,
            );
        }
        if matching(ApprovalState::Denied).is_some() {
            return status_result(
                call,
                ToolOutcome::Denied,
                "approval_denied",
                json!({"message": "The operator denied this command."}),
                max_result_bytes,
            );
        }
        let summary = command_summary(profile, program, arguments.len());
        let status = match control.request_approval(
            &ApprovalDeclaration {
                session_id: self.session_id,
                workspace_id: self.workspace_id,
                operation: ApprovalOperation::Command,
                request_sha256: digest,
                summary,
                ttl_secs: COMMAND_APPROVAL_TTL_SECS,
            },
            now,
        ) {
            Ok(status) => status,
            Err(error) => {
                return status_result(
                    call,
                    ToolOutcome::Failed,
                    "approval_failed",
                    json!({"error": error.to_string()}),
                    max_result_bytes,
                );
            }
        };
        drop(control);
        status_result(
            call,
            ToolOutcome::Failed,
            "approval_required",
            json!({
                "approval_id": status.id,
                "message": "The operator must approve this command before it can run.",
            }),
            max_result_bytes,
        )
    }

    /// Run one approved command with a fixed, bounded request.
    fn execute_command(
        &self,
        call: &ToolCall,
        request: &crate::run::RunRequest,
        max_result_bytes: usize,
    ) -> ToolExecution {
        let cancel = crate::run::CancelSignal::new();
        let stop = Arc::new(AtomicBool::new(false));
        let token = self.cancel.clone();
        let watcher = {
            let cancel = cancel.clone();
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    if token.is_cancelled() {
                        cancel.cancel();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            })
        };
        let result = crate::run::run(self.data_directory, request, &cancel);
        stop.store(true, Ordering::SeqCst);
        let _unreportable = watcher.join();
        match result {
            Ok(report) => {
                let body = json!({
                    "tool": call.name.as_str(),
                    "status": "executed",
                    "result": report.finish.result,
                    "elapsed_ms": report.finish.elapsed_ms,
                    "stdout": report.stdout.text,
                    "stderr": report.stderr.text,
                    "stdout_truncated": report.stdout.truncated,
                    "stderr_truncated": report.stderr.truncated,
                });
                let message = labelled_result(call, &body.to_string());
                if message.len() > max_result_bytes.min(MAX_MESSAGE_BYTES) {
                    return ToolExecution::recoverable("output_too_large", call);
                }
                ToolExecution::success(message)
            }
            Err(error) => status_result(
                call,
                ToolOutcome::Failed,
                "command_failed",
                json!({"error": error.to_string()}),
                max_result_bytes,
            ),
        }
    }
}

/// Fixed limits for one model command run.
fn command_limits() -> crate::run::ProcessLimits {
    crate::run::ProcessLimits {
        deadline: COMMAND_DEADLINE,
        max_stdout_bytes: COMMAND_OUTPUT_BYTES,
        max_stderr_bytes: COMMAND_OUTPUT_BYTES,
        tree: Some(crate::run::TreeLimits {
            max_memory_bytes: 1024 * 1024 * 1024,
            max_processes: 64,
            max_cpu_time: Duration::from_secs(20),
            max_disk_bytes: Some(256 * 1024 * 1024),
        }),
        ..crate::run::ProcessLimits::default()
    }
}

/// Bind one model command call to the exact run request.
fn command_digest(session_id: Uuid, request: &crate::run::RunRequest) -> String {
    let mut bytes = Vec::new();
    push_field(&mut bytes, b"omgcode-command-v1");
    push_field(&mut bytes, session_id.as_bytes());
    push_field(&mut bytes, request.workspace_id.as_uuid().as_bytes());
    push_field(&mut bytes, request.profile.as_bytes());
    push_field(&mut bytes, request.program.as_os_str().as_encoded_bytes());
    push_field(
        &mut bytes,
        &u64::try_from(request.arguments.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    for argument in &request.arguments {
        push_field(&mut bytes, argument.as_encoded_bytes());
    }
    match &request.working_directory {
        Some(directory) => push_field(&mut bytes, directory.as_os_str().as_encoded_bytes()),
        None => push_field(&mut bytes, b""),
    }
    push_field(
        &mut bytes,
        &[u8::from(request.host_services), u8::from(request.network)],
    );
    let tree = request.limits.tree;
    let numbers = [
        u64::try_from(request.limits.deadline.as_millis()).unwrap_or(u64::MAX),
        u64::try_from(request.limits.max_stdout_bytes).unwrap_or(u64::MAX),
        u64::try_from(request.limits.max_stderr_bytes).unwrap_or(u64::MAX),
        tree.map_or(0, |tree| tree.max_memory_bytes),
        tree.map_or(0, |tree| u64::from(tree.max_processes)),
        tree.map_or(0, |tree| {
            u64::try_from(tree.max_cpu_time.as_millis()).unwrap_or(u64::MAX)
        }),
        tree.and_then(|tree| tree.max_disk_bytes).unwrap_or(0),
    ];
    for number in numbers {
        push_field(&mut bytes, &number.to_le_bytes());
    }
    sha256_hex(&bytes)
}

fn push_field(bytes: &mut Vec<u8>, value: &[u8]) {
    bytes.extend(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    bytes.extend(value);
}

/// A bounded operator-facing label for an approval request.
fn command_summary(profile: &str, program: &str, arguments: usize) -> String {
    let label = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program);
    let mut summary = format!("run {profile}/{label} ({arguments} arguments)");
    if summary.len() > 180 {
        let mut end = 180;
        while !summary.is_char_boundary(end) {
            end -= 1;
        }
        summary.truncate(end);
    }
    summary
}

/// Where `old_text` occurs in a file's text.
enum TextMatch {
    /// It does not occur.
    None,
    /// It starts at this byte offset and nowhere else.
    Once(usize),
    /// It occurs at two or more offsets, counting overlapping occurrences.
    Many,
}

/// Find the only occurrence of a non-empty `needle`, counting overlaps.
///
/// Overlapping starts count as separate occurrences, so `aa` in `aaa` is
/// ambiguous. Each search resumes one character after the previous start,
/// which keeps every slice on a character boundary.
fn unique_match(haystack: &str, needle: &str) -> TextMatch {
    let Some(first) = haystack.find(needle) else {
        return TextMatch::None;
    };
    let step = needle.chars().next().map_or(1, char::len_utf8);
    let resume = first.saturating_add(step);
    match haystack.get(resume..) {
        Some(rest) if rest.contains(needle) => TextMatch::Many,
        _ => TextMatch::Once(first),
    }
}

/// Replace `length` bytes at `start` with `replacement`, within the edit bound.
///
/// Returns `None` when the result would exceed [`MAX_EDIT_FILE_BYTES`] or the
/// range is not on character boundaries.
fn replace_once(text: &str, start: usize, length: usize, replacement: &str) -> Option<String> {
    let end = start.checked_add(length)?;
    let before = text.get(..start)?;
    let after = text.get(end..)?;
    let size = before
        .len()
        .checked_add(replacement.len())?
        .checked_add(after.len())?;
    if size > MAX_EDIT_FILE_BYTES {
        return None;
    }
    let mut proposed = String::with_capacity(size);
    proposed.push_str(before);
    proposed.push_str(replacement);
    proposed.push_str(after);
    Some(proposed)
}

/// Describe exact bytes by size and SHA-256.
fn artifact(bytes: &[u8]) -> Option<RecordedArtifact> {
    Some(RecordedArtifact {
        bytes: u64::try_from(bytes.len()).ok()?,
        sha256: sha256_hex(bytes),
    })
}

/// Join a validated relative path's components with `/`.
///
/// Separators that name the same native path, such as a doubled `/` or a
/// Windows `\`, give the same digest input and summary.
fn normalized_path(path: &str) -> String {
    Path::new(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Bind one model edit to the session, workspace, path, and the exact size and
/// SHA-256 of both the live current bytes and the proposed bytes.
///
/// Each field is length-prefixed under a domain tag, so no field boundary is
/// ambiguous and no other approval digest can collide with this one.
fn edit_digest(
    session_id: Uuid,
    workspace_id: WorkspaceId,
    path: &str,
    current: &RecordedArtifact,
    proposed: &RecordedArtifact,
) -> String {
    let mut bytes = Vec::new();
    push_field(&mut bytes, EDIT_DIGEST_DOMAIN);
    push_field(&mut bytes, session_id.as_bytes());
    push_field(&mut bytes, workspace_id.as_uuid().as_bytes());
    push_field(&mut bytes, path.as_bytes());
    push_field(&mut bytes, &current.bytes.to_le_bytes());
    push_field(&mut bytes, current.sha256.as_bytes());
    push_field(&mut bytes, &proposed.bytes.to_le_bytes());
    push_field(&mut bytes, proposed.sha256.as_bytes());
    sha256_hex(&bytes)
}

/// A bounded operator-facing edit label with sizes and short digests only.
///
/// It never contains file text. Control, invisible, and bidirectional
/// formatting characters in the path become `?`, and a long path is shortened on a
/// character boundary so the summary fits [`MAX_EDIT_SUMMARY_BYTES`].
fn edit_summary(path: &str, current: &RecordedArtifact, proposed: &RecordedArtifact) -> String {
    let short = |digest: &str| digest.get(..12).unwrap_or(digest).to_owned();
    let suffix = format!(
        ": {} -> {} bytes, sha256 {} -> {}",
        current.bytes,
        proposed.bytes,
        short(&current.sha256),
        short(&proposed.sha256)
    );
    let prefix = "edit ";
    let budget = MAX_EDIT_SUMMARY_BYTES
        .saturating_sub(prefix.len())
        .saturating_sub(suffix.len());
    let mut label: String = path
        .chars()
        .map(|character| {
            if character.is_control() || is_hidden_format(character) {
                '?'
            } else {
                character
            }
        })
        .collect();
    if label.len() > budget {
        let mut end = budget.saturating_sub(3);
        while !label.is_char_boundary(end) {
            end -= 1;
        }
        label.truncate(end);
        label.push_str("...");
    }
    format!("{prefix}{label}{suffix}")
}

/// Invisible, line-breaking, or bidirectional formatting characters that can
/// hide or reorder displayed text.
const fn is_hidden_format(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{2069}'
            | '\u{feff}'
    )
}

/// Tell the model that an operator must approve the edit before a retry.
fn edit_approval_required(
    call: &ToolCall,
    approval_id: Uuid,
    max_result_bytes: usize,
) -> ToolExecution {
    status_result(
        call,
        ToolOutcome::Failed,
        "approval_required",
        json!({
            "approval_id": approval_id,
            "message": "Nothing changed. The operator must approve this edit. Stop now and retry the identical call in a later turn after approval.",
        }),
        max_result_bytes,
    )
}

/// Classify a failed edit read or apply.
///
/// Admission, conflict, capacity, and unsupported-filesystem outcomes are
/// recoverable: the target is unchanged, so the model can report them and
/// the operator can resolve them. A permission denial stays terminal, as it is
/// for every tool.
fn classify_edit_error(call: &ToolCall, error: FileError) -> ToolExecution {
    let checkpoint = match error {
        FileError::Checkpoint(checkpoint) => checkpoint,
        // An unresolved file operation blocks broker work until a trusted
        // acknowledgement; nothing was written by this call.
        FileError::Pending { .. } => return ToolExecution::recoverable("operation_pending", call),
        other => return classify_file_error(call, other),
    };
    let code = checkpoint.code();
    match checkpoint {
        CheckpointError::Conflict
        | CheckpointError::Unchanged
        | CheckpointError::Limit
        | CheckpointError::Busy
        | CheckpointError::Pending(_)
        | CheckpointError::WrongOperation
        | CheckpointError::DeletionPending(_)
        | CheckpointError::Unavailable
        | CheckpointError::TargetMismatch => ToolExecution::recoverable(code, call),
        CheckpointError::Io(ref source) if source.kind() == io::ErrorKind::Unsupported => {
            ToolExecution::recoverable(code, call)
        }
        CheckpointError::Denied(_) => {
            ToolExecution::denied(code, call, FileError::Checkpoint(checkpoint))
        }
        // Terminal: settings that cannot be validated, a storage I/O or sync
        // failure, invalid private records, a poisoned writer, or an uncertain
        // outcome can leave evidence or a changed target that only the operator
        // can inspect and acknowledge. Further broker work is blocked anyway,
        // so continuing the turn cannot help.
        CheckpointError::Control(_)
        | CheckpointError::Io(_)
        | CheckpointError::Records(_)
        | CheckpointError::Json(_)
        | CheckpointError::Invalid(_)
        | CheckpointError::Poisoned
        | CheckpointError::Uncertain { .. } => {
            ToolExecution::failed(code, call, FileError::Checkpoint(checkpoint))
        }
    }
}

/// Render a tool status as a bounded, source-labelled tool result.
fn status_result(
    call: &ToolCall,
    outcome: ToolOutcome,
    code: &'static str,
    detail: Value,
    max_result_bytes: usize,
) -> ToolExecution {
    let body = json!({
        "tool": call.name.as_str(),
        "status": code,
        "detail": detail,
    });
    let message = labelled_result(call, &body.to_string());
    if message.len() > max_result_bytes.min(MAX_MESSAGE_BYTES) {
        return ToolExecution::recoverable("output_too_large", call);
    }
    ToolExecution {
        outcome,
        code: Some(code),
        text: message,
        terminal: None,
    }
}

/// Render a UTF-8 read result within the selected tool-message bound.
fn read_result(call: &ToolCall, text: &str, max_result_bytes: usize) -> ToolExecution {
    let body = match serde_json::to_string(&json!({
        "tool": call.name.as_str(),
        "path": call.path(),
        "text": text,
    })) {
        Ok(body) => body,
        Err(_) => return ToolExecution::recoverable("encode_error", call),
    };
    let message = labelled_result(call, &body);
    if message.len() > max_result_bytes.min(MAX_MESSAGE_BYTES) {
        return ToolExecution::recoverable("output_too_large", call);
    }
    ToolExecution::success(message)
}

/// Render a bounded listing result, stopping before the selected message limit.
fn list_result(call: &ToolCall, listing: FileListing, max_result_bytes: usize) -> ToolExecution {
    let message_limit = max_result_bytes.min(MAX_MESSAGE_BYTES);
    let had_entries = !listing.entries.is_empty();
    let mut entries = Vec::new();
    let mut truncated = listing.truncated;
    for entry in listing.entries {
        let entry_value = match display_name(&entry.name) {
            Some(name) => json!({"name": name, "kind": entry.kind}),
            // A non-Unicode native name cannot be addressed by a JSON string.
            None => json!({
                "name": Value::Null,
                "kind": entry.kind,
                "addressable": false,
                "native": entry.name,
            }),
        };
        entries.push(entry_value);
        let body = match serde_json::to_string(&json!({
            "tool": call.name.as_str(),
            "path": call.path(),
            "entries": &entries,
            "truncated": truncated,
        })) {
            Ok(body) => body,
            Err(_) => return ToolExecution::recoverable("encode_error", call),
        };
        if labelled_result(call, &body).len() > message_limit {
            entries.pop();
            truncated = true;
            break;
        }
    }
    if had_entries && entries.is_empty() {
        return ToolExecution::recoverable("output_too_large", call);
    }
    let body = match serde_json::to_string(&json!({
        "tool": call.name.as_str(),
        "path": call.path(),
        "entries": &entries,
        "truncated": truncated,
    })) {
        Ok(body) => body,
        Err(_) => return ToolExecution::recoverable("encode_error", call),
    };
    let message = labelled_result(call, &body);
    if message.len() > message_limit {
        return ToolExecution::recoverable("output_too_large", call);
    }
    ToolExecution::success(message)
}

/// Classify broker failures without treating request mistakes as authority.
fn classify_file_error(call: &ToolCall, error: FileError) -> ToolExecution {
    match error {
        FileError::Denied(_) => ToolExecution::denied("permission_denied", call, error),
        FileError::Invalid(_) | FileError::InvalidPath(_) => {
            ToolExecution::recoverable("invalid_request", call)
        }
        FileError::Conflict => ToolExecution::recoverable("conflict", call),
        FileError::Native(native) => match native.kind() {
            io::ErrorKind::NotFound => ToolExecution::recoverable("not_found", call),
            io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => {
                ToolExecution::recoverable("invalid_request", call)
            }
            io::ErrorKind::AlreadyExists => ToolExecution::recoverable("conflict", call),
            io::ErrorKind::PermissionDenied => {
                ToolExecution::denied("native_denied", call, FileError::Native(native))
            }
            _ => ToolExecution::failed("native_error", call, FileError::Native(native)),
        },
        other => ToolExecution::failed("tool_failed", call, other),
    }
}

/// Return a lossless Unicode name when the native name can be addressed as text.
fn display_name(name: &NativePath) -> Option<String> {
    match name {
        NativePath::UnixBytes(units) => std::str::from_utf8(units).ok().map(str::to_owned),
        NativePath::WindowsUtf16(units) => String::from_utf16(units).ok(),
    }
}

/// Label a successful tool result as core-observed, untrusted workspace data.
fn labelled_result(call: &ToolCall, body: &str) -> String {
    format!(
        "Tool result [call {}; source core_observed; untrusted workspace data]:\n{body}",
        call.call_id
    )
}

/// Label a tool error without including host paths or provider text.
fn labelled_error(call: &ToolCall, code: &'static str) -> String {
    let body = json!({"tool": call.name.as_str(), "error": code});
    format!(
        "Tool error [call {}; source core_observed; untrusted workspace data]:\n{body}",
        call.call_id
    )
}

/// Reject absolute, root, parent, and current-directory path forms.
fn validate_relative_path(path: &str, allow_empty: bool) -> Result<(), ToolCallError> {
    if path.is_empty() {
        return if allow_empty {
            Ok(())
        } else {
            Err(ToolCallError::InvalidPath)
        };
    }
    if path.len() > 4_096 || path.chars().any(char::is_control) {
        return Err(ToolCallError::InvalidPath);
    }
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return Err(ToolCallError::InvalidPath);
    }
    if candidate
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ToolCallError::InvalidPath);
    }
    Ok(())
}

#[cfg(test)]
mod summary_tests {
    use super::{
        MAX_EDIT_SUMMARY_BYTES, TextMatch, artifact, command_summary, edit_summary,
        normalized_path, replace_once, unique_match,
    };

    #[test]
    fn an_edit_summary_is_bounded_and_holds_no_text_or_control_characters() {
        let current = artifact(b"secret current text").expect("current");
        let proposed = artifact(b"secret proposed text").expect("proposed");
        let summary = edit_summary("src/main.rs", &current, &proposed);
        assert_eq!(
            summary,
            format!(
                "edit src/main.rs: 19 -> 20 bytes, sha256 {} -> {}",
                &current.sha256[..12],
                &proposed.sha256[..12]
            )
        );
        assert!(!summary.contains("secret"));

        let hostile = format!("{}\u{202e}\u{7}\u{2028}{}", "d/".repeat(40), "é".repeat(80));
        let summary = edit_summary(&hostile, &current, &proposed);
        assert!(summary.len() <= MAX_EDIT_SUMMARY_BYTES, "{}", summary.len());
        assert!(summary.is_char_boundary(summary.len()));
        assert!(!summary.chars().any(char::is_control));
        assert!(!summary.contains('\u{202e}') && !summary.contains('\u{2028}'));
        assert!(summary.contains("...: 19 -> 20 bytes, sha256 "));
    }

    #[test]
    fn overlapping_occurrences_are_ambiguous_and_a_single_one_is_found() {
        assert!(matches!(unique_match("aaa", "aa"), TextMatch::Many));
        assert!(matches!(unique_match("a b a", "a"), TextMatch::Many));
        assert!(matches!(unique_match("éaé", "é"), TextMatch::Many));
        assert!(matches!(unique_match("abc", "x"), TextMatch::None));
        assert!(matches!(unique_match("xéy", "éy"), TextMatch::Once(1)));
        assert_eq!(replace_once("xéy", 1, 3, "-").as_deref(), Some("x-"));
        assert_eq!(replace_once("xéy", 2, 1, "-"), None, "not a char boundary");
        assert_eq!(normalized_path("a//b/c"), "a/b/c");
    }

    #[test]
    fn a_command_summary_truncates_on_a_utf8_boundary() {
        let program = format!("{}{}", "a".repeat(168), 'é');
        let summary = command_summary("system", &program, 3);
        assert!(summary.len() <= 180);
        assert!(summary.is_char_boundary(summary.len()));
        assert!(summary.starts_with("run system/"));
    }
}
