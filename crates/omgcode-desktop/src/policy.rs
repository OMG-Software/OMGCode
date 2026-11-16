//! The window policy: which webview may call which command.
//!
//! This is the second of two layers (ADR-0046 decision 2). Tauri's capability
//! files are the first. Every command handler calls [`authorize`] before doing
//! anything else, so a capability file edited by mistake cannot expose a decision
//! command to the wrong window. The calling label comes from the runtime, which
//! binds the IPC channel to the webview that owns it; page script cannot choose it.

use crate::error::CommandError;

/// The two webview windows. Labels are fixed at creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowLabel {
    /// Session tree, transcripts, and chat.
    Main,
    /// The only window that may list, review, and decide approvals.
    Approvals,
}

impl WindowLabel {
    /// The label string passed to the window builder and matched in capabilities.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Approvals => "approvals",
        }
    }
}

/// Every application command. The set is closed: adding a command means adding
/// a variant here, a name in `command_names.rs`, and a capability grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    AppInfo,
    SessionTree,
    SessionSearch,
    SessionTranscript,
    SessionArchive,
    SessionRestore,
    SessionInterrupt,
    WorkspaceList,
    SessionCreate,
    ChatDefaults,
    ChatSend,
    ChatStatus,
    ChatCancel,
    ApprovalsSummary,
    ApprovalsShowWindow,
    ApprovalsList,
    ApprovalDetail,
    ApprovalDecide,
}

impl Command {
    /// All commands, in the order of `command_names.rs`.
    pub const ALL: [Self; 18] = [
        Self::AppInfo,
        Self::SessionTree,
        Self::SessionSearch,
        Self::SessionTranscript,
        Self::SessionArchive,
        Self::SessionRestore,
        Self::SessionInterrupt,
        Self::WorkspaceList,
        Self::SessionCreate,
        Self::ChatDefaults,
        Self::ChatSend,
        Self::ChatStatus,
        Self::ChatCancel,
        Self::ApprovalsSummary,
        Self::ApprovalsShowWindow,
        Self::ApprovalsList,
        Self::ApprovalDetail,
        Self::ApprovalDecide,
    ];

    /// The command name exactly as the frontend invokes it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::AppInfo => "app_info",
            Self::SessionTree => "session_tree",
            Self::SessionSearch => "session_search",
            Self::SessionTranscript => "session_transcript",
            Self::SessionArchive => "session_archive",
            Self::SessionRestore => "session_restore",
            Self::SessionInterrupt => "session_interrupt",
            Self::WorkspaceList => "workspace_list",
            Self::SessionCreate => "session_create",
            Self::ChatDefaults => "chat_defaults",
            Self::ChatSend => "chat_send",
            Self::ChatStatus => "chat_status",
            Self::ChatCancel => "chat_cancel",
            Self::ApprovalsSummary => "approvals_summary",
            Self::ApprovalsShowWindow => "approvals_show_window",
            Self::ApprovalsList => "approvals_list",
            Self::ApprovalDetail => "approval_detail",
            Self::ApprovalDecide => "approval_decide",
        }
    }

    /// The one window allowed to call this command.
    #[must_use]
    pub const fn window(self) -> WindowLabel {
        match self {
            Self::ApprovalsList | Self::ApprovalDetail | Self::ApprovalDecide => {
                WindowLabel::Approvals
            }
            Self::AppInfo
            | Self::SessionTree
            | Self::SessionSearch
            | Self::SessionTranscript
            | Self::SessionArchive
            | Self::SessionRestore
            | Self::SessionInterrupt
            | Self::WorkspaceList
            | Self::SessionCreate
            | Self::ChatDefaults
            | Self::ChatSend
            | Self::ChatStatus
            | Self::ChatCancel
            | Self::ApprovalsSummary
            | Self::ApprovalsShowWindow => WindowLabel::Main,
        }
    }
}

/// Allows `command` only when `caller_label` is exactly its window's label.
///
/// Call this first in every handler, with the label of the webview that sent
/// the request (never a label taken from the request body). An unknown or empty
/// label is refused.
///
/// # Errors
///
/// Returns `forbidden_window` when the caller is not the command's window.
pub fn authorize(command: Command, caller_label: &str) -> Result<(), CommandError> {
    if caller_label == command.window().as_str() {
        Ok(())
    } else {
        Err(CommandError::forbidden_window())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    include!("command_names.rs");

    fn with_window(window: WindowLabel) -> Vec<Command> {
        Command::ALL
            .into_iter()
            .filter(|command| command.window() == window)
            .collect()
    }

    #[test]
    fn every_command_has_exactly_one_window_and_the_counts_match() {
        assert_eq!(Command::ALL.len(), 18);
        assert_eq!(with_window(WindowLabel::Main).len(), 15);
        assert_eq!(with_window(WindowLabel::Approvals).len(), 3);
    }

    #[test]
    fn approval_commands_are_approvals_only() {
        for command in [
            Command::ApprovalsList,
            Command::ApprovalDetail,
            Command::ApprovalDecide,
        ] {
            assert_eq!(command.window(), WindowLabel::Approvals);
            assert_eq!(authorize(command, "approvals"), Ok(()));
            for label in ["main", "unknown", "", "Approvals", "approvals ", "main\0"] {
                assert_eq!(
                    authorize(command, label),
                    Err(CommandError::forbidden_window()),
                    "{} must be refused for {label:?}",
                    command.name()
                );
            }
        }
    }

    #[test]
    fn main_commands_are_refused_for_every_other_label() {
        for command in with_window(WindowLabel::Main) {
            assert_eq!(authorize(command, "main"), Ok(()));
            for label in ["approvals", "unknown", ""] {
                assert_eq!(
                    authorize(command, label),
                    Err(CommandError::forbidden_window()),
                    "{} must be refused for {label:?}",
                    command.name()
                );
            }
        }
    }

    #[test]
    fn names_are_unique_snake_case() {
        let names: BTreeSet<&str> = Command::ALL.iter().map(|c| c.name()).collect();
        assert_eq!(names.len(), Command::ALL.len());
        for name in names {
            assert!(!name.is_empty());
            assert!(
                name.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "{name} is not snake_case"
            );
            assert!(!name.starts_with('_') && !name.ends_with('_') && !name.contains("__"));
        }
    }

    #[test]
    fn shared_name_list_has_eighteen_unique_names_matching_the_enum() {
        let shared: BTreeSet<&str> = COMMAND_NAMES.iter().copied().collect();
        assert_eq!(COMMAND_NAMES.len(), 18);
        assert_eq!(shared.len(), 18);
        let from_enum: Vec<&str> = Command::ALL.iter().map(|c| c.name()).collect();
        assert_eq!(from_enum, COMMAND_NAMES);
    }
}
