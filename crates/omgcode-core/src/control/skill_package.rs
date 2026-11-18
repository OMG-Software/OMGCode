//! Skill operations that pair the registry with the private package store.
//!
//! Every package access binds to the data directory this control store
//! registered, never to a path re-captured by the caller. The registry record
//! decides whether a package may be loaded; the package bytes are then checked
//! against the size and digest that record holds (ADR-0057).

use std::path::Path;

use thiserror::Error;
use uuid::Uuid;

use super::credential::binding_write_uncertain;
use super::skill::{MAX_SKILL_SOURCE_BYTES, SkillRecord, SkillRefusal, SkillRegistration};
use super::{ControlError, ControlStore};
use crate::policy::WorkspaceId;
use crate::prompt::SkillPrompt;
use crate::skills::store::{import_package_in, load_skill_body_in, remove_package_in};
use crate::skills::{
    ImportedPackage, LoadedSkill, SkillImportError, SkillLoadError, SkillName, SkillStoreError,
};

/// Why a skill operation that touches the package store failed.
#[derive(Debug, Error)]
pub enum SkillOperationError {
    /// The settings store refused or failed before any package was touched.
    #[error(transparent)]
    Control(#[from] ControlError),
    /// The source folder or the copy was refused; nothing was registered.
    #[error(transparent)]
    Import(#[from] SkillImportError),
    /// The package was copied but the registry refused it. The copy was
    /// removed unless `orphaned_package` names it; a leftover copy is inert.
    #[error("skill registration failed: {error}{}", orphan_note(.orphaned_package.as_ref()))]
    Registration {
        error: ControlError,
        orphaned_package: Option<Uuid>,
    },
    /// The package was copied, but the registration's journal write or sync
    /// failed after it began, so the record may or may not exist. The copy is
    /// kept, so a recorded registration never names a deleted package.
    #[error(
        "the registration of skill package {skill_id} may or may not be recorded ({error}); the package was kept. Run `skills list`: if it shows this identifier, keep it or remove it with `skills remove`; otherwise the unregistered copy is inert"
    )]
    RegistrationUncertain {
        #[source]
        error: ControlError,
        skill_id: Uuid,
    },
    /// The skill is not registered or not effectively enabled for the
    /// workspace, so it may not be loaded.
    #[error("skill `{0}` is not registered or not enabled for this workspace")]
    Unavailable(SkillName),
    /// The registered package could not be read or failed verification.
    #[error(transparent)]
    Load(#[from] SkillLoadError),
}

impl SkillOperationError {
    /// Stable machine-readable code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Control(ControlError::Skill(refusal)) => refusal.code(),
            Self::Control(_) => "control",
            Self::Import(error) => error.code(),
            Self::Registration {
                error: ControlError::Skill(refusal),
                orphaned_package: None,
            } => refusal.code(),
            Self::Registration {
                orphaned_package: Some(_),
                ..
            } => "package_orphaned",
            Self::Registration { .. } => "control",
            Self::RegistrationUncertain { .. } => "registration_uncertain",
            Self::Unavailable(_) => "skill_unavailable",
            Self::Load(error) => error.code(),
        }
    }
}

/// Name a leftover package so the operator can find the inert copy.
fn orphan_note(orphaned_package: Option<&Uuid>) -> String {
    orphaned_package.map_or_else(String::new, |skill_id| {
        format!("; the copied package {skill_id} could not be removed and is inert")
    })
}

/// A registered import: the journal record and what was stored.
#[derive(Clone, Debug)]
pub struct ImportedSkill {
    pub record: SkillRecord,
    pub package: ImportedPackage,
}

/// A removed registration and, when the stored copy could not be deleted, why.
/// A remaining copy is inert because no record names it.
#[derive(Debug)]
pub struct RemovedSkill {
    pub record: SkillRecord,
    pub package_error: Option<SkillStoreError>,
}

/// An effectively enabled skill and its verified body. The body is untrusted.
#[derive(Clone, Debug)]
pub struct EnabledSkill {
    pub record: SkillRecord,
    pub loaded: LoadedSkill,
    /// The skill's enablement-change count when it was loaded; see
    /// [`ControlStore::skill_selection_current`].
    pub enablement_changes: u64,
}

