use super::*;
use std::fs;

use crate::control::{FILE_NAME, ProfileDeclaration};

struct Scene {
    store: ControlStore,
    data: PathBuf,
    root: PathBuf,
    outside: PathBuf,
    _directory: tempfile::TempDir,
}

/// A store with a development root and a folder beside it for toolchains.
fn scene() -> Scene {
    let directory = tempfile::tempdir().expect("temporary directory");
    let data = directory.path().join("data");
    let root = directory.path().join("projects");
    let outside = directory.path().join("tools");
    fs::create_dir_all(&root).expect("development root");
    fs::create_dir_all(&outside).expect("toolchain folder");
    let mut store = ControlStore::open(&data).expect("store");
    store.set_development_root(&root, 0).expect("root");
    Scene {
        store,
        data,
        root,
        outside,
        _directory: directory,
    }
}

impl Scene {
    fn register(&mut self, name: &str) -> WorkspaceId {
        let path = self.root.join(name);
        fs::create_dir(&path).expect("workspace directory");
        let revision = self.store.revision();
        self.store
            .register_workspace(&path, revision)
            .expect("workspace")
            .id()
    }

    /// A profile named `name` whose roots are these folders beside the root.
    fn profile(&mut self, name: &str, roots: &[&str]) -> Uuid {
        let read_only_roots = roots
            .iter()
            .map(|root| {
                let path = self.outside.join(root);
                fs::create_dir_all(&path).expect("profile root");
                path
            })
            .collect();
        let revision = self.store.revision();
        self.store
            .add_profile(
                &ProfileDeclaration {
                    name: name.into(),
                    read_only_roots,
                    ..ProfileDeclaration::default()
                },
                revision,
            )
            .expect("profile")
            .id()
    }

    fn assign(&mut self, workspace: WorkspaceId, name: &str) -> Result<Uuid, ControlError> {
        let revision = self.store.revision();
        self.store.assign_profile(workspace, name, revision)
    }
}

/// Assignments are per workspace, advance the revision, show in status,
/// survive replay, and are removed.
#[test]
fn assignments_are_recorded_replayed_and_removed() {
    let mut scene = scene();
    let app = scene.register("app");
    let other = scene.register("other");
    let rust = scene.profile("rust", &["rust"]);
    let node = scene.profile("node", &["node"]);
    let revision = scene.store.revision();

    assert_eq!(scene.assign(app, "RUST").expect("assign"), rust);
    scene.assign(app, "node").expect("assign node");
    assert_eq!(scene.store.revision(), revision + 2);
    assert_eq!(scene.store.assigned_profiles(app), [rust, node]);
    assert!(scene.store.profile_assigned(app, rust));
    assert!(!scene.store.profile_assigned(other, rust));
    assert!(scene.store.assigned_profiles(other).is_empty());

    let encoded = serde_json::to_value(scene.store.snapshot()).expect("snapshot JSON");
    assert_eq!(
        encoded["workspaces"][0]["assigned_profiles"],
        serde_json::json!([rust, node])
    );
    assert!(encoded["workspaces"][1].get("assigned_profiles").is_none());

    let data = scene.data.clone();
    drop(scene.store);
    let mut store = ControlStore::open(&data).expect("replay");
    assert_eq!(store.assigned_profiles(app), [rust, node]);
    let revision = store.revision();
    assert_eq!(
        store
            .unassign_profile(app, "rust", revision)
            .expect("unassign"),
        rust
    );
    assert_eq!(store.revision(), revision + 1);
    assert_eq!(store.assigned_profiles(app), [node]);
    drop(store);
    let store = ControlStore::open(&data).expect("replay after unassign");
    assert_eq!(store.assigned_profiles(app), [node]);
}

/// Stale revisions, unknown workspaces and profiles, duplicates, and absent
/// unassignments are refused without changing the revision.
#[test]
fn refused_assignments_leave_the_settings_unchanged() {
    let mut scene = scene();
    let app = scene.register("app");
    scene.profile("rust", &["rust"]);
    scene.profile("node", &["node"]);
    scene.assign(app, "rust").expect("assign");
    let revision = scene.store.revision();

    assert!(matches!(
        scene.store.assign_profile(app, "node", revision - 1),
        Err(ControlError::Conflict { .. })
    ));
    assert!(matches!(
        scene.store.unassign_profile(app, "rust", revision - 1),
        Err(ControlError::Conflict { .. })
    ));
    assert!(matches!(
        scene
            .store
            .assign_profile(WorkspaceId::fresh(), "node", revision),
        Err(ControlError::UnknownWorkspace)
    ));
    assert!(matches!(
        scene.store.assign_profile(app, "absent", revision),
        Err(ControlError::UnknownProfile)
    ));
    assert!(matches!(
        scene.store.assign_profile(app, "Rust", revision),
        Err(ControlError::ProfileRefused(_))
    ));
    assert!(matches!(
        scene.store.unassign_profile(app, "node", revision),
        Err(ControlError::ProfileRefused(_))
    ));
    assert!(matches!(
        scene.store.plan_profile_access(app, "rust"),
        Err(ControlError::ProfileRefused(_))
    ));
    assert_eq!(scene.store.revision(), revision);
    assert_eq!(scene.store.assigned_profiles(app).len(), 1);
}

