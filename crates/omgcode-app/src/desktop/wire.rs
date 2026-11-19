//! Request and response shapes of the desktop command surface.
//!
//! These types are the Rust side of the wire contract that the interface
//! declares in `crates/omgcode-desktop/ui/src/ipc/types.ts`. Field names are
//! `snake_case`; an optional value that the interface types as `T | null` is
//! always present and serializes as `null` when absent.
//!
//! Requests derive `Deserialize` with `deny_unknown_fields`, so a caller cannot
//! smuggle an extra field, and no request type carries a filesystem path.
//! Responses derive `Serialize` only. Every response label that comes from
//! recorded data has already passed through core's display-safe escaping where
//! the service contract says so; transcript text, tool arguments, and search
//! snippets are returned as recorded and must be rendered as text.

use omgcode_core::control::{ApprovalOperation, ApprovalState};
use omgcode_core::session::{MessageRole, MessageSource, ToolOutcome};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::error::DesktopError;

/// Whether a listing or search covers active or archived sessions.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TreeView {
    /// Sessions that are not archived.
    Active,
    /// Archived sessions.
    Archived,
}

/// The key a session listing sorts by.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TreeSort {
    /// Time of the last complete event.
    Updated,
    /// Time of the session start.
    Created,
    /// Title, case-insensitive.
    Title,
}

/// Arguments of `session_tree`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TreeRequest {
    /// Active or archived sessions.
    pub view: TreeView,
    /// The sort key.
    pub sort: TreeSort,
    /// Sort largest or latest first (last to first for titles).
    pub descending: bool,
}

/// Arguments of `session_search`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    /// The text to search for. It is never search-engine syntax.
    pub query: String,
    /// Active or archived sessions.
    pub view: TreeView,
    /// Most hits to return, clamped to 1 through 200.
    pub limit: u32,
}

/// Arguments of `session_transcript`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TranscriptRequest {
    /// The session to read.
    pub session_id: Uuid,
    /// Return entries with a larger sequence number; `null` starts at the top.
    pub after_seq: Option<u64>,
    /// Most entries to return, clamped to 1 through 200.
    pub limit: u32,
}

/// Arguments of every command that names only a session.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SessionRequest {
    /// The session to act on.
    pub session_id: Uuid,
}

/// Arguments of `session_create`. A caller names a registered workspace by
/// identifier and never supplies a path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SessionCreateRequest {
    /// A workspace the operator registered with the trusted CLI.
    pub workspace_id: Uuid,
    /// The new session's title; core validates it.
    pub title: String,
}

/// Chat settings as the interface sends and receives them.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChatSettingsDto {
    /// Must equal the endpoint the service was opened with.
    pub endpoint: String,
    /// The model identifier available on the selected server.
    pub model: String,
    /// Validated context capacity of the loaded model.
    pub context_tokens: u64,
    /// Output reserve for each model request.
    pub max_output_tokens: u32,
    /// User-level preferences, sent as untrusted-origin custom instructions.
    pub instructions: String,
    /// Compact before a turn when the estimate reaches this value.
    pub compact_threshold: u64,
    /// Whether to compact at the effective trigger before a turn.
    pub auto_compact: bool,
}

/// Arguments of `chat_send`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ChatSendRequest {
    /// The session to chat in.
    pub session_id: Uuid,
    /// The user's message.
    pub message: String,
    /// Model and budgets for this turn.
    pub settings: ChatSettingsDto,
    /// Names of skills the operator selected for this turn only (ADR-0057).
    /// Each must be enabled for the session's workspace. Empty for none.
    pub skills: Vec<String>,
}

/// Arguments of `approval_detail`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequest {
    /// The approval record to review.
    pub id: Uuid,
}

/// The operator's decision.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Grant the approval.
    Approve,
    /// Refuse the approval.
    Deny,
}

/// Arguments of `approval_decide`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ApprovalDecideRequest {
    /// The approval record to decide.
    pub id: Uuid,
    /// Approve or deny.
    pub decision: Decision,
    /// The operator's writers-stopped acknowledgement; used for `file_write`.
    pub writers_stopped: bool,
    /// The token `approval_detail` returned; required to approve.
    pub review_token: Option<String>,
}

