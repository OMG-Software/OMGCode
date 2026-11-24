//! Workspace unregistration (ADR-0081): what the event drops, what it refuses,
//! and that replay applies it.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use uuid::Uuid;

use super::container::ContainerGrantAccess;
use super::skill::{SkillRegistration, SkillScope, SkillSetting};
use super::{Action, ControlError, ControlStore};
use crate::policy::{Access, PermissionSet, WorkspaceId};
use crate::skills::SkillName;
use crate::workspace::DirectoryRecord;

/// Fields drop in declaration order: the store holds the journal open, so it
/// must drop before the directory that contains the journal.
struct Scene {
    store: ControlStore,
    data: PathBuf,
    root: PathBuf,
    cache: PathBuf,
    _directory: tempfile::TempDir,
}

fn scene() -> Scene {
    let directory = tempfile::tempdir().expect("temporary directory");
    let data = directory.path().join("data");
    let root = directory.path().join("projects");
    let cache = directory.path().join("cache");
    fs::create_dir(&root).expect("root");
    fs::create_dir(&cache).expect("cache");
    let mut store = ControlStore::open(&data).expect("store");
    store.set_development_root(&root, 0).expect("root");
    Scene {
        store,
        data,
        root,
        cache,
        _directory: directory,
    }
}

impl Scene {
    fn register(&mut self, name: &str) -> (WorkspaceId, PathBuf) {
        let path = self.root.join(name);
        fs::create_dir(&path).expect("workspace directory");
        let revision = self.store.revision();
        let id = self
            .store
            .register_workspace(&path, revision)
            .expect("workspace")
            .id();
        (id, path)
    }
}

fn skill_registration(name: &str) -> SkillRegistration {
    SkillRegistration {
        skill_id: Uuid::new_v4(),
        name: SkillName::new(name).expect("skill name"),
        description: "Reviews a change.".into(),
        license: None,
        compatibility: None,
        metadata: BTreeMap::new(),
        allowed_tools: Vec::new(),
        package_sha256: "a".repeat(64),
        skill_md_sha256: "b".repeat(64),
        skill_md_bytes: 10,
        file_count: 1,
        total_bytes: 10,
        source: "skills".into(),
    }
}

/// One event drops the registration and everything that names it, the folder
/// is untouched, replay applies it, and the same folder registers again under
/// a new identifier.
#[test]
fn unregistering_drops_every_setting_that_names_the_workspace_and_replays() {
    let mut scene = scene();
    let (first, first_path) = scene.register("first");
    let (second, _second_path) = scene.register("second");
    let permissions = PermissionSet {
        read: Access::Allow,
        model_tools: true,
        ..PermissionSet::default()
    };
    let revision = scene.store.revision();
    scene
        .store
        .set_permissions(first, permissions, revision)
        .expect("permissions");
    let revision = scene.store.revision();
    scene
        .store
        .set_package_cache_root(first, &scene.cache, revision)
        .expect("package cache root");
    let stored = first_path.join(omgcode_platform::broker::RESERVED_STORE_NAME);
    fs::create_dir(&stored).expect("checkpoint folder");
    fs::write(stored.join("kept.txt"), b"kept").expect("checkpoint file");
    scene
        .store
        .bind_workspace_store(first, DirectoryRecord::capture(&stored).expect("record"))
        .expect("bound");
    let revision = scene.store.revision();
    let skill = scene
        .store
        .add_skill(&skill_registration("reviewer"), revision)
        .expect("skill");
    for workspace_id in [first, second] {
        let revision = scene.store.revision();
        scene
            .store
            .set_skill_enablement(
                skill.name(),
                SkillScope::Workspace { workspace_id },
                SkillSetting::Enabled,
                revision,
            )
            .expect("override");
    }
    assert!(scene.store.workspace_store(first).is_some());
    assert!(scene.store.package_cache_root(first).is_some());

    let before = scene.store.revision();
    let changes = scene.store.skill_enablement_changes(skill.skill_id());
    scene
        .store
        .unregister_workspace(first, before)
        .expect("unregistered");

    assert_eq!(scene.store.revision(), before + 1);
    assert!(scene.store.workspace(first).is_none());
    assert!(scene.store.workspace(second).is_some());
    assert!(scene.store.package_cache_root(first).is_none());
    assert!(scene.store.workspace_store(first).is_none());
    let overrides = scene.store.skill_statuses(None)[0]
        .workspace_overrides
        .clone();
    assert_eq!(overrides.len(), 1);
    assert_eq!(overrides[0].workspace_id, second);
    assert_eq!(
        scene.store.skill_enablement_changes(skill.skill_id()),
        changes + 1,
        "the dropped override counts as an enablement change"
    );
    // Nothing on disk changed.
    assert_eq!(
        fs::read(stored.join("kept.txt")).expect("checkpoint file"),
        b"kept"
    );
    assert!(first_path.is_dir());

    // Replay applies the event and reaches the same state.
    let journal = scene
        .store
        .entries
        .iter()
        .map(|entry| serde_json::to_string(entry).expect("event JSON"))
        .collect::<Vec<_>>();
    assert!(
        journal
            .last()
            .is_some_and(|line| line.contains("\"type\":\"workspace_unregistered\"")),
        "an older build rejects this event name"
    );
    let revision = scene.store.revision();
    let data = scene.data.clone();
    drop(scene.store);
    let mut reopened = ControlStore::open_existing(&data)
        .expect("replay")
        .expect("store");
    assert_eq!(reopened.revision(), revision);
    assert!(reopened.workspace(first).is_none());
    assert!(reopened.workspace(second).is_some());
    assert!(reopened.package_cache_root(first).is_none());
    assert!(reopened.workspace_store(first).is_none());

    // The same folder registers again, as a new workspace with the defaults.
    let again = reopened
        .register_workspace(&first_path, revision)
        .expect("registered again");
    assert_ne!(again.id(), first);
    assert_eq!(again.permissions(), PermissionSet::default());
    assert!(reopened.workspace(first).is_none());
}