/// Removing a profile ends its assignment in every workspace, and
/// unregistering a workspace drops its assignments; both survive replay.
#[test]
fn removal_and_unregistration_drop_assignments() {
    let mut scene = scene();
    let app = scene.register("app");
    let other = scene.register("other");
    scene.profile("rust", &["rust"]);
    let node = scene.profile("node", &["node"]);
    for workspace in [app, other] {
        scene.assign(workspace, "rust").expect("assign rust");
        scene.assign(workspace, "node").expect("assign node");
    }
    let revision = scene.store.revision();
    scene
        .store
        .remove_profile("rust", revision)
        .expect("remove");
    assert_eq!(scene.store.assigned_profiles(app), [node]);
    assert_eq!(scene.store.assigned_profiles(other), [node]);

    let revision = scene.store.revision();
    scene
        .store
        .unregister_workspace(other, revision)
        .expect("unregister");
    assert!(scene.store.assigned_profiles(other).is_empty());

    let data = scene.data.clone();
    drop(scene.store);
    let store = ControlStore::open(&data).expect("replay");
    assert_eq!(store.assigned_profiles(app), [node]);
    assert!(store.assigned_profiles(other).is_empty());
}

/// Replay refuses an assignment of a profile that was never recorded.
#[test]
fn replay_refuses_an_assignment_of_an_unknown_profile() {
    let mut scene = scene();
    let app = scene.register("app");
    let rust = scene.profile("rust", &["rust"]);
    scene.assign(app, "rust").expect("assign");
    let data = scene.data.clone();
    drop(scene.store);
    let journal = data.join(FILE_NAME);
    let text = fs::read_to_string(&journal).expect("journal text");
    let assigned = text
        .lines()
        .find(|line| line.contains("profile_assigned"))
        .expect("assignment event")
        .to_owned();
    let tampered = text.replacen(
        &assigned,
        &assigned.replace(&rust.to_string(), &Uuid::new_v4().to_string()),
        1,
    );
    assert_ne!(text, tampered);
    fs::write(&journal, tampered).expect("tamper journal");
    assert!(matches!(
        ControlStore::open(&data),
        Err(ControlError::Invalid("assignment of an unknown profile"))
    ));
}

/// Off Windows nothing is planned; on Windows a fresh folder the current
/// account owns is planned once, a root inside another planned root is left
/// out, and a root with a recorded grant is skipped. Planning reads
/// access-control lists only.
#[test]
fn a_plan_lists_each_needed_root_once() {
    let mut scene = scene();
    let app = scene.register("app");
    scene.profile("nested", &["outer/inner", "outer"]);
    let plan = scene
        .store
        .plan_profile_access(app, "nested")
        .expect("plan");
    if CONTAINER_GRANTS_SUPPORTED {
        let planned: Vec<PathBuf> = plan.paths();
        assert_eq!(planned.len(), 1, "{plan:?}");
        assert!(planned[0].ends_with("outer"), "{plan:?}");
        assert!(plan.needs_administrator().is_empty());
        assert!(plan.unknown().is_empty());

        scene.store.test_record_grant(
            app,
            &scene.outside.join("outer"),
            ContainerGrantAccess::ReadExecute,
        );
        let plan = scene
            .store
            .plan_profile_access(app, "nested")
            .expect("plan with a grant");
        // The recorded grant is not applied, so the inner root, which no
        // longer has a planned folder above it, is planned instead.
        assert_eq!(plan.paths().len(), 1, "{plan:?}");
        assert!(plan.paths()[0].ends_with("inner"), "{plan:?}");
    } else {
        assert_eq!(plan, ProfileAccessPlan::default());
    }
}

/// A batch plan covers every named profile once. A plan made from folders
/// before they are recorded, as discovery does, equals the plan made again
/// from the recorded roots, however the paths were spelled.
#[test]
fn a_batch_plan_matches_the_plan_from_unrecorded_folders() {
    let mut scene = scene();
    let app = scene.register("app");
    let rust = scene.outside.join("rust");
    let node = scene.outside.join("node");
    fs::create_dir_all(&rust).expect("rust");
    fs::create_dir_all(&node).expect("node");
    let spelled = [
        scene.outside.join(".").join("rust"),
        scene.outside.join("node").join("."),
    ];
    let before = scene
        .store
        .plan_folder_access(app, &spelled)
        .expect("plan before recording");
    for (name, root) in [("rust", &rust), ("node", &node)] {
        let revision = scene.store.revision();
        scene
            .store
            .add_profile(
                &ProfileDeclaration {
                    name: name.into(),
                    read_only_roots: vec![root.clone()],
                    ..ProfileDeclaration::default()
                },
                revision,
            )
            .expect("profile");
    }
    let after = scene
        .store
        .plan_profiles_access(app, &["rust", "node"])
        .expect("batch plan");
    assert_eq!(before, after);
    if CONTAINER_GRANTS_SUPPORTED {
        assert_eq!(after.to_grant().len(), 2, "{after:?}");
    } else {
        assert_eq!(after, ProfileAccessPlan::default());
    }
    assert!(matches!(
        scene.store.plan_profiles_access(app, &["rust", "RUST"]),
        Err(ControlError::ProfileRefused(_))
    ));
}