/// How a session is doing, for display.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// Nothing is running and no turn is open.
    Idle,
    /// A chat turn is running in this service.
    Running,
    /// The journal records a turn that never finished.
    TurnOpen,
    /// Another process holds the journal.
    Busy,
    /// The journal failed validation.
    Invalid,
}

/// One session as the tree, search, and lifecycle commands show it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SessionSummary {
    /// The session identifier.
    pub session_id: Uuid,
    /// Display-safe title, or a fixed placeholder when none is known.
    pub title: String,
    /// RFC 3339 start time, when known.
    pub created_at: Option<String>,
    /// RFC 3339 time of the last event, when known.
    pub updated_at: Option<String>,
    /// The session's status.
    pub status: SessionStatus,
    /// Whether the session is archived.
    pub archived: bool,
    /// Number of complete journal events.
    pub event_count: u64,
    /// Stable failure code for an `invalid` journal.
    pub invalid_code: Option<String>,
}

/// Sessions that share a recorded workspace folder.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkspaceGroup {
    /// An opaque stable identifier of the folder.
    pub group_id: String,
    /// Display-safe folder text. Never used to open anything.
    pub folder_display: String,
    /// The group's sessions in listing order.
    pub sessions: Vec<SessionSummary>,
}

/// The `session_tree` response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SessionTree {
    /// Groups in order of first appearance.
    pub groups: Vec<WorkspaceGroup>,
    /// How many sessions the view holds before the listing limit applied.
    pub indexed_sessions: u64,
    /// Directory entries the last refresh skipped as links or unrecognized.
    pub skipped_entries: u64,
}

/// A run of snippet text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SnippetPart {
    /// Recorded text.
    pub text: String,
    /// Whether the search matched this run.
    pub matched: bool,
}

/// What kind of text a search hit matched.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchRoleDto {
    /// A user message.
    User,
    /// An assistant message.
    Assistant,
    /// A tool result message.
    Tool,
    /// A compaction summary.
    Summary,
    /// The session title.
    Title,
}

/// One matching text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SearchHit {
    /// The session the text belongs to.
    pub session: SessionSummary,
    /// Display-safe folder text.
    pub folder_display: String,
    /// The journal sequence number, or `null` for a title.
    pub seq: Option<u64>,
    /// What matched.
    pub role: SearchRoleDto,
    /// Text around the match.
    pub snippet: Vec<SnippetPart>,
}

/// The `session_search` response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SearchResult {
    /// Hits in relevance order.
    pub hits: Vec<SearchHit>,
    /// Whether more hits exist than the limit allowed.
    pub truncated: bool,
}

/// The role of a recorded message.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRoleDto {
    /// A user message.
    User,
    /// An assistant message.
    Assistant,
    /// A tool result message.
    Tool,
}

impl From<MessageRole> for MessageRoleDto {
    fn from(role: MessageRole) -> Self {
        match role {
            MessageRole::User => Self::User,
            MessageRole::Assistant => Self::Assistant,
            MessageRole::Tool => Self::Tool,
        }
    }
}

/// The core-observed outcome of a tool call.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcomeDto {
    /// The operation completed.
    Succeeded,
    /// The request failed.
    Failed,
    /// Policy denied the operation.
    Denied,
}

impl From<ToolOutcome> for ToolOutcomeDto {
    fn from(outcome: ToolOutcome) -> Self {
        match outcome {
            ToolOutcome::Succeeded => Self::Succeeded,
            ToolOutcome::Failed => Self::Failed,
            ToolOutcome::Denied => Self::Denied,
        }
    }
}

/// The recorded origin label of a message, as text.
pub(super) const fn source_label(source: MessageSource) -> &'static str {
    match source {
        MessageSource::UserCli => "user_cli",
        MessageSource::Provider => "provider",
        MessageSource::CoreTool => "core_tool",
    }
}

