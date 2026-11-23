//! Whether a chat turn offers the model the workspace tools.
//!
//! The session journal only selects which registered workspace to consult.
//! Authority comes from the live settings preference, rechecked by the core
//! runtime before every tool call. Nothing here grants authority.

use std::path::Path;

use omgcode_core::control::ControlStore;
use omgcode_core::policy::WorkspaceId;
use omgcode_core::session::SessionJournal;
use uuid::Uuid;

use crate::error::{AppError, ErrorKind};

/// Whether this build can serve the model read/list tools.
///
/// The capability report and tool selection share this value, so chat never
/// offers tools the platform cannot run.
pub const MODEL_TOOLS_SUPPORTED: bool = cfg!(any(target_os = "linux", target_os = "windows"));

/// Conservative byte allowance for the advertised tool definitions.
///
/// The largest set (read, list, edit, multi-edit, create, rename, delete, and
/// command) serializes to 6,174 bytes in the adapter's wire shape; a unit test
/// keeps this allowance above it.
pub(crate) const TOOL_SCHEMA_ESTIMATE_BYTES: usize = 7 * 1024;

/// Whether this chat turn offers the model the workspace tools.
#[derive(Debug)]
pub enum ToolMode {
    /// The selected workspace's live preference enables model tools.
    Enabled {
        /// The registered workspace taken from current settings.
        workspace_id: WorkspaceId,
    },
    /// Text-only chat, with the reason tools were not offered.
    Disabled {
        /// A stable reason code, such as `preference_disabled`.
        reason: &'static str,
    },
}

/// Decide the tool mode from current settings without holding the settings lock.
///
/// An unsupported platform, an absent store, an unregistered workspace, or a
/// disabled preference gives text-only chat. Invalid settings fail closed with
/// a `control` error. A version 1 journal cannot record tool events, so it
/// stays text-only. The settings lock is released before this returns.
pub fn select(data_root: &Path, journal: &SessionJournal) -> Result<ToolMode, AppError> {
    if !MODEL_TOOLS_SUPPORTED {
        return Ok(ToolMode::Disabled {
            reason: "platform_unsupported",
        });
    }
    let Some(control) = ControlStore::open_existing_waiting(data_root)
        .map_err(|error| AppError::new(ErrorKind::Control, error.to_string()))?
    else {
        return Ok(ToolMode::Disabled {
            reason: "settings_absent",
        });
    };
    // The journal's UUID only selects a registration; the ID used afterwards
    // and its preference come from current settings.
    let (workspace_id, enabled) =
        match control.registered_workspace(journal.snapshot().workspace_id) {
            None => {
                return Ok(ToolMode::Disabled {
                    reason: "workspace_unregistered",
                });
            }
            Some(workspace) => (workspace.id(), workspace.permissions().model_tools),
        };
    // Release the settings lock before any model request.
    drop(control);
    if !enabled {
        return Ok(ToolMode::Disabled {
            reason: "preference_disabled",
        });
    }
    if journal.schema_version() < 2 {
        return Ok(ToolMode::Disabled {
            reason: "session_schema_v1",
        });
    }
    Ok(ToolMode::Enabled { workspace_id })
}

/// Whether live settings authorize replaying saved tool results for a session:
/// the same `model_tools` and `read` rule the turn runtime applies.
///
/// The session is named by its recorded workspace UUID and schema version, so
/// compaction can recheck while it holds the journal. Each call reads current
/// settings. Every condition that gives text-only chat gives `false`; invalid
/// settings return a `control` error.
pub fn tool_replay_allowed(
    data_root: &Path,
    workspace: Uuid,
    schema_version: u32,
) -> Result<bool, AppError> {
    if !MODEL_TOOLS_SUPPORTED || schema_version < 2 {
        return Ok(false);
    }
    let Some(control) = ControlStore::open_existing_waiting(data_root)
        .map_err(|error| AppError::new(ErrorKind::Control, error.to_string()))?
    else {
        return Ok(false);
    };
    Ok(control
        .registered_workspace(workspace)
        .is_some_and(|workspace| {
            let permissions = workspace.permissions();
            permissions.model_read().permits()
        }))
}

#[cfg(test)]
mod tests {
    use omgcode_core::turn::{
        command_tool_definition, create_tool_definition, delete_tool_definition,
        edit_tool_definition, multi_edit_tool_definition, rename_tool_definition, tool_definitions,
    };
    use serde_json::{Value, json};

    use super::TOOL_SCHEMA_ESTIMATE_BYTES;

    /// The estimate must cover the largest advertised tool set in the exact
    /// OpenAI-compatible wire shape the adapter sends.
    #[test]
    fn the_schema_estimate_covers_every_advertised_tool_definition() {
        let mut definitions = tool_definitions();
        definitions.push(edit_tool_definition());
        definitions.push(multi_edit_tool_definition());
        definitions.push(create_tool_definition());
        definitions.push(rename_tool_definition());
        definitions.push(delete_tool_definition());
        definitions.push(command_tool_definition());
        assert_eq!(definitions.len(), 8, "the eight-tool cap is fully used");
        let wire: Vec<Value> = definitions
            .iter()
            .map(|definition| json!({"type": "function", "function": definition}))
            .collect();
        let bytes = serde_json::to_vec(&wire).expect("serialize tool definitions");
        println!("advertised tool definitions: {} bytes", bytes.len());
        assert!(
            bytes.len() <= TOOL_SCHEMA_ESTIMATE_BYTES,
            "{} bytes exceed the {TOOL_SCHEMA_ESTIMATE_BYTES}-byte estimate",
            bytes.len()
        );
    }
}
