//! Trusted operator commands for the imported-skill registry.
//!
//! Skill text is untrusted. Every command writes one JSON object, so a skill
//! body is shown only as an escaped JSON string and never as raw terminal
//! text. Nothing here loads a skill into a model request or runs a file.
#![forbid(unsafe_code)]

use clap::{Args, Subcommand};
use omgcode_core::control::{
    ControlError, ControlStore, SkillOperationError, SkillScope, SkillSetting,
};
use omgcode_core::policy::WorkspaceId;
use omgcode_core::skills::SkillName;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::control_cli::{control_error, open_existing};
use crate::{CliError, print_json};

#[derive(Subcommand)]
pub(crate) enum SkillsCommand {
    /// Copy a skill folder into private storage and register it, disabled.
    /// The folder's name must equal the skill name. Nothing in it is run.
    Add {
        folder: PathBuf,
        #[arg(long)]
        revision: u64,
    },
    /// List registered skills with their settings.
    List {
        /// Also report each skill's effective setting for this workspace.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Show one skill's record and its stored SKILL.md body as JSON data.
    Show {
        name: String,
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Enable a skill for the application or for one workspace.
    Enable {
        name: String,
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long)]
        revision: u64,
    },
    /// Disable a skill for the application or for one workspace.
    Disable {
        name: String,
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long)]
        revision: u64,
    },
    /// Remove a workspace's override so it follows the application setting.
    Inherit {
        name: String,
        #[arg(long)]
        workspace: String,
        #[arg(long)]
        revision: u64,
    },
    /// Remove a skill and delete its stored copy. The source folder is kept.
    Remove {
        name: String,
        #[arg(long)]
        revision: u64,
    },
}

#[derive(Args)]
#[group(required = true, multiple = false)]
pub(crate) struct ScopeArgs {
    /// Apply to every workspace without its own override.
    #[arg(long)]
    application: bool,
    /// Apply to one registered workspace.
    #[arg(long)]
    workspace: Option<String>,
}

/// The `capabilities` entry. Skill scripts never run; import and registry are
/// trusted CLI operations, and only the CLI chat can load a skill.
pub(crate) fn capability() -> Value {
    json!({
        "import": true,
        "registry": true,
        "chat_loading": ["cli"],
        "scripts": false,
    })
}

pub(crate) fn run(command: SkillsCommand, data_root: &Path) -> Result<(), CliError> {
    match command {
        SkillsCommand::Add { folder, revision } => add(&folder, revision, data_root),
        SkillsCommand::List { workspace } => list(workspace.as_deref(), data_root),
        SkillsCommand::Show { name, workspace } => show(&name, workspace.as_deref(), data_root),
        SkillsCommand::Enable {
            name,
            scope,
            revision,
        } => set(&name, &scope, SkillSetting::Enabled, revision, data_root),
        SkillsCommand::Disable {
            name,
            scope,
            revision,
        } => set(&name, &scope, SkillSetting::Disabled, revision, data_root),
        SkillsCommand::Inherit {
            name,
            workspace,
            revision,
        } => {
            let scope = ScopeArgs {
                application: false,
                workspace: Some(workspace),
            };
            set(&name, &scope, SkillSetting::Inherit, revision, data_root)
        }
        SkillsCommand::Remove { name, revision } => remove(&name, revision, data_root),
    }
}

fn open_store(data_root: &Path) -> Result<ControlStore, CliError> {
    open_existing(data_root)?
        .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))
}

fn skill_name(value: &str) -> Result<SkillName, CliError> {
    SkillName::new(value).map_err(|error| CliError::new(error.code(), "Invalid skill name"))
}

fn workspace_id(value: &str) -> Result<WorkspaceId, CliError> {
    WorkspaceId::from_str(value)
        .map_err(|_| CliError::new("configuration", "Invalid workspace identifier"))
}

fn optional_workspace(value: Option<&str>) -> Result<Option<WorkspaceId>, CliError> {
    value.map(workspace_id).transpose()
}