/// The recorded data of one transcript event. All text is untrusted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntryKind {
    /// A user, assistant, or tool message.
    Message {
        /// Display role.
        role: MessageRoleDto,
        /// Recorded origin label.
        source: String,
        /// Message text as recorded.
        text: String,
        /// Tool call identifier, only on tool messages.
        call_id: Option<String>,
    },
    /// A turn began.
    TurnStarted {
        /// The turn identifier.
        request_id: Uuid,
    },
    /// A turn completed.
    TurnCompleted {
        /// The turn identifier.
        request_id: Uuid,
    },
    /// A turn failed.
    TurnFailed {
        /// The turn identifier.
        request_id: Uuid,
        /// The recorded failure code.
        code: Option<String>,
    },
    /// The model asked for a tool call.
    ToolRequested {
        /// The turn identifier.
        request_id: Uuid,
        /// Provider call identifier.
        call_id: String,
        /// Requested tool name.
        tool: String,
        /// Exact arguments text as recorded.
        arguments: String,
    },
    /// Core recorded a tool call's outcome.
    ToolCompleted {
        /// The turn identifier.
        request_id: Uuid,
        /// Provider call identifier.
        call_id: String,
        /// The observed outcome.
        outcome: ToolOutcomeDto,
        /// Stable error code for a failed or denied call.
        code: Option<String>,
        /// Size of the rendered output.
        output_bytes: u64,
    },
    /// The descriptive part of a request manifest.
    RequestManifest {
        /// The turn identifier.
        request_id: Uuid,
        /// The model used.
        model: String,
        /// The compiled base prompt's version label.
        prompt_version: String,
        /// Tool names advertised to the model.
        tools: Vec<String>,
    },
    /// A summary replaced earlier messages.
    ContextCompacted {
        /// The summary text, which is untrusted model output.
        summary: String,
        /// How many messages it represents.
        compacted_messages: u64,
        /// The summarizer model.
        model: String,
    },
    /// The session was archived.
    SessionArchived,
    /// The session was restored.
    SessionRestored,
}

/// One transcript event.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TranscriptEntry {
    /// Zero-based journal sequence number.
    pub seq: u64,
    /// RFC 3339 event time.
    pub timestamp: String,
    /// The event and its data.
    #[serde(flatten)]
    pub kind: EntryKind,
}

/// The `session_transcript` response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TranscriptPage {
    /// The session, summarized from the inspection this page came from.
    pub session: SessionSummary,
    /// Display-safe folder text.
    pub folder_display: String,
    /// Entries in journal order.
    pub entries: Vec<TranscriptEntry>,
    /// The last returned sequence number when more entries remain.
    pub next_after_seq: Option<u64>,
    /// Whether bytes followed the last complete record when inspected.
    pub incomplete_tail: bool,
}

/// How full the model context is.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ContextMeterDto {
    /// Conservative estimate of the input tokens.
    pub estimated_tokens: u64,
    /// The effective compaction trigger.
    pub trigger_tokens: u64,
    /// The model's context capacity.
    pub context_tokens: u64,
    /// Whether a compaction summary is active.
    pub compacted: bool,
}

/// Whether the turn offered workspace tools.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolsMode {
    /// Tools were offered.
    Enabled,
    /// A text-only turn.
    Disabled,
}

/// What a completed turn reports.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ChatOutcome {
    /// The provider finish reason.
    pub finish_reason: String,
    /// Whether tools were offered.
    pub tools_mode: ToolsMode,
    /// Why a text-only turn offered no tools.
    pub tools_disabled_reason: Option<String>,
    /// Model requests made; 0 for a text-only turn.
    pub rounds: u64,
    /// Tool calls dispatched; 0 for a text-only turn.
    pub tool_calls: u64,
    /// Saved tool results replaced by a withheld notice.
    pub withheld_tool_results: u64,
    /// The context accounting.
    pub context: Option<ContextMeterDto>,
}

/// The state of a session's chat turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatState {
    /// The service has no record for the session.
    Idle,
    /// A turn is running.
    Running,
    /// A turn is running and cancellation was requested.
    Cancelling,
    /// The last turn completed.
    Completed,
    /// The last turn failed.
    Failed,
}

/// The `chat_*` response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ChatStatus {
    /// The session.
    pub session_id: Uuid,
    /// The turn state.
    pub state: ChatState,
    /// The outcome of a completed turn.
    pub outcome: Option<ChatOutcome>,
    /// The failure of a failed turn.
    pub error: Option<DesktopError>,
}