/// An assignment whose plan changed since the operator confirmed it is
/// refused before anything is recorded. Off Windows the plan is empty, so the
/// assignment records the event and grants nothing.
#[test]
fn an_assignment_applies_only_the_confirmed_plan() {
    let mut scene = scene();
    let app = scene.register("app");
    let rust = scene.profile("rust", &["rust"]);
    let revision = scene.store.revision();
    let confirmed = ProfileAccessPlan::default();
    let result = scene
        .store
        .assign_profile_with_access(app, "rust", &confirmed, revision);
    if CONTAINER_GRANTS_SUPPORTED {
        assert!(
            matches!(
                &result,
                Err(AssignmentFailure {
                    error: ControlError::ProfileRefused(_),
                    released,
                }) if released.workspaces.is_empty()
            ),
            "{result:?}"
        );
        assert_eq!(scene.store.revision(), revision);
        assert!(scene.store.assigned_profiles(app).is_empty());
        assert!(scene.store.container_grants().is_empty());
    } else {
        let assignment = result.expect("assignment off Windows");
        assert_eq!(assignment.profile_ids, [rust]);
        assert!(assignment.granted.is_empty());
        assert_eq!(scene.store.assigned_profiles(app), [rust]);
        assert_eq!(scene.store.revision(), revision + 1);
    }
}

/// Unassigning revokes only the read-execute grants on the profile's own
/// roots that no profile still assigned lists, never the workspace's
/// read-write grant or its package cache root.
#[test]
fn only_grants_no_remaining_profile_needs_are_unneeded() {
    let mut scene = scene();
    let app = scene.register("app");
    let other = scene.register("other");
    scene.profile("rust", &["rust", "shared", "cache"]);
    scene.profile("node", &["shared"]);
    scene.assign(app, "rust").expect("assign rust");
    scene.assign(app, "node").expect("assign node");
    let read = ContainerGrantAccess::ReadExecute;
    let own = scene
        .store
        .test_record_grant(app, &scene.outside.join("rust"), read);
    scene
        .store
        .test_record_grant(app, &scene.outside.join("shared"), read);
    let cache = scene.outside.join("cache");
    scene.store.test_record_grant(app, &cache, read);
    let revision = scene.store.revision();
    scene
        .store
        .set_package_cache_root(app, &cache, revision)
        .expect("cache root");
    scene.store.test_record_grant(
        app,
        &scene.root.join("app"),
        ContainerGrantAccess::ReadWrite,
    );
    scene
        .store
        .test_record_grant(other, &scene.outside.join("rust"), read);

    assert_eq!(
        scene
            .store
            .grants_released_by_unassignment(app, "rust")
            .expect("preview"),
        [own]
    );
    assert_eq!(
        scene
            .store
            .grants_released_by_removal("rust")
            .expect("removal preview"),
        [(app, vec![own])]
    );
    assert!(matches!(
        scene.store.grants_released_by_unassignment(other, "rust"),
        Err(ControlError::ProfileRefused(_))
    ));
    // The unassignment itself records the event, then revokes the same
    // grants; the store has no real ACL entries, so only the selection is
    // checked here.
    let revision = scene.store.revision();
    scene
        .store
        .unassign_profile(app, "rust", revision)
        .expect("unassign");
    assert!(
        scene
            .store
            .grants_released_by_removal("rust")
            .expect("none")
            .is_empty()
    );
}

/// A revocation attempts every grant, names the workspace of each, once and in
/// order, whether or not the grant was revoked, and keeps only those
/// workspaces' runs to wait for. On Windows the recorded grants have no real
/// access-control entry, and removing nothing succeeds; elsewhere every
/// revocation fails, and a failure does not stop the later ones.
#[test]
fn revocations_name_each_attempted_workspace_and_its_runs() {
    let mut scene = scene();
    let app = scene.register("app");
    let other = scene.register("other");
    let third = scene.register("third");
    let read = ContainerGrantAccess::ReadExecute;
    let mut grant = |workspace: WorkspaceId, folder: &str| {
        let path = scene.outside.join(folder);
        fs::create_dir_all(&path).expect("grant folder");
        scene.store.test_record_grant(workspace, &path, read)
    };
    let first = grant(app, "a");
    let second = grant(app, "b");
    let last = grant(other, "c");

    let revocations = scene.store.revoke_each(&[first, second, last]);

    let (run_app, run_other, run_third) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let pending = [(app, run_app), (other, run_other), (third, run_third)];
    if CONTAINER_GRANTS_SUPPORTED {
        assert!(
            revocations.failures.is_empty(),
            "{:?}",
            revocations.failures
        );
        assert_eq!(revocations.revoked.len(), 3);
    } else {
        assert!(revocations.revoked.is_empty());
        let failed: Vec<Uuid> = revocations
            .failures
            .iter()
            .map(|(grant, error)| {
                assert!(matches!(error, ControlError::Container(_)), "{error:?}");
                *grant
            })
            .collect();
        assert_eq!(failed, [first, second, last]);
    }
    assert_eq!(revocations.workspaces, [app, other]);
    assert_eq!(revocations.withdrawn_runs(&pending), [run_app, run_other]);
    assert!(Revocations::default().withdrawn_runs(&pending).is_empty());
}

