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
    assert!(ControlStore::open(&data).is_err());
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
            matches!(result, Err(ControlError::ProfileRefused(_))),
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

/// A revocation names the workspace of every grant it attempted, each once and
/// in order, whether or not the grant was revoked, and keeps only those
/// workspaces' runs to wait for. On Windows the recorded grants have no real
/// access-control entry, and removing nothing succeeds; elsewhere the first
/// revocation fails, which still names its workspace.
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
        assert!(revocations.failure.is_none(), "{:?}", revocations.failure);
        assert_eq!(revocations.revoked.len(), 3);
        assert_eq!(revocations.workspaces, [app, other]);
        assert_eq!(revocations.withdrawn_runs(&pending), [run_app, run_other]);
    } else {
        assert!(revocations.revoked.is_empty());
        assert!(
            matches!(revocations.failure, Some((id, ControlError::Container(_))) if id == first),
            "{:?}",
            revocations.failure
        );
        // The failure stops the loop, so `other` was never attempted.
        assert_eq!(revocations.workspaces, [app]);
        assert_eq!(revocations.withdrawn_runs(&pending), [run_app]);
    }
    assert!(Revocations::default().withdrawn_runs(&pending).is_empty());
}