/// One pushed chat event (ADR-0049).
///
/// Events carry provisional text only. The journal and `chat_status` hold the
/// record; `Delta` text is already token-redacted but is otherwise as the model
/// sent it, so a front end must render it as text. The `kind` member names the
/// variant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatEvent {
    /// A model request of the turn began. Earlier preview text is superseded.
    RequestStarted {
        /// The session.
        session_id: Uuid,
        /// The request number within the turn, counted from 1.
        request: u32,
    },
    /// Provisional assistant text of the current request, at most 1,024 bytes.
    Delta {
        /// The session.
        session_id: Uuid,
        /// The request number within the turn.
        request: u32,
        /// The text.
        text: String,
    },
    /// The 16 KiB preview bound was reached for this request; later text of the
    /// request is not sent.
    PreviewTruncated {
        /// The session.
        session_id: Uuid,
        /// The request number within the turn.
        request: u32,
    },
    /// The turn left `running`. The caller reads `chat_status`.
    Finished {
        /// The session.
        session_id: Uuid,
    },
}

/// A workspace the operator registered, with its saved preferences.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkspaceChoice {
    /// The workspace identifier.
    pub workspace_id: Uuid,
    /// Display-safe folder text.
    pub folder_display: String,
    /// The model-tools preference.
    pub model_tools: bool,
    /// The read preference.
    pub read: bool,
    /// The write preference.
    pub write: bool,
    /// The execute preference.
    pub execute: bool,
}

/// The `workspace_list` response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkspaceList {
    /// Whether the operator selected a development root.
    pub development_root_set: bool,
    /// Registered workspaces.
    pub workspaces: Vec<WorkspaceChoice>,
}

/// The `approvals_summary` response.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ApprovalsSummary {
    /// How many approvals are pending.
    pub pending: u64,
}

/// What an approval covers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOperationDto {
    /// Start a confined command.
    Command,
    /// Replace a workspace file.
    FileWrite,
    /// A Git operation with remote access.
    GitPush,
    /// A destination-scoped network operation.
    Network,
}

impl From<ApprovalOperation> for ApprovalOperationDto {
    fn from(operation: ApprovalOperation) -> Self {
        match operation {
            ApprovalOperation::Command => Self::Command,
            ApprovalOperation::FileWrite => Self::FileWrite,
            ApprovalOperation::GitPush => Self::GitPush,
            ApprovalOperation::Network => Self::Network,
        }
    }
}

/// Where an approval stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStateDto {
    /// Waiting for a decision.
    Pending,
    /// Granted, not yet used.
    Approved,
    /// Refused.
    Denied,
    /// Used by exactly one call.
    Consumed,
    /// Past its lifetime.
    Expired,
}

impl From<ApprovalState> for ApprovalStateDto {
    fn from(state: ApprovalState) -> Self {
        match state {
            ApprovalState::Pending => Self::Pending,
            ApprovalState::Approved => Self::Approved,
            ApprovalState::Denied => Self::Denied,
            ApprovalState::Consumed => Self::Consumed,
            ApprovalState::Expired => Self::Expired,
        }
    }
}

/// One approval record, joined with its session's label.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ApprovalView {
    /// The approval identifier.
    pub id: Uuid,
    /// The session that asked.
    pub session_id: Uuid,
    /// Display-safe session title, when the index knows the session.
    pub session_title: Option<String>,
    /// Display-safe folder text, when the index knows the session.
    pub folder_display: Option<String>,
    /// What the approval covers.
    pub operation: ApprovalOperationDto,
    /// Display-safe one-line summary.
    pub summary: String,
    /// Where the approval stands.
    pub state: ApprovalStateDto,
    /// Unix time the request was recorded.
    pub created_unix: u64,
    /// Unix time the approval expires.
    pub expires_unix: u64,
    /// SHA-256 of the exact request the approval is bound to.
    pub request_sha256: String,
    /// Whether the operator acknowledged that other writers are stopped.
    pub writers_stopped: bool,
}

/// The `approvals_list` response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ApprovalList {
    /// The service's current Unix time.
    pub now_unix: u64,
    /// Pending approvals first, then the most recent others.
    pub approvals: Vec<ApprovalView>,
}

