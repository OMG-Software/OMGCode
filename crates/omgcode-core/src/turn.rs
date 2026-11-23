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

use omgcode_platform::broker::{is_missing_entry, is_oversize_entry};
use serde::Serialize;
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

use crate::action_journal::{
    ActionJournal, ActionJournalError, ActionOutcome, ActionTool, Authorization, RefusalReason,
    RefusedAction, StartedAction,
};
use crate::control::{
    ApprovalBinding, ApprovalDeclaration, ApprovalOperation, ApprovalState, ControlError,
    ControlStore, now_unix, valid_profile_name,
};
use crate::files::{FileBroker, FileError, FileListing, FileTarget};
use crate::policy::{
    Access, DecisionReason, PermissionSet, SessionBinding, WorkspaceId, WriteTool,
};
use crate::prompt::{
    self, BASE_PROMPT, BASE_PROMPT_VERSION, ConversationMessage, ConversationRole, ModelMessage,
    PromptError, SkillPrompt,
};
use crate::replacement::{CheckpointError, RecordedArtifact};
use crate::session::{
    MAX_MESSAGE_BYTES, ManifestSkill, MessageRole, NativePath, RecordedMessage, RequestManifest,
    SessionError, SessionJournal, ToolOutcome, sha256_hex,
};

mod approval_wait;
mod review;

pub use approval_wait::{ApprovalWaitSlot, AwaitedCall};

pub use review::{
    ApprovalReview, DisplaySafeStream, ReviewCandidate, ReviewError, ReviewField, ReviewedRequest,
    StaleReason, display_safe, display_safe_line, review_approval,
};

/// Revision of the tool schemas advertised by this runtime.
///
/// Version `0.3.0` added `workspace_edit_file`. Version `0.4.0` added
/// `workspace_multi_edit_file` and `workspace_create_file`. Version `0.5.0` adds
/// `workspace_rename_file` and `workspace_delete_file`. Journals record the
/// version that was advertised; earlier versions replay unchanged.
pub const TOOL_SCHEMA_VERSION: &str = "0.5.0";

/// Maximum tool calls accepted from one model response.
pub const MAX_TOOL_CALLS_PER_ROUND: usize = 8;
/// Maximum tool calls dispatched across one turn.
pub const MAX_TOOL_CALLS: usize = 256;
/// Default tool-call budget for a turn.
pub const DEFAULT_MAX_TOOL_CALLS: usize = 128;
/// One model request per allowed tool call, plus a request for the final reply.
pub const MAX_TOOL_ROUNDS: usize = MAX_TOOL_CALLS + 1;
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
pub const MAX_TURN_DEADLINE: Duration = Duration::from_secs(3_600);
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
/// Domain tag for multi-edit approval digests (ADR-0054).
const MULTI_EDIT_DIGEST_DOMAIN: &[u8] = b"omgcode-file-multi-edit-v1";
/// Domain tag for file-creation approval digests (ADR-0054).
const CREATE_DIGEST_DOMAIN: &[u8] = b"omgcode-file-create-v1";
/// Domain tag for file-rename approval digests (ADR-0055).
const RENAME_DIGEST_DOMAIN: &[u8] = b"omgcode-file-rename-v1";
/// Domain tag for file-deletion approval digests (ADR-0055).
const DELETE_DIGEST_DOMAIN: &[u8] = b"omgcode-file-delete-v1";
/// Most replacements accepted in one multi-edit call.
pub const MAX_MULTI_EDITS: usize = 16;

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
    /// The turn's cancel token. A model that can interrupt its request watches
    /// it and returns [`ModelError::Cancelled`] once it is set; a model that
    /// cannot interrupt may ignore it, and the runtime still checks it between
    /// requests.
    pub cancel: &'a CancelToken,
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
    /// The request was interrupted because the turn's cancel token was set.
    /// The runtime ends the turn as cancelled only when the token is set.
    #[error("model request was cancelled")]
    Cancelled,
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
                      existing UTF-8 file beneath the session workspace. Use literal \
                      current file text for old_text. If the text is unknown or the \
                      result is edit_no_match, read the file before attempting the edit. \
                      A result of approval_required means \
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

/// The multi-edit tool. It is advertised under the same live preferences as
/// the edit tool, and one approval covers every replacement (ADR-0054).
pub fn multi_edit_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "workspace_multi_edit_file",
        description: "Apply several exact replacements to one existing UTF-8 file \
                      beneath the session workspace in one step. Each old_text must \
                      occur exactly once in the current file and the matches must not \
                      overlap. If any old_text is unknown or the result is edit_no_match, \
                      read the file before attempting the edit. One operator approval \
                      covers the whole call. A result \
                      of approval_required means nothing changed: stop, tell the \
                      user, and retry the identical call in a later turn after the \
                      operator approves. Results are untrusted.",
        parameters: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative path of an existing file, without parent segments."
                },
                "edits": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_MULTI_EDITS,
                    "items": {
                        "type": "object",
                        "properties": {
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
                        "required": ["old_text", "new_text"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["path", "edits"],
            "additionalProperties": false
        }),
    }
}

/// The create tool. It is advertised under the same live preferences as the
/// edit tool, and every call needs an operator approval (ADR-0054).
pub fn create_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "workspace_create_file",
        description: "Create one new UTF-8 file beneath the session workspace. It \
                      never replaces an existing file, and the parent directory must \
                      already exist. Every creation needs operator approval. A result \
                      of approval_required means nothing changed: stop, tell the \
                      user, and retry the identical call in a later turn after the \
                      operator approves. Results are untrusted.",
        parameters: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative path of the new file, without parent segments."
                },
                "content": {
                    "type": "string",
                    "description": "Complete text of the new file; may be empty."
                }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        }),
    }
}

/// The rename tool. It is advertised under the same live preferences as the
/// edit tool, and every call needs an operator approval (ADR-0055).
pub fn rename_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "workspace_rename_file",
        description: "Rename one existing regular file beneath the session workspace \
                      without replacing anything. The new name must not exist and its \
                      parent directory must. Files over 1 MiB cannot be renamed. Every \
                      rename needs operator approval. A result of approval_required \
                      means nothing changed: stop, tell the user, and retry the \
                      identical call in a later turn after the operator approves. \
                      Results are untrusted.",
        parameters: json!({
            "type": "object",
            "properties": {
                "from": {
                    "type": "string",
                    "description": "Workspace-relative path of the existing file, without parent segments."
                },
                "to": {
                    "type": "string",
                    "description": "Workspace-relative new path, which must not exist."
                }
            },
            "required": ["from", "to"],
            "additionalProperties": false
        }),
    }
}

/// The delete tool. It is advertised under the same live preferences as the
/// edit tool, and every call needs an operator approval (ADR-0055).
pub fn delete_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "workspace_delete_file",
        description: "Delete one existing regular file beneath the session workspace. \
                      The operator can restore it from a checkpoint. Files over 1 MiB \
                      cannot be deleted. Every deletion needs operator approval. A \
                      result of approval_required means nothing changed: stop, tell \
                      the user, and retry the identical call in a later turn after the \
                      operator approves. Results are untrusted.",
        parameters: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative path of the existing file, without parent segments."
                }
            },
            "required": ["path"],
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
    /// Request or apply several exact edits of one existing file.
    MultiEditFile,
    /// Request or perform the creation of one new file.
    CreateFile,
    /// Request or perform the rename of one file without replacing anything.
    RenameFile,
    /// Request or perform the checkpointed deletion of one file.
    DeleteFile,
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
            Self::MultiEditFile => "workspace_multi_edit_file",
            Self::CreateFile => "workspace_create_file",
            Self::RenameFile => "workspace_rename_file",
            Self::DeleteFile => "workspace_delete_file",
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
    /// Replace exactly one occurrence of each `old_text` in an existing file.
    MultiEditFile {
        /// Validated workspace-relative path.
        path: String,
        /// One to [`MAX_MULTI_EDITS`] replacements, matched against the
        /// original content. Untrusted model data.
        edits: Vec<TextEdit>,
    },
    /// Create one new file.
    CreateFile {
        /// Validated workspace-relative path.
        path: String,
        /// Complete new file text, possibly empty. Untrusted model data.
        content: String,
    },
    /// Rename one file to a name that does not exist.
    RenameFile {
        /// Validated workspace-relative path of the existing file.
        from: String,
        /// Validated workspace-relative new path.
        to: String,
    },
    /// Delete one file after a checkpoint.
    DeleteFile {
        /// Validated workspace-relative path.
        path: String,
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

/// One exact replacement inside a multi-edit call. Both texts are untrusted
/// model data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextEdit {
    /// Non-empty text that must occur exactly once in the original file.
    pub old_text: String,
    /// Replacement text, possibly empty.
    pub new_text: String,
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
            "workspace_multi_edit_file" => ToolName::MultiEditFile,
            "workspace_create_file" => ToolName::CreateFile,
            "workspace_rename_file" => ToolName::RenameFile,
            "workspace_delete_file" => ToolName::DeleteFile,
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
            ToolName::MultiEditFile => {
                let mut path = None;
                let mut edits = None;
                for (key, value) in arguments_object {
                    match key.as_str() {
                        "path" => {
                            path = Some(value.as_str().ok_or(ToolCallError::InvalidArguments)?);
                        }
                        "edits" => {
                            let values = value.as_array().ok_or(ToolCallError::InvalidArguments)?;
                            if !(1..=MAX_MULTI_EDITS).contains(&values.len()) {
                                return Err(ToolCallError::InvalidArguments);
                            }
                            let mut parsed = Vec::with_capacity(values.len());
                            for item in values {
                                parsed.push(parse_text_edit(item)?);
                            }
                            edits = Some(parsed);
                        }
                        _ => return Err(ToolCallError::InvalidArguments),
                    }
                }
                let path = path.ok_or(ToolCallError::InvalidArguments)?;
                let edits = edits.ok_or(ToolCallError::InvalidArguments)?;
                validate_relative_path(path, false)?;
                ToolArguments::MultiEditFile {
                    path: path.to_owned(),
                    edits,
                }
            }
            ToolName::CreateFile => {
                let mut path = None;
                let mut content = None;
                for (key, value) in arguments_object {
                    let text = value.as_str().ok_or(ToolCallError::InvalidArguments)?;
                    match key.as_str() {
                        "path" => path = Some(text),
                        "content" => content = Some(text),
                        _ => return Err(ToolCallError::InvalidArguments),
                    }
                }
                let path = path.ok_or(ToolCallError::InvalidArguments)?;
                let content = content.ok_or(ToolCallError::InvalidArguments)?;
                validate_relative_path(path, false)?;
                ToolArguments::CreateFile {
                    path: path.to_owned(),
                    content: content.to_owned(),
                }
            }
            ToolName::RenameFile => {
                let mut from = None;
                let mut to = None;
                for (key, value) in arguments_object {
                    let text = value.as_str().ok_or(ToolCallError::InvalidArguments)?;
                    match key.as_str() {
                        "from" => from = Some(text),
                        "to" => to = Some(text),
                        _ => return Err(ToolCallError::InvalidArguments),
                    }
                }
                let from = from.ok_or(ToolCallError::InvalidArguments)?;
                let to = to.ok_or(ToolCallError::InvalidArguments)?;
                validate_relative_path(from, false)?;
                validate_relative_path(to, false)?;
                ToolArguments::RenameFile {
                    from: from.to_owned(),
                    to: to.to_owned(),
                }
            }
            ToolName::DeleteFile => {
                let mut path = None;
                for (key, value) in arguments_object {
                    let text = value.as_str().ok_or(ToolCallError::InvalidArguments)?;
                    match key.as_str() {
                        "path" => path = Some(text),
                        _ => return Err(ToolCallError::InvalidArguments),
                    }
                }
                let path = path.ok_or(ToolCallError::InvalidArguments)?;
                validate_relative_path(path, false)?;
                ToolArguments::DeleteFile {
                    path: path.to_owned(),
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
            | ToolArguments::EditFile { path, .. }
            | ToolArguments::MultiEditFile { path, .. }
            | ToolArguments::CreateFile { path, .. }
            | ToolArguments::DeleteFile { path } => path,
            ToolArguments::RenameFile { from, .. } => from,
            ToolArguments::Command { .. } => "",
        }
    }

    /// Return the exact arguments JSON recorded with the request intent.
    pub fn arguments_json(&self) -> &str {
        &self.arguments_json
    }
}