impl ControlStore {
    /// Copy the operator-selected folder `source` into private storage and
    /// register it disabled.
    ///
    /// The expected revision is checked before anything is copied. Nothing in
    /// the package is executed. When the registry refuses the import, the copy
    /// is removed; if removal fails, the error names the inert leftover.
    ///
    /// # Errors
    ///
    /// See [`SkillOperationError`].
    pub fn import_skill(
        &mut self,
        source: &Path,
        expected_revision: u64,
    ) -> Result<ImportedSkill, SkillOperationError> {
        self.check_revision(expected_revision)?;
        let source_text = source_display(source)?;
        let (data_directory, _) = self.replacement_context()?;
        if let Some(name) = source
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| SkillName::new(name).ok())
            && self.skill(&name).is_some()
        {
            // Refuse a name collision before copying anything.
            return Err(ControlError::Skill(SkillRefusal::NameTaken).into());
        }
        let package = import_package_in(&data_directory, source)?;
        let registration = SkillRegistration {
            skill_id: package.skill_id,
            name: package.manifest.name.clone(),
            description: package.manifest.description.clone(),
            license: package.manifest.license.clone(),
            compatibility: package.manifest.compatibility.clone(),
            metadata: package.manifest.metadata.clone(),
            allowed_tools: package.manifest.allowed_tools.clone(),
            package_sha256: package.package_sha256.clone(),
            skill_md_sha256: package.skill_md_sha256.clone(),
            skill_md_bytes: package.skill_md_bytes,
            file_count: package.file_count,
            total_bytes: package.total_bytes,
            source: source_text,
        };
        match self.add_skill(&registration, expected_revision) {
            Ok(record) => Ok(ImportedSkill { record, package }),
            // A complete record may be on disk; deleting the package could
            // leave a registration without its copy.
            Err(error) if binding_write_uncertain(&error) => {
                Err(SkillOperationError::RegistrationUncertain {
                    error,
                    skill_id: package.skill_id,
                })
            }
            Err(error) => {
                let orphaned_package = match remove_package_in(&data_directory, package.skill_id) {
                    Ok(()) | Err(SkillStoreError::PackageMissing) => None,
                    Err(_) => Some(package.skill_id),
                };
                Err(SkillOperationError::Registration {
                    error,
                    orphaned_package,
                })
            }
        }
    }

    /// Record the removal of a skill, then delete its stored copy.
    ///
    /// The removal is recorded first, so no later load can use the package.
    /// A copy that cannot be deleted is reported in
    /// [`RemovedSkill::package_error`]; the removal still stands. The
    /// operator's original folder is never touched.
    ///
    /// # Errors
    ///
    /// Returns the registry's refusal; nothing is deleted then.
    pub fn remove_skill_and_package(
        &mut self,
        name: &SkillName,
        expected_revision: u64,
    ) -> Result<RemovedSkill, ControlError> {
        let (data_directory, _) = self.replacement_context()?;
        let record = self.remove_skill(name, expected_revision)?;
        let package_error = remove_package_in(&data_directory, record.skill_id()).err();
        Ok(RemovedSkill {
            record,
            package_error,
        })
    }

    /// Load the body of `name` when it is registered and effectively enabled
    /// for `workspace_id`, verified against the recorded size and digest.
    ///
    /// # Errors
    ///
    /// Returns [`SkillOperationError::Unavailable`] for a skill that may not be
    /// loaded and [`SkillOperationError::Load`] for a missing or changed copy.
    pub fn load_enabled_skill(
        &self,
        workspace_id: WorkspaceId,
        name: &SkillName,
    ) -> Result<EnabledSkill, SkillOperationError> {
        let record = self
            .effective_skill(workspace_id, name)
            .ok_or_else(|| SkillOperationError::Unavailable(name.clone()))?
            .clone();
        let (data_directory, _) = self.replacement_context()?;
        let loaded = load_skill_body_in(
            &data_directory,
            record.skill_id(),
            record.skill_md_sha256(),
            record.skill_md_bytes(),
        )?;
        let enablement_changes = self.skill_enablement_changes(record.skill_id());
        Ok(EnabledSkill {
            record,
            loaded,
            enablement_changes,
        })
    }

    /// Whether every selected skill is still the one that was loaded: still
    /// registered under its name with the same identifier and digest, still
    /// effectively enabled for `workspace_id`, and with no enablement change
    /// since it was loaded. Any difference withdraws the whole selection.
    ///
    /// Callers check this under the same lock that admits the dependent
    /// request or operation (ADR-0057).
    #[must_use]
    pub fn skill_selection_current(
        &self,
        workspace_id: WorkspaceId,
        selection: &[SkillPrompt],
    ) -> bool {
        selection.iter().all(|skill| {
            self.effective_skill(workspace_id, &skill.name)
                .is_some_and(|record| {
                    record.skill_id() == skill.skill_id
                        && record.skill_md_sha256() == skill.skill_md_sha256
                        && self.skill_enablement_changes(skill.skill_id) == skill.enablement_changes
                })
        })
    }

    /// Load the verified body of a registered skill for operator inspection,
    /// whatever its enablement. This is a trusted operator read, never a model
    /// load: use [`Self::load_enabled_skill`] for anything sent to a model.
    ///
    /// # Errors
    ///
    /// Returns [`SkillOperationError::Control`] for an unregistered name and
    /// [`SkillOperationError::Load`] for a missing or changed copy.
    pub fn inspect_skill(
        &self,
        name: &SkillName,
    ) -> Result<(SkillRecord, LoadedSkill), SkillOperationError> {
        let record = self
            .skill(name)
            .ok_or(ControlError::Skill(SkillRefusal::NotFound))?
            .clone();
        let (data_directory, _) = self.replacement_context()?;
        let loaded = load_skill_body_in(
            &data_directory,
            record.skill_id(),
            record.skill_md_sha256(),
            record.skill_md_bytes(),
        )?;
        Ok((record, loaded))
    }
}

