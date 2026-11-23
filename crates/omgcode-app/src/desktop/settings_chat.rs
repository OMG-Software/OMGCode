//! `settings`-window chat defaults (ADR-0080).
//!
//! The window shows and saves the chat budgets and the global custom
//! instructions beside the model connection, and each workspace's own custom
//! instructions with its other settings. None of them is a permission: they
//! need no native confirmation, change no settings-store revision, and
//! `chat_send` validates every value again when a turn uses it. Both saves
//! take the service's preferences lock, which the `main` window's composer
//! save also takes, so the two windows never overwrite each other's fields.

use std::path::Path;

use uuid::Uuid;

use super::DesktopService;
use super::error::DesktopError;
use super::instructions;
use super::preferences::{self, MAX_SAVED_INSTRUCTIONS_BYTES};
use super::wire::{
    ChatBudgetsDto, ChatBudgetsSaveRequest, ChatDefaultsView, PreferencesSource, SettingsOverview,
    WorkspaceInstructionsRequest,
};
use crate::chat::validate_turn_budgets;
use omgcode_core::turn::{MAX_TOOL_CALLS, MAX_TURN_DEADLINE};

impl DesktopService {
    /// `settings` only. Save the chat budgets and the global custom
    /// instructions. The composer's own fields keep their saved values.
    ///
    /// # Errors
    ///
    /// `invalid_request` for a value outside its bound, `preferences` when the
    /// file cannot be written, `internal` for a poisoned lock, and the errors
    /// of [`settings_overview`](Self::settings_overview).
    pub fn settings_save_chat_defaults(
        &self,
        request: ChatBudgetsSaveRequest,
    ) -> Result<SettingsOverview, DesktopError> {
        let defaults = request.defaults;
        check_budgets(&defaults).map_err(DesktopError::invalid_request)?;
        {
            let _lock = self
                .inner
                .preferences
                .lock()
                .map_err(|_| DesktopError::poisoned())?;
            preferences::update(&self.inner.data_root, |saved| {
                saved.context_tokens = defaults.context_tokens;
                saved.max_output_tokens = defaults.max_output_tokens;
                saved.max_tool_calls = defaults.max_tool_calls;
                saved.turn_timeout_seconds = defaults.turn_timeout_seconds;
                saved.instructions = defaults.instructions;
            })?;
        }
        self.settings_overview()
    }

    /// `settings` only. Replace one registered workspace's custom
    /// instructions; empty text removes them.
    ///
    /// # Errors
    ///
    /// `unknown_workspace`, `invalid_request` for text over the bound,
    /// `preferences` when the file cannot be written, `internal` for a
    /// poisoned lock, and the errors of
    /// [`settings_overview`](Self::settings_overview).
    pub fn settings_set_workspace_instructions(
        &self,
        request: WorkspaceInstructionsRequest,
    ) -> Result<SettingsOverview, DesktopError> {
        {
            let store = self.inner.open_or_create_control()?;
            store
                .registered_workspace(request.workspace_id)
                .ok_or_else(DesktopError::unknown_workspace)?;
        }
        {
            let _lock = self
                .inner
                .preferences
                .lock()
                .map_err(|_| DesktopError::poisoned())?;
            instructions::save(
                &self.inner.data_root,
                request.workspace_id,
                &request.instructions,
            )?;
        }
        self.settings_overview()
    }
}

/// Check the values a `settings` save may record.
fn check_budgets(defaults: &ChatBudgetsDto) -> Result<(), String> {
    if defaults.context_tokens == 0 {
        return Err("Set the context size to a positive number of tokens.".to_owned());
    }
    if defaults.max_output_tokens == 0 {
        return Err("Set the output reserve to a positive number of tokens.".to_owned());
    }
    if defaults.instructions.len() > MAX_SAVED_INSTRUCTIONS_BYTES {
        return Err(format!(
            "The global instructions are longer than {} KiB.",
            MAX_SAVED_INSTRUCTIONS_BYTES / 1024
        ));
    }
    validate_turn_budgets(defaults.max_tool_calls, defaults.turn_timeout_seconds)
}

/// The chat defaults the `settings` window shows: the saved values, or the
/// built-in ones when none are saved or the file cannot be read.
pub(super) fn chat_defaults_view(data_root: &Path) -> ChatDefaultsView {
    let (saved, source) = match preferences::load(data_root) {
        Ok(Some(saved)) => (saved, PreferencesSource::Saved),
        Ok(None) => (preferences::built_in(), PreferencesSource::Defaults),
        Err(_) => (preferences::built_in(), PreferencesSource::Unreadable),
    };
    ChatDefaultsView {
        defaults: ChatBudgetsDto {
            context_tokens: saved.context_tokens,
            max_output_tokens: saved.max_output_tokens,
            max_tool_calls: saved.max_tool_calls,
            turn_timeout_seconds: saved.turn_timeout_seconds,
            instructions: saved.instructions,
        },
        source,
        tool_call_limit_max: MAX_TOOL_CALLS,
        turn_timeout_seconds_max: MAX_TURN_DEADLINE.as_secs(),
        instructions_max_bytes: MAX_SAVED_INSTRUCTIONS_BYTES,
    }
}

/// One workspace's instructions for the overview, or `None` when its file
/// cannot be read.
pub(super) fn workspace_instructions_view(data_root: &Path, workspace: Uuid) -> Option<String> {
    instructions::load(data_root, workspace).ok()
}
