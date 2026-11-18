//! The skill registry in the control journal.
//!
//! A record names one imported skill package and the digests of its bytes. It
//! stores no skill text beyond the manifest fields. Registering or enabling a
//! skill grants nothing: a skill is untrusted task text, and only an explicit
//! chat selection loads it (ADR-0057, `docs/skills.md`).
//!
//! The journal is the only registry. Every field is validated when it is
//! recorded and again when the journal is replayed, so a hand-edited line
//! cannot introduce a record the commands would refuse. Enablement is a
//! per-skill setting at the application scope, with an optional override for
//! each registered workspace. A skill is effectively enabled for a workspace
//! when the workspace override says so; without an override the application
//! setting applies; without either the skill is disabled.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    Action, ControlError, ControlSnapshot, ControlStore, deserialize_workspace_id, valid_uuid,
};
use crate::policy::WorkspaceId;
use crate::skills::{MAX_REGISTERED_SKILLS, SkillName};

/// Most characters of a skill description.
pub const MAX_SKILL_DESCRIPTION_CHARS: usize = 1024;
/// Most characters of a skill license text.
pub const MAX_SKILL_LICENSE_CHARS: usize = 256;
/// Most characters of a skill compatibility text.
pub const MAX_SKILL_COMPATIBILITY_CHARS: usize = 500;
/// Most manifest metadata entries.
pub const MAX_SKILL_METADATA_ENTRIES: usize = 16;
/// Most characters of a metadata key.
pub const MAX_SKILL_METADATA_KEY_CHARS: usize = 64;
/// Most characters of a metadata value.
pub const MAX_SKILL_METADATA_VALUE_CHARS: usize = 256;
/// Most advisory allowed-tool entries.
pub const MAX_SKILL_ALLOWED_TOOLS: usize = 32;
/// Most total bytes of the advisory allowed-tool entries.
pub const MAX_SKILL_ALLOWED_TOOLS_BYTES: usize = 1024;
/// Most bytes of the recorded `SKILL.md`.
pub const MAX_SKILL_MD_BYTES: u64 = 64 * 1024;
/// Most files in a package.
pub const MAX_SKILL_FILES: u32 = 128;
/// Most bytes in a package.
pub const MAX_SKILL_PACKAGE_BYTES: u64 = 4 * 1024 * 1024;
/// Most bytes of the recorded source folder text.
pub const MAX_SKILL_SOURCE_BYTES: usize = 1024;

const DIGEST_HEX_CHARS: usize = 64;

/// Why a skill registry request or record was refused. Each has a stable code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkillRefusal {
    /// A registered skill already has this name.
    NameTaken,
    /// A registered skill already has this identifier.
    IdTaken,
    /// No registered skill has this name or identifier.
    NotFound,
    /// The registry holds the most skills it may.
    TooManySkills,
    /// A record field is outside its bound or has a forbidden character.
    InvalidField(&'static str),
    /// `inherit` removes a workspace override, so it needs a workspace scope.
    InheritNeedsWorkspace,
    /// A workspace scope must name a registered workspace.
    UnknownWorkspace,
}

impl SkillRefusal {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NameTaken => "skill_name_taken",
            Self::IdTaken => "skill_id_taken",
            Self::NotFound => "skill_not_found",
            Self::TooManySkills => "too_many_skills",
            Self::InvalidField(_) => "skill_invalid_field",
            Self::InheritNeedsWorkspace => "skill_inherit_needs_workspace",
            Self::UnknownWorkspace => "skill_workspace_unknown",
        }
    }

    const fn message(self) -> &'static str {
        match self {
            Self::NameTaken => "a skill with this name is already registered; remove it first",
            Self::IdTaken => "a skill with this identifier is already registered",
            Self::NotFound => "no registered skill has this name or identifier",
            Self::TooManySkills => "at most 64 skills may be registered",
            Self::InvalidField(reason) => reason,
            Self::InheritNeedsWorkspace => "inherit applies only to a workspace scope",
            Self::UnknownWorkspace => "the workspace is not registered",
        }
    }
}

impl fmt::Display for SkillRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for SkillRefusal {}

impl From<SkillRefusal> for ControlError {
    fn from(value: SkillRefusal) -> Self {
        Self::Skill(value)
    }
}