/// Parse one element of a multi-edit `edits` array: exactly `old_text`
/// (non-empty) and `new_text`, both strings.
fn parse_text_edit(value: &Value) -> Result<TextEdit, ToolCallError> {
    let object = value.as_object().ok_or(ToolCallError::InvalidArguments)?;
    let mut old_text = None;
    let mut new_text = None;
    for (key, value) in object {
        let text = value.as_str().ok_or(ToolCallError::InvalidArguments)?;
        match key.as_str() {
            "old_text" => old_text = Some(text),
            "new_text" => new_text = Some(text),
            _ => return Err(ToolCallError::InvalidArguments),
        }
    }
    let old_text = old_text.ok_or(ToolCallError::InvalidArguments)?;
    let new_text = new_text.ok_or(ToolCallError::InvalidArguments)?;
    if old_text.is_empty() {
        return Err(ToolCallError::InvalidArguments);
    }
    Ok(TextEdit {
        old_text: old_text.to_owned(),
        new_text: new_text.to_owned(),
    })
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
            max_rounds: DEFAULT_MAX_TOOL_CALLS + 1,
            max_calls_per_round: MAX_TOOL_CALLS_PER_ROUND,
            max_tool_calls: DEFAULT_MAX_TOOL_CALLS,
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
    #[error(
        "The latest response would exceed the per-turn limit of {limit} tool calls ({used} used, {requested} requested). Skipped all calls in that response"
    )]
    ToolCalls {
        /// The configured maximum.
        limit: usize,
        /// Calls already accounted for in earlier responses.
        used: usize,
        /// Calls requested by the refused response.
        requested: usize,
    },
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
    /// A selected skill is no longer registered and enabled for the workspace,
    /// or its recorded digest changed. Nothing further was dispatched.
    #[error("a selected skill was withdrawn")]
    SkillWithdrawn,
    /// Current policy or native checks denied the tool operation.
    #[error("tool operation denied: {0}")]
    ToolDenied(#[source] Box<FileError>),
    /// A terminal tool failure stopped the turn without retry.
    #[error("tool operation failed: {0}")]
    ToolFailed(#[source] Box<FileError>),
    /// The model action journal could not record a call, so the turn stopped
    /// (ADR-0063). An action is never started without its start record.
    #[error("the model action journal is unavailable: {0}")]
    AuditUnavailable(#[source] Box<ActionJournalError>),
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
    skills: Vec<SkillPrompt>,
    approval_wait: Option<ApprovalWaitSlot>,
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
            skills: Vec::new(),
            approval_wait: None,
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

    /// Send these operator-selected, already verified skills with every model
    /// request of the turn.
    ///
    /// The skill text is untrusted, request-only, and never recorded. The
    /// runtime records each skill's identity and digest in the manifest and
    /// rechecks live settings before every later model request and every tool
    /// dispatch. A skill that is no longer registered, enabled for the
    /// workspace, and unchanged ends the turn as `skill_withdrawn`.
    #[must_use]
    pub fn with_skills(mut self, skills: Vec<SkillPrompt>) -> Self {
        self.skills = skills;
        self
    }

    /// Wait inside the turn for the operator to decide each approval a model
    /// call records, then complete the same call (ADR-0063).
    ///
    /// Without it, a call that needs approval returns `approval_required` and
    /// the model must repeat the identical call in a later turn. While
    /// waiting, the settings lock is released between reads, cancellation is
    /// checked every 250 ms, and the turn deadline is paused. A wait ends when
    /// the record is approved, denied, expires, or can no longer be read.
    /// The call being waited on is published in `slot`, so a reviewer can
    /// check it while this turn holds the session journal.
    #[must_use]
    pub fn with_approval_wait(mut self, slot: ApprovalWaitSlot) -> Self {
        self.approval_wait = Some(slot);
        self
    }

    /// Use `cancel` as the turn's token instead of the one the runtime made.
    ///
    /// A caller whose cancellation must also reach work done before the
    /// runtime exists, such as compaction before a turn, creates one token
    /// and hands it here (ADR-0051). A token already set when [`Self::run`]
    /// starts returns `Cancelled` before the turn marker, so nothing is
    /// recorded.
    #[must_use]
    pub fn with_cancel_token(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
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
        let mut deadline = Instant::now()
            .checked_add(self.limits.deadline)
            .ok_or(TurnError::Timeout)?;
        self.check_checkpoints(deadline)?;

        // One settings snapshot selects the advertised tools; every call
        // rechecks the live preferences before dispatch.
        let permissions = self.workspace_permissions();
        let mut tools = tool_definitions();
        if let Some(permissions) = permissions {
            for tool in WriteTool::ALL {
                if permissions.model_write(tool).permits() {
                    tools.push(write_tool_definition(tool));
                }
            }
            if permissions.model_command().permits() {
                tools.push(command_tool_definition());
            }
        }
        // The runtime owns the exact instruction and tool metadata it sends.
        manifest.custom_instructions = custom_instructions.to_owned();
        manifest.skills = self
            .skills
            .iter()
            .map(|skill| ManifestSkill {
                skill_id: skill.skill_id,
                name: skill.name.clone(),
                skill_md_sha256: skill.skill_md_sha256.clone(),
            })
            .collect();
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
        prompt::assemble_with_skills(custom_instructions, &self.skills, &history)?;
        prompt::check_tool_turn_history_capacity(history.len(), self.limits.max_tool_calls)?;
        // A skill withdrawn since the caller loaded it ends the turn before
        // anything is recorded.
        if !self.skills_current() {
            return Err(TurnError::SkillWithdrawn);
        }
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

            // The first request follows the check above; every later request
            // rechecks live settings before it is built.
            if rounds > 1 && !self.skills_current() {
                return Err(self.abort_for(request_id, TurnError::SkillWithdrawn));
            }
            let messages =
                match prompt::assemble_with_skills(custom_instructions, &self.skills, &history) {
                    Ok(messages) => messages,
                    Err(error) => {
                        return Err(self.abort(request_id, "prompt_limit", error.into()));
                    }
                };
            let reply = match model.complete(ToolModelRequest {
                messages: &messages,
                tools: &tools,
                cancel: &self.cancel,
            }) {
                Ok(reply) => reply,
                // Only a set token makes this a cancellation. A model that
                // reports it otherwise is faulty and is recorded as such.
                Err(ModelError::Cancelled) if self.cancel.is_cancelled() => {
                    return Err(self.abort_for(request_id, TurnError::Cancelled));
                }
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

            let requested = reply.tool_calls.len();
            let next_total = match total_calls.checked_add(requested) {
                Some(total) => total,
                None => return Err(self.abort_for(request_id, TurnError::InvalidLimits)),
            };
            if next_total > self.limits.max_tool_calls {
                let error = TurnError::Limit(TurnLimit::ToolCalls {
                    limit: self.limits.max_tool_calls,
                    used: total_calls,
                    requested,
                });
                return Err(self.abort_for(request_id, error));
            }
            if requested > self.limits.max_calls_per_round {
                let error = TurnError::Limit(TurnLimit::CallsPerRound);
                return Err(self.abort_for(request_id, error));
            }
            total_calls = next_total;

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
                // Dispatch nothing once a selected skill is withdrawn.
                if !self.skills_current() {
                    return Err(self.abort_for(request_id, TurnError::SkillWithdrawn));
                }
                // Reserve the largest possible tool message against the
                // assembled-prompt budget before recording an intent or
                // dispatching the operation.
                history.push(ConversationMessage {
                    role: ConversationRole::User,
                    content: "x".repeat(self.limits.max_result_bytes),
                });
                let prompt_check =
                    prompt::assemble_with_skills(custom_instructions, &self.skills, &history);
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
                    skills: &self.skills,
                    approval_wait: self.approval_wait.as_ref(),
                }
                .dispatch(call, self.limits.max_result_bytes);
                // Time spent waiting for the operator is not the turn's own.
                deadline = deadline.checked_add(execution.waited).unwrap_or(deadline);
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
                    Some(TerminalToolError::SkillWithdrawn) => {
                        return Err(self.abort_for(request_id, TurnError::SkillWithdrawn));
                    }
                    Some(TerminalToolError::Cancelled) => {
                        return Err(self.abort_for(request_id, TurnError::Cancelled));
                    }
                    Some(TerminalToolError::Audit(error)) => {
                        return Err(self.abort(
                            request_id,
                            "audit_unavailable",
                            TurnError::AuditUnavailable(Box::new(error)),
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
            .is_some_and(|permissions| permissions.model_read().permits())
    }

    /// Read the workspace's current preferences, releasing the settings lock.
    fn workspace_permissions(&self) -> Option<PermissionSet> {
        match open_control(self.data_directory) {
            Ok(Some(control)) => control
                .workspace(self.workspace_id)
                .map(|workspace| workspace.permissions()),
            Ok(None) | Err(_) => None,
        }
    }

    /// Report whether every selected skill is still registered, effectively
    /// enabled for the workspace, and unchanged. Settings are read live and
    /// the lock is released before returning; any settings failure counts as
    /// withdrawn. A turn without skills never opens the store.
    fn skills_current(&self) -> bool {
        if self.skills.is_empty() {
            return true;
        }
        let Ok(Some(control)) = open_control(self.data_directory) else {
            return false;
        };
        control.skill_selection_current(self.workspace_id, &self.skills)
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
            TurnError::SkillWithdrawn => "skill_withdrawn",
            TurnError::ToolDenied(_) => "tool_denied",
            TurnError::ToolFailed(_) => "tool_failed",
            TurnError::AuditUnavailable(_) => "audit_unavailable",
            TurnError::Session(_) => "persistence",
            _ => "turn_failed",
        };
        self.abort(request_id, code, error)
    }
}

/// The definition offered for one model file-write tool.
fn write_tool_definition(tool: WriteTool) -> ToolDefinition {
    match tool {
        WriteTool::Edit => edit_tool_definition(),
        WriteTool::MultiEdit => multi_edit_tool_definition(),
        WriteTool::Create => create_tool_definition(),
        WriteTool::Rename => rename_tool_definition(),
        WriteTool::Delete => delete_tool_definition(),
    }
}

/// The file-write tool a call names, or `None` for any other tool.
const fn write_tool(arguments: &ToolArguments) -> Option<WriteTool> {
    match arguments {
        ToolArguments::EditFile { .. } => Some(WriteTool::Edit),
        ToolArguments::MultiEditFile { .. } => Some(WriteTool::MultiEdit),
        ToolArguments::CreateFile { .. } => Some(WriteTool::Create),
        ToolArguments::RenameFile { .. } => Some(WriteTool::Rename),
        ToolArguments::DeleteFile { .. } => Some(WriteTool::Delete),
        ToolArguments::ReadFile { .. }
        | ToolArguments::ListDirectory { .. }
        | ToolArguments::Command { .. } => None,
    }
}

/// How often an in-turn wait rereads an approval record.
const APPROVAL_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The most times one call waits for a decision. A later wait happens only
/// when the approved request no longer matches the live state, such as a file
/// that changed while the operator decided, and a new request was recorded.
const MAX_APPROVAL_WAITS: usize = 3;

/// What ended an in-turn wait for an operator decision.
enum Decision {
    Approved,
    Denied,
    Expired,
    Cancelled,
    /// The record could not be read, was consumed elsewhere, or vanished.
    Unavailable,
}

/// How long recording an action waits for another writer of the action
/// journal before the action is refused.
const AUDIT_BUSY_WAIT: Duration = Duration::from_secs(2);
/// Open the settings store, waiting briefly while another holder releases it.
fn open_control(data_directory: &Path) -> Result<Option<ControlStore>, ControlError> {
    ControlStore::open_existing_waiting(data_directory)
}

/// Domain tag for read and list approval digests (ADR-0063).
const READ_DIGEST_DOMAIN: &[u8] = b"omgcode-file-read-v1";

/// One model read or list call, as its approval digest binds it.
///
/// The digest covers the session, workspace, tool, normalized path, and the
/// limit the model asked for. It binds no file state: a read changes nothing,
/// and the operator approves reading that path, not particular bytes.
pub(super) struct ReadRequest {
    pub(super) list: bool,
    pub(super) path: String,
    pub(super) limit: usize,
}

impl ReadRequest {
    pub(super) fn of(call: &ToolCall) -> Option<Self> {
        match &call.arguments {
            ToolArguments::ReadFile { path, max_bytes } => Some(Self {
                list: false,
                path: normalized_path(path),
                limit: *max_bytes,
            }),
            ToolArguments::ListDirectory { path, max_entries } => Some(Self {
                list: true,
                path: normalized_path(path),
                limit: *max_entries,
            }),
            _ => None,
        }
    }

    pub(super) fn digest(&self, session_id: Uuid, workspace_id: WorkspaceId) -> String {
        let mut bytes = Vec::new();
        push_field(&mut bytes, READ_DIGEST_DOMAIN);
        push_field(&mut bytes, session_id.as_bytes());
        push_field(&mut bytes, workspace_id.as_uuid().as_bytes());
        push_field(&mut bytes, if self.list { b"list" } else { b"read" });
        push_field(&mut bytes, self.path.as_bytes());
        push_field(
            &mut bytes,
            &u64::try_from(self.limit).unwrap_or(u64::MAX).to_le_bytes(),
        );
        sha256_hex(&bytes)
    }

    pub(super) const fn action(&self) -> &'static str {
        if self.list {
            "List a directory"
        } else {
            "Read a file"
        }
    }

    pub(super) const fn limit_label(&self) -> &'static str {
        if self.list {
            "Most entries"
        } else {
            "Most bytes"
        }
    }

    const fn noun(&self) -> &'static str {
        if self.list {
            "directory listing"
        } else {
            "file read"
        }
    }

    /// A bounded label that names the path and limit and never file text.
    fn summary(&self) -> String {
        let (prefix, unit) = if self.list {
            ("list ", "entries")
        } else {
            ("read ", "bytes")
        };
        let path = if self.path.is_empty() {
            "."
        } else {
            &self.path
        };
        bounded_path_summary(prefix, path, &format!(": up to {} {unit}", self.limit))
    }
}

/// The audit tool name of a call.
const fn action_tool(arguments: &ToolArguments) -> ActionTool {
    match arguments {
        ToolArguments::ReadFile { .. } => ActionTool::ReadFile,
        ToolArguments::ListDirectory { .. } => ActionTool::ListDirectory,
        ToolArguments::EditFile { .. } => ActionTool::EditFile,
        ToolArguments::MultiEditFile { .. } => ActionTool::MultiEditFile,
        ToolArguments::CreateFile { .. } => ActionTool::CreateFile,
        ToolArguments::RenameFile { .. } => ActionTool::RenameFile,
        ToolArguments::DeleteFile { .. } => ActionTool::DeleteFile,
        ToolArguments::Command { .. } => ActionTool::RunCommand,
    }
}

/// The workspace paths a call names, as the model sent them.
fn action_targets(arguments: &ToolArguments) -> Vec<&str> {
    match arguments {
        ToolArguments::ReadFile { path, .. }
        | ToolArguments::ListDirectory { path, .. }
        | ToolArguments::EditFile { path, .. }
        | ToolArguments::MultiEditFile { path, .. }
        | ToolArguments::CreateFile { path, .. }
        | ToolArguments::DeleteFile { path } => vec![path.as_str()],
        ToolArguments::RenameFile { from, to } => vec![from.as_str(), to.as_str()],
        ToolArguments::Command { .. } => Vec::new(),
    }
}

/// Why a call that never started an action was refused, for its audit record.
fn refusal_reason(execution: &ToolExecution) -> RefusalReason {
    match (execution.code, execution.approval_id) {
        (Some("approval_required"), Some(approval_id)) => {
            RefusalReason::ApprovalRequired { approval_id }
        }
        (Some("approval_denied"), Some(approval_id)) => {
            RefusalReason::ApprovalDenied { approval_id }
        }
        (Some("approval_expired"), Some(approval_id)) => {
            RefusalReason::ApprovalExpired { approval_id }
        }
        (Some("cancelled"), _) => RefusalReason::Cancelled,
        (Some("policy_unavailable" | "approval_failed"), _) => RefusalReason::PolicyUnavailable,
        _ if execution.outcome == ToolOutcome::Denied => RefusalReason::AccessDenied,
        _ => RefusalReason::InvalidRequest,
    }
}

/// A terminal tool failure retained until its audit records are durable.
enum TerminalToolError {
    Denied(FileError),
    Failed(FileError),
    /// A selected skill was withdrawn before the operation was admitted.
    SkillWithdrawn,
    /// The turn was cancelled while the call waited for an operator decision.
    Cancelled,
    /// The action journal could not record the call (ADR-0063).
    Audit(ActionJournalError),
}

/// One bounded tool execution result, including the exact text shown to the model.
struct ToolExecution {
    outcome: ToolOutcome,
    code: Option<&'static str>,
    text: String,
    terminal: Option<TerminalToolError>,
    /// The approval record this result refers to: the one it asks the
    /// operator to decide, or the one that was denied or expired.
    approval_id: Option<Uuid>,
    /// The broker, checkpoint, or run record of a performed action.
    operation_id: Option<Uuid>,
    /// Whether the action journal already holds this call's record.
    audited: bool,
    /// Time spent waiting for operator decisions, which the turn deadline
    /// does not count.
    waited: Duration,
}

impl ToolExecution {
    /// Name the approval record an approval result refers to. A result
    /// replaced by `output_too_large` keeps no record.
    fn about(mut self, approval_id: Uuid) -> Self {
        if self.code != Some("output_too_large") {
            self.approval_id = Some(approval_id);
        }
        self
    }

    /// Link a performed action to its broker, checkpoint, or run record.
    const fn with_operation(mut self, operation_id: Option<Uuid>) -> Self {
        self.operation_id = operation_id;
        self
    }

    /// Report whether this result asks the operator to decide a record.
    fn awaits_decision(&self) -> Option<Uuid> {
        if self.code == Some("approval_required") {
            self.approval_id
        } else {
            None
        }
    }

    fn success(text: String) -> Self {
        Self {
            outcome: ToolOutcome::Succeeded,
            code: None,
            text,
            terminal: None,
            approval_id: None,
            waited: Duration::ZERO,
            operation_id: None,
            audited: false,
        }
    }

    fn recoverable(code: &'static str, call: &ToolCall) -> Self {
        Self {
            outcome: ToolOutcome::Failed,
            code: Some(code),
            text: labelled_error(call, code),
            terminal: None,
            approval_id: None,
            waited: Duration::ZERO,
            operation_id: None,
            audited: false,
        }
    }

    /// A recoverable refusal with a short fixed-format message that names an
    /// edit index or count but never file or model text.
    fn recoverable_detail(code: &'static str, call: &ToolCall, message: &str) -> Self {
        let body = json!({"tool": call.name.as_str(), "error": code, "message": message});
        Self {
            outcome: ToolOutcome::Failed,
            code: Some(code),
            text: format!(
                "Tool error [call {}; source core_observed; untrusted workspace data]:\n{body}",
                call.call_id
            ),
            terminal: None,
            approval_id: None,
            waited: Duration::ZERO,
            operation_id: None,
            audited: false,
        }
    }

    fn denied(code: &'static str, call: &ToolCall, error: FileError) -> Self {
        Self {
            outcome: ToolOutcome::Denied,
            code: Some(code),
            text: labelled_error(call, code),
            terminal: Some(TerminalToolError::Denied(error)),
            approval_id: None,
            waited: Duration::ZERO,
            operation_id: None,
            audited: false,
        }
    }

    fn failed(code: &'static str, call: &ToolCall, error: FileError) -> Self {
        Self {
            outcome: ToolOutcome::Failed,
            code: Some(code),
            text: labelled_error(call, code),
            terminal: Some(TerminalToolError::Failed(error)),
            approval_id: None,
            waited: Duration::ZERO,
            operation_id: None,
            audited: false,
        }
    }
}

/// Dispatch one validated call through a freshly opened store and the native broker.
struct WorkspaceToolDispatcher<'a> {
    data_directory: &'a Path,
    session_id: Uuid,
    workspace_id: WorkspaceId,
    cancel: CancelToken,
    skills: &'a [SkillPrompt],
    approval_wait: Option<&'a ApprovalWaitSlot>,
}

