//! The per-part context estimate of a session's next turn (ADR-0087).
//!
//! [`DesktopService::session_context_breakdown`] serves the `main` window's
//! Context tab. It resolves everything from trusted state by session id: the
//! journal through the index, the saved chat budgets and global instructions,
//! the session workspace's saved instructions, and live settings for the tool
//! decision. It sends nothing to a model, records nothing in any journal, and
//! returns names and numbers only, never instruction or message text.
//!
//! The arithmetic is the pre-turn trigger's own (`estimate_history_parts` in
//! core, applied by the chat module), so the parts add up to the estimate the
//! composer's meter compares with the trigger, apart from the next user
//! message, which is not known yet.

use omgcode_core::context::ContextBudget;
use omgcode_core::session::SessionJournal;

use super::DesktopService;
use super::error::DesktopError;
use super::instructions;
use super::wire::{ContextBreakdown, ContextPartDto, ContextPartId, SessionRequest};
use crate::chat::{NextTurnParts, estimate_next_turn_parts};
use crate::tool_mode::{self, ToolMode};

/// The fixed label of what the estimate covers.
const ESTIMATE_SCOPE: &str = "pre_turn_conservative";

impl DesktopService {
    /// Estimate, by part, what the session's next turn would send.
    ///
    /// The parts are the base prompt, the global and the workspace custom
    /// instructions, the tool definitions (only when live settings would offer
    /// tools), the active compaction summary, the retained conversation, and
    /// the fixed framing no part owns; skills are listed as not included
    /// because they are chosen for one turn. The totals are those of the
    /// composer's meter: the estimate, the output reserve, the effective
    /// trigger, the saved capacity (`null` when none is saved), and whether
    /// auto-compaction is on. They come from the saved chat budgets, the same
    /// source the composer sends with a turn.
    ///
    /// A session whose workspace is no longer registered is not an error. As in
    /// `chat_send`, its workspace instructions are not sent, even when a saved
    /// file remains, so that part and the tools part are reported as not
    /// included with the reason `workspace_unregistered` (ADR-0081).
    ///
    /// The journal is opened for reading its state and released before the
    /// call returns; no event is appended. Run it on a blocking task.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown session; `busy` while a turn runs for it
    /// here or another process holds the journal; `invalid_journal`;
    /// `instructions_unreadable` for a workspace instructions file that cannot
    /// be read (a turn would refuse for the same reason); `invalid_request`
    /// when the global and workspace instructions together exceed the bound;
    /// `control` for unreadable settings; `context` when a count overflows.
    pub fn session_context_breakdown(
        &self,
        request: SessionRequest,
    ) -> Result<ContextBreakdown, DesktopError> {
        let inner = &self.inner;
        let session_id = request.session_id;
        if inner.is_running(session_id)? {
            return Err(DesktopError::busy());
        }
        let path = inner.resolve_path(session_id)?;
        let settings = self.chat_defaults().settings;
        let journal =
            SessionJournal::open(&path).map_err(|error| DesktopError::from_session(&error))?;
        // The same read `chat_send` makes, under the lock every writer of
        // these files holds, so one whole save is seen. As there, a workspace
        // that is no longer registered contributes none, even when a file
        // remains (ADR-0081).
        let workspace_id = journal.snapshot().workspace_id;
        let registered = inner.workspace_registered(workspace_id)?;
        let workspace_text = if registered {
            let _lock = inner
                .preferences
                .lock()
                .map_err(|_| DesktopError::poisoned())?;
            instructions::load(&inner.data_root, workspace_id)
                .map_err(|_| DesktopError::instructions_unreadable())?
        } else {
            String::new()
        };
        let workspace_reason = if !registered {
            Some("workspace_unregistered")
        } else if workspace_text.is_empty() {
            Some("none_saved")
        } else {
            None
        };
        let composed = instructions::compose(&settings.instructions, &workspace_text)?;
        let global_bytes = instructions::global_share(&settings.instructions, &workspace_text);
        let mode = tool_mode::select(&inner.data_root, &journal)
            .map_err(|error| DesktopError::from_app(&error))?;
        let (tools_enabled, tools_reason) = match mode {
            ToolMode::Enabled { .. } => (true, None),
            ToolMode::Disabled { reason } => (false, Some(reason)),
        };
        let parts = estimate_next_turn_parts(
            &journal,
            &inner.data_root,
            &composed,
            global_bytes,
            tools_enabled,
        )
        .map_err(|error| DesktopError::from_app(&error))?;
        drop(journal);

        let budget = ContextBudget {
            threshold: settings.compact_threshold,
            context_tokens: settings.context_tokens,
            max_output_tokens: settings.max_output_tokens,
        };
        Ok(ContextBreakdown {
            session_id,
            parts: part_list(
                &parts,
                !settings.instructions.is_empty(),
                workspace_reason,
                tools_reason,
            ),
            estimated_input_tokens: parts.total,
            max_output_tokens: settings.max_output_tokens,
            // Without a saved capacity the safe input budget is unknown, so
            // only the threshold applies.
            effective_trigger: if settings.context_tokens == 0 {
                settings.compact_threshold
            } else {
                budget.trigger()
            },
            context_tokens: (settings.context_tokens > 0).then_some(settings.context_tokens),
            auto_compaction: settings.auto_compact,
            estimate_scope: ESTIMATE_SCOPE,
        })
    }
}

/// The wire parts, in display order.
fn part_list(
    parts: &NextTurnParts,
    has_global: bool,
    workspace_reason: Option<&'static str>,
    tools_reason: Option<&'static str>,
) -> Vec<ContextPartDto> {
    let part = |id, tokens, included, reason| ContextPartDto {
        id,
        tokens,
        included,
        messages: None,
        reason,
    };
    vec![
        part(ContextPartId::System, parts.system, true, None),
        part(
            ContextPartId::GlobalInstructions,
            parts.global_instructions,
            has_global,
            (!has_global).then_some("none_saved"),
        ),
        part(
            ContextPartId::WorkspaceInstructions,
            parts.workspace_instructions,
            workspace_reason.is_none(),
            workspace_reason,
        ),
        part(
            ContextPartId::Tools,
            parts.tools,
            tools_reason.is_none(),
            tools_reason,
        ),
        part(
            ContextPartId::Summary,
            parts.summary.unwrap_or(0),
            parts.summary.is_some(),
            parts.summary.is_none().then_some("no_compaction"),
        ),
        ContextPartDto {
            messages: Some(parts.history_messages),
            ..part(ContextPartId::History, parts.history, true, None)
        },
        part(ContextPartId::Skills, 0, false, Some("selected_per_turn")),
        part(ContextPartId::Framing, parts.framing, true, None),
    ]
}
