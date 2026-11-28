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

pub mod mcp;
pub mod stream;
