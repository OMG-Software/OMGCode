//! One chat turn against the selected local model, without any front end.
//!
//! This module performs no printing, no environment reads, no argument
//! parsing, and never exits the process. Whether the turn offers workspace
//! tools is decided from live settings by the core; this module grants no
//! authority. The settings store is opened and dropped inside its own steps and
//! is never held while a model request is in flight.

mod output;
mod preflight;
mod text;
mod tools;

use std::path::Path;

use omgcode_core::context::DEFAULT_COMPACT_THRESHOLD;
use omgcode_core::prompt::{self, BASE_PROMPT, BASE_PROMPT_VERSION};
use omgcode_core::provider::{LocalModelClient, LocalModelConfig};
use omgcode_core::session::{RequestManifest, SessionJournal};
use omgcode_core::turn::CancelToken;

pub use self::output::ChatTurnOutput;
use crate::context::summary_output_tokens;
use crate::error::{AppError, ErrorKind};
use crate::secrets::redact_known_secret;
use crate::tool_mode::{TOOL_SCHEMA_ESTIMATE_BYTES, ToolMode, select};

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
            instructions: String::new(),
            compact_threshold: DEFAULT_COMPACT_THRESHOLD,
            auto_compact: false,
        }
    }
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
}

/// Optional callbacks a front end can supply to a turn.
///
/// Today the only hook delivers the cancellation token. Cancellation is
/// cooperative: the core checks it between rounds and tool calls, so it cannot
/// interrupt a provider request already in flight. The text-only path has no
/// cancellation point at all, so the hook is never called there.
#[derive(Default)]
pub struct ChatHooks<'a> {
    /// Called once on the tool-enabled path, immediately after the turn
    /// runtime is built and before the first model request. The callback runs
    /// on the turn's thread and must not block; hand the token to another
    /// thread and return.
    pub on_cancel_token: Option<&'a mut dyn FnMut(CancelToken)>,
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
    /// Fails with `provider` when the endpoint, model budgets, or token are
    /// unusable. No network request is made.
    pub fn new(settings: &ChatSettings, token: Option<&str>) -> Result<Self, AppError> {
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
    let turn = Turn {
        message: &message,
        instructions: &instructions,
        settings,
        token: input.token,
        data_root,
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
    let mode = select(data_root, journal)?;
    let tools_enabled = matches!(mode, ToolMode::Enabled { .. });
    let tool_extra = if tools_enabled {
        TOOL_SCHEMA_ESTIMATE_BYTES
    } else {
        0
    };
    let meter =
        preflight::preflight_context(journal, &turn, &clients.summary, tool_extra, tools_enabled)?;
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
        ToolMode::Disabled { reason } => {
            text::run(journal, &clients.chat, &turn, reason, manifest, meter)
                .map(ChatTurnOutput::text)
        }
    }
}