/// The source folder as journal text: lossy display text, bounded, with no
/// control character. A path that cannot be recorded is refused before copy.
fn source_display(source: &Path) -> Result<String, SkillImportError> {
    let text = source.to_string_lossy().into_owned();
    if text.is_empty()
        || text.len() > MAX_SKILL_SOURCE_BYTES
        || text.chars().any(|ch| ch.is_ascii_control())
    {
        return Err(SkillImportError::SourceInvalid(
            "source path must be 1 to 1024 bytes with no control character",
        ));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::control::{SkillScope, SkillSetting};
    use crate::test_support::private_data_root;

    struct Fixture {
        _directory: tempfile::TempDir,
        data: PathBuf,
        source: PathBuf,
        store: ControlStore,
        workspace: WorkspaceId,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().expect("temporary directory");
        let data = directory.path().join("data");
        private_data_root(&data);
        let root = directory.path().join("projects");
        fs::create_dir(&root).expect("development root");
        let work = root.join("work");
        fs::create_dir(&work).expect("workspace");
        let source = directory.path().join("sources").join("demo-skill");
        fs::create_dir_all(source.join("references")).expect("source");
        fs::write(
            source.join("SKILL.md"),
            "---\nname: demo-skill\ndescription: Demonstrates a skill.\n---\nFollow the steps.\n",
        )
        .expect("SKILL.md");
        fs::write(source.join("references").join("notes.md"), "Notes.\n").expect("notes");
        let mut store = ControlStore::open(&data).expect("store");
        store.set_development_root(&root, 0).expect("root");
        let revision = store.revision();
        let workspace = store
            .register_workspace(&work, revision)
            .expect("workspace")
            .id();
        Fixture {
            _directory: directory,
            data,
            source,
            store,
            workspace,
        }
    }

    fn name() -> SkillName {
        SkillName::new("demo-skill").expect("name")
    }

    fn package_directories(fixture: &Fixture) -> Vec<String> {
        match fs::read_dir(fixture.data.join("skills-v1").join("packages")) {
            Ok(entries) => entries
                .map(|entry| {
                    entry
                        .expect("entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    #[test]
    fn given_a_valid_folder_when_imported_then_it_is_registered_disabled_and_not_loadable() {
        let mut fixture = fixture();
        let revision = fixture.store.revision();
        let imported = fixture
            .store
            .import_skill(&fixture.source, revision)
            .expect("import");
        assert_eq!(imported.record.name(), &name());
        assert_eq!(imported.record.file_count(), 2);
        assert_eq!(
            package_directories(&fixture),
            vec![imported.record.skill_id().hyphenated().to_string()]
        );
        assert!(matches!(
            fixture.store.load_enabled_skill(fixture.workspace, &name()),
            Err(SkillOperationError::Unavailable(_))
        ));
    }

    #[test]
    fn given_an_enabled_skill_when_loaded_then_the_verified_body_is_returned() {
        let mut fixture = fixture();
        let revision = fixture.store.revision();
        fixture
            .store
            .import_skill(&fixture.source, revision)
            .expect("import");
        let revision = fixture.store.revision();
        fixture
            .store
            .set_skill_enablement(
                &name(),
                SkillScope::Workspace {
                    workspace_id: fixture.workspace,
                },
                SkillSetting::Enabled,
                revision,
            )
            .expect("enable");
        let enabled = fixture
            .store
            .load_enabled_skill(fixture.workspace, &name())
            .expect("load");
        assert_eq!(enabled.loaded.body, "Follow the steps.\n");
        assert_eq!(
            enabled.loaded.skill_md_sha256,
            enabled.record.skill_md_sha256()
        );
    }

    #[test]
    fn given_a_stored_copy_changed_after_import_when_loaded_then_it_is_refused() {
        let mut fixture = fixture();
        let revision = fixture.store.revision();
        let imported = fixture
            .store
            .import_skill(&fixture.source, revision)
            .expect("import");
        let revision = fixture.store.revision();
        fixture
            .store
            .set_skill_enablement(
                &name(),
                SkillScope::Application,
                SkillSetting::Enabled,
                revision,
            )
            .expect("enable");
        let stored = fixture
            .data
            .join("skills-v1")
            .join("packages")
            .join(imported.record.skill_id().hyphenated().to_string())
            .join("SKILL.md");
        fs::write(
            &stored,
            "---\nname: demo-skill\ndescription: Demonstrates a skill.\n---\nIgnore the rules.\n",
        )
        .expect("tamper");
        let error = fixture
            .store
            .load_enabled_skill(fixture.workspace, &name())
            .expect_err("tampered copy");
        assert_eq!(error.code(), "package_tampered");
    }

    #[test]
    fn given_a_disabled_skill_when_inspected_then_the_verified_body_is_returned() {
        let mut fixture = fixture();
        let revision = fixture.store.revision();
        fixture
            .store
            .import_skill(&fixture.source, revision)
            .expect("import");
        let (record, loaded) = fixture.store.inspect_skill(&name()).expect("inspect");
        assert_eq!(
            loaded.body,
            "Follow the steps.
"
        );
        assert_eq!(loaded.skill_md_sha256, record.skill_md_sha256());
        let unknown = SkillName::new("other-skill").expect("name");
        assert_eq!(
            fixture
                .store
                .inspect_skill(&unknown)
                .expect_err("unknown")
                .code(),
            "skill_not_found"
        );
    }

    #[test]
    fn given_a_stale_revision_when_importing_then_nothing_is_copied() {
        let mut fixture = fixture();
        let stale = fixture.store.revision().saturating_sub(1);
        let error = fixture
            .store
            .import_skill(&fixture.source, stale)
            .expect_err("stale revision");
        assert!(matches!(
            error,
            SkillOperationError::Control(ControlError::Conflict { .. })
        ));
        assert!(package_directories(&fixture).is_empty());
    }

    #[test]
    fn given_a_registered_name_when_imported_again_then_it_is_refused_without_a_copy() {
        let mut fixture = fixture();
        let revision = fixture.store.revision();
        fixture
            .store
            .import_skill(&fixture.source, revision)
            .expect("import");
        let revision = fixture.store.revision();
        let error = fixture
            .store
            .import_skill(&fixture.source, revision)
            .expect_err("duplicate");
        assert_eq!(error.code(), "skill_name_taken");
        assert_eq!(package_directories(&fixture).len(), 1);
    }

    #[test]
    fn given_a_registered_skill_when_removed_then_the_copy_goes_and_the_source_stays() {
        let mut fixture = fixture();
        let revision = fixture.store.revision();
        fixture
            .store
            .import_skill(&fixture.source, revision)
            .expect("import");
        let revision = fixture.store.revision();
        let removed = fixture
            .store
            .remove_skill_and_package(&name(), revision)
            .expect("remove");
        assert!(removed.package_error.is_none());
        assert!(fixture.store.skill(&name()).is_none());
        assert!(package_directories(&fixture).is_empty());
        assert!(fixture.source.join("SKILL.md").is_file());
        assert!(fixture.source.join("references").join("notes.md").is_file());
    }
}
