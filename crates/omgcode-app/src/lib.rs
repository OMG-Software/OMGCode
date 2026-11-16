//! Front-end-neutral application services for OMGCode.
//!
//! The trusted CLI and the desktop shell both call this crate so that they run
//! exactly the same chat turn, session creation, and compaction. The crate
//! grants no authority of its own: every authorization decision (workspace
//! preferences, tool approvals, checkpointed edits, confinement) stays in
//! `omgcode-core`. It does no argument parsing, printing, or process exit, and
//! it reads the process environment only in [`read_lm_token`] (also reached
//! through [`resolve_lm_token`] with [`TokenSource::Environment`]), which the caller
//! invokes.
//!
//! The optional local server token is a credential, read from the environment or
//! from a selected provider profile in protected storage (ADR-0047). It is borrowed per call,
//! used to build a request header and to remove the same text from everything
//! recorded or sent, and never appears in an [`AppError`], a `Debug` string, or
//! serialized output.
#![forbid(unsafe_code)]

mod chat;
mod context;
mod credential_source;
pub mod desktop;
mod error;
mod paths;
mod secrets;
mod sessions;
mod tool_mode;

pub use chat::{
    ChatClients, ChatHooks, ChatSettings, ChatTurnInput, ChatTurnOutput, run_chat_turn,
    run_chat_turn_with_clients,
};
pub use context::{CompactionMeter, ContextMeter, compact_journal, summary_output_tokens};
pub use credential_source::{LmToken, TokenSource, resolve_lm_token};
pub use error::{AppError, ErrorKind};
pub use paths::data_root;
pub use secrets::{read_lm_token, redact_known_secret};
pub use sessions::create_session;
pub use tool_mode::{MODEL_TOOLS_SUPPORTED, ToolMode, select, tool_replay_allowed};