/// A grant on an outer folder that a still-assigned profile's root lies
/// inside is kept, because that root was readable through it when it was
/// planned and has no grant of its own; it is released once the inner root
/// has its own grant or its profile is unassigned.
#[test]
fn a_grant_a_nested_root_relies_on_is_kept() {
    let mut scene = scene();
    let app = scene.register("app");
    scene.profile("outer", &["sdk"]);
    scene.profile("inner", &["sdk/bin"]);
    scene.assign(app, "outer").expect("assign outer");
    scene.assign(app, "inner").expect("assign inner");
    let read = ContainerGrantAccess::ReadExecute;
    let sdk = scene
        .store
        .test_record_grant(app, &scene.outside.join("sdk"), read);
    let released = |store: &ControlStore| {
        store
            .grants_released_by_unassignment(app, "outer")
            .expect("preview")
    };

    assert!(released(&scene.store).is_empty());
    assert_eq!(
        scene
            .store
            .grants_released_by_removal("outer")
            .expect("removal preview"),
        [(app, Vec::new())]
    );

    let revision = scene.store.revision();
    scene
        .store
        .unassign_profile(app, "inner", revision)
        .expect("unassign inner");
    assert_eq!(released(&scene.store), [sdk]);

    scene.assign(app, "inner").expect("assign inner again");
    scene
        .store
        .test_record_grant(app, &scene.outside.join("sdk/bin"), read);
    assert_eq!(released(&scene.store), [sdk]);
}

/// A pending grant on a nested root may hold no access, so it does not
/// stand in for the outer grant that root relies on; an active one does.
#[test]
fn only_an_active_grant_stands_in_for_an_outer_grant() {
    let mut scene = scene();
    let app = scene.register("app");
    scene.profile("outer", &["sdk"]);
    scene.profile("inner", &["sdk/bin"]);
    scene.assign(app, "outer").expect("assign outer");
    scene.assign(app, "inner").expect("assign inner");
    let read = ContainerGrantAccess::ReadExecute;
    let sdk = scene
        .store
        .test_record_grant(app, &scene.outside.join("sdk"), read);
    let pending = scene
        .store
        .test_record_pending_grant(app, &scene.outside.join("sdk/bin"), read);
    let released = |store: &ControlStore| {
        store
            .grants_released_by_unassignment(app, "outer")
            .expect("preview")
    };

    assert!(released(&scene.store).is_empty());

    scene
        .store
        .append(
            Action::ContainerGrantApplied { grant_id: pending },
            scene.store.revision(),
        )
        .expect("applied");
    assert_eq!(released(&scene.store), [sdk]);
}

/// A nested root that was moved or removed, so its containment cannot be
/// read live, still counts by its recorded path: its profile keeps the
/// outer grant while assigned, and unassigning or removing that profile
/// then releases it.
#[test]
fn a_removed_nested_root_keeps_and_then_releases_the_outer_grant() {
    let mut scene = scene();
    let app = scene.register("app");
    scene.profile("outer", &["sdk"]);
    scene.profile("inner", &["sdk/bin"]);
    scene.assign(app, "outer").expect("assign outer");
    scene.assign(app, "inner").expect("assign inner");
    let sdk = scene.store.test_record_grant(
        app,
        &scene.outside.join("sdk"),
        ContainerGrantAccess::ReadExecute,
    );
    fs::remove_dir(scene.outside.join("sdk/bin")).expect("remove the nested root");

    assert!(
        scene
            .store
            .grants_released_by_unassignment(app, "outer")
            .expect("preview")
            .is_empty()
    );

    let revision = scene.store.revision();
    scene
        .store
        .unassign_profile(app, "outer", revision)
        .expect("unassign outer");
    assert_eq!(
        scene
            .store
            .grants_released_by_unassignment(app, "inner")
            .expect("preview"),
        [sdk]
    );
    assert_eq!(
        scene
            .store
            .grants_released_by_removal("inner")
            .expect("removal preview"),
        [(app, vec![sdk])]
    );
}

/// When the outer profile is unassigned first, its grant is kept for the
/// nested root; unassigning or removing the inner profile then releases it,
/// so the grant does not outlive both assignments. A package cache root
/// inside the outer folder keeps the grant until the cache has its own.
#[test]
fn an_outer_grant_kept_for_a_nested_root_is_released_with_it() {
    let mut scene = scene();
    let app = scene.register("app");
    scene.profile("outer", &["sdk"]);
    scene.profile("inner", &["sdk/bin"]);
    scene.assign(app, "outer").expect("assign outer");
    scene.assign(app, "inner").expect("assign inner");
    let read = ContainerGrantAccess::ReadExecute;
    let sdk = scene
        .store
        .test_record_grant(app, &scene.outside.join("sdk"), read);
    let revision = scene.store.revision();
    scene
        .store
        .unassign_profile(app, "outer", revision)
        .expect("unassign outer");
    let released = |store: &ControlStore| {
        store
            .grants_released_by_unassignment(app, "inner")
            .expect("preview")
    };

    assert_eq!(released(&scene.store), [sdk]);
    assert_eq!(
        scene
            .store
            .grants_released_by_removal("inner")
            .expect("removal preview"),
        [(app, vec![sdk])]
    );

    let cache = scene.outside.join("sdk/cache");
    fs::create_dir_all(&cache).expect("cache folder");
    let revision = scene.store.revision();
    scene
        .store
        .set_package_cache_root(app, &cache, revision)
        .expect("cache root");
    assert!(released(&scene.store).is_empty());
    scene.store.test_record_grant(app, &cache, read);
    assert_eq!(released(&scene.store), [sdk]);
}