/// Where an enablement setting applies.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SkillScope {
    /// Every workspace without its own override.
    Application,
    /// One registered workspace.
    Workspace {
        #[serde(deserialize_with = "deserialize_workspace_id")]
        workspace_id: WorkspaceId,
    },
}

/// An enablement setting. `Inherit` is valid only for a workspace scope and
/// removes that workspace's override.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSetting {
    Enabled,
    Disabled,
    Inherit,
}

/// One recorded enablement: never `Inherit`, which deletes the record.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SkillEnablement {
    skill_id: Uuid,
    scope: SkillScope,
    enabled: bool,
}

impl SkillEnablement {
    /// The skill this setting applies to.
    pub const fn skill_id(&self) -> Uuid {
        self.skill_id
    }

    /// Where it applies.
    pub const fn scope(&self) -> SkillScope {
        self.scope
    }

    /// Whether the scope enables the skill.
    pub const fn enabled(&self) -> bool {
        self.enabled
    }
}

/// An imported skill, as the registry records it.
///
/// Construct one only through [`ControlStore::add_skill`] or replay, which
/// validate every field.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillRecord {
    skill_id: Uuid,
    name: SkillName,
    description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    compatibility: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    metadata: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    allowed_tools: Vec<String>,
    package_sha256: String,
    skill_md_sha256: String,
    skill_md_bytes: u64,
    file_count: u32,
    total_bytes: u64,
    source: String,
}

impl SkillRecord {
    /// Identifier of the stored package directory.
    pub const fn skill_id(&self) -> Uuid {
        self.skill_id
    }

    /// The skill name, which equals its package directory name.
    pub const fn name(&self) -> &SkillName {
        &self.name
    }

    /// The manifest description. May contain line feeds.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The manifest license text, if any.
    pub fn license(&self) -> Option<&str> {
        self.license.as_deref()
    }

    /// The manifest compatibility text, if any.
    pub fn compatibility(&self) -> Option<&str> {
        self.compatibility.as_deref()
    }

    /// The manifest metadata.
    pub const fn metadata(&self) -> &BTreeMap<String, String> {
        &self.metadata
    }

    /// The advisory allowed-tool entries. They never narrow or widen anything.
    pub fn allowed_tools(&self) -> &[String] {
        &self.allowed_tools
    }

    /// SHA-256 of the canonical package manifest, lowercase hex.
    pub fn package_sha256(&self) -> &str {
        &self.package_sha256
    }

    /// SHA-256 of the stored `SKILL.md`, lowercase hex.
    pub fn skill_md_sha256(&self) -> &str {
        &self.skill_md_sha256
    }

    /// Size of the stored `SKILL.md` in bytes.
    pub const fn skill_md_bytes(&self) -> u64 {
        self.skill_md_bytes
    }

    /// Number of files in the package.
    pub const fn file_count(&self) -> u32 {
        self.file_count
    }

    /// Total bytes of the package files.
    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// The operator's source folder as lossy display text. Never a key.
    pub fn source(&self) -> &str {
        &self.source
    }
}

/// An import's result, before it is validated and recorded.
#[derive(Clone, Debug)]
pub struct SkillRegistration {
    pub skill_id: Uuid,
    pub name: SkillName,
    pub description: String,
    pub license: Option<String>,
    pub compatibility: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub allowed_tools: Vec<String>,
    pub package_sha256: String,
    pub skill_md_sha256: String,
    pub skill_md_bytes: u64,
    pub file_count: u32,
    pub total_bytes: u64,
    pub source: String,
}

impl From<&SkillRegistration> for SkillRecord {
    fn from(value: &SkillRegistration) -> Self {
        Self {
            skill_id: value.skill_id,
            name: value.name.clone(),
            description: value.description.clone(),
            license: value.license.clone(),
            compatibility: value.compatibility.clone(),
            metadata: value.metadata.clone(),
            allowed_tools: value.allowed_tools.clone(),
            package_sha256: value.package_sha256.clone(),
            skill_md_sha256: value.skill_md_sha256.clone(),
            skill_md_bytes: value.skill_md_bytes,
            file_count: value.file_count,
            total_bytes: value.total_bytes,
            source: value.source.clone(),
        }
    }
}

