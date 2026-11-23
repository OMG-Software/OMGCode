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
use super::preferences::{self, LoadError, MAX_SAVED_INSTRUCTIONS_BYTES};
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
    /// `invalid_request` for a value outside its bound or global instructions
    /// that would not fit beside a workspace's; `preferences` when a
    /// workspace's instructions file or the preferences file exists but
    /// cannot be read, before anything is written, or when the file cannot be
    /// written; `internal` for a poisoned lock; and the errors of
    /// [`settings_overview`](Self::settings_overview).
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
            // Under the lock, so no workspace save can lengthen a text after
            // this check. Removing the text needs no room.
            if !defaults.instructions.is_empty() {
                let room = self
                    .longest_workspace_instructions()?
                    .map(instructions::global_room)
                    .ok_or_else(DesktopError::instructions_room_unknown)?;
                if defaults.instructions.len() > room {
                    return Err(DesktopError::invalid_request(format!(
                        "The global instructions can be at most {room} bytes, so that a turn can \
                         send them with the longest workspace instructions and their headings. \
                         Shorten them, or that workspace's instructions."
                    )));
                }
            }
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
    /// `unknown_workspace`; `invalid_request` for text that would not fit
    /// beside the saved global instructions; `preferences` when the saved
    /// preferences file exists but cannot be read, before anything is
    /// written, so the room is unknown, or when the file cannot be written
    /// or removed; `internal` for a poisoned lock; and the errors of
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
            // A turn sends the saved global text with these. Removing them
            // needs no room.
            if !request.instructions.is_empty() {
                let room = global_for_room(&self.inner.data_root)
                    .map(|global| instructions::workspace_room(&global))
                    .ok_or_else(DesktopError::instructions_room_unknown)?;
                if request.instructions.len() > room {
                    return Err(DesktopError::invalid_request(format!(
                        "The workspace instructions can be at most {room} bytes, so that a turn \
                         can send them with the global instructions and their headings. Shorten \
                         them, or the global instructions."
                    )));
                }
            }
            instructions::save(
                &self.inner.data_root,
                request.workspace_id,
                &request.instructions,
            )?;
        }
        self.settings_overview()
    }
}

impl DesktopService {
    /// The length of the longest saved instruction text of any registered
    /// workspace, by the rule of [`longest_for_room`].
    fn longest_workspace_instructions(&self) -> Result<Option<usize>, DesktopError> {
        let Some(store) = self.inner.open_control()? else {
            return Ok(Some(0));
        };
        Ok(longest_for_room(
            &self.inner.data_root,
            store
                .workspaces()
                .map(|workspace| *workspace.id().as_uuid()),
        ))
    }
}

/// The saved global instructions that a workspace text must fit beside. Both
/// saves and the overview use this rule (ADR-0080), so the window's counters
/// show the room a save enforces. A damaged file gives the built-in empty
/// text, as it does for a turn. A file that exists but cannot be read now
/// gives `None`: its length is unknown, and it could become readable again.
pub(super) fn global_for_room(data_root: &Path) -> Option<String> {
    match preferences::load(data_root) {
        Ok(saved) => Some(saved.map(|saved| saved.instructions).unwrap_or_default()),
        Err(LoadError::Io) => None,
        Err(LoadError::TooLarge | LoadError::Format | LoadError::Invalid) => Some(String::new()),
    }
}

/// The length of the longest saved instruction text of these workspaces,
/// which the global text must fit beside. A damaged file counts as none,
/// since it blocks that workspace's turns until it is saved again, which
/// checks the room. A file that exists but cannot be read now gives `None`,
/// as in [`global_for_room`].
pub(super) fn longest_for_room(
    data_root: &Path,
    workspaces: impl IntoIterator<Item = Uuid>,
) -> Option<usize> {
    let mut longest = 0;
    for workspace in workspaces {
        match instructions::load(data_root, workspace) {
            Ok(text) => longest = longest.max(text.len()),
            Err(LoadError::Io) => return None,
            Err(LoadError::TooLarge | LoadError::Format | LoadError::Invalid) => {}
        }
    }
    Some(longest)
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
        max_output_tokens_max: u32::MAX,
        tool_call_limit_max: MAX_TOOL_CALLS,
        turn_timeout_seconds_max: MAX_TURN_DEADLINE.as_secs(),
        instructions_max_bytes: Some(MAX_SAVED_INSTRUCTIONS_BYTES),
    }
}

/// One workspace's instructions for the overview, or `None` when its file
/// cannot be read.
pub(super) fn workspace_instructions_view(data_root: &Path, workspace: Uuid) -> Option<String> {
    instructions::load(data_root, workspace).ok()
}