impl WorkspaceToolDispatcher<'_> {
    /// Dispatch one call and, when enabled, wait for each approval it records.
    ///
    /// After an approval the same call is dispatched again, so every live
    /// check, read, and digest is repeated before the approval is consumed.
    fn dispatch(&mut self, call: &ToolCall, max_result_bytes: usize) -> ToolExecution {
        let mut execution = self.dispatch_waiting(call, max_result_bytes);
        if !execution.audited {
            self.audit_refusal(call, &mut execution);
        }
        execution
    }

    /// [`Self::dispatch`] without the refusal record.
    fn dispatch_waiting(&mut self, call: &ToolCall, max_result_bytes: usize) -> ToolExecution {
        let mut waited = Duration::ZERO;
        let mut waits = 0_usize;
        loop {
            let mut execution = self.dispatch_once(call, max_result_bytes);
            execution.waited = waited;
            let Some(approval_id) = execution.awaits_decision() else {
                return execution;
            };
            let Some(slot) = self.approval_wait else {
                return execution;
            };
            if waits >= MAX_APPROVAL_WAITS {
                return execution;
            }
            waits += 1;
            let started = Instant::now();
            let published = slot.publish(AwaitedCall {
                approval_id,
                tool: call.name.as_str().to_owned(),
                arguments: call.arguments_json().to_owned(),
            });
            let decision = self.await_decision(approval_id);
            drop(published);
            waited = waited.saturating_add(started.elapsed());
            let mut ended = match decision {
                Decision::Approved => continue,
                Decision::Unavailable => execution,
                Decision::Denied => status_result(
                    call,
                    ToolOutcome::Denied,
                    "approval_denied",
                    json!({
                        "approval_id": approval_id,
                        "message": "The operator denied this request. Nothing changed. Do not retry it.",
                    }),
                    max_result_bytes,
                ),
                Decision::Expired => status_result(
                    call,
                    ToolOutcome::Failed,
                    "approval_expired",
                    json!({
                        "approval_id": approval_id,
                        "message": "The approval request expired without a decision. Nothing changed.",
                    }),
                    max_result_bytes,
                ),
                Decision::Cancelled => ToolExecution {
                    outcome: ToolOutcome::Failed,
                    code: Some("cancelled"),
                    text: labelled_error(call, "cancelled"),
                    terminal: Some(TerminalToolError::Cancelled),
                    approval_id: None,
                    waited: Duration::ZERO,
                    operation_id: None,
                    audited: false,
                },
            };
            ended.waited = waited;
            if matches!(ended.code, Some("approval_denied" | "approval_expired")) {
                ended.approval_id = Some(approval_id);
            }
            return ended;
        }
    }

    /// Open the action journal, waiting briefly for another writer.
    fn open_action_journal(&self) -> Result<ActionJournal, ActionJournalError> {
        let started = Instant::now();
        loop {
            match ActionJournal::open(self.data_directory) {
                Err(ActionJournalError::Busy) if started.elapsed() < AUDIT_BUSY_WAIT => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                other => return other,
            }
        }
    }

    /// Record that an authorized action is about to start.
    ///
    /// The journal is released before the action runs, so a long command
    /// never blocks another session's records. A failure refuses the action:
    /// nothing is read, changed, or run without a start record. An approval
    /// that the gate already consumed stays spent.
    fn audit_start(
        &self,
        call: &ToolCall,
        request_sha256: Option<&str>,
        authorization: Authorization,
    ) -> Result<Uuid, Box<ToolExecution>> {
        let targets = action_targets(&call.arguments);
        let started = StartedAction {
            session_id: self.session_id,
            workspace_id: self.workspace_id,
            call_id: &call.call_id,
            tool: action_tool(&call.arguments),
            targets: &targets,
            request_sha256,
            authorization,
        };
        self.open_action_journal()
            .and_then(|mut journal| journal.record_started(&started))
            .map_err(|error| {
                let mut refused = ToolExecution::recoverable("audit_unavailable", call);
                refused.outcome = ToolOutcome::Failed;
                refused.terminal = Some(TerminalToolError::Audit(error));
                // A failed start is not refused on record: the journal is the
                // thing that failed.
                refused.audited = true;
                Box::new(refused)
            })
    }

    /// Record how a started action ended.
    ///
    /// A terminal failure is recorded as `uncertain`, because those are the
    /// outcomes that can leave a changed target or private evidence for the
    /// operator. If the record cannot be written, the start stays unfinished
    /// in the journal and the turn stops with `audit_unavailable` after the
    /// result is recorded in the session journal.
    fn audit_finish(&self, action_id: Uuid, mut execution: ToolExecution) -> ToolExecution {
        let outcome = match (&execution.outcome, &execution.terminal) {
            (ToolOutcome::Succeeded, _) => ActionOutcome::Succeeded,
            (_, Some(_)) => ActionOutcome::Uncertain,
            _ => ActionOutcome::Failed,
        };
        let recorded = self.open_action_journal().and_then(|mut journal| {
            journal.record_finished(action_id, outcome, execution.code, execution.operation_id)
        });
        execution.audited = true;
        if let Err(error) = recorded
            && execution.terminal.is_none()
        {
            execution.terminal = Some(TerminalToolError::Audit(error));
        }
        execution
    }

    /// Record a call that started no action. If the record cannot be written
    /// the turn stops with `audit_unavailable`.
    fn audit_refusal(&self, call: &ToolCall, execution: &mut ToolExecution) {
        let targets = action_targets(&call.arguments);
        let refused = RefusedAction {
            session_id: self.session_id,
            workspace_id: self.workspace_id,
            call_id: &call.call_id,
            tool: action_tool(&call.arguments),
            targets: &targets,
            request_sha256: None,
            reason: refusal_reason(execution),
        };
        let recorded = self
            .open_action_journal()
            .and_then(|mut journal| journal.record_refused(&refused));
        execution.audited = true;
        if let Err(error) = recorded
            && execution.terminal.is_none()
        {
            execution.terminal = Some(TerminalToolError::Audit(error));
        }
    }

    /// Wait for the operator to decide one approval record.
    ///
    /// The settings store is opened only for each read and dropped before the
    /// sleep, so the approvals window and the trusted CLI can decide the
    /// record. A busy store is retried at the next poll. The wait is bounded
    /// by the longest approval lifetime plus a short margin, in case the
    /// wall clock that sets expiry does not advance.
    fn await_decision(&self, approval_id: Uuid) -> Decision {
        let started = Instant::now();
        let limit = Duration::from_secs(EDIT_APPROVAL_TTL_SECS.max(COMMAND_APPROVAL_TTL_SECS) + 5);
        loop {
            if self.cancel.is_cancelled() {
                return Decision::Cancelled;
            }
            match ControlStore::open_existing(self.data_directory) {
                Ok(Some(control)) => match control.approval_status(approval_id, now_unix()) {
                    Some(status) => match status.state {
                        ApprovalState::Pending => {}
                        ApprovalState::Approved => return Decision::Approved,
                        ApprovalState::Denied => return Decision::Denied,
                        ApprovalState::Expired => return Decision::Expired,
                        ApprovalState::Consumed => return Decision::Unavailable,
                    },
                    None => return Decision::Unavailable,
                },
                Err(ControlError::Busy) => {}
                Ok(None) | Err(_) => return Decision::Unavailable,
            }
            if started.elapsed() >= limit {
                return Decision::Expired;
            }
            std::thread::sleep(APPROVAL_POLL_INTERVAL);
        }
    }

    fn dispatch_once(&mut self, call: &ToolCall, max_result_bytes: usize) -> ToolExecution {
        // Hold the settings lock only for this operation, never across model I/O.
        // A permission change can therefore take effect before the next call.
        let mut control = match open_control(self.data_directory) {
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
        // Recheck the selection under the lock that admits this operation, so
        // a withdrawal recorded after the turn's last check still stops it.
        if !control.skill_selection_current(self.workspace_id, self.skills) {
            return ToolExecution {
                outcome: ToolOutcome::Denied,
                code: Some("skill_withdrawn"),
                text: labelled_error(call, "skill_withdrawn"),
                terminal: Some(TerminalToolError::SkillWithdrawn),
                approval_id: None,
                waited: Duration::ZERO,
                operation_id: None,
                audited: false,
            };
        }
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
            let access = permissions.model_command();
            if !access.permits() {
                return ToolExecution::denied(
                    "command_disabled",
                    call,
                    FileError::Denied(DecisionReason::PermissionDisabled),
                );
            }
            return self.dispatch_command(control, call, access, max_result_bytes);
        }
        if let Some(tool) = write_tool(&call.arguments) {
            let access = permissions.model_write(tool);
            if !access.permits() {
                return ToolExecution::denied(
                    "edit_disabled",
                    call,
                    FileError::Denied(DecisionReason::PermissionDisabled),
                );
            }
            return match tool {
                WriteTool::Edit => self.dispatch_edit(control, call, access, max_result_bytes),
                WriteTool::MultiEdit => {
                    self.dispatch_multi_edit(control, call, access, max_result_bytes)
                }
                WriteTool::Create => self.dispatch_create(control, call, access, max_result_bytes),
                WriteTool::Rename => self.dispatch_rename(control, call, access, max_result_bytes),
                WriteTool::Delete => self.dispatch_delete(control, call, access, max_result_bytes),
            };
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
        // A denied read reaches the broker, which refuses it with its policy
        // reason. A permitted one passes the gate first: `allow` needs no
        // approval, and `prompt` needs one for this exact path and limit.
        let read_access = permissions.model_read();
        let action = if read_access.permits()
            && let Some(read) = ReadRequest::of(call)
        {
            let digest = read.digest(self.session_id, self.workspace_id);
            let request = WriteRequest {
                digest: digest.clone(),
                summary: read.summary(),
                // A read result is bounded when it is rendered, before the
                // model sees it, and a read changes nothing.
                widest_success: String::new(),
                noun: read.noun(),
                operation: ApprovalOperation::FileRead,
            };
            let authorization =
                match self.write_gate(&mut control, call, request, read_access, max_result_bytes) {
                    WriteGate::Approved(authorization) => authorization,
                    WriteGate::Respond(execution) => return execution,
                };
            match self.audit_start(call, Some(&digest), authorization) {
                Ok(action) => Some(action),
                Err(refused) => return *refused,
            }
        } else {
            None
        };
        let mut broker = FileBroker::new(&mut control);
        let execution = match &call.arguments {
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
            ToolArguments::EditFile { .. }
            | ToolArguments::MultiEditFile { .. }
            | ToolArguments::CreateFile { .. }
            | ToolArguments::RenameFile { .. }
            | ToolArguments::DeleteFile { .. } => {
                ToolExecution::recoverable("invalid_request", call)
            }
            ToolArguments::Command { .. } => ToolExecution::recoverable("invalid_command", call),
        };
        drop(control);
        match action {
            Some(action) => self.audit_finish(action, execution),
            None => execution,
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
        access: Access,
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
        let target = edit_target(self.session_id, self.workspace_id, path);
        let current = match read_edit_source(&mut control, &binding, &target) {
            Ok(bytes) => bytes,
            Err(error) => return classify_edit_error(call, error),
        };
        let prepared = match prepare_edit(path, old_text, new_text, &current) {
            Ok(prepared) => prepared,
            Err(code) => return ToolExecution::recoverable(code, call),
        };
        let digest = edit_digest(
            self.session_id,
            self.workspace_id,
            &prepared.normalized,
            &prepared.current_artifact,
            &prepared.proposed_artifact,
        );
        let request = WriteRequest {
            digest: digest.clone(),
            summary: edit_summary(
                &prepared.normalized,
                &prepared.current_artifact,
                &prepared.proposed_artifact,
            ),
            widest_success: edit_success_message(
                call,
                &prepared.normalized,
                u64::MAX,
                &"f".repeat(64),
                Some(Uuid::max()),
                None,
            ),
            noun: "edit",
            operation: ApprovalOperation::FileWrite,
        };
        let authorization =
            match self.write_gate(&mut control, call, request, access, max_result_bytes) {
                WriteGate::Approved(authorization) => authorization,
                WriteGate::Respond(execution) => return execution,
            };
        let action = match self.audit_start(call, Some(&digest), authorization) {
            Ok(action) => action,
            Err(refused) => return *refused,
        };
        let execution = apply_prepared_edit(&mut control, &binding, &target, call, &prepared, None);
        drop(control);
        self.audit_finish(action, execution)
    }

    /// Request or apply several exact edits of one file under one approval.
    ///
    /// The control flow matches [`Self::dispatch_edit`]: one live audited read
    /// supplies the expected current value, the shared gate handles the
    /// approval, and the same checkpointed apply publishes the result.
    fn dispatch_multi_edit(
        &self,
        mut control: ControlStore,
        call: &ToolCall,
        access: Access,
        max_result_bytes: usize,
    ) -> ToolExecution {
        let ToolArguments::MultiEditFile { path, edits } = &call.arguments else {
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
        let target = edit_target(self.session_id, self.workspace_id, path);
        let current = match read_edit_source(&mut control, &binding, &target) {
            Ok(bytes) => bytes,
            Err(error) => return classify_edit_error(call, error),
        };
        let prepared = match prepare_multi_edit(path, edits, &current) {
            Ok(prepared) => prepared,
            Err(refusal) => return refusal.into_execution(call),
        };
        let digest = multi_edit_digest(
            self.session_id,
            self.workspace_id,
            &prepared.normalized,
            &prepared.current_artifact,
            &prepared.proposed_artifact,
        );
        let request = WriteRequest {
            digest: digest.clone(),
            summary: multi_edit_summary(
                &prepared.normalized,
                edits.len(),
                &prepared.current_artifact,
                &prepared.proposed_artifact,
            ),
            widest_success: edit_success_message(
                call,
                &prepared.normalized,
                u64::MAX,
                &"f".repeat(64),
                Some(Uuid::max()),
                Some(edits.len()),
            ),
            noun: "multi-edit",
            operation: ApprovalOperation::FileWrite,
        };
        let authorization =
            match self.write_gate(&mut control, call, request, access, max_result_bytes) {
                WriteGate::Approved(authorization) => authorization,
                WriteGate::Respond(execution) => return execution,
            };
        let action = match self.audit_start(call, Some(&digest), authorization) {
            Ok(action) => action,
            Err(refused) => return *refused,
        };
        let execution = apply_prepared_edit(
            &mut control,
            &binding,
            &target,
            call,
            &prepared,
            Some(edits.len()),
        );
        drop(control);
        self.audit_finish(action, execution)
    }

    /// Request or perform the creation of one new file.
    ///
    /// Before any approval is recorded, a live audited read must find the path
    /// absent and a listing must find its parent. An approval is consumed
    /// before the exclusive create, so a file that another writer creates in
    /// the meantime spends it. Creation is not checkpointed: there is no prior
    /// content to keep, and the exclusive create refuses an existing entry.
    fn dispatch_create(
        &self,
        mut control: ControlStore,
        call: &ToolCall,
        access: Access,
        max_result_bytes: usize,
    ) -> ToolExecution {
        let ToolArguments::CreateFile { path, content } = &call.arguments else {
            return ToolExecution::recoverable("invalid_request", call);
        };
        let Some(content_artifact) = artifact(content.as_bytes()) else {
            return ToolExecution::recoverable("edit_too_large", call);
        };
        if content.len() > MAX_EDIT_FILE_BYTES {
            return ToolExecution::recoverable("edit_too_large", call);
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
        let target = edit_target(self.session_id, self.workspace_id, path);
        if let Some(refusal) = create_precheck(&mut control, &binding, &target, call) {
            return refusal;
        }
        let normalized = normalized_path(path);
        let digest = create_digest(
            self.session_id,
            self.workspace_id,
            &normalized,
            &content_artifact,
        );
        let request = WriteRequest {
            digest: digest.clone(),
            summary: create_summary(&normalized, &content_artifact),
            widest_success: create_success_message(
                call,
                &normalized,
                u64::MAX,
                &"f".repeat(64),
                Some(Uuid::max()),
            ),
            noun: "file creation",
            operation: ApprovalOperation::FileWrite,
        };
        let authorization =
            match self.write_gate(&mut control, call, request, access, max_result_bytes) {
                WriteGate::Approved(authorization) => authorization,
                WriteGate::Respond(execution) => return execution,
            };
        let action = match self.audit_start(call, Some(&digest), authorization) {
            Ok(action) => action,
            Err(refused) => return *refused,
        };
        // The approval is spent. A concurrent creator makes this a conflict.
        let execution = match FileBroker::new(&mut control).create_file(
            &binding,
            &target,
            content.as_bytes(),
        ) {
            Ok(created) => ToolExecution::success(create_success_message(
                call,
                &normalized,
                content_artifact.bytes,
                &content_artifact.sha256,
                Some(created.operation_id),
            ))
            .with_operation(Some(created.operation_id)),
            Err(FileError::Conflict) => ToolExecution::recoverable("file_exists", call),
            // An uncertain creation stays a terminal failure and leaves its
            // pending audit record for the trusted acknowledgement.
            Err(error) => classify_edit_error(call, error),
        };
        drop(control);
        self.audit_finish(action, execution)
    }

    /// Request or perform the rename of one file without replacing anything.
    ///
    /// Before any approval is recorded, a live audited read of the source
    /// supplies its exact size and SHA-256, the destination must be absent,
    /// and its parent directory must exist. An approval is consumed before the
    /// rename, so a destination that another writer creates in the meantime
    /// spends it. Rename is not checkpointed: no content is destroyed and
    /// renaming back reverses it.
    fn dispatch_rename(
        &self,
        mut control: ControlStore,
        call: &ToolCall,
        access: Access,
        max_result_bytes: usize,
    ) -> ToolExecution {
        let ToolArguments::RenameFile { from, to } = &call.arguments else {
            return ToolExecution::recoverable("invalid_request", call);
        };
        let from_normalized = normalized_path(from);
        let to_normalized = normalized_path(to);
        if from_normalized == to_normalized {
            return ToolExecution::recoverable("invalid_request", call);
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
        let source = edit_target(self.session_id, self.workspace_id, from);
        let destination = edit_target(self.session_id, self.workspace_id, to);
        let current = match read_move_source(&mut control, &binding, &source, call) {
            Ok(artifact) => artifact,
            Err(refusal) => return *refusal,
        };
        if let Some(refusal) = rename_precheck(&mut control, &binding, &destination, call) {
            return refusal;
        }
        let digest = rename_digest(
            self.session_id,
            self.workspace_id,
            &from_normalized,
            &to_normalized,
            &current,
        );
        let request = WriteRequest {
            digest: digest.clone(),
            summary: rename_summary(&from_normalized, &to_normalized, &current),
            widest_success: rename_success_message(
                call,
                &from_normalized,
                &to_normalized,
                u64::MAX,
                &"f".repeat(64),
                Some(Uuid::max()),
            ),
            noun: "rename",
            operation: ApprovalOperation::FileWrite,
        };
        let authorization =
            match self.write_gate(&mut control, call, request, access, max_result_bytes) {
                WriteGate::Approved(authorization) => authorization,
                WriteGate::Respond(execution) => return execution,
            };
        let action = match self.audit_start(call, Some(&digest), authorization) {
            Ok(action) => action,
            Err(refused) => return *refused,
        };
        // The approval is spent. A changed source or a new destination is a
        // refusal; an uncertain rename stays terminal and leaves its pending
        // audit record for the trusted acknowledgement.
        let execution = match FileBroker::new(&mut control).rename_file(
            &binding,
            &source,
            &destination,
            &current,
        ) {
            Ok(renamed) => ToolExecution::success(rename_success_message(
                call,
                &from_normalized,
                &to_normalized,
                current.bytes,
                &current.sha256,
                Some(renamed.operation_id),
            ))
            .with_operation(Some(renamed.operation_id)),
            Err(FileError::Conflict) => ToolExecution::recoverable("file_exists", call),
            // An unsupported object or filesystem (another device, an owner
            // other than the effective user) changed nothing.
            Err(FileError::Native(native)) if native.kind() == io::ErrorKind::Unsupported => {
                ToolExecution::recoverable("invalid_request", call)
            }
            Err(error) => classify_edit_error(call, error),
        };
        drop(control);
        self.audit_finish(action, execution)
    }

    /// Request or perform the deletion of one file after a checkpoint.
    ///
    /// A live audited read supplies the exact size and SHA-256 that the
    /// checkpointed removal must still find. An approval is consumed before the
    /// removal, so a changed file spends it. The removal keeps a durable copy
    /// of the bytes in the checkpoint store, from which the operator can
    /// restore the file.
    fn dispatch_delete(
        &self,
        mut control: ControlStore,
        call: &ToolCall,
        access: Access,
        max_result_bytes: usize,
    ) -> ToolExecution {
        let ToolArguments::DeleteFile { path } = &call.arguments else {
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
        let target = edit_target(self.session_id, self.workspace_id, path);
        let current = match read_move_source(&mut control, &binding, &target, call) {
            Ok(artifact) => artifact,
            Err(refusal) => return *refusal,
        };
        let normalized = normalized_path(path);
        let digest = delete_digest(self.session_id, self.workspace_id, &normalized, &current);
        let request = WriteRequest {
            digest: digest.clone(),
            summary: delete_summary(&normalized, &current),
            widest_success: delete_success_message(
                call,
                &normalized,
                u64::MAX,
                &"f".repeat(64),
                Some(Uuid::max()),
            ),
            noun: "deletion",
            operation: ApprovalOperation::FileWrite,
        };
        let authorization =
            match self.write_gate(&mut control, call, request, access, max_result_bytes) {
                WriteGate::Approved(authorization) => authorization,
                WriteGate::Respond(execution) => return execution,
            };
        let action = match self.audit_start(call, Some(&digest), authorization) {
            Ok(action) => action,
            Err(refused) => return *refused,
        };
        let execution =
            match FileBroker::new(&mut control).apply_removal(&binding, &target, &current) {
                // The widest report was checked by the gate, so this one fits.
                Ok(receipt) if receipt.applied => ToolExecution::success(delete_success_message(
                    call,
                    &normalized,
                    current.bytes,
                    &current.sha256,
                    receipt.operation_id,
                ))
                .with_operation(receipt.operation_id),
                Ok(_) => ToolExecution::recoverable("invalid_request", call),
                // An object the checkpoint cannot capture (a read-only, hidden, or
                // otherwise unsupported file) is refused before any private
                // evidence exists and changed nothing.
                Err(FileError::Native(native)) if native.kind() == io::ErrorKind::Unsupported => {
                    ToolExecution::recoverable("invalid_request", call)
                }
                Err(error) => classify_edit_error(call, error),
            };
        drop(control);
        self.audit_finish(action, execution)
    }

    /// The approval flow shared by every model file-write tool.
    ///
    /// Under an `Allow` preference the call proceeds without an approval
    /// record once the widest success report is known to fit. Under `Prompt`,
    /// an approved record is consumed only after that check, and before the
    /// caller writes. A pending record repeats the
    /// request, a denied one refuses, and anything else records a new pending
    /// request. Lookup filters are the session, workspace, `file_write`
    /// operation, exact digest, and current settings revision.
    fn write_gate(
        &self,
        control: &mut ControlStore,
        call: &ToolCall,
        request: WriteRequest,
        access: Access,
        max_result_bytes: usize,
    ) -> WriteGate {
        let WriteRequest {
            digest,
            summary,
            widest_success,
            noun,
            operation,
        } = request;
        let now = now_unix();
        let revision = control.revision();
        // Check that the success report fits before anything is spent or
        // written, using the widest size, digest, and identifier it can
        // carry. Otherwise an applied change could only be reported as a
        // failure. An approval stays unconsumed for a later retry.
        if widest_success.len() > max_result_bytes.min(MAX_MESSAGE_BYTES) {
            return WriteGate::Respond(ToolExecution::recoverable("output_too_large", call));
        }
        if access == Access::Allow {
            // The live preference, read under this same settings lock, is the
            // authorization; no approval record is created or consumed.
            return WriteGate::Approved(Authorization::StandingAllow {
                settings_revision: revision,
            });
        }
        let statuses = control.approval_statuses(now);
        let matching = |state| {
            statuses.iter().find(|status| {
                status.session_id == self.session_id
                    && status.workspace_id == self.workspace_id
                    && status.operation == operation
                    && status.request_sha256 == digest
                    && status.settings_revision == revision
                    && status.state == state
            })
        };
        if let Some(approved) = matching(ApprovalState::Approved) {
            let approval = ApprovalBinding {
                session_id: self.session_id,
                workspace_id: self.workspace_id,
                operation,
                request_sha256: digest.clone(),
            };
            if let Err(error) = control.consume_approval(approved.id, &approval, now) {
                return WriteGate::Respond(status_result(
                    call,
                    ToolOutcome::Failed,
                    "approval_failed",
                    json!({"error": error.to_string()}),
                    max_result_bytes,
                ));
            }
            // The record is spent before the write, so no crash or retry can
            // reuse it.
            return WriteGate::Approved(Authorization::Approval {
                approval_id: approved.id,
                settings_revision: revision,
            });
        }
        if let Some(pending) = matching(ApprovalState::Pending) {
            return WriteGate::Respond(file_write_approval_required(
                call,
                pending.id,
                noun,
                max_result_bytes,
            ));
        }
        if let Some(denied) = matching(ApprovalState::Denied) {
            return WriteGate::Respond(
                status_result(
                    call,
                    ToolOutcome::Denied,
                    "approval_denied",
                    json!({"message": format!("The operator denied this {noun}. Do not retry it.")}),
                    max_result_bytes,
                )
                .about(denied.id),
            );
        }
        let status = match control.request_approval(
            &ApprovalDeclaration {
                session_id: self.session_id,
                workspace_id: self.workspace_id,
                operation,
                request_sha256: digest,
                summary,
                ttl_secs: EDIT_APPROVAL_TTL_SECS,
            },
            now,
        ) {
            Ok(status) => status,
            Err(error) => {
                return WriteGate::Respond(status_result(
                    call,
                    ToolOutcome::Failed,
                    "approval_failed",
                    json!({"error": error.to_string()}),
                    max_result_bytes,
                ));
            }
        };
        WriteGate::Respond(file_write_approval_required(
            call,
            status.id,
            noun,
            max_result_bytes,
        ))
    }

    /// Request or execute one command. The control store is dropped before the
    /// run starts so the runner can take its own settings lock.
    fn dispatch_command(
        &self,
        mut control: ControlStore,
        call: &ToolCall,
        access: Access,
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
        let request =
            command_run_request(self.workspace_id, profile, program, arguments, cwd.as_ref());
        let digest = command_digest(self.session_id, &request);
        if access == Access::Allow {
            // The live preference, read under the settings lock that admitted
            // this call, is the authorization. This lock is released before
            // the run, so the runner rechecks the standing allow under its own
            // lock and withdraws the run if it later leaves `allow`.
            let authorization = Authorization::StandingAllow {
                settings_revision: control.revision(),
            };
            drop(control);
            let request = crate::run::RunRequest {
                standing_allow: true,
                ..request
            };
            return self.run_audited(call, &request, &digest, authorization, max_result_bytes);
        }
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
                request_sha256: digest.clone(),
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
            let authorization = Authorization::Approval {
                approval_id: approved.id,
                settings_revision: revision,
            };
            return self.run_audited(call, &request, &digest, authorization, max_result_bytes);
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
            )
            .about(pending.id);
        }
        if let Some(denied) = matching(ApprovalState::Denied) {
            return status_result(
                call,
                ToolOutcome::Denied,
                "approval_denied",
                json!({"message": "The operator denied this command."}),
                max_result_bytes,
            )
            .about(denied.id);
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
        .about(status.id)
    }

    /// Record, run, and record the end of one authorized command.
    fn run_audited(
        &self,
        call: &ToolCall,
        request: &crate::run::RunRequest,
        digest: &str,
        authorization: Authorization,
        max_result_bytes: usize,
    ) -> ToolExecution {
        let action = match self.audit_start(call, Some(digest), authorization) {
            Ok(action) => action,
            Err(refused) => return *refused,
        };
        let execution = self.execute_command(call, request, max_result_bytes);
        self.audit_finish(action, execution)
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
                let run_id = Some(report.start.run_id);
                if message.len() > max_result_bytes.min(MAX_MESSAGE_BYTES) {
                    return ToolExecution::recoverable("output_too_large", call)
                        .with_operation(run_id);
                }
                ToolExecution::success(message).with_operation(run_id)
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

/// Build the run request for one model command call.
///
/// This is the only place that turns parsed command arguments into a
/// [`crate::run::RunRequest`]. Dispatch runs and digests the result, and
/// approval review recomputes the same digest from it, so the two cannot
/// diverge. Network and host services are always off and the limits are fixed.
fn command_run_request(
    workspace_id: WorkspaceId,
    profile: &str,
    program: &str,
    arguments: &[String],
    cwd: Option<&String>,
) -> crate::run::RunRequest {
    crate::run::RunRequest {
        workspace_id,
        profile: profile.to_owned(),
        program: PathBuf::from(program),
        arguments: arguments.iter().map(OsString::from).collect(),
        working_directory: cwd.map(PathBuf::from),
        host_services: false,
        network: false,
        model_tools: true,
        standing_allow: false,
        limits: command_limits(),
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

/// What the shared approval gate needs to know about one file-write request.
struct WriteRequest {
    /// The tool-specific request digest the approval is bound to.
    digest: String,
    /// The bounded operator-facing summary recorded with a new request.
    summary: String,
    /// The success report with the widest size, digest, and identifier.
    widest_success: String,
    /// The noun used in model-facing approval messages.
    noun: &'static str,
    /// `file_write` for the write tools, `file_read` for read and list.
    operation: ApprovalOperation,
}

/// The result of the shared approval gate.
enum WriteGate {
    /// The live preference is `Allow`, or an approved record was consumed;
    /// the caller may write.
    Approved(Authorization),
    /// The caller must return this result and write nothing.
    Respond(ToolExecution),
}

/// A recoverable refusal from multi-edit preparation. `detail` names an edit
/// index or count and never holds file or model text.
struct EditRefusal {
    code: &'static str,
    detail: Option<String>,
}

impl EditRefusal {
    const fn plain(code: &'static str) -> Self {
        Self { code, detail: None }
    }

    fn at(code: &'static str, detail: String) -> Self {
        Self {
            code,
            detail: Some(detail),
        }
    }

    fn into_execution(self, call: &ToolCall) -> ToolExecution {
        match self.detail {
            Some(detail) => ToolExecution::recoverable_detail(self.code, call, &detail),
            None => ToolExecution::recoverable(self.code, call),
        }
    }
}

/// One exact edit prepared against the live bytes of its target file.
struct PreparedEdit {
    /// The `/`-joined path used in the digest and the summary.
    normalized: String,
    /// Size and SHA-256 of the live current bytes.
    current_artifact: RecordedArtifact,
    /// The complete proposed file text.
    proposed: String,
    /// Size and SHA-256 of the proposed bytes.
    proposed_artifact: RecordedArtifact,
}

/// The broker target for one model edit path.
fn edit_target(session_id: Uuid, workspace_id: WorkspaceId, path: &str) -> FileTarget {
    FileTarget {
        session_id,
        workspace_id,
        path: PathBuf::from(path),
    }
}

/// Read the live current bytes of an edit target through the audited broker.
///
/// This is the only edit read. The broker checks the live `read` permission
/// and appends its own audit record. Dispatch and approval review both call it.
fn read_edit_source(
    control: &mut ControlStore,
    binding: &SessionBinding,
    target: &FileTarget,
) -> Result<Vec<u8>, FileError> {
    FileBroker::new(control)
        .read(binding, target, MAX_EDIT_FILE_BYTES)
        .map(|read| read.bytes)
}

/// Turn parsed edit arguments and the live current bytes into the exact edit.
///
/// This is the only place that applies the unique-match rule, the no-op check,
/// and the size bounds. An error is the recoverable tool code that dispatch
/// reports; approval review treats any of them as "not this request".
fn prepare_edit(
    path: &str,
    old_text: &str,
    new_text: &str,
    current: &[u8],
) -> Result<PreparedEdit, &'static str> {
    let text = std::str::from_utf8(current).map_err(|_| "non_utf8")?;
    let start = match unique_match(text, old_text) {
        TextMatch::None => return Err("edit_no_match"),
        TextMatch::Many => return Err("edit_ambiguous"),
        TextMatch::Once(start) => start,
    };
    if old_text == new_text {
        return Err("edit_unchanged");
    }
    let proposed = replace_once(text, start, old_text.len(), new_text).ok_or("edit_too_large")?;
    let (Some(current_artifact), Some(proposed_artifact)) =
        (artifact(current), artifact(proposed.as_bytes()))
    else {
        return Err("edit_too_large");
    };
    Ok(PreparedEdit {
        normalized: normalized_path(path),
        current_artifact,
        proposed,
        proposed_artifact,
    })
}

/// Turn parsed multi-edit arguments and the live current bytes into one
/// proposed buffer.
///
/// Every `old_text` is matched against the original content and must occur
/// exactly once there. Matches may not overlap, so no replacement can see the
/// output of another; touching matches are allowed. The replacements apply in
/// ascending offset order. Refusal details name 1-based edit indexes only.
fn prepare_multi_edit(
    path: &str,
    edits: &[TextEdit],
    current: &[u8],
) -> Result<PreparedEdit, EditRefusal> {
    let text = std::str::from_utf8(current).map_err(|_| EditRefusal::plain("non_utf8"))?;
    // (start, end, index of the edit)
    let mut ranges: Vec<(usize, usize, usize)> = Vec::with_capacity(edits.len());
    for (index, edit) in edits.iter().enumerate() {
        let number = index.saturating_add(1);
        match unique_match(text, &edit.old_text) {
            TextMatch::None => {
                return Err(EditRefusal::at(
                    "edit_no_match",
                    format!("old_text of edit {number} does not occur in the file"),
                ));
            }
            TextMatch::Many => {
                return Err(EditRefusal::at(
                    "edit_ambiguous",
                    format!("old_text of edit {number} occurs more than once in the file"),
                ));
            }
            TextMatch::Once(start) => {
                let end = start
                    .checked_add(edit.old_text.len())
                    .ok_or_else(|| EditRefusal::plain("edit_too_large"))?;
                ranges.push((start, end, index));
            }
        }
    }
    ranges.sort_by_key(|&(start, _, index)| (start, index));
    for pair in ranges.windows(2) {
        let (_, first_end, first_index) = pair[0];
        let (second_start, _, second_index) = pair[1];
        if second_start < first_end {
            return Err(EditRefusal::at(
                "edit_overlap",
                format!(
                    "edits {} and {} match overlapping text",
                    first_index.saturating_add(1),
                    second_index.saturating_add(1)
                ),
            ));
        }
    }
    let mut size = text.len();
    for &(start, end, index) in &ranges {
        let replacement = edits.get(index).map_or(0, |edit| edit.new_text.len());
        size = size
            .checked_sub(end.saturating_sub(start))
            .and_then(|size| size.checked_add(replacement))
            .ok_or_else(|| EditRefusal::plain("edit_too_large"))?;
    }
    if size > MAX_EDIT_FILE_BYTES {
        return Err(EditRefusal::plain("edit_too_large"));
    }
    let mut proposed = String::with_capacity(size);
    let mut cursor = 0usize;
    for &(start, end, index) in &ranges {
        let (Some(kept), Some(edit)) = (text.get(cursor..start), edits.get(index)) else {
            return Err(EditRefusal::plain("edit_too_large"));
        };
        proposed.push_str(kept);
        proposed.push_str(&edit.new_text);
        cursor = end;
    }
    let Some(tail) = text.get(cursor..) else {
        return Err(EditRefusal::plain("edit_too_large"));
    };
    proposed.push_str(tail);
    if proposed == text {
        return Err(EditRefusal::plain("edit_unchanged"));
    }
    let (Some(current_artifact), Some(proposed_artifact)) =
        (artifact(current), artifact(proposed.as_bytes()))
    else {
        return Err(EditRefusal::plain("edit_too_large"));
    };
    Ok(PreparedEdit {
        normalized: normalized_path(path),
        current_artifact,
        proposed,
        proposed_artifact,
    })
}

/// The labelled report of an applied edit. It never contains file text.
fn edit_success_message(
    call: &ToolCall,
    path: &str,
    bytes: u64,
    sha256: &str,
    operation_id: Option<Uuid>,
    edits: Option<usize>,
) -> String {
    let mut body = json!({
        "tool": call.name.as_str(),
        "status": "applied",
        "path": path,
        "bytes": bytes,
        "sha256": sha256,
        "operation_id": operation_id,
    });
    if let (Some(edits), Some(object)) = (edits, body.as_object_mut()) {
        object.insert("edits".to_owned(), json!(edits));
    }
    labelled_result(call, &body.to_string())
}

/// The labelled report of a created file. It never contains file text.
fn create_success_message(
    call: &ToolCall,
    path: &str,
    bytes: u64,
    sha256: &str,
    operation_id: Option<Uuid>,
) -> String {
    let body = json!({
        "tool": call.name.as_str(),
        "status": "created",
        "path": path,
        "bytes": bytes,
        "sha256": sha256,
        "operation_id": operation_id,
    });
    labelled_result(call, &body.to_string())
}

/// The labelled report of a renamed file. It never contains file text.
fn rename_success_message(
    call: &ToolCall,
    from: &str,
    to: &str,
    bytes: u64,
    sha256: &str,
    operation_id: Option<Uuid>,
) -> String {
    let body = json!({
        "tool": call.name.as_str(),
        "status": "renamed",
        "from": from,
        "to": to,
        "bytes": bytes,
        "sha256": sha256,
        "operation_id": operation_id,
    });
    labelled_result(call, &body.to_string())
}

/// The labelled report of a deleted file. It never contains file text. The
/// operation ID names the checkpoint from which the operator can restore it.
fn delete_success_message(
    call: &ToolCall,
    path: &str,
    bytes: u64,
    sha256: &str,
    operation_id: Option<Uuid>,
) -> String {
    let body = json!({
        "tool": call.name.as_str(),
        "status": "deleted",
        "path": path,
        "bytes": bytes,
        "sha256": sha256,
        "operation_id": operation_id,
    });
    labelled_result(call, &body.to_string())
}

/// Apply a prepared edit through the checkpointed apply and report it.
///
/// The apply rechecks live read/write, the binding, the qualified filesystem,
/// and that the target still has the exact captured size and digest. The
/// caller has already spent the approval.
fn apply_prepared_edit(
    control: &mut ControlStore,
    binding: &SessionBinding,
    target: &FileTarget,
    call: &ToolCall,
    prepared: &PreparedEdit,
    edits: Option<usize>,
) -> ToolExecution {
    let applied = FileBroker::new(control).apply_replacement(
        binding,
        target,
        &prepared.current_artifact,
        prepared.proposed.as_bytes(),
    );
    match applied {
        // The widest report was checked by the gate, so this one fits.
        Ok(receipt) if receipt.applied => ToolExecution::success(edit_success_message(
            call,
            &prepared.normalized,
            receipt.proposed.bytes,
            &receipt.proposed.sha256,
            receipt.operation_id,
            edits,
        ))
        .with_operation(receipt.operation_id),
        // Different proposed bytes always publish or fail; an unapplied
        // receipt can only mean the bytes were identical.
        Ok(_) => ToolExecution::recoverable("edit_unchanged", call),
        Err(error) => classify_edit_error(call, error),
    }
}

/// Refuse a creation whose target exists or whose parent is missing, before
/// any approval is recorded. Returns `None` when creation may be requested.
///
/// A live audited read must report the path absent, and a one-entry listing
/// of a non-root parent must succeed. Both are audited broker calls under the
/// live `read` permission.
fn create_precheck(
    control: &mut ControlStore,
    binding: &SessionBinding,
    target: &FileTarget,
    call: &ToolCall,
) -> Option<ToolExecution> {
    match read_edit_source(control, binding, target) {
        Ok(_) => return Some(ToolExecution::recoverable("file_exists", call)),
        Err(FileError::Native(native)) if is_missing_entry(&native) => {}
        Err(error) => return Some(classify_edit_error(call, error)),
    }
    let parent = target
        .path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())?;
    let parent_target = FileTarget {
        session_id: target.session_id,
        workspace_id: target.workspace_id,
        path: parent.to_path_buf(),
    };
    match FileBroker::new(control).list(binding, &parent_target, 1) {
        Ok(_) => None,
        Err(FileError::Native(native)) if is_missing_entry(&native) => {
            Some(ToolExecution::recoverable("not_found", call))
        }
        Err(error) => Some(classify_edit_error(call, error)),
    }
}

/// Read the live source of a rename or deletion and describe it by size and
/// SHA-256, or return the recoverable refusal.
///
/// This is the audited read of at most 1 MiB that edit uses, so a larger file
/// is refused as `edit_too_large`. The bytes are not interpreted: neither a
/// rename nor a deletion needs text, so a non-UTF-8 file is accepted.
fn read_move_source(
    control: &mut ControlStore,
    binding: &SessionBinding,
    target: &FileTarget,
    call: &ToolCall,
) -> Result<RecordedArtifact, Box<ToolExecution>> {
    match read_edit_source(control, binding, target) {
        Ok(bytes) => artifact(&bytes)
            .ok_or_else(|| Box::new(ToolExecution::recoverable("edit_too_large", call))),
        Err(FileError::Native(native)) if is_oversize_entry(&native) => {
            Err(Box::new(ToolExecution::recoverable("edit_too_large", call)))
        }
        Err(error) => Err(Box::new(classify_edit_error(call, error))),
    }
}

/// Refuse a rename whose destination exists or whose parent is missing, before
/// any approval is recorded. Returns `None` when the rename may be requested.
fn rename_precheck(
    control: &mut ControlStore,
    binding: &SessionBinding,
    destination: &FileTarget,
    call: &ToolCall,
) -> Option<ToolExecution> {
    create_precheck(control, binding, destination, call)
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
    replacement_digest(
        EDIT_DIGEST_DOMAIN,
        session_id,
        workspace_id,
        path,
        current,
        proposed,
    )
}

/// Bind one model multi-edit like [`edit_digest`], under its own domain tag so
/// an approval for one tool can never be consumed by the other.
fn multi_edit_digest(
    session_id: Uuid,
    workspace_id: WorkspaceId,
    path: &str,
    current: &RecordedArtifact,
    proposed: &RecordedArtifact,
) -> String {
    replacement_digest(
        MULTI_EDIT_DIGEST_DOMAIN,
        session_id,
        workspace_id,
        path,
        current,
        proposed,
    )
}

/// Bind one model file creation to the session, workspace, path, and the
/// exact size and SHA-256 of the new content, under its own domain tag.
fn create_digest(
    session_id: Uuid,
    workspace_id: WorkspaceId,
    path: &str,
    content: &RecordedArtifact,
) -> String {
    let mut bytes = Vec::new();
    push_field(&mut bytes, CREATE_DIGEST_DOMAIN);
    push_field(&mut bytes, session_id.as_bytes());
    push_field(&mut bytes, workspace_id.as_uuid().as_bytes());
    push_field(&mut bytes, path.as_bytes());
    push_field(&mut bytes, &content.bytes.to_le_bytes());
    push_field(&mut bytes, content.sha256.as_bytes());
    sha256_hex(&bytes)
}

/// Bind one model rename to the session, workspace, both normalized names,
/// and the exact size and SHA-256 of the live source, under its own domain tag.
fn rename_digest(
    session_id: Uuid,
    workspace_id: WorkspaceId,
    from: &str,
    to: &str,
    source: &RecordedArtifact,
) -> String {
    let mut bytes = Vec::new();
    push_field(&mut bytes, RENAME_DIGEST_DOMAIN);
    push_field(&mut bytes, session_id.as_bytes());
    push_field(&mut bytes, workspace_id.as_uuid().as_bytes());
    push_field(&mut bytes, from.as_bytes());
    push_field(&mut bytes, to.as_bytes());
    push_field(&mut bytes, &source.bytes.to_le_bytes());
    push_field(&mut bytes, source.sha256.as_bytes());
    sha256_hex(&bytes)
}

/// Bind one model deletion to the session, workspace, normalized path, and the
/// exact size and SHA-256 of the live file, under its own domain tag.
fn delete_digest(
    session_id: Uuid,
    workspace_id: WorkspaceId,
    path: &str,
    current: &RecordedArtifact,
) -> String {
    let mut bytes = Vec::new();
    push_field(&mut bytes, DELETE_DIGEST_DOMAIN);
    push_field(&mut bytes, session_id.as_bytes());
    push_field(&mut bytes, workspace_id.as_uuid().as_bytes());
    push_field(&mut bytes, path.as_bytes());
    push_field(&mut bytes, &current.bytes.to_le_bytes());
    push_field(&mut bytes, current.sha256.as_bytes());
    sha256_hex(&bytes)
}

fn replacement_digest(
    domain: &[u8],
    session_id: Uuid,
    workspace_id: WorkspaceId,
    path: &str,
    current: &RecordedArtifact,
    proposed: &RecordedArtifact,
) -> String {
    let mut bytes = Vec::new();
    push_field(&mut bytes, domain);
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
    let suffix = format!(": {}", size_change(current, proposed));
    bounded_path_summary("edit ", path, &suffix)
}

/// A bounded multi-edit label: `multi-edit {path} ({n} edits): ...`. It never
/// contains file text.
fn multi_edit_summary(
    path: &str,
    edits: usize,
    current: &RecordedArtifact,
    proposed: &RecordedArtifact,
) -> String {
    let suffix = format!(" ({edits} edits): {}", size_change(current, proposed));
    bounded_path_summary("multi-edit ", path, &suffix)
}

/// A bounded creation label: `create {path}: {n} bytes, sha256 {12}`. It never
/// contains file text.
fn create_summary(path: &str, content: &RecordedArtifact) -> String {
    let suffix = format!(
        ": {} bytes, sha256 {}",
        content.bytes,
        short_digest(&content.sha256)
    );
    bounded_path_summary("create ", path, &suffix)
}

/// A bounded rename label: `rename {from} -> {to}: {n} bytes, sha256 {12}`.
/// It never contains file text. Both paths are sanitized like an edit path and
/// share the budget, so the summary fits [`MAX_EDIT_SUMMARY_BYTES`].
fn rename_summary(from: &str, to: &str, source: &RecordedArtifact) -> String {
    let suffix = format!(
        ": {} bytes, sha256 {}",
        source.bytes,
        short_digest(&source.sha256)
    );
    let prefix = "rename ";
    let arrow = " -> ";
    // Split what the fixed text leaves between the two names. A short name
    // leaves its unused share to the other.
    let budget = MAX_EDIT_SUMMARY_BYTES.saturating_sub(prefix.len() + arrow.len() + suffix.len());
    let sanitized = |path: &str| sanitize_label(path);
    let (from, to) = (sanitized(from), sanitized(to));
    let half = budget / 2;
    let (from_budget, to_budget) = if from.len() <= half {
        (from.len(), budget.saturating_sub(from.len()))
    } else if to.len() <= half {
        (budget.saturating_sub(to.len()), to.len())
    } else {
        (half, budget.saturating_sub(half))
    };
    format!(
        "{prefix}{}{arrow}{}{suffix}",
        shorten_label(from, from_budget),
        shorten_label(to, to_budget)
    )
}

/// A bounded deletion label: `delete {path}: {n} bytes, sha256 {12}`. It never
/// contains file text.
fn delete_summary(path: &str, current: &RecordedArtifact) -> String {
    let suffix = format!(
        ": {} bytes, sha256 {}",
        current.bytes,
        short_digest(&current.sha256)
    );
    bounded_path_summary("delete ", path, &suffix)
}

fn short_digest(digest: &str) -> &str {
    digest.get(..12).unwrap_or(digest)
}

fn size_change(current: &RecordedArtifact, proposed: &RecordedArtifact) -> String {
    format!(
        "{} -> {} bytes, sha256 {} -> {}",
        current.bytes,
        proposed.bytes,
        short_digest(&current.sha256),
        short_digest(&proposed.sha256)
    )
}

/// Join a fixed prefix, a sanitized and shortened path, and a fixed suffix so
/// the whole summary fits [`MAX_EDIT_SUMMARY_BYTES`].
fn bounded_path_summary(prefix: &str, path: &str, suffix: &str) -> String {
    let budget = MAX_EDIT_SUMMARY_BYTES
        .saturating_sub(prefix.len())
        .saturating_sub(suffix.len());
    let label = shorten_label(sanitize_label(path), budget);
    format!("{prefix}{label}{suffix}")
}

/// Replace control, invisible, and bidirectional formatting characters with
/// `?`, so a path cannot hide or reorder displayed text.
fn sanitize_label(path: &str) -> String {
    path.chars()
        .map(|character| {
            if character.is_control() || is_hidden_format(character) {
                '?'
            } else {
                character
            }
        })
        .collect()
}

/// Shorten a sanitized label to `budget` bytes on a character boundary,
/// ending in `...` when it was cut.
fn shorten_label(mut label: String, budget: usize) -> String {
    if label.len() > budget {
        let mut end = budget.saturating_sub(3);
        while !label.is_char_boundary(end) {
            end -= 1;
        }
        label.truncate(end);
        label.push_str("...");
    }
    label
}

/// Invisible, line-breaking, or bidirectional formatting characters that can
/// hide or reorder displayed text.
///
/// Categories: bidirectional and zero-width marks (U+061C, U+200B to U+200F,
/// U+2028 to U+202E, U+2060 to U+2069, U+FEFF); characters that render as
/// nothing or merge into a neighbour (soft hyphen U+00AD, combining grapheme
/// joiner U+034F, Hangul fillers U+115F, U+1160, U+3164, U+FFA0, Khmer
/// invisible vowels U+17B4 to U+17B5, Mongolian selectors and separator
/// U+180B to U+180F); variation selectors (U+FE00 to U+FE0F); interlinear
/// annotation anchors and the object replacement character (U+FFF9 to
/// U+FFFC); musical formatting controls (U+1D173 to U+1D17A); and the tag
/// characters with the variation selectors supplement (U+E0000 to U+E0FFF).
///
/// This list does not cover look-alike letters, mixed scripts, or ordinary
/// spaces.
const fn is_hidden_format(character: char) -> bool {
    matches!(
        character,
        '\u{00ad}'
            | '\u{034f}'
            | '\u{061c}'
            | '\u{115f}'..='\u{1160}'
            | '\u{17b4}'..='\u{17b5}'
            | '\u{180b}'..='\u{180f}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{2069}'
            | '\u{3164}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{ffa0}'
            | '\u{fff9}'..='\u{fffc}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0000}'..='\u{e0fff}'
    )
}

/// Tell the model that an operator must approve the edit before a retry.
fn file_write_approval_required(
    call: &ToolCall,
    approval_id: Uuid,
    noun: &str,
    max_result_bytes: usize,
) -> ToolExecution {
    status_result(
        call,
        ToolOutcome::Failed,
        "approval_required",
        json!({
            "approval_id": approval_id,
            "message": format!("Nothing changed. The operator must approve this {noun}. Stop now and retry the identical call in a later turn after approval."),
        }),
        max_result_bytes,
    )
    .about(approval_id)
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
        // can inspect and acknowledge. A missing or changed workspace
        // checkpoint folder needs the operator too (ADR-0070). Further broker
        // work is blocked anyway, so continuing the turn cannot help.
        CheckpointError::Control(_)
        | CheckpointError::Io(_)
        | CheckpointError::Records(_)
        | CheckpointError::Json(_)
        | CheckpointError::Invalid(_)
        | CheckpointError::Poisoned
        | CheckpointError::StoreMissing
        | CheckpointError::StoreChanged(_)
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
        approval_id: None,
        waited: Duration::ZERO,
        operation_id: None,
        audited: false,
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
        // Windows reports an absent name as a native status inside an
        // `ErrorKind::Other` error; it is the same recoverable result.
        FileError::Native(native) if is_missing_entry(&native) => {
            ToolExecution::recoverable("not_found", call)
        }
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
mod skill_dispatch_tests;

#[cfg(test)]
mod summary_tests {
    use super::{
        MAX_EDIT_SUMMARY_BYTES, TextMatch, artifact, command_summary, create_digest,
        create_summary, edit_digest, edit_summary, multi_edit_digest, multi_edit_summary,
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
    fn multi_edit_and_create_summaries_are_bounded_and_hold_no_text() {
        let current = artifact(b"secret current text").expect("current");
        let proposed = artifact(b"secret proposed text").expect("proposed");
        let summary = multi_edit_summary("src/main.rs", 3, &current, &proposed);
        assert_eq!(
            summary,
            format!(
                "multi-edit src/main.rs (3 edits): 19 -> 20 bytes, sha256 {} -> {}",
                &current.sha256[..12],
                &proposed.sha256[..12]
            )
        );
        let created = create_summary("src/new.rs", &current);
        assert_eq!(
            created,
            format!(
                "create src/new.rs: 19 bytes, sha256 {}",
                &current.sha256[..12]
            )
        );
        assert!(!summary.contains("secret") && !created.contains("secret"));

        let hostile = format!("{}\u{202e}\u{7}\u{2028}{}", "d/".repeat(40), "é".repeat(80));
        for summary in [
            multi_edit_summary(&hostile, 16, &current, &proposed),
            create_summary(&hostile, &current),
        ] {
            assert!(summary.len() <= MAX_EDIT_SUMMARY_BYTES, "{}", summary.len());
            assert!(!summary.chars().any(char::is_control));
            assert!(!summary.contains('\u{202e}') && !summary.contains('\u{2028}'));
            assert!(summary.contains("..."));
        }
    }

    #[test]
    fn the_three_file_write_digests_never_collide() {
        let session = uuid::Uuid::from_u128(1);
        let workspace = crate::policy::WorkspaceId::fresh();
        let current = artifact(b"alpha").expect("current");
        let proposed = artifact(b"beta").expect("proposed");
        let edit = edit_digest(session, workspace, "a.txt", &current, &proposed);
        let multi = multi_edit_digest(session, workspace, "a.txt", &current, &proposed);
        let create = create_digest(session, workspace, "a.txt", &proposed);
        assert_ne!(edit, multi);
        assert_ne!(edit, create);
        assert_ne!(multi, create);
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