/// A skill with its recorded settings, for listings.
#[derive(Clone, Debug, Serialize)]
pub struct SkillStatus {
    #[serde(flatten)]
    pub skill: SkillRecord,
    /// The application setting, or `None` when unset (disabled).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub application: Option<bool>,
    /// Workspace overrides: workspace and whether it enables the skill.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub workspace_overrides: Vec<SkillWorkspaceOverride>,
    /// Effective enablement for the requested workspace, if one was named.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective: Option<bool>,
}

/// One workspace's override of the application setting.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SkillWorkspaceOverride {
    pub workspace_id: WorkspaceId,
    pub enabled: bool,
}

impl ControlSnapshot {
    /// Every registered skill, in registration order.
    pub fn skills(&self) -> &[SkillRecord] {
        &self.skills
    }

    /// The registered skill with this name.
    pub fn skill(&self, name: &SkillName) -> Option<&SkillRecord> {
        self.skills.iter().find(|record| record.name == *name)
    }

    /// The skill, only when it is registered and effectively enabled for this
    /// workspace. A workspace override wins over the application setting, and
    /// an unset skill is disabled. Enabling a skill grants no permission.
    pub fn effective_skill(
        &self,
        workspace_id: WorkspaceId,
        name: &SkillName,
    ) -> Option<&SkillRecord> {
        let record = self.skill(name)?;
        self.effective_enabled(record.skill_id, workspace_id)
            .then_some(record)
    }

    /// Every skill with its application setting and workspace overrides. With
    /// a workspace, each entry also reports the effective value there.
    pub fn skill_statuses(&self, workspace_id: Option<WorkspaceId>) -> Vec<SkillStatus> {
        self.skills
            .iter()
            .map(|record| {
                let mut application = None;
                let mut workspace_overrides = Vec::new();
                for setting in self
                    .skill_enablement
                    .iter()
                    .filter(|setting| setting.skill_id == record.skill_id)
                {
                    match setting.scope {
                        SkillScope::Application => application = Some(setting.enabled),
                        SkillScope::Workspace { workspace_id } => {
                            workspace_overrides.push(SkillWorkspaceOverride {
                                workspace_id,
                                enabled: setting.enabled,
                            });
                        }
                    }
                }
                SkillStatus {
                    skill: record.clone(),
                    application,
                    workspace_overrides,
                    effective: workspace_id
                        .map(|workspace| self.effective_enabled(record.skill_id, workspace)),
                }
            })
            .collect()
    }

    fn setting(&self, skill_id: Uuid, scope: SkillScope) -> Option<bool> {
        self.skill_enablement
            .iter()
            .find(|setting| setting.skill_id == skill_id && setting.scope == scope)
            .map(|setting| setting.enabled)
    }

    fn effective_enabled(&self, skill_id: Uuid, workspace_id: WorkspaceId) -> bool {
        self.setting(skill_id, SkillScope::Workspace { workspace_id })
            .or_else(|| self.setting(skill_id, SkillScope::Application))
            .unwrap_or(false)
    }
}

impl ControlStore {
    /// Every registered skill, in registration order.
    pub fn skills(&self) -> &[SkillRecord] {
        self.snapshot.skills()
    }

    /// The registered skill with this name.
    pub fn skill(&self, name: &SkillName) -> Option<&SkillRecord> {
        self.snapshot.skill(name)
    }

    /// The skill when it is registered and effectively enabled for the
    /// workspace. See [`ControlSnapshot::effective_skill`].
    pub fn effective_skill(
        &self,
        workspace_id: WorkspaceId,
        name: &SkillName,
    ) -> Option<&SkillRecord> {
        self.snapshot.effective_skill(workspace_id, name)
    }

    /// Every skill with its settings. See [`ControlSnapshot::skill_statuses`].
    pub fn skill_statuses(&self, workspace_id: Option<WorkspaceId>) -> Vec<SkillStatus> {
        self.snapshot.skill_statuses(workspace_id)
    }

    /// Register an imported skill. The skill starts disabled. This advances
    /// the settings revision and grants nothing.
    pub fn add_skill(
        &mut self,
        registration: &SkillRegistration,
        expected_revision: u64,
    ) -> Result<SkillRecord, ControlError> {
        self.check_revision(expected_revision)?;
        let record = SkillRecord::from(registration);
        check_add(&self.snapshot, &record)?;
        self.append(Action::SkillAdded(record.clone()), self.next_revision()?)?;
        Ok(record)
    }