/// A registry refusal keeps its stable skill code; other failures are `control`.
fn registry_error(error: ControlError) -> CliError {
    match error {
        ControlError::Skill(refusal) => CliError::new(refusal.code(), refusal.to_string()),
        other => control_error(other),
    }
}

fn operation_error(error: &SkillOperationError) -> CliError {
    CliError::new(error.code(), error.to_string())
}

fn add(folder: &Path, revision: u64, data_root: &Path) -> Result<(), CliError> {
    // The path is the trusted operator's. Make it absolute without resolving
    // links; core opens the final component without following a link.
    let source = std::path::absolute(folder)
        .map_err(|_| CliError::new("configuration", "Could not make the folder path absolute"))?;
    let mut store = open_store(data_root)?;
    let imported = store
        .import_skill(&source, revision)
        .map_err(|error| operation_error(&error))?;
    let files: Vec<Value> = imported
        .package
        .files
        .iter()
        .map(|file| json!({"path": file.path, "size": file.size, "sha256": file.sha256}))
        .collect();
    print_json(&json!({
        "skill": imported.record,
        "files": files,
        "revision": store.revision(),
    }))
}

fn list(workspace: Option<&str>, data_root: &Path) -> Result<(), CliError> {
    let workspace = optional_workspace(workspace)?;
    let store = open_store(data_root)?;
    print_json(&json!({
        "skills": store.skill_statuses(workspace),
        "revision": store.revision(),
    }))
}

fn show(name: &str, workspace: Option<&str>, data_root: &Path) -> Result<(), CliError> {
    let name = skill_name(name)?;
    let workspace = optional_workspace(workspace)?;
    let store = open_store(data_root)?;
    // Inspection is a trusted operator read, so it needs no enablement. The
    // file list is omitted: the record's file count, total size, and package
    // digest identify the stored package without a second private-store walk.
    let (_record, loaded) = store
        .inspect_skill(&name)
        .map_err(|error| operation_error(&error))?;
    let status = store
        .skill_statuses(workspace)
        .into_iter()
        .find(|status| status.skill.name() == &name);
    let mut skill = serde_json::to_value(&status)
        .map_err(|_| CliError::new("output", "Could not render the skill status"))?;
    if let Value::Object(map) = &mut skill {
        map.insert("body".to_owned(), Value::String(loaded.body));
    }
    print_json(&json!({"skill": skill, "revision": store.revision()}))
}

fn set(
    name: &str,
    scope: &ScopeArgs,
    setting: SkillSetting,
    revision: u64,
    data_root: &Path,
) -> Result<(), CliError> {
    let name = skill_name(name)?;
    let scope = match (&scope.workspace, scope.application) {
        (Some(id), false) => SkillScope::Workspace {
            workspace_id: workspace_id(id)?,
        },
        (None, true) => SkillScope::Application,
        _ => {
            return Err(CliError::new(
                "configuration",
                "Choose exactly one of --application or --workspace",
            ));
        }
    };
    let mut store = open_store(data_root)?;
    store
        .set_skill_enablement(&name, scope, setting, revision)
        .map_err(registry_error)?;
    let workspace = match scope {
        SkillScope::Workspace { workspace_id } => Some(workspace_id),
        SkillScope::Application => None,
    };
    let status = store
        .skill_statuses(workspace)
        .into_iter()
        .find(|status| status.skill.name() == &name);
    print_json(&json!({"skill": status, "revision": store.revision()}))
}

fn remove(name: &str, revision: u64, data_root: &Path) -> Result<(), CliError> {
    let name = skill_name(name)?;
    let mut store = open_store(data_root)?;
    let removed = store
        .remove_skill_and_package(&name, revision)
        .map_err(registry_error)?;
    let warnings: Vec<Value> = removed
        .package_error
        .iter()
        .map(|error| {
            json!({
                "code": "package_orphaned",
                "reason": error.code(),
                "message": "The registration was removed but its stored copy was not fully deleted; the copy is inert.",
            })
        })
        .collect();
    print_json(&json!({
        "removed": removed.record,
        "warnings": warnings,
        "revision": store.revision(),
    }))
}
