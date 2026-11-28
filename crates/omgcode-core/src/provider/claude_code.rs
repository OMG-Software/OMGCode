//! Pure building blocks for driving the official Claude Code CLI.
//!
//! Claude Code runs its own model loop as a child process
//! (`claude -p --output-format stream-json --verbose --include-partial-messages`)
//! and calls OMGCode's tools through an MCP server on stdio. Everything the
//! process writes is untrusted data, and neither module performs I/O beyond
//! the reader it is handed:
//!
//! - [`stream`] splits the process's standard output into bounded lines,
//!   parses each line into a typed event, and checks that the session was
//!   launched without built-in tools, plugins, skills, or API-key billing.
//! - [`mcp`] is the I/O-free state machine of the MCP server (JSON-RPC 2.0,
//!   one message per line) that forwards every tool call to the caller.
//!
//! The shapes were observed with Claude Code 2.1.325 on 2026-11-28. A shape
//! this code does not recognise is ignored only where documented; a known
//! message with the wrong shape is an error. Nothing here grants authority.

pub mod history;
pub mod launch;
pub mod mcp;
pub mod model;
pub mod stream;
mod watch;

pub use launch::{
    CLAUDE_CODE_EFFORTS, ClaudeCodeClient, ClaudeCodeConfig, RELAY_NONCE_VARIABLE,
    RELAY_SUBCOMMAND, relay,
};
pub use model::ClaudeCodeModel;

/// Why a Claude Code run failed (ADR-0109). No text from the process is
/// kept: its output is untrusted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaudeCodeFailure {
    /// The process reported that its sign-in failed or is missing.
    NotSignedIn,
    /// The process offered a tool, MCP server, plugin, skill, slash command,
    /// or API-key source OMGCode did not choose, or named another tool.
    Lockdown,
    /// The process's output or MCP traffic did not have the supported shape.
    Protocol,
    /// The process could not start, ended, failed, or timed out before its
    /// result.
    Failed,
    /// A recorded settings change revoked a grant of the connection or
    /// removed it while the turn ran, or the settings could not be checked;
    /// the process was ended.
    Withdrawn,
    /// The turn records of every connection are full, so the turn started
    /// no process.
    TurnRecordsFull,
}

impl ClaudeCodeFailure {
    /// The stable failure code.
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotSignedIn => "claude_code_not_signed_in",
            Self::Lockdown => "claude_code_lockdown",
            Self::Protocol => "claude_code_protocol",
            Self::Failed => "claude_code_failed",
            Self::Withdrawn => "claude_code_withdrawn",
            Self::TurnRecordsFull => "claude_code_turn_records_full",
        }
    }

    /// A fixed description for people.
    pub const fn description(self) -> &'static str {
        match self {
            Self::NotSignedIn => {
                "Claude Code is not signed in for this connection; run `claude auth login` with CLAUDE_CONFIG_DIR set to the connection's sign-in folder"
            }
            Self::Lockdown => {
                "Claude Code started with a tool, MCP server, plugin, skill, slash command, or API-key source OMGCode did not choose, or named another tool; the process was ended"
            }
            Self::Protocol => {
                "Claude Code sent output or MCP traffic OMGCode could not accept; the process was ended"
            }
            Self::Failed => {
                "the Claude Code process could not start, or failed, exited, or timed out before its result"
            }
            Self::Withdrawn => {
                "the settings revoked a grant of this Claude Code connection or removed it during the turn, or could not be checked; the process was ended"
            }
            Self::TurnRecordsFull => {
                "the Claude Code turn records are full, so no process was started; for each connection, check that no `claude` process of it remains, including its MCP relays and their descendants, then run `workspace claude-code-acknowledge <profile id>`; keep a connection's records while that cannot be confirmed"
            }
        }
    }
}
