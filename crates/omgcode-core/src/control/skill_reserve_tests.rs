//! A full settings journal still lets the operator withdraw a skill, and an
//! uncertain registration write keeps the package it may name.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use uuid::Uuid;

use super::skill::SkillRegistration;
use super::*;
use crate::skills::SkillName;
use crate::test_support::private_data_root;

fn registration(name: &str) -> SkillRegistration {
    SkillRegistration {
        skill_id: Uuid::new_v4(),
        name: SkillName::new(name).expect("name"),
        description: "Reviews a change.".into(),
        license: None,
        compatibility: None,
        metadata: BTreeMap::new(),
        allowed_tools: Vec::new(),
        package_sha256: "a".repeat(64),
        skill_md_sha256: "b".repeat(64),
        skill_md_bytes: 100,
        file_count: 1,
        total_bytes: 100,
        source: "C:\\skills\\example".into(),
    }
}

fn store() -> (tempfile::TempDir, ControlStore) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let store = ControlStore::open(&directory.path().join("data")).expect("store");
    (directory, store)
}

#[test]
fn the_largest_skill_removal_event_fits_its_reserve() {
    let entry = AuditEntry {
        schema_version: u32::MAX,
        store_id: Uuid::new_v4(),
        event_id: Uuid::new_v4(),
        sequence: u64::MAX,
        timestamp: "2026-11-18T19:15:40.641064900+14:00".into(),
        revision: u64::MAX,
        action: Action::SkillRemoved {
            skill_id: Uuid::new_v4(),
        },
    };
    let encoded = serde_json::to_vec(&entry).expect("event JSON");

    assert!(
        encoded.len() < SKILL_REMOVAL_RESERVE_BYTES,
        "{} bytes",
        encoded.len()
    );
}

#[test]
fn given_a_journal_filled_to_the_reserve_when_settings_change_then_only_removal_fits() {
    let (_directory, mut store) = store();
    let name = SkillName::new("demo-skill").expect("name");
    store
        .add_skill(&registration("demo-skill"), store.revision())
        .expect("add");
    store
        .set_skill_enablement(
            &name,
            SkillScope::Application,
            SkillSetting::Enabled,
            store.revision(),
        )
        .expect("enable");
    // Leave just the removal reserve for the one skill plus a little.
    store.bytes = MAX_BYTES - SKILL_REMOVAL_RESERVE_BYTES - 64;

    let refused = store.set_skill_enablement(
        &name,
        SkillScope::Application,
        SkillSetting::Disabled,
        store.revision(),
    );
    assert!(
        matches!(refused, Err(ControlError::LimitReached)),
        "{refused:?}"
    );
    assert!(matches!(
        store.add_skill(&registration("other-skill"), store.revision()),
        Err(ControlError::LimitReached)
    ));

    let removed = store
        .remove_skill(&name, store.revision())
        .expect("removal uses the reserve");
    assert_eq!(removed.name(), &name);
    assert!(store.skills().is_empty());
}

struct ImportScene {
    _directory: tempfile::TempDir,
    data: PathBuf,
    source: PathBuf,
}

fn import_scene() -> ImportScene {
    let directory = tempfile::tempdir().expect("temporary directory");
    let data = directory.path().join("data");
    private_data_root(&data);
    let source = directory.path().join("demo-skill");
    fs::create_dir(&source).expect("source");
    fs::write(
        source.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: Demonstrates a skill.\n---\nFollow the steps.\n",
    )
    .expect("SKILL.md");
    ImportScene {
        _directory: directory,
        data,
        source,
    }
}

fn package_names(data: &std::path::Path) -> Vec<String> {
    fs::read_dir(data.join("skills-v1").join("packages"))
        .expect("packages directory")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

#[test]
fn given_a_sync_failure_after_the_registration_write_when_importing_then_the_package_is_kept() {
    let scene = import_scene();
    let mut store = ControlStore::open(&scene.data).expect("store");
    store.fail_next_sync = true;
    let revision = store.revision();

    let error = store
        .import_skill(&scene.source, revision)
        .expect_err("uncertain registration");

    let SkillOperationError::RegistrationUncertain { skill_id, .. } = &error else {
        panic!("expected an uncertain registration, got {error:?}");
    };
    assert_eq!(error.code(), "registration_uncertain");
    assert!(error.to_string().contains(&skill_id.to_string()));
    assert_eq!(package_names(&scene.data), vec![skill_id.to_string()]);
    drop(store);
    // The complete record reached the file, so replay restores it, and the
    // kept package still serves it.
    let mut reopened = ControlStore::open_existing(&scene.data)
        .expect("replay")
        .expect("store");
    let name = SkillName::new("demo-skill").expect("name");
    assert_eq!(
        reopened.skill(&name).map(SkillRecord::skill_id),
        Some(*skill_id)
    );
    let workspace = {
        let root = scene.data.parent().expect("parent").join("projects");
        fs::create_dir_all(root.join("work")).expect("workspace");
        reopened
            .set_development_root(&root, reopened.revision())
            .expect("root");
        let revision = reopened.revision();
        reopened
            .register_workspace(&root.join("work"), revision)
            .expect("workspace")
            .id()
    };
    reopened
        .set_skill_enablement(
            &name,
            SkillScope::Application,
            SkillSetting::Enabled,
            reopened.revision(),
        )
        .expect("enable");
    reopened
        .load_enabled_skill(workspace, &name)
        .expect("the kept package loads");
}

#[test]
fn a_registration_error_names_a_package_it_could_not_remove() {
    let skill_id = Uuid::new_v4();
    let error = SkillOperationError::Registration {
        error: ControlError::Skill(SkillRefusal::NameTaken),
        orphaned_package: Some(skill_id),
    };

    assert_eq!(error.code(), "package_orphaned");
    assert!(error.to_string().contains(&skill_id.to_string()));
    let cleaned = SkillOperationError::Registration {
        error: ControlError::Skill(SkillRefusal::NameTaken),
        orphaned_package: None,
    };
    assert!(!cleaned.to_string().contains("could not be removed"));
}
