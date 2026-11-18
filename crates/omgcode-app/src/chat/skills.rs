//! Operator-selected skills for one chat turn (ADR-0057).
//!
//! A skill is untrusted task text the operator chose for this turn. Loading
//! one grants nothing: it cannot enable a tool, approve a call, or change a
//! preference. The control store is opened for the load and released before
//! any model request.

use std::collections::HashSet;

use omgcode_core::control::{ControlStore, SkillOperationError};
use omgcode_core::prompt::{MAX_SKILL_BODIES_BYTES, SkillPrompt};
use omgcode_core::session::SessionJournal;
use omgcode_core::skills::{MAX_SELECTED_SKILLS, SkillName};
use serde::Serialize;

use crate::error::AppError;
use crate::secrets::redact_known_secret;

/// A skill that was loaded for a turn: its name and `SKILL.md` digest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LoadedSkillSummary {
    /// The registered skill name.
    pub name: String,
    /// The lowercase SHA-256 of the loaded `SKILL.md`.
    pub skill_md_sha256: String,
}

impl LoadedSkillSummary {
    pub(super) fn from_prompts(skills: &[SkillPrompt]) -> Vec<Self> {
        skills
            .iter()
            .map(|skill| Self {
                name: skill.name.as_str().to_owned(),
                skill_md_sha256: skill.skill_md_sha256.clone(),
            })
            .collect()
    }
}

/// Check the selection shape: at most four names, none repeated.
pub fn validate_selection(names: &[SkillName]) -> Result<(), AppError> {
    if names.len() > MAX_SELECTED_SKILLS {
        return Err(AppError::skill(
            "skill_selection_invalid",
            format!("Select at most {MAX_SELECTED_SKILLS} skills for one turn"),
        ));
    }
    let mut seen = HashSet::new();
    if names.iter().any(|name| !seen.insert(name)) {
        return Err(AppError::skill(
            "skill_selection_invalid",
            "A skill may be selected only once",
        ));
    }
    Ok(())
}

/// Load every selected skill for the session's workspace.
///
/// Each must be registered, effectively enabled for the workspace, and
/// unchanged on disk. Any failure refuses the whole turn before a model
/// request. The settings store is released before this returns. Known-token
/// text is removed from each body, as from every other request text.
pub(super) fn load_selected(
    data_root: &std::path::Path,
    journal: &SessionJournal,
    names: &[SkillName],
    token: Option<&str>,
) -> Result<Vec<SkillPrompt>, AppError> {
    validate_selection(names)?;
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let control = ControlStore::open_existing(data_root)
        .map_err(|error| AppError::skill("control", error.to_string()))?
        .ok_or_else(|| unavailable(&names[0]))?;
    // The journal's UUID only selects a registration; the ID used afterwards
    // comes from current settings.
    let workspace_id = control
        .registered_workspace(journal.snapshot().workspace_id)
        .map(|workspace| workspace.id())
        .ok_or_else(|| unavailable(&names[0]))?;
    let mut skills = Vec::with_capacity(names.len());
    let mut body_bytes = 0_usize;
    for name in names {
        let enabled = control
            .load_enabled_skill(workspace_id, name)
            .map_err(|error| operation_error(&error))?;
        body_bytes = body_bytes.saturating_add(enabled.loaded.body.len());
        if body_bytes > MAX_SKILL_BODIES_BYTES {
            return Err(AppError::skill(
                "skills_too_large",
                "The selected skill bodies exceed 64 KiB together",
            ));
        }
        skills.push(SkillPrompt {
            skill_id: enabled.record.skill_id(),
            name: name.clone(),
            skill_md_sha256: enabled.loaded.skill_md_sha256.clone(),
            body: redact_known_secret(&enabled.loaded.body, token),
        });
    }
    Ok(skills)
}

fn unavailable(name: &SkillName) -> AppError {
    operation_error(&SkillOperationError::Unavailable(name.clone()))
}

fn operation_error(error: &SkillOperationError) -> AppError {
    AppError::skill(error.code(), error.to_string())
}