/// One labelled, display-safe value of a reviewed request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ApprovalField {
    /// The field label.
    pub label: String,
    /// The display-safe value.
    pub value: String,
    /// Whether the value may span lines.
    pub multiline: bool,
}

/// Whether the recorded request that an approval is bound to was found.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Binding {
    /// A recorded request reproduces the approval's digest.
    Exact,
    /// Something of the right kind exists but nothing matches the digest.
    Stale,
    /// No recorded request exists to review.
    NotFound,
}

/// The `approval_detail` response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ApprovalDetail {
    /// The approval record.
    pub approval: ApprovalView,
    /// The review result.
    pub binding: Binding,
    /// The reviewed tool, only for an `exact` binding.
    pub tool: Option<String>,
    /// The reviewed fields, only for an `exact` binding.
    pub fields: Vec<ApprovalField>,
    /// A single-use token that allows one approve, only for an `exact`,
    /// pending approval.
    pub review_token: Option<String>,
}

/// Chat defaults the `main` window may save (ADR-0059 decision 8). They grant
/// nothing: `chat_send` validates every value again when it is used. The
/// endpoint is not a preference; it stays fixed by the application.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChatPreferencesDto {
    /// The model identifier to start from.
    pub model: String,
    /// The context size to start from; `0` means not set.
    pub context_tokens: u64,
    /// The output reserve to start from.
    pub max_output_tokens: u32,
    /// Custom instructions to start from. Untrusted-origin text, as in a turn.
    pub instructions: String,
    /// The compaction threshold to start from.
    pub compact_threshold: u64,
    /// Whether auto-compaction starts enabled.
    pub auto_compact: bool,
}

/// Arguments of `chat_preferences_save`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ChatPreferencesSaveRequest {
    /// The preferences to save.
    pub preferences: ChatPreferencesDto,
}

/// Where the chat defaults came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferencesSource {
    /// No preferences are saved; built-in defaults apply.
    Defaults,
    /// Saved preferences apply.
    Saved,
    /// A preferences file exists but could not be read or failed validation.
    /// Built-in defaults apply. Saving again replaces the file.
    Unreadable,
}

/// The `chat_defaults` response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ChatDefaults {
    /// The settings the composer starts from. The endpoint is the fixed one.
    pub settings: ChatSettingsDto,
    /// Where `settings` came from.
    pub preferences: PreferencesSource,
}

/// The `session_skills` response: skills the operator may select for one turn
/// in this session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SessionSkills {
    /// Whether the session's recorded workspace is registered here. When it
    /// is not, no skill can be selected.
    pub workspace_registered: bool,
    /// Names of the skills effectively enabled for that workspace, in
    /// registration order. A name is a validated skill name, not a path.
    pub skills: Vec<String>,
}

/// One registered workspace as the `settings` window shows it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkspaceSettingsView {
    /// The workspace identifier.
    pub workspace_id: Uuid,
    /// Display-safe folder text.
    pub folder_display: String,
    /// The model-tools preference.
    pub model_tools: bool,
    /// The read preference.
    pub read: bool,
    /// The write preference.
    pub write: bool,
    /// The execute preference.
    pub execute: bool,
    /// The task-network preference.
    pub task_network: bool,
    /// The host-services preference.
    pub host_services: bool,
    /// Display-safe text of the selected package cache root, if any.
    pub package_cache_root: Option<String>,
}

/// Arguments of a settings change that names only the expected revision
/// (`settings_choose_root`, `settings_register_workspace`). The folder comes
/// from a native dialog that Rust opens, never from the caller (ADR-0059
/// decision 4).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SettingsRevisionRequest {
    /// The settings revision the window last read.
    pub revision: u64,
}

/// Arguments of a settings change for one registered workspace
/// (`settings_choose_package_cache`, `settings_clear_package_cache`).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRevisionRequest {
    /// A registered workspace identifier.
    pub workspace_id: Uuid,
    /// The settings revision the window last read.
    pub revision: u64,
}

/// The complete preference set a window asks for. Every flag is required,
/// so a missing field is an error rather than a silent default.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionFlags {
    /// Offer the local model the workspace tools.
    pub model_tools: bool,
    /// Allow reading workspace data.
    pub read: bool,
    /// Allow writing workspace data.
    pub write: bool,
    /// Allow process execution.
    pub execute: bool,
    /// Allow task network access.
    pub task_network: bool,
    /// Allow host-service Unix-socket access for a trusted `run`.
    pub host_services: bool,
}

