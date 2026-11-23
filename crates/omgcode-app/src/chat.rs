//! One chat turn against the selected local model, without any front end.
//!
//! This module performs no printing, no environment reads, no argument
//! parsing, and never exits the process. Whether the turn offers workspace
//! tools is decided from live settings by the core; this module grants no
//! authority. The settings store is opened and dropped inside its own steps and
//! is never held while a model request is in flight.

mod output;
mod preflight;
mod skills;
mod stream;
mod text;
mod tools;

use std::path::Path;

use omgcode_core::context::DEFAULT_COMPACT_THRESHOLD;
use omgcode_core::prompt::{self, BASE_PROMPT, BASE_PROMPT_VERSION, SkillPrompt};
use omgcode_core::provider::{LocalModelClient, LocalModelConfig};
use omgcode_core::session::{ManifestSkill, RequestManifest, SessionJournal};
use omgcode_core::skills::SkillName;
use omgcode_core::turn::{
    ApprovalWaitSlot, CancelToken, DEFAULT_MAX_TOOL_CALLS, MAX_TOOL_CALLS, MAX_TURN_DEADLINE,
};

pub use self::output::ChatTurnOutput;
pub use self::skills::{LoadedSkillSummary, validate_selection as validate_skill_selection};
use crate::context::summary_output_tokens;
use crate::error::{AppError, ErrorKind};
use crate::secrets::redact_known_secret;
use crate::tool_mode::{TOOL_SCHEMA_ESTIMATE_BYTES, ToolMode, select};

/// Default cooperative deadline for a tool-enabled chat turn, in seconds.
///
/// Each model request may take up to 30 seconds, and a turn that reads,
/// writes, and checks several files makes several requests in sequence, so
/// two minutes lets an ordinary multi-step turn finish (ADR-0077).
pub const DEFAULT_TURN_TIMEOUT_SECONDS: u64 = 120;

/// Selected model, budgets, and user-level preferences for chat turns.
///
/// The settings carry no credential. They are user preferences and cannot
/// replace the compiled base prompt or grant any workspace authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatSettings {
    /// A literal loopback HTTP URL with an explicit port and `/v1` path.
    pub endpoint: String,
    /// The model identifier already available to the selected server.
    pub model: String,
    /// Validated context capacity for the selected loaded model.
    pub context_tokens: u64,
    /// Output reserve for each model request.
    pub max_output_tokens: u32,
    /// Maximum model tool calls in this turn, across all responses.
    pub max_tool_calls: usize,
    /// Cooperative deadline for tool-enabled turns, excluding approval waits.
    pub turn_timeout_seconds: u64,
    /// User-level preferences, sent as untrusted-origin custom instructions.
    pub instructions: String,
    /// Compact before a turn when the conservative estimate reaches this value.
    pub compact_threshold: u64,
    /// Whether to compact at the effective trigger before a turn.
    pub auto_compact: bool,
}

impl ChatSettings {
    /// Settings with the CLI's defaults for everything but the model and its context.
    pub fn new(endpoint: impl Into<String>, model: impl Into<String>, context_tokens: u64) -> Self {
        Self {
            endpoint: endpoint.into(),
            model: model.into(),
            context_tokens,
            max_output_tokens: 1024,
            max_tool_calls: DEFAULT_MAX_TOOL_CALLS,
            turn_timeout_seconds: DEFAULT_TURN_TIMEOUT_SECONDS,
            instructions: String::new(),
            compact_threshold: DEFAULT_COMPACT_THRESHOLD,
            auto_compact: false,
        }
    }
}

pub(crate) fn validate_turn_budgets(
    max_tool_calls: usize,
    timeout_seconds: u64,
) -> Result<(), String> {
    if !(1..=MAX_TOOL_CALLS).contains(&max_tool_calls) {
        return Err(format!(
            "Tool calls per turn must be between 1 and {MAX_TOOL_CALLS}."
        ));
    }
    let max_timeout_seconds = MAX_TURN_DEADLINE.as_secs();
    if !(1..=max_timeout_seconds).contains(&timeout_seconds) {
        return Err(format!(
            "Tool-enabled turn timeout must be between 1 and {max_timeout_seconds} seconds."
        ));
    }
    Ok(())
}

/// The inputs of one chat turn.
///
/// It has no `Debug` implementation because `token` is a credential.
pub struct ChatTurnInput<'a> {
    /// The current user request. Known-token text is removed before use.
    pub message: &'a str,
    /// The model and budgets for this turn.
    pub settings: &'a ChatSettings,
    /// The optional local server token, used for the request header and for
    /// removing the same text from everything recorded or sent. It is borrowed
    /// for the call and never stored, logged, or serialized.
    pub token: Option<&'a str>,
    /// Skills the operator selected for this turn only, at most four and none
    /// repeated. Each must be registered and enabled for the session's
    /// workspace, or the turn is refused before any model request. A skill is
    /// untrusted text: it grants no permission and is never recorded in the
    /// session history, replayed, or compacted. Pass `&[]` for none.
    pub skills: &'a [SkillName],
}