/// A workspace whose container grant is pending, or being revoked, is refused
/// a plan, because its runs are refused until that grant is settled. Another
/// workspace is not affected.
#[test]
fn a_pending_grant_refuses_an_assignment_plan() {
    let mut scene = scene();
    let app = scene.register("app");
    let other = scene.register("other");
    scene.profile("rust", &["rust"]);
    let cache = scene.outside.join("cache");
    fs::create_dir_all(&cache).expect("folder");
    scene
        .store
        .test_record_pending_grant(app, &cache, ContainerGrantAccess::ReadExecute);

    assert!(matches!(
        scene.store.plan_profile_access(app, "rust"),
        Err(ControlError::GrantRefused(_))
    ));
    assert!(matches!(
        scene
            .store
            .plan_folder_access(app, &[scene.outside.join("rust")]),
        Err(ControlError::GrantRefused(_))
    ));
    let revision = scene.store.revision();
    assert!(matches!(
        scene.store.assign_profile_with_access(
            app,
            "rust",
            &ProfileAccessPlan::default(),
            revision
        ),
        Err(AssignmentFailure {
            error: ControlError::GrantRefused(_),
            ..
        })
    ));
    assert!(scene.store.assigned_profiles(app).is_empty());
    scene
        .store
        .plan_profile_access(other, "rust")
        .expect("another workspace plans");
}

/// A grant whose folder is no longer the directory the operator confirmed is
/// refused before anything is recorded. Off Windows no grant is made at all.
#[test]
fn a_grant_of_a_changed_folder_is_refused_before_it_is_recorded() {
    let mut scene = scene();
    let app = scene.register("app");
    let confirmed_path = scene.outside.join("confirmed");
    let other_path = scene.outside.join("other");
    fs::create_dir_all(&confirmed_path).expect("confirmed folder");
    fs::create_dir_all(&other_path).expect("other folder");
    let confirmed = DirectoryRecord::capture(&confirmed_path).expect("identity");
    let revision = scene.store.revision();

    let result = scene.store.grant_confirmed_container_access(
        app,
        &other_path,
        Some(&confirmed),
        ContainerGrantAccess::ReadExecute,
        revision,
    );

    if CONTAINER_GRANTS_SUPPORTED {
        assert!(
            matches!(result, Err(ControlError::GrantRefused(_))),
            "{result:?}"
        );
    } else {
        assert!(
            matches!(result, Err(ControlError::Container(_))),
            "{result:?}"
        );
    }
    assert_eq!(scene.store.revision(), revision);
    assert!(scene.store.container_grants().is_empty());
}

/// An unregistration whose removal cannot be recorded after its revocations
/// returns them, so the caller waits for the runs they withdrew, and the
/// workspace stays registered. On Windows the recorded grant has no real
/// access-control entry, and removing nothing succeeds; elsewhere the
/// revocation fails, which also keeps the workspace registered.
#[test]
fn an_unregistration_that_cannot_be_recorded_returns_its_revocations() {
    let mut scene = scene();
    let app = scene.register("app");
    let path = scene.outside.join("tool");
    fs::create_dir_all(&path).expect("folder");
    let grant = scene
        .store
        .test_record_grant(app, &path, ContainerGrantAccess::ReadExecute);
    // Two events remain: the revocation's request and outcome fit in the
    // reserve kept for the grant, and the removal does not.
    let initial = scene.store.entries[0].clone();
    scene
        .store
        .entries
        .resize(crate::control::MAX_EVENTS - 2, initial);
    let revision = scene.store.revision();

    let unregistration = scene
        .store
        .unregister_workspace_with_access(app, revision)
        .expect("revocations are returned");

    assert!(!unregistration.unregistered());
    assert!(scene.store.workspace(app).is_some());
    assert_eq!(unregistration.revocations.workspaces, [app]);
    if CONTAINER_GRANTS_SUPPORTED {
        assert!(
            unregistration.revocations.failures.is_empty(),
            "{:?}",
            unregistration.revocations.failures
        );
        assert_eq!(unregistration.revocations.revoked.len(), 1);
        assert_eq!(unregistration.revocations.revoked[0].grant_id, grant);
        assert!(matches!(
            unregistration.removal,
            Some(ControlError::LimitReached)
        ));
        assert!(scene.store.container_grants().is_empty());
    } else {
        assert_eq!(unregistration.revocations.failures.len(), 1);
        assert!(unregistration.removal.is_none());
    }
}