/// Arguments of `settings_set_permissions`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PermissionsChangeRequest {
    /// A registered workspace identifier.
    pub workspace_id: Uuid,
    /// The settings revision the window last read.
    pub revision: u64,
    /// The complete preference set wanted.
    pub permissions: PermissionFlags,
}

/// How this process obtains its model connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionSourceView {
    /// The launch endpoint (or its default) and `OMGCODE_LM_TOKEN`.
    Environment,
    /// A provider profile selected with `--provider-profile` at launch.
    Profile,
    /// A token supplier injected by an embedder or a test.
    Injected,
}

/// The model connection of this process, for the `settings` window only.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ConnectionView {
    /// The endpoint every turn uses.
    pub endpoint: String,
    /// Where the endpoint and token come from.
    pub source: ConnectionSourceView,
    /// The selected profile name when `source` is `profile`.
    pub profile: Option<String>,
}

/// One recorded provider profile. It never carries a secret.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProviderProfileView {
    /// The profile name.
    pub name: String,
    /// The provider kind, as the CLI names it.
    pub kind: &'static str,
    /// The authentication mode, as the CLI names it.
    pub auth: &'static str,
    /// The recorded endpoint, if any.
    pub endpoint: Option<String>,
    /// Whether a stored credential is bound. The credential itself is never
    /// read for this view.
    pub credential_bound: bool,
}

/// One recorded toolchain profile. Variable values are not shown.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolchainProfileView {
    /// The profile name.
    pub name: String,
    /// `linux` or `windows`.
    pub platform: &'static str,
    /// Display-safe read-only roots.
    pub read_only_roots: Vec<String>,
    /// Display-safe `PATH` entries.
    pub path_entries: Vec<String>,
    /// Names of the extra environment variables.
    pub environment_names: Vec<String>,
}

/// One recorded package-network profile. Configuration only.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PackageProfileView {
    /// The profile name.
    pub name: String,
    /// The declared origins.
    pub origins: Vec<String>,
    /// How many origins have a bound registry credential.
    pub credentials_bound: u64,
}

/// One recorded container grant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContainerGrantView {
    /// The grant identifier.
    pub grant_id: Uuid,
    /// The workspace whose container holds the grant.
    pub workspace_id: Uuid,
    /// Display-safe text of the granted directory.
    pub directory: String,
    /// `read_execute` or `read_write`.
    pub access: &'static str,
    /// `pending`, `active`, or `revoking`.
    pub state: &'static str,
}

/// One workspace's override of a skill's application setting.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SkillOverrideView {
    /// The workspace.
    pub workspace_id: Uuid,
    /// Whether the override enables the skill there.
    pub enabled: bool,
}

/// One registered skill's settings. The description and body are not shown:
/// they come from the imported package and are untrusted text (ADR-0059).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SkillSettingView {
    /// The validated skill name.
    pub name: String,
    /// The application setting, or `null` when unset (disabled).
    pub application: Option<bool>,
    /// Workspace overrides.
    pub workspace_overrides: Vec<SkillOverrideView>,
}

/// The view of the trusted configuration that every `settings` window command
/// returns, for the `settings` window only.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SettingsOverview {
    /// The global settings revision. A change must name it.
    pub revision: u64,
    /// Display-safe development root, if one is set.
    pub development_root: Option<String>,
    /// Registered workspaces.
    pub workspaces: Vec<WorkspaceSettingsView>,
    /// This process's model connection.
    pub connection: ConnectionView,
    /// Recorded provider profiles.
    pub provider_profiles: Vec<ProviderProfileView>,
    /// Recorded toolchain profiles.
    pub toolchain_profiles: Vec<ToolchainProfileView>,
    /// Recorded package-network profiles.
    pub package_profiles: Vec<PackageProfileView>,
    /// Recorded container grants.
    pub container_grants: Vec<ContainerGrantView>,
    /// Registered skills and their settings.
    pub skills: Vec<SkillSettingView>,
}
