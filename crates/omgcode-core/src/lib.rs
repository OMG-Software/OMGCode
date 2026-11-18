//! Core contracts for the OMGCode coding-assistant prototype.
#![forbid(unsafe_code)]

pub mod context;
pub mod control;
pub mod edit;
mod file_audit;
pub mod files;
pub mod packages;
pub mod policy;
pub mod prompt;
pub mod provider;
pub mod replacement;
pub mod run;
pub mod run_journal;
pub mod session;
pub mod session_index;
pub mod skills;
mod storage;
#[cfg(test)]
mod test_support;
pub mod turn;
mod workspace;