/// One provisional event of a streamed model request (ADR-0049).
///
/// Streamed text is provisional and untrusted. The journal, not this text, is
/// the record: a request that fails later leaves text that was never a reply.
/// The turn has already removed the known server token from `Text`, but the
/// text is otherwise as the model sent it, so a front end must render it as
/// text, never as markup or terminal control data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChatStreamEvent<'a> {
    /// A model request of the turn is about to start. Text of earlier requests
    /// of the same turn is superseded.
    RequestStarted,
    /// A non-empty piece of assistant text of the current request.
    Text(&'a str),
}

/// Optional callbacks and cancellation source a front end can supply to a turn.
///
/// Every turn has one cancel token, created before compaction and before any
/// model request (ADR-0051). Cancellation is cooperative: the core checks it
/// between rounds and tool calls, and every model request, streamed or not,
/// including the compaction requests before a turn, watches it and stops
/// within about 25 ms (ADR-0050, ADR-0051). A tool call already running is
/// not interrupted, except a command, whose run is cancelled. One hook
/// delivers the token, and the other streams provisional text.
#[derive(Default)]
pub struct ChatHooks<'a> {
    /// The token the turn uses. When `None`, the turn creates its own. A front
    /// end that must be able to cancel before the turn starts, such as an
    /// interrupt watcher installed beforehand, supplies it here. A token
    /// already set when the turn starts ends it as cancelled before any model
    /// request.
    pub cancel: Option<CancelToken>,
    /// Called once with a clone of the turn's token, after the session checks
    /// and before the compaction check and the first model request, on both
    /// the text-only and the tool-enabled path. The callback runs on the
    /// turn's thread and must not block; hand the token to another thread and
    /// return.
    pub on_cancel_token: Option<&'a mut dyn FnMut(CancelToken)>,
    /// When set, every model request of the turn is streamed and this callback
    /// receives [`ChatStreamEvent::RequestStarted`] before each request and
    /// [`ChatStreamEvent::Text`] for each piece. It runs on the turn's thread
    /// while the response is being read, so it must not block and must not
    /// hold a lock that the turn's caller needs. When `None`, every request is
    /// non-streaming. Compaction requests are never streamed, and tool-call
    /// names and arguments are never passed to it.
    pub on_stream: Option<&'a mut dyn FnMut(ChatStreamEvent<'_>)>,
    /// Wait inside the turn for the operator to decide each approval a model
    /// call records, then complete the same call (ADR-0063). The turn
    /// publishes the call it waits on in the slot, so the approvals window
    /// can review it while the turn holds the journal. The desktop shell sets
    /// it; `None` keeps the earlier behaviour, where the model must repeat the
    /// call in a later turn. Cancellation still interrupts the wait.
    pub approval_wait: Option<ApprovalWaitSlot>,
}

/// The two validated model clients a turn uses.
///
/// Building them validates the endpoint and token before any storage is
/// opened. The clients keep their own copy of the token header; they do not
/// implement `Debug` output of it.
pub struct ChatClients {
    chat: LocalModelClient,
    summary: LocalModelClient,
}

impl ChatClients {
    /// Validate `settings` and build the chat client and the summary client.
    ///
    /// Returns `configuration` for an out-of-range turn budget and
    /// `provider` for an unusable endpoint, model budget, or token. No network
    /// request is made.
    pub fn new(settings: &ChatSettings, token: Option<&str>) -> Result<Self, AppError> {
        validate_turn_budgets(settings.max_tool_calls, settings.turn_timeout_seconds)
            .map_err(|message| AppError::new(ErrorKind::Configuration, message))?;
        let chat = LocalModelClient::new(
            LocalModelConfig {
                base_url: settings.endpoint.clone(),
                model: settings.model.clone(),
                context_tokens: settings.context_tokens,
                max_output_tokens: settings.max_output_tokens,
            },
            // The client owns its header; the caller keeps one copy for redaction.
            token.map(str::to_owned),
        )
        .map_err(AppError::provider)?;
        let summary = LocalModelClient::new(
            LocalModelConfig {
                base_url: settings.endpoint.clone(),
                model: settings.model.clone(),
                context_tokens: settings.context_tokens,
                max_output_tokens: summary_output_tokens(settings.context_tokens),
            },
            token.map(str::to_owned),
        )
        .map_err(AppError::provider)?;
        Ok(Self { chat, summary })
    }
}

/// The pieces of one turn after secret redaction, shared by its steps.
struct Turn<'a> {
    message: &'a str,
    instructions: &'a str,
    settings: &'a ChatSettings,
    token: Option<&'a str>,
    data_root: &'a Path,
    /// The turn's one cancel token, shared by compaction and every request.
    cancel: &'a CancelToken,
    /// The verified skills sent with this turn's requests only.
    skills: &'a [SkillPrompt],
}