/// Windows only, with a real grant on a temporary folder: an assignment that
/// cannot be recorded, because the journal reaches its event limit after the
/// grant, revokes that grant again, so no access outlives the failed consent.
#[cfg(windows)]
#[test]
fn an_assignment_that_cannot_be_recorded_revokes_its_grant() {
    let mut scene = scene();
    let app = scene.register("app");
    scene.profile("rust", &["rust"]);
    let plan = scene.store.plan_profile_access(app, "rust").expect("plan");
    assert_eq!(plan.to_grant().len(), 1, "{plan:?}");
    // With no grant and no skill recorded, the grant's request needs its
    // event and the three a pending grant reserves, its outcome one more, and
    // the assignment one beside the two an active grant reserves: 4,092
    // events leave room for the grant and none for the assignment.
    let initial = scene.store.entries[0].clone();
    scene
        .store
        .entries
        .resize(crate::control::MAX_EVENTS - 4, initial);
    let revision = scene.store.revision();

    let result = scene
        .store
        .assign_profile_with_access(app, "rust", &plan, revision);

    let (left, cleanup) = revoke_left(&mut scene.store);
    assert!(cleanup.is_empty(), "cleanup failed: {cleanup:?}");
    let failure = result.expect_err("the assignment cannot be recorded");
    assert!(
        matches!(failure.error, ControlError::LimitReached),
        "{failure:?}"
    );
    // The grant it made is revoked and named, with its workspace.
    assert_eq!(failure.released.revoked.len(), 1, "{failure:?}");
    assert!(failure.released.failures.is_empty(), "{failure:?}");
    assert_eq!(failure.released.workspaces, [app]);
    assert!(
        left.is_empty(),
        "a grant outlived the failed assignment: {left:?}"
    );
    assert!(scene.store.assigned_profiles(app).is_empty());
}

/// What [`release_after_removal`] observed, for assertions made after the
/// real grants are cleaned up.
#[cfg(windows)]
#[derive(Debug)]
struct ReleaseAfterRemoval {
    /// The folders each assignment's plan granted, in order: outer, inner.
    planned: [usize; 2],
    /// The revocations of unassigning `outer`, then `inner`.
    released: [Revocations; 2],
    /// The grants recorded after both unassignments.
    left: usize,
    /// A fresh plan for `outer` after the grant was revoked.
    replanned: ProfileAccessPlan,
}

/// Assign `outer` (`sdk`) and `inner` (`sdk/bin`) with real grants, remove
/// `sdk/bin`, then unassign `outer` and `inner` in turn.
#[cfg(windows)]
fn release_after_removal(
    store: &mut ControlStore,
    app: WorkspaceId,
    outside: &Path,
) -> Result<ReleaseAfterRemoval, String> {
    let mut planned = [0; 2];
    for (index, name) in ["outer", "inner"].into_iter().enumerate() {
        let plan = store
            .plan_profile_access(app, name)
            .map_err(|error| format!("plan {name}: {error}"))?;
        planned[index] = plan.to_grant().len();
        let revision = store.revision();
        store
            .assign_profile_with_access(app, name, &plan, revision)
            .map_err(|failure| format!("assign {name}: {failure:?}"))?;
    }
    fs::remove_dir(outside.join("sdk/bin"))
        .map_err(|error| format!("remove the nested root: {error}"))?;
    let mut unassign = |name: &str| {
        let revision = store.revision();
        store
            .unassign_profile_with_access(app, name, revision)
            .map(|(_, revocations)| revocations)
            .map_err(|error| format!("unassign {name}: {error}"))
    };
    let released = [unassign("outer")?, unassign("inner")?];
    let replanned = store
        .plan_profile_access(app, "outer")
        .map_err(|error| format!("plan outer again: {error}"))?;
    Ok(ReleaseAfterRemoval {
        planned,
        released,
        left: store.container_grants().len(),
        replanned,
    })
}

/// Windows only, with a real grant on a temporary folder: a nested root that
/// was removed keeps the outer grant while its profile is assigned, and
/// unassigning that profile then removes the grant's access-control entry,
/// so the folder needs a grant again.
#[cfg(windows)]
#[test]
fn a_removed_nested_root_releases_the_real_outer_grant_with_its_profile() {
    let mut scene = scene();
    let app = scene.register("app");
    scene.profile("outer", &["sdk"]);
    scene.profile("inner", &["sdk/bin"]);
    let outside = scene.outside.clone();

    let result = release_after_removal(&mut scene.store, app, &outside);

    let (_, cleanup) = revoke_left(&mut scene.store);
    assert!(cleanup.is_empty(), "cleanup failed: {cleanup:?}");
    let seen = result.expect("assign, remove, and unassign");
    // The outer grant made the nested root readable, so it got none.
    assert_eq!(seen.planned, [1, 0], "{seen:?}");
    let [outer, inner] = &seen.released;
    assert!(
        outer.revoked.is_empty() && outer.failures.is_empty(),
        "{seen:?}"
    );
    assert!(outer.workspaces.is_empty(), "{seen:?}");
    assert_eq!(inner.revoked.len(), 1, "{seen:?}");
    assert!(inner.failures.is_empty(), "{seen:?}");
    assert_eq!(seen.left, 0, "{seen:?}");
    let [again] = seen.replanned.to_grant() else {
        panic!("the folder needs its grant again: {seen:?}");
    };
    assert_eq!(again.path().file_name(), Some(std::ffi::OsStr::new("sdk")));
}