/// A container grant in any state refuses the removal until it is revoked,
/// and the refusal changes nothing.
#[test]
fn a_container_grant_refuses_the_removal_until_it_is_revoked() {
    let mut scene = scene();
    let (workspace, path) = scene.register("app");
    let grant = scene
        .store
        .test_record_grant(workspace, &path, ContainerGrantAccess::ReadWrite);
    let revision = scene.store.revision();
    let events = scene.store.event_count();
    assert!(matches!(
        scene.store.check_removable(workspace),
        Err(ControlError::GrantRefused(_))
    ));
    assert!(matches!(
        scene.store.unregister_workspace(workspace, revision),
        Err(ControlError::GrantRefused(_))
    ));
    assert_eq!(scene.store.revision(), revision);
    assert_eq!(scene.store.event_count(), events);
    assert!(scene.store.workspace(workspace).is_some());

    scene.store.test_record_revocation(grant);
    scene
        .store
        .unregister_workspace(workspace, revision)
        .expect("removable once revoked");
    assert!(scene.store.workspace(workspace).is_none());
}

/// A stale revision conflicts, an unknown workspace is refused, and replay
/// refuses an event that names an unknown workspace or one with a grant.
#[test]
fn stale_unknown_and_inconsistent_removals_are_refused() {
    let mut scene = scene();
    let (workspace, path) = scene.register("app");
    let revision = scene.store.revision();
    assert!(matches!(
        scene.store.unregister_workspace(workspace, revision - 1),
        Err(ControlError::Conflict { .. })
    ));
    assert!(matches!(
        scene
            .store
            .unregister_workspace(WorkspaceId::fresh(), revision),
        Err(ControlError::UnknownWorkspace)
    ));
    // Replay validates the event against the preceding state.
    assert!(matches!(
        scene.store.append(
            Action::WorkspaceUnregistered {
                workspace_id: WorkspaceId::fresh()
            },
            revision + 1
        ),
        Err(ControlError::Invalid(_))
    ));
    scene
        .store
        .test_record_grant(workspace, &path, ContainerGrantAccess::ReadWrite);
    let revision = scene.store.revision();
    assert!(matches!(
        scene.store.append(
            Action::WorkspaceUnregistered {
                workspace_id: workspace
            },
            revision + 1
        ),
        Err(ControlError::Invalid(_))
    ));
    assert!(scene.store.workspace(workspace).is_some());
}