    /// Remove a skill and its enablement settings. This advances the
    /// settings revision. The caller then deletes the package directory.
    pub fn remove_skill(
        &mut self,
        name: &SkillName,
        expected_revision: u64,
    ) -> Result<SkillRecord, ControlError> {
        self.check_revision(expected_revision)?;
        let record = self.skill(name).ok_or(SkillRefusal::NotFound)?.clone();
        self.append(
            Action::SkillRemoved {
                skill_id: record.skill_id,
            },
            self.next_revision()?,
        )?;
        Ok(record)
    }

    /// Enable, disable, or (for a workspace) inherit a skill. This advances
    /// the settings revision.
    pub fn set_skill_enablement(
        &mut self,
        name: &SkillName,
        scope: SkillScope,
        setting: SkillSetting,
        expected_revision: u64,
    ) -> Result<SkillRecord, ControlError> {
        self.check_revision(expected_revision)?;
        let record = self.skill(name).ok_or(SkillRefusal::NotFound)?.clone();
        check_scope(&self.snapshot, scope, setting)?;
        self.append(
            Action::SkillEnablementSet {
                skill_id: record.skill_id,
                scope,
                setting,
            },
            self.next_revision()?,
        )?;
        Ok(record)
    }
}

/// Validate and apply one skill action during replay or append preflight.
pub(super) fn apply(state: &mut ControlSnapshot, action: &Action) -> Result<(), ControlError> {
    match action {
        Action::SkillAdded(record) => {
            check_add(state, record).map_err(replay_error)?;
            state.skills.push(record.clone());
        }
        Action::SkillRemoved { skill_id } => {
            let index = state
                .skills
                .iter()
                .position(|record| record.skill_id == *skill_id)
                .ok_or(ControlError::Invalid("removal of an unknown skill"))?;
            state.skills.remove(index);
            state
                .skill_enablement
                .retain(|setting| setting.skill_id != *skill_id);
        }
        Action::SkillEnablementSet {
            skill_id,
            scope,
            setting,
        } => {
            if !state
                .skills
                .iter()
                .any(|record| record.skill_id == *skill_id)
            {
                return Err(ControlError::Invalid("enablement of an unknown skill"));
            }
            check_scope(state, *scope, *setting).map_err(replay_error)?;
            let existing = state
                .skill_enablement
                .iter()
                .position(|item| item.skill_id == *skill_id && item.scope == *scope);
            match (setting, existing) {
                (SkillSetting::Inherit, Some(index)) => {
                    state.skill_enablement.remove(index);
                }
                (SkillSetting::Inherit, None) => {}
                (_, Some(index)) => {
                    if let Some(item) = state.skill_enablement.get_mut(index) {
                        item.enabled = *setting == SkillSetting::Enabled;
                    }
                }
                (_, None) => state.skill_enablement.push(SkillEnablement {
                    skill_id: *skill_id,
                    scope: *scope,
                    enabled: *setting == SkillSetting::Enabled,
                }),
            }
        }
        _ => {}
    }
    Ok(())
}

fn replay_error(refusal: SkillRefusal) -> ControlError {
    ControlError::Invalid(refusal.message())
}

fn check_scope(
    state: &ControlSnapshot,
    scope: SkillScope,
    setting: SkillSetting,
) -> Result<(), SkillRefusal> {
    match scope {
        SkillScope::Application => {
            if setting == SkillSetting::Inherit {
                return Err(SkillRefusal::InheritNeedsWorkspace);
            }
        }
        SkillScope::Workspace { workspace_id } => {
            if !state
                .workspaces
                .iter()
                .any(|workspace| workspace.id == workspace_id)
            {
                return Err(SkillRefusal::UnknownWorkspace);
            }
        }
    }
    Ok(())
}

fn check_add(state: &ControlSnapshot, record: &SkillRecord) -> Result<(), SkillRefusal> {
    validate_record(record)?;
    if state
        .skills
        .iter()
        .any(|existing| existing.name == record.name)
    {
        return Err(SkillRefusal::NameTaken);
    }
    if state
        .skills
        .iter()
        .any(|existing| existing.skill_id == record.skill_id)
    {
        return Err(SkillRefusal::IdTaken);
    }
    if state.skills.len() >= MAX_REGISTERED_SKILLS {
        return Err(SkillRefusal::TooManySkills);
    }
    Ok(())
}