/// Revoke every recorded grant, attempting each, and return the grants
/// that were left and each revocation that failed, with its grant, so a
/// Windows test that applied real access-control entries can report what
/// it could not clean up.
#[cfg(windows)]
fn revoke_left(store: &mut ControlStore) -> (Vec<Uuid>, Vec<String>) {
    let left: Vec<Uuid> = store
        .container_grants()
        .iter()
        .map(|status| status.grant().id())
        .collect();
    let failures = left
        .iter()
        .filter_map(|grant| {
            store
                .revoke_container_grant(*grant)
                .err()
                .map(|error| format!("{grant}: {error}"))
        })
        .collect();
    (left, failures)
}

/// When the rollback of a failed assignment cannot revoke the grant it
/// made, the failure returns that grant, and the grant stays recorded
/// `revoking` until the operator revokes it. The revocation's
/// access-control change is made to fail after its request is recorded, as
/// a real failure leaves it.
#[cfg(windows)]
#[test]
fn an_assignment_whose_rollback_cannot_revoke_returns_the_grant() {
    let mut scene = scene();
    let app = scene.register("app");
    scene.profile("rust", &["rust"]);
    let plan = scene.store.plan_profile_access(app, "rust").expect("plan");
    assert_eq!(plan.to_grant().len(), 1, "{plan:?}");
    // As in the test above: room for the grant and none for the assignment.
    let initial = scene.store.entries[0].clone();
    scene
        .store
        .entries
        .resize(crate::control::MAX_EVENTS - 4, initial);
    let revision = scene.store.revision();
    scene.store.fail_next_revocation = true;

    let result = scene
        .store
        .assign_profile_with_access(app, "rust", &plan, revision);

    let states: Vec<(Uuid, ContainerGrantState)> = scene
        .store
        .container_grants()
        .iter()
        .map(|status| (status.grant().id(), status.state()))
        .collect();
    let (_, cleanup) = revoke_left(&mut scene.store);
    assert!(cleanup.is_empty(), "cleanup failed: {cleanup:?}");
    let failure = result.expect_err("the assignment cannot be recorded");
    assert!(
        matches!(failure.error, ControlError::LimitReached),
        "{failure:?}"
    );
    let [(grant, ContainerGrantState::Revoking)] = states.as_slice() else {
        panic!("one grant left revoking: {states:?}");
    };
    assert!(failure.released.revoked.is_empty(), "{failure:?}");
    assert!(
        matches!(
            failure.released.failures.as_slice(),
            [(failed, ControlError::Container(_))] if failed == grant
        ),
        "{failure:?}"
    );
    assert_eq!(failure.released.workspaces, [app]);
}

/// A failed assignment's release returns each revocation it attempted, and
/// a revocation that fails is returned rather than dropped; a grant a
/// still-assigned profile needs is not attempted, and a package cache root
/// inside a grant it made does not keep that grant. On Windows the recorded
/// grant has no real access-control entry and is revoked; elsewhere its
/// revocation fails and is reported with its grant.
#[test]
fn a_failed_assignment_returns_the_grants_it_could_not_release() {
    let mut scene = scene();
    let app = scene.register("app");
    scene.profile("kept", &["kept"]);
    scene.assign(app, "kept").expect("assign kept");
    let read = ContainerGrantAccess::ReadExecute;
    let needed = scene
        .store
        .test_record_grant(app, &scene.outside.join("kept"), read);
    let path = scene.outside.join("made");
    let cache = path.join("cache");
    fs::create_dir_all(&cache).expect("grant folder");
    let made = scene.store.test_record_grant(app, &path, read);
    let revision = scene.store.revision();
    scene
        .store
        .set_package_cache_root(app, &cache, revision)
        .expect("cache root");

    let released = scene.store.release_unneeded(app, &[needed, made]);

    assert_eq!(released.workspaces, [app]);
    if CONTAINER_GRANTS_SUPPORTED {
        assert!(released.failures.is_empty(), "{:?}", released.failures);
        assert_eq!(released.revoked.len(), 1);
    } else {
        assert!(released.revoked.is_empty());
        assert!(
            matches!(
                released.failures.as_slice(),
                [(grant, ControlError::Container(_))] if *grant == made
            ),
            "{:?}",
            released.failures
        );
    }
    assert!(
        scene
            .store
            .container_grants()
            .iter()
            .any(|status| status.grant().id() == needed),
        "a grant a profile still needs is kept"
    );
}