/// Run one chat turn, building the clients from the settings first.
///
/// This is the whole turn for a caller that supplies only settings, a message,
/// and the optional token. See [`run_chat_turn_with_clients`] for the order of
/// checks and the errors.
pub fn run_chat_turn(
    journal: &mut SessionJournal,
    data_root: &Path,
    input: &ChatTurnInput<'_>,
    hooks: &mut ChatHooks<'_>,
) -> Result<ChatTurnOutput, AppError> {
    let clients = ChatClients::new(input.settings, input.token)?;
    run_chat_turn_with_clients(journal, data_root, input, &clients, hooks)
}

/// Run one chat turn with clients built earlier by [`ChatClients::new`].
///
/// Deterministic failures return before the turn starts: an archived session or
/// an incomplete previous turn (`session`), unreadable settings (`control`),
/// an unreachable context trigger without auto-compaction (`context`), and a
/// prompt that cannot be assembled (`prompt`). After the turn starts, a failed
/// provider request or runtime is recorded in the journal where the core can
/// do so, and an uncertain storage failure is reported, never papered over.
///
/// A cancelled turn returns `cancelled` (ADR-0051). A cancellation during the
/// compaction before a turn records nothing, because neither the compaction
/// nor the turn was recorded yet; a cancellation after the turn started is
/// recorded with the failure reason `cancelled` and leaves no open turn.
///
/// The message and instructions are cleaned of the known token before use. The
/// token is used only for redaction and is never part of any returned error.
pub fn run_chat_turn_with_clients(
    journal: &mut SessionJournal,
    data_root: &Path,
    input: &ChatTurnInput<'_>,
    clients: &ChatClients,
    hooks: &mut ChatHooks<'_>,
) -> Result<ChatTurnOutput, AppError> {
    let settings = input.settings;
    let message = redact_known_secret(input.message, input.token);
    let instructions = redact_known_secret(&settings.instructions, input.token);
    let cancel = hooks.cancel.clone().unwrap_or_default();
    // Selection shape errors precede every other check, so nothing is
    // recorded or requested for a malformed selection.
    skills::validate_selection(input.skills)?;
    validate_turn_budgets(settings.max_tool_calls, settings.turn_timeout_seconds)
        .map_err(|message| AppError::new(ErrorKind::Configuration, message))?;
    let mut turn = Turn {
        message: &message,
        instructions: &instructions,
        settings,
        token: input.token,
        data_root,
        cancel: &cancel,
        skills: &[],
    };
    if journal.snapshot().archived {
        return Err(AppError::new(
            ErrorKind::Session,
            "Restore the archived session before chatting",
        ));
    }
    if journal.snapshot().active_turn.is_some() {
        return Err(AppError::new(
            ErrorKind::Session,
            tools::ACTIVE_TURN_MESSAGE,
        ));
    }
    // The front end can cancel from here on, which covers the compaction
    // requests below as well as the turn's own requests.
    if let Some(hook) = hooks.on_cancel_token.as_deref_mut() {
        hook(cancel.clone());
    }
    let mode = select(data_root, journal)?;
    // Loading precedes the compaction check and every model request. A skill
    // that is unavailable or changed refuses the turn with nothing recorded.
    let loaded_skills = skills::load_selected(data_root, journal, input.skills, input.token)?;
    turn.skills = &loaded_skills;
    let tools_enabled = matches!(mode, ToolMode::Enabled { .. });
    let skill_extra = skills::context_bytes(&loaded_skills);
    let tool_extra = skill_extra
        + if tools_enabled {
            TOOL_SCHEMA_ESTIMATE_BYTES
        } else {
            0
        };
    let meter =
        preflight::preflight_context(journal, &turn, &clients.summary, tool_extra, tools_enabled)?;
    // Compaction requests can take long; a selection withdrawn meanwhile
    // stops the turn before its first request.
    skills::recheck(data_root, journal, &loaded_skills)?;
    let manifest = RequestManifest {
        base_prompt: BASE_PROMPT.to_owned(),
        prompt_version: BASE_PROMPT_VERSION.to_owned(),
        prompt_sha256: prompt::base_prompt_sha256(),
        custom_instructions: instructions.clone(),
        model: settings.model.clone(),
        base_url: settings.endpoint.clone(),
        context_tokens: settings.context_tokens,
        max_output_tokens: settings.max_output_tokens,
        tool_schema_version: None,
        tools: None,
        skills: loaded_skills
            .iter()
            .map(|skill| ManifestSkill {
                skill_id: skill.skill_id,
                name: skill.name.clone(),
                skill_md_sha256: skill.skill_md_sha256.clone(),
            })
            .collect(),
    };
    // Live settings decide whether this turn offers the workspace tools.
    match mode {
        ToolMode::Enabled { workspace_id } => tools::run(
            journal,
            workspace_id,
            &clients.chat,
            &turn,
            manifest,
            meter,
            hooks,
        )
        .map(ChatTurnOutput::tools),
        ToolMode::Disabled { reason } => text::run(
            journal,
            &clients.chat,
            &turn,
            reason,
            manifest,
            meter,
            hooks,
        )
        .map(ChatTurnOutput::text),
    }
}