fn bad(reason: &'static str) -> SkillRefusal {
    SkillRefusal::InvalidField(reason)
}

fn char_count_in(text: &str, minimum: usize, maximum: usize) -> bool {
    // Bound the byte length first so no scan is unbounded: a char is at most
    // four bytes.
    text.len() <= maximum.saturating_mul(4) && {
        let count = text.chars().count();
        count >= minimum && count <= maximum
    }
}

fn no_controls(text: &str, allow_line_feed: bool) -> bool {
    text.chars()
        .all(|ch| !ch.is_ascii_control() || (allow_line_feed && ch == '\n'))
}

fn valid_digest(text: &str) -> bool {
    text.len() == DIGEST_HEX_CHARS
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Check every field of a record against its bound. Replay uses this too.
fn validate_record(record: &SkillRecord) -> Result<(), SkillRefusal> {
    if !valid_uuid(record.skill_id) {
        return Err(bad("the skill identifier must be a version 4 UUID"));
    }
    if !char_count_in(&record.description, 1, MAX_SKILL_DESCRIPTION_CHARS)
        || !no_controls(&record.description, true)
    {
        return Err(bad(
            "the description must be 1 to 1024 characters with no control character but line feed",
        ));
    }
    if let Some(license) = &record.license
        && (!char_count_in(license, 1, MAX_SKILL_LICENSE_CHARS) || !no_controls(license, false))
    {
        return Err(bad(
            "the license must be 1 to 256 characters with no control character",
        ));
    }
    if let Some(compatibility) = &record.compatibility
        && (!char_count_in(compatibility, 1, MAX_SKILL_COMPATIBILITY_CHARS)
            || !no_controls(compatibility, false))
    {
        return Err(bad(
            "the compatibility text must be 1 to 500 characters with no control character",
        ));
    }
    if record.metadata.len() > MAX_SKILL_METADATA_ENTRIES {
        return Err(bad("metadata may have at most 16 entries"));
    }
    for (key, value) in &record.metadata {
        if !char_count_in(key, 1, MAX_SKILL_METADATA_KEY_CHARS)
            || !char_count_in(value, 0, MAX_SKILL_METADATA_VALUE_CHARS)
            || !no_controls(key, false)
            || !no_controls(value, false)
        {
            return Err(bad(
                "metadata keys are 1 to 64 and values 0 to 256 characters with no control character",
            ));
        }
    }
    if record.allowed_tools.len() > MAX_SKILL_ALLOWED_TOOLS {
        return Err(bad("allowed tools may have at most 32 entries"));
    }
    let mut tool_bytes = 0_usize;
    for tool in &record.allowed_tools {
        tool_bytes = tool_bytes
            .checked_add(tool.len())
            .ok_or_else(|| bad("allowed tools text is too long"))?;
        if tool.is_empty()
            || tool
                .chars()
                .any(|ch| ch.is_ascii_whitespace() || ch.is_ascii_control())
        {
            return Err(bad(
                "allowed tool entries must be non-empty with no whitespace or control character",
            ));
        }
    }
    if tool_bytes > MAX_SKILL_ALLOWED_TOOLS_BYTES {
        return Err(bad("allowed tools text may total at most 1024 bytes"));
    }
    if !valid_digest(&record.package_sha256) || !valid_digest(&record.skill_md_sha256) {
        return Err(bad("digests must be 64 lowercase hexadecimal characters"));
    }
    if record.skill_md_bytes == 0 || record.skill_md_bytes > MAX_SKILL_MD_BYTES {
        return Err(bad("SKILL.md must be 1 to 65536 bytes"));
    }
    if record.file_count == 0 || record.file_count > MAX_SKILL_FILES {
        return Err(bad("a package has 1 to 128 files"));
    }
    if record.total_bytes < record.skill_md_bytes || record.total_bytes > MAX_SKILL_PACKAGE_BYTES {
        return Err(bad(
            "a package totals at least its SKILL.md and at most 4194304 bytes",
        ));
    }
    if record.source.is_empty()
        || record.source.len() > MAX_SKILL_SOURCE_BYTES
        || !no_controls(&record.source, false)
    {
        return Err(bad(
            "the source must be 1 to 1024 bytes with no control character",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