/// A folder grant registration does not attempt keeps the reason
/// [`root_grant_need`] gives the same check, so neither surface reports it as
/// granted or as not needed: an account that cannot change the folder's
/// access-control list needs an administrator, and a check that failed
/// leaves the access unknown.
#[test]
fn a_skipped_folder_grant_keeps_its_reason() {
    assert_eq!(SkippedGrant::from_check(&Ok(true)), None);
    assert_eq!(
        SkippedGrant::from_check(&Ok(false)),
        Some(SkippedGrant::NeedsAdministrator)
    );
    assert_eq!(
        SkippedGrant::from_check(&Err(std::io::Error::other("unreadable"))),
        Some(SkippedGrant::AccessUnknown)
    );
    assert_eq!(
        SkippedGrant::NeedsAdministrator.as_str(),
        "needs_administrator"
    );
    assert_eq!(SkippedGrant::AccessUnknown.as_str(), "access_unknown");
}

/// Off Windows, registering needs no folder grant.
#[cfg(not(windows))]
#[test]
fn registering_off_windows_needs_no_folder_grant() {
    let mut scene = scene();
    let folder = scene.root.join("app");
    fs::create_dir(&folder).expect("workspace directory");
    let revision = scene.store.revision();
    let registered = scene
        .store
        .register_workspace_with_access(&folder, revision)
        .expect("registered");
    assert!(
        matches!(registered.grant, Ok(FolderGrant::NotNeeded)),
        "{:?}",
        registered.grant
    );
    assert!(scene.store.container_grants().is_empty());
}

/// On Windows, registering a folder this account owns grants it, and
/// registering it again finds the grant and needs none.
#[cfg(windows)]
#[test]
fn registering_grants_the_folder_once() {
    let mut scene = scene();
    let folder = scene.root.join("app");
    fs::create_dir(&folder).expect("workspace directory");
    let revision = scene.store.revision();
    let first = scene
        .store
        .register_workspace_with_access(&folder, revision);
    let again = first.as_ref().ok().map(|_| {
        let revision = scene.store.revision();
        scene
            .store
            .register_workspace_with_access(&folder, revision)
            .map(|registered| registered.grant)
    });
    let (left, failures) = revoke_left(&mut scene.store);
    let first = first.expect("registered");
    let Ok(FolderGrant::Granted(grant)) = &first.grant else {
        panic!("the folder was not granted: {:?}", first.grant);
    };
    assert_eq!(grant.access(), ContainerGrantAccess::ReadWrite);
    assert_eq!(left, vec![grant.id()]);
    assert!(
        matches!(again, Some(Ok(Ok(FolderGrant::NotNeeded)))),
        "{again:?}"
    );
    assert!(failures.is_empty(), "{failures:?}");
}

/// On Windows, a read-execute grant of the workspace on its own folder, which
/// core accepts, does not stand in for the read-write grant its runs need:
/// registering the folder again is refused and names that grant, instead of
/// reporting that no grant was needed.
#[cfg(windows)]
#[test]
fn a_read_execute_grant_on_the_folder_refuses_registering_again() {
    let mut scene = scene();
    let app = scene.register("app");
    let folder = scene.root.join("app");
    let revision = scene.store.revision();
    let granted = scene.store.grant_container_access(
        app,
        &folder,
        ContainerGrantAccess::ReadExecute,
        revision,
    );
    let again = granted.as_ref().ok().map(|_| {
        let revision = scene.store.revision();
        scene
            .store
            .register_workspace_with_access(&folder, revision)
            .map(|registered| (registered.registration.id(), registered.grant))
    });
    let named = scene
        .store
        .unusable_folder_grant(app)
        .map(|status| status.grant().id());
    let (left, failures) = revoke_left(&mut scene.store);
    let granted = granted.expect("read-execute grant");
    let Some(Ok((registered, grant))) = again else {
        panic!("the folder was not registered again: {again:?}");
    };
    assert_eq!(registered, app, "registering again keeps the identifier");
    assert!(
        matches!(grant, Err(ControlError::GrantRefused(reason)) if reason.contains("not an active read-write grant")),
        "{grant:?}"
    );
    assert_eq!(named, Some(granted.id()));
    assert_eq!(left, vec![granted.id()], "no second grant was recorded");
    assert!(failures.is_empty(), "{failures:?}");
}

/// An active read-write grant on the folder is what registering needs, so
/// it is not named as one to revoke; an unknown workspace names nothing.
#[cfg(windows)]
#[test]
fn an_active_read_write_grant_is_not_named_as_unusable() {
    let mut scene = scene();
    let folder = scene.root.join("app");
    fs::create_dir(&folder).expect("workspace directory");
    let revision = scene.store.revision();
    let registered = scene
        .store
        .register_workspace_with_access(&folder, revision);
    let named = registered.as_ref().ok().map(|registered| {
        scene
            .store
            .unusable_folder_grant(registered.registration.id())
            .map(|status| status.grant().id())
    });
    let (_, failures) = revoke_left(&mut scene.store);
    let registered = registered.expect("registered");
    assert!(
        matches!(registered.grant, Ok(FolderGrant::Granted(_))),
        "{:?}",
        registered.grant
    );
    assert_eq!(named, Some(None));
    assert!(
        scene
            .store
            .unusable_folder_grant(WorkspaceId::fresh())
            .is_none()
    );
    assert!(failures.is_empty(), "{failures:?}");
}
