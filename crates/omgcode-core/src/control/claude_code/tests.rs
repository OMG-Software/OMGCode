//! Claude Code connection tests (ADR-0109). Windows tests change real ACLs on
//! temporary directories and delete the container profile they create.

use std::fs;
use std::path::{Path, PathBuf};

use super::super::{Action, ControlError, ControlStore, ProviderProfileDeclaration};
use super::*;

const PROGRAM_NAME: &str = if cfg!(windows) {
    "claude.exe"
} else {
    "claude"
};

struct Scene {
    _directory: tempfile::TempDir,
    data: PathBuf,
    root: PathBuf,
    workspace: PathBuf,
    program: PathBuf,
    sign_in: PathBuf,
    relay: PathBuf,
}

impl Scene {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let data = directory.path().join("data");
        let root = directory.path().join("projects");
        let workspace = root.join("app");
        let tools = directory.path().join("tools");
        let sign_in = directory.path().join("claude-sign-in");
        let relay = directory.path().join("relay");
        for path in [&root, &workspace, &tools, &sign_in, &relay] {
            fs::create_dir(path).expect("scene directory");
        }
        let program = tools.join(PROGRAM_NAME);
        fs::write(&program, b"not a real program").expect("program");
        Self {
            _directory: directory,
            data,
            root,
            workspace,
            program,
            sign_in,
            relay,
        }
    }

    /// A store with the root and one registered workspace, at revision 2.
    fn store(&self) -> ControlStore {
        let mut store = ControlStore::open(&self.data).expect("control store");
        store.set_development_root(&self.root, 0).expect("root");
        store
            .register_workspace(&self.workspace, 1)
            .expect("workspace");
        store
    }

    fn declaration(&self, name: &str) -> ClaudeCodeDeclaration {
        ClaudeCodeDeclaration {
            name: name.to_owned(),
            program: self.program.clone(),
            config_folder: self.sign_in.clone(),
        }
    }
}

fn grant_for(profile_id: Uuid, role: ClaudeCodeGrantRole, path: &Path) -> ClaudeCodeGrant {
    ClaudeCodeGrant {
        id: Uuid::new_v4(),
        profile_id,
        role,
        container: claude_code_container(profile_id)
            .expect("name")
            .as_str()
            .to_owned(),
        sid: "S-1-15-2-1-2-3-4-5-6-7".to_owned(),
        directory: DirectoryRecord::capture(path).expect("directory"),
    }
}

/// A profile records the canonical program and both folders, replays, can
/// be found again, and is not selectable as an endpoint connection.
#[test]
fn a_claude_code_profile_is_recorded_replayed_and_removed() {
    let scene = Scene::new();
    let mut store = scene.store();
    let revision = store.revision();
    store
        .check_claude_code_profile(&scene.declaration("claude"))
        .expect("check");
    assert_eq!(store.revision(), revision, "a check records nothing");
    let profile = store
        .add_claude_code_profile(&scene.declaration("claude"), revision)
        .expect("profile");
    assert_eq!(store.revision(), revision + 1);
    assert_eq!(profile.kind(), ProviderKind::ClaudeCode);
    assert_eq!(profile.auth(), ProviderAuth::Subscription);
    assert!(profile.endpoint().is_none());
    let setup = profile.claude_code_setup().expect("setup");
    let canonical = fs::canonicalize(&scene.program).expect("canonical");
    assert_eq!(setup.program().expect("program"), canonical);
    assert_eq!(setup.live_program().expect("live program"), canonical);
    assert_eq!(
        setup.live_config_folder().expect("live folder"),
        fs::canonicalize(&scene.sign_in).expect("canonical folder")
    );
    let encoded = serde_json::to_value(store.snapshot()).expect("snapshot");
    assert_eq!(encoded["provider_profiles"][0]["kind"], "claude_code");
    assert!(encoded["provider_profiles"][0]["claude_code"]["program"].is_object());
    assert!(encoded["provider_profiles"][0].get("endpoint").is_none());
    drop(store);

    let mut store = ControlStore::open(&scene.data).expect("replay");
    let (profile_id, replayed) = store.claude_code_setup("CLAUDE").expect("setup");
    assert_eq!(profile_id, profile.id());
    assert_eq!(&replayed, setup);
    assert!(matches!(
        store.check_provider_selectable("claude"),
        Err(ControlError::ProviderRefused(_))
    ));
    let revoked = store
        .revoke_claude_code_grants("claude")
        .expect("no grants");
    assert_eq!(revoked.revoked, 0);
    assert_eq!(revoked.profile_deleted, cfg!(windows));
    let removed = store
        .remove_provider_profile("claude", revision + 1)
        .expect("remove");
    assert_eq!(removed.profile_id, profile.id());
    assert!(removed.credential.is_none());
    assert!(store.provider_profiles().is_empty());
}

/// The generic add cannot record a `claude_code` profile, and the Claude
/// Code add accepts nothing that the generic rules or folder rules refuse.
#[test]
fn invalid_claude_code_declarations_are_refused_without_a_record() {
    let scene = Scene::new();
    let mut store = scene.store();
    let revision = store.revision();
    for auth in ["subscription", "api_key", "none"] {
        let generic = ProviderProfileDeclaration {
            name: "claude".into(),
            kind: "claude_code".into(),
            auth: auth.into(),
            endpoint: None,
        };
        assert!(
            matches!(
                store.add_provider_profile(&generic, revision),
                Err(ControlError::ProviderRefused(_))
            ),
            "{auth}"
        );
    }

    let not_a_file = scene.sign_in.clone();
    let missing = scene.program.with_file_name("missing.exe");
    let inside_data = scene.data.join("claude-sign-in");
    fs::create_dir(&inside_data).expect("folder in storage");
    let inside_workspace = scene.workspace.join("sign-in");
    fs::create_dir(&inside_workspace).expect("folder in workspace");
    let program_folder = scene.program.parent().expect("folder").to_path_buf();
    let mut cases = vec![
        (
            "bad name",
            "-claude",
            scene.program.clone(),
            scene.sign_in.clone(),
        ),
        ("missing program", "claude", missing, scene.sign_in.clone()),
        (
            "directory program",
            "claude",
            not_a_file,
            scene.sign_in.clone(),
        ),
        (
            "missing folder",
            "claude",
            scene.program.clone(),
            scene.sign_in.join("missing"),
        ),
        (
            "program folder",
            "claude",
            scene.program.clone(),
            program_folder,
        ),
        (
            "storage",
            "claude",
            scene.program.clone(),
            inside_data.clone(),
        ),
        (
            "data root",
            "claude",
            scene.program.clone(),
            scene.data.clone(),
        ),
        (
            "workspace",
            "claude",
            scene.program.clone(),
            inside_workspace.clone(),
        ),
        (
            "development root",
            "claude",
            scene.program.clone(),
            scene.root.clone(),
        ),
    ];
    if cfg!(windows) {
        let script = scene.program.with_file_name("claude.cmd");
        fs::write(&script, b"@echo off").expect("script");
        cases.push(("not an exe", "claude", script, scene.sign_in.clone()));
    }
    if let Some(root) = scene.sign_in.ancestors().last() {
        cases.push((
            "filesystem root",
            "claude",
            scene.program.clone(),
            root.to_path_buf(),
        ));
    }
    for (name, profile, program, config_folder) in cases {
        let declaration = ClaudeCodeDeclaration {
            name: profile.to_owned(),
            program,
            config_folder,
        };
        assert!(
            store.check_claude_code_profile(&declaration).is_err(),
            "{name}"
        );
        assert!(
            store
                .add_claude_code_profile(&declaration, revision)
                .is_err(),
            "{name}"
        );
        assert_eq!(store.revision(), revision, "{name}");
        assert!(store.provider_profiles().is_empty(), "{name}");
    }

    store
        .add_claude_code_profile(&scene.declaration("claude"), revision)
        .expect("valid");
    assert!(matches!(
        store.add_claude_code_profile(&scene.declaration("Claude"), revision + 1),
        Err(ControlError::ProviderRefused(_))
    ));
}

/// Replay and append preflight refuse grants that do not match a recorded
/// Claude Code profile and lifecycle steps out of order.
#[test]
fn replay_refuses_inconsistent_claude_code_grants() {
    let scene = Scene::new();
    let mut store = scene.store();
    let profile = store
        .add_claude_code_profile(&scene.declaration("claude"), store.revision())
        .expect("profile");
    let other = store
        .add_provider_profile(
            &ProviderProfileDeclaration {
                name: "local".into(),
                kind: "lm_studio".into(),
                auth: "none".into(),
                endpoint: None,
            },
            store.revision(),
        )
        .expect("other profile");
    let base = store.snapshot();
    let valid = grant_for(profile.id(), ClaudeCodeGrantRole::Program, &scene.relay);
    let mut wrong_name = valid.clone();
    wrong_name.container = "OMGCode.ClaudeCode.other".to_owned();
    let mut wrong_sid = valid.clone();
    wrong_sid.sid = "S-1-5-21-1".to_owned();
    let mut nil_id = valid.clone();
    nil_id.id = Uuid::nil();
    let not_claude = grant_for(other.id(), ClaudeCodeGrantRole::Program, &scene.relay);
    let unknown = grant_for(Uuid::new_v4(), ClaudeCodeGrantRole::Program, &scene.relay);
    for (name, grant) in [
        ("container name", wrong_name),
        ("SID", wrong_sid),
        ("identifier", nil_id),
        ("not a claude_code profile", not_claude),
        ("unknown profile", unknown),
    ] {
        let mut state = base.clone();
        assert!(
            matches!(
                apply(&mut state, &Action::ClaudeCodeGrantRequested { grant }),
                Err(ControlError::Invalid(_))
            ),
            "{name}"
        );
        assert!(state.claude_code_grants.is_empty(), "{name}");
    }

    let mut state = base.clone();
    let id = valid.id;
    apply(
        &mut state,
        &Action::ClaudeCodeGrantRequested {
            grant: valid.clone(),
        },
    )
    .expect("request");
    let same_role = grant_for(profile.id(), ClaudeCodeGrantRole::Program, &scene.sign_in);
    let same_folder = grant_for(profile.id(), ClaudeCodeGrantRole::Relay, &scene.relay);
    for (name, grant) in [
        ("duplicate", valid),
        ("same role", same_role),
        ("same folder", same_folder),
    ] {
        assert!(
            apply(&mut state, &Action::ClaudeCodeGrantRequested { grant }).is_err(),
            "{name}"
        );
    }
    for (name, action) in [
        (
            "revoked before requested",
            Action::ClaudeCodeGrantRevoked { grant_id: id },
        ),
        (
            "unknown applied",
            Action::ClaudeCodeGrantApplied {
                grant_id: Uuid::new_v4(),
            },
        ),
    ] {
        assert!(apply(&mut state, &action).is_err(), "{name}");
    }
    apply(&mut state, &Action::ClaudeCodeGrantApplied { grant_id: id }).expect("applied");
    assert!(
        apply(&mut state, &Action::ClaudeCodeGrantApplied { grant_id: id }).is_err(),
        "applied twice"
    );
    assert!(has_grants(&state, profile.id()));
    assert_eq!(grant_reserve_events(&state), 2);
    let removal = Action::ProviderProfileRemoved {
        profile_id: profile.id(),
        credential_id: None,
    };
    let next = state.revision + 1;
    assert!(
        super::super::provider::apply(&mut state, &removal, next).is_err(),
        "a profile with grants cannot be removed"
    );
    apply(
        &mut state,
        &Action::ClaudeCodeRevokeRequested { grant_id: id },
    )
    .expect("revoking");
    apply(&mut state, &Action::ClaudeCodeGrantRevoked { grant_id: id }).expect("revoked");
    assert!(!has_grants(&state, profile.id()));
    super::super::provider::apply(&mut state, &removal, next).expect("removal after revocation");
}

/// A recorded grant blocks removal through the store as well as in replay,
/// and grant outcomes keep the revision.
#[test]
fn a_recorded_grant_blocks_profile_removal() {
    let scene = Scene::new();
    let mut store = scene.store();
    let profile = store
        .add_claude_code_profile(&scene.declaration("claude"), store.revision())
        .expect("profile");
    let grant = grant_for(profile.id(), ClaudeCodeGrantRole::Relay, &scene.relay);
    let id = grant.id;
    store
        .append(
            Action::ClaudeCodeGrantRequested { grant },
            store.next_revision().expect("revision"),
        )
        .expect("request");
    let revision = store.revision();
    store
        .append(Action::ClaudeCodeGrantApplied { grant_id: id }, revision)
        .expect("applied keeps the revision");
    assert!(matches!(
        store.remove_provider_profile("claude", revision),
        Err(ControlError::ProviderRefused(_))
    ));
    assert_eq!(store.claude_code_grants().len(), 1);
    drop(store);
    let store = ControlStore::open(&scene.data).expect("replay");
    let status = store.claude_code_grants().first().expect("grant");
    assert_eq!(status.grant().id(), id);
    assert_eq!(status.state(), ContainerGrantState::Active);
    let encoded = serde_json::to_value(store.snapshot()).expect("snapshot");
    assert_eq!(encoded["claude_code_grants"][0]["role"], "relay");
    assert_eq!(encoded["claude_code_grants"][0]["state"], "active");
}

#[test]
fn claude_code_container_names_are_stable_and_distinct() {
    let first = Uuid::new_v4();
    let name = claude_code_container(first).expect("name");
    assert_eq!(name, claude_code_container(first).expect("again"));
    assert_ne!(name, claude_code_container(Uuid::new_v4()).expect("other"));
    assert!(name.as_str().starts_with("OMGCode.ClaudeCode."));
    assert_eq!(name.as_str().len(), "OMGCode.ClaudeCode.".len() + 32);
    let runtime = claude_code_runtime_folder(Path::new("data"), first);
    assert_eq!(
        runtime,
        Path::new("data")
            .join("claude-code")
            .join(first.simple().to_string())
    );
}

/// Only a Claude Code connection can be revoked, and a connection with no
/// grant still attempts the container profile deletion, so a retry finishes it.
#[test]
fn revoking_refuses_other_profiles_and_retries_deletion_without_grants() {
    let scene = Scene::new();
    let mut store = scene.store();
    store
        .add_claude_code_profile(&scene.declaration("claude"), store.revision())
        .expect("profile");
    store
        .add_provider_profile(
            &ProviderProfileDeclaration {
                name: "local".into(),
                kind: "lm_studio".into(),
                auth: "none".into(),
                endpoint: None,
            },
            store.revision(),
        )
        .expect("other profile");
    let events = store.entries.len();
    assert!(matches!(
        store
            .revoke_claude_code_grants("local")
            .map_err(|failure| failure.error),
        Err(ControlError::ProviderRefused(
            "this provider profile is not a claude_code connection"
        ))
    ));
    assert!(matches!(
        store
            .revoke_claude_code_grants("missing")
            .map_err(|failure| failure.error),
        Err(ControlError::UnknownProvider)
    ));
    for _ in 0..2 {
        let revoked = store.revoke_claude_code_grants("claude").expect("revoke");
        assert_eq!(revoked.revoked, 0);
        assert_eq!(revoked.profile_deleted, cfg!(windows));
    }
    assert_eq!(store.entries.len(), events);
}

/// A development root that equals, contains, or lies inside a recorded
/// sign-in folder is refused before it is recorded; a separate root is not.
#[test]
fn a_development_root_overlapping_a_sign_in_folder_is_refused() {
    let scene = Scene::new();
    let mut store = scene.store();
    let outer = scene.data.with_file_name("outer");
    let nested = outer.join("sign-in");
    fs::create_dir_all(&nested).expect("nested sign-in folder");
    for (name, folder) in [("claude", &scene.sign_in), ("nested", &nested)] {
        let declaration = ClaudeCodeDeclaration {
            name: name.to_owned(),
            program: scene.program.clone(),
            config_folder: folder.clone(),
        };
        store
            .add_claude_code_profile(&declaration, store.revision())
            .expect("profile");
    }
    let inside = nested.join("work");
    fs::create_dir(&inside).expect("folder inside a sign-in folder");
    let events = store.entries.len();
    for (case, root) in [
        ("equal", &scene.sign_in),
        ("ancestor", &outer),
        ("descendant", &inside),
    ] {
        assert!(
            matches!(
                store.set_development_root(root, store.revision()),
                Err(ControlError::GrantRefused(reason))
                    if reason.starts_with("the development root must not overlap a Claude Code sign-in folder")
            ),
            "{case}"
        );
        assert_eq!(store.entries.len(), events, "{case}");
    }
    let apart = scene.data.with_file_name("elsewhere");
    fs::create_dir(&apart).expect("separate root");
    store
        .set_development_root(&apart, store.revision())
        .expect("a separate root");
    assert_eq!(store.entries.len(), events + 1);
}

/// A turn re-checks that the sign-in folder is still outside the current
/// development root, and outside every workspace container grant.
#[test]
fn resolving_refuses_a_sign_in_folder_that_now_overlaps_other_scopes() {
    let scene = Scene::new();
    let mut store = scene.store();
    store
        .add_claude_code_profile(&scene.declaration("claude"), store.revision())
        .expect("profile");
    let workspace = store.workspaces().next().expect("workspace").id();

    let grant =
        store.test_record_grant(workspace, &scene.sign_in, ContainerGrantAccess::ReadExecute);
    assert!(matches!(
        store.resolve_claude_code("claude", &scene.program),
        Err(ControlError::ProviderRefused(
            "the Claude sign-in folder must not overlap a workspace container grant; revoke that grant first"
        ))
    ));
    store.test_record_revocation(grant);

    let inside = scene.sign_in.join("work");
    fs::create_dir(&inside).expect("folder inside the sign-in folder");
    let root = DirectoryRecord::capture(&inside).expect("root");
    store
        .append(
            Action::RootSet { directory: root },
            store.next_revision().expect("revision"),
        )
        .expect("a root recorded before the check existed");
    assert!(matches!(
        store.resolve_claude_code("claude", &scene.program),
        Err(ControlError::ProviderRefused(
            "the Claude sign-in folder must be outside the development root"
        ))
    ));
}

/// A recorded workspace container grant keeps a sign-in folder from being
/// recorded when it overlaps, and a separate folder is still accepted.
#[test]
fn a_sign_in_folder_overlapping_a_container_grant_is_refused() {
    let scene = Scene::new();
    let mut store = scene.store();
    let granted = scene.data.with_file_name("granted");
    let inside = granted.join("sign-in");
    fs::create_dir_all(&inside).expect("folder inside the grant");
    let workspace = store.workspaces().next().expect("workspace").id();
    store.test_record_grant(workspace, &granted, ContainerGrantAccess::ReadExecute);
    for folder in [&granted, &inside] {
        let declaration = ClaudeCodeDeclaration {
            name: "claude".to_owned(),
            program: scene.program.clone(),
            config_folder: folder.clone(),
        };
        assert!(matches!(
            store.check_claude_code_profile(&declaration),
            Err(ControlError::ProviderRefused(
                "the Claude sign-in folder must not overlap a workspace container grant; revoke that grant first"
            ))
        ));
    }
    store
        .check_claude_code_profile(&scene.declaration("claude"))
        .expect("a separate sign-in folder");
}

/// A workspace container grant whose folder moved may still carry its
/// access-control entry wherever the folder went, so no sign-in folder is
/// accepted until that grant is revoked.
#[test]
fn a_stale_container_grant_record_refuses_every_sign_in_folder() {
    let scene = Scene::new();
    let mut store = scene.store();
    let granted = scene.data.with_file_name("granted");
    fs::create_dir(&granted).expect("granted folder");
    let workspace = store.workspaces().next().expect("workspace").id();
    let grant = store.test_record_grant(workspace, &granted, ContainerGrantAccess::ReadExecute);
    store
        .check_claude_code_profile(&scene.declaration("claude"))
        .expect("a current, separate grant");
    fs::rename(&granted, scene.data.with_file_name("moved")).expect("move the folder");
    assert!(matches!(
        store.check_claude_code_profile(&scene.declaration("claude")),
        Err(ControlError::ProviderRefused(
            "a workspace container grant's folder changed since it was granted; revoke that grant first"
        ))
    ));
    store.test_record_revocation(grant);
    store
        .check_claude_code_profile(&scene.declaration("claude"))
        .expect("accepted once the stale grant is revoked");
}

/// Two connections cannot record the same or nested sign-in folders, and an
/// unchanged connection still passes its own check.
#[test]
fn connections_keep_separate_sign_in_folders() {
    let scene = Scene::new();
    let mut store = scene.store();
    let profile = store
        .add_claude_code_profile(&scene.declaration("claude"), store.revision())
        .expect("profile");
    let inside = scene.sign_in.join("other");
    fs::create_dir(&inside).expect("nested folder");
    let apart = scene.data.with_file_name("apart");
    fs::create_dir(&apart).expect("separate folder");
    for (case, folder) in [("same", &scene.sign_in), ("nested", &inside)] {
        let declaration = ClaudeCodeDeclaration {
            name: "second".to_owned(),
            program: scene.program.clone(),
            config_folder: folder.clone(),
        };
        assert!(
            matches!(
                store.add_claude_code_profile(&declaration, store.revision()),
                Err(ControlError::ProviderRefused(
                    "the Claude sign-in folder must not overlap another Claude Code connection's sign-in folder"
                ))
            ),
            "{case}"
        );
    }
    let declaration = ClaudeCodeDeclaration {
        name: "second".to_owned(),
        program: scene.program.clone(),
        config_folder: apart,
    };
    store
        .add_claude_code_profile(&declaration, store.revision())
        .expect("a separate folder");
    let (_, setup) = store.claude_code_setup("claude").expect("setup");
    store
        .check_config_folder(&setup.config_folder, Some(profile.id()))
        .expect("an unchanged connection passes its own check");
    assert!(
        store
            .check_config_folder(&setup.config_folder, None)
            .is_err(),
        "without the exception the connection's own folder overlaps"
    );
}

/// A sign-in folder that moved could lie anywhere now, so no development root
/// or other connection's sign-in folder is accepted until its connection is
/// removed.
#[test]
fn a_stale_sign_in_record_keeps_every_scope_unproven() {
    let scene = Scene::new();
    let mut store = scene.store();
    store
        .add_claude_code_profile(&scene.declaration("claude"), store.revision())
        .expect("profile");
    let apart = scene.data.with_file_name("apart");
    let other = scene.data.with_file_name("other-sign-in");
    for folder in [&apart, &other] {
        fs::create_dir(folder).expect("separate folder");
    }
    fs::rename(&scene.sign_in, scene.data.with_file_name("moved")).expect("move the folder");
    let stale = |result: Result<(), ControlError>| {
        matches!(
            result,
            Err(ControlError::GrantRefused(reason))
                if reason.starts_with("a Claude Code connection's sign-in folder changed since it was recorded")
        )
    };
    let events = store.entries.len();
    assert!(stale(store.set_development_root(&apart, store.revision())));
    let declaration = ClaudeCodeDeclaration {
        name: "second".to_owned(),
        program: scene.program.clone(),
        config_folder: other,
    };
    assert!(stale(store.check_claude_code_profile(&declaration)));
    assert_eq!(store.entries.len(), events);

    let _ = store
        .remove_provider_profile("claude", store.revision())
        .expect("the stale connection is removed");
    store
        .set_development_root(&apart, store.revision())
        .expect("a separate root once the record is gone");
    store
        .check_claude_code_profile(&declaration)
        .expect("a separate sign-in folder once the record is gone");
}

/// A sign-in folder equal to or inside another connection's program or
/// relay grant is refused, and a separate folder is accepted.
#[test]
fn a_sign_in_folder_inside_another_connections_readable_grant_is_refused() {
    let scene = Scene::new();
    let mut store = scene.store();
    let profile = store
        .add_claude_code_profile(&scene.declaration("claude"), store.revision())
        .expect("profile");
    record_claude_code_grants(&mut store, &scene, profile.id());
    let inside = scene.relay.join("sign-in");
    let apart = scene.data.with_file_name("apart");
    for folder in [&inside, &apart] {
        fs::create_dir(folder).expect("folder");
    }
    for (case, folder) in [("equal", &scene.relay), ("inside", &inside)] {
        let declaration = ClaudeCodeDeclaration {
            name: "second".to_owned(),
            program: scene.program.clone(),
            config_folder: folder.clone(),
        };
        assert!(
            matches!(
                store.check_claude_code_profile(&declaration),
                Err(ControlError::ProviderRefused(
                    "the Claude sign-in folder must not overlap another Claude Code connection's program or relay grant"
                ))
            ),
            "{case}"
        );
    }
    let declaration = ClaudeCodeDeclaration {
        name: "second".to_owned(),
        program: scene.program.clone(),
        config_folder: apart,
    };
    store
        .check_claude_code_profile(&declaration)
        .expect("a separate sign-in folder");
}

/// A program folder that overlaps another connection's sign-in folder is
/// refused before it is recorded.
#[test]
fn a_program_folder_overlapping_another_sign_in_folder_is_refused() {
    let scene = Scene::new();
    let mut store = scene.store();
    store
        .add_claude_code_profile(&scene.declaration("claude"), store.revision())
        .expect("profile");
    let tools = scene.sign_in.join("bin");
    fs::create_dir(&tools).expect("program folder");
    let program = tools.join(PROGRAM_NAME);
    fs::write(&program, b"not a real program").expect("program");
    let apart = scene.data.with_file_name("apart");
    fs::create_dir(&apart).expect("separate sign-in folder");
    let events = store.entries.len();
    let declaration = ClaudeCodeDeclaration {
        name: "second".to_owned(),
        program,
        config_folder: apart,
    };
    assert!(matches!(
        store.add_claude_code_profile(&declaration, store.revision()),
        Err(ControlError::GrantRefused(
            "a Claude Code folder must not overlap another Claude Code connection's sign-in folder"
        ))
    ));
    assert_eq!(store.entries.len(), events);
}

/// Record an applied grant for every role without changing an ACL, so a
/// connection resolves on Windows, and return the grant identifiers by role.
fn record_claude_code_grants(
    store: &mut ControlStore,
    scene: &Scene,
    profile_id: Uuid,
) -> Vec<(ClaudeCodeGrantRole, Uuid)> {
    let data = store.data_directory.live_path().expect("data directory");
    let runtime = claude_code_runtime_folder(&data, profile_id);
    fs::create_dir_all(&runtime).expect("runtime folder");
    let program_folder = scene.program.parent().expect("program folder").to_owned();
    let mut recorded = Vec::new();
    for (role, folder) in [
        (ClaudeCodeGrantRole::Program, &program_folder),
        (ClaudeCodeGrantRole::Configuration, &scene.sign_in),
        (ClaudeCodeGrantRole::Runtime, &runtime),
        (ClaudeCodeGrantRole::Relay, &scene.relay),
    ] {
        recorded.push((
            role,
            record_claude_code_grant(store, profile_id, role, folder),
        ));
    }
    recorded
}

fn record_claude_code_grant(
    store: &mut ControlStore,
    profile_id: Uuid,
    role: ClaudeCodeGrantRole,
    folder: &Path,
) -> Uuid {
    let grant = grant_for(profile_id, role, folder);
    let grant_id = grant.id;
    let revision = store.next_revision().expect("revision");
    store
        .append(Action::ClaudeCodeGrantRequested { grant }, revision)
        .expect("request");
    store
        .append(
            Action::ClaudeCodeGrantApplied { grant_id },
            store.revision(),
        )
        .expect("applied");
    grant_id
}

fn record_claude_code_revocation(store: &mut ControlStore, grant_id: Uuid) {
    for action in [
        Action::ClaudeCodeRevokeRequested { grant_id },
        Action::ClaudeCodeGrantRevoked { grant_id },
    ] {
        store.append(action, store.revision()).expect("revocation");
    }
}

/// A recorded connection, granted on Windows, resolved for one turn. The
/// store is released, so the turn's checks can read the settings.
fn resolved_turn(scene: &Scene) -> (Uuid, Vec<(ClaudeCodeGrantRole, Uuid)>, ClaudeCodeWatch) {
    let mut store = scene.store();
    let profile = store
        .add_claude_code_profile(&scene.declaration("claude"), store.revision())
        .expect("profile");
    let grants = if cfg!(windows) {
        record_claude_code_grants(&mut store, scene, profile.id())
    } else {
        Vec::new()
    };
    let relay = scene.relay.join(PROGRAM_NAME);
    fs::write(&relay, b"not a real relay").expect("relay");
    let paths = store
        .resolve_claude_code("claude", &relay)
        .expect("resolved");
    (profile.id(), grants, paths.watch)
}

/// A running turn checks every settings state recorded after it resolved
/// its connection: unchanged settings allow it, settings that another
/// command holds are checked again later, and a removed connection
/// withdraws it.
#[test]
fn a_running_turn_is_withdrawn_when_its_connection_is_removed() {
    let scene = Scene::new();
    let (profile_id, grants, watch) = resolved_turn(&scene);
    let mut cursor = watch.cursor();
    assert_eq!(watch.recheck(&mut cursor), Recheck::Allowed);
    assert_eq!(watch.recheck(&mut cursor), Recheck::Allowed, "unchanged");

    let mut store = ControlStore::open(&scene.data).expect("store");
    assert_eq!(
        watch.recheck(&mut cursor),
        Recheck::Allowed,
        "opening the settings changes nothing"
    );
    for (_, grant_id) in grants {
        record_claude_code_revocation(&mut store, grant_id);
    }
    let removed = store
        .remove_provider_profile("claude", store.revision())
        .expect("removed");
    assert_eq!(removed.profile_id, profile_id);
    // A new connection with the same name is another connection.
    store
        .add_claude_code_profile(&scene.declaration("claude"), store.revision())
        .expect("profile again");
    assert_eq!(
        watch.recheck(&mut cursor),
        Recheck::Busy,
        "changed settings that another command holds are read later"
    );
    drop(store);
    let expected = if cfg!(windows) {
        "a grant of the Claude Code connection was revoked"
    } else {
        "the Claude Code connection was removed"
    };
    assert_eq!(watch.recheck(&mut cursor), Recheck::Withdrawn(expected));
    let store = ControlStore::open(&scene.data).expect("store");
    assert_eq!(
        store.claude_code_withdrawn(profile_id),
        Some("the Claude Code connection was removed")
    );
}

/// A grant revoked and then granted again before the turn checks the
/// settings still withdraws the turn: the state between is checked too.
#[cfg(windows)]
#[test]
fn a_grant_revoked_and_restored_between_checks_still_withdraws_the_turn() {
    let scene = Scene::new();
    let (profile_id, grants, watch) = resolved_turn(&scene);
    let mut cursor = watch.cursor();
    assert_eq!(watch.recheck(&mut cursor), Recheck::Allowed);

    let mut store = ControlStore::open(&scene.data).expect("store");
    let (role, grant_id) = grants[0];
    record_claude_code_revocation(&mut store, grant_id);
    record_claude_code_grant(
        &mut store,
        profile_id,
        role,
        scene.program.parent().expect("folder"),
    );
    assert_eq!(store.claude_code_withdrawn(profile_id), None, "restored");
    drop(store);
    assert_eq!(
        watch.recheck(&mut cursor),
        Recheck::Withdrawn("a grant of the Claude Code connection was revoked")
    );
}

/// A revocation waits until every running turn released the connection's
/// shared turn lock, and a revocation that holds the lock keeps a new turn
/// from starting.
#[test]
fn a_revocation_waits_for_every_running_turn() {
    let scene = Scene::new();
    let (profile_id, _, watch) = resolved_turn(&scene);
    let short = std::time::Duration::from_millis(200);
    wait_for_claude_code_turns(&scene.data, profile_id, short).expect("no turn ever started");

    let first = watch.lock_turn().expect("lock").expect("free");
    let second = watch.lock_turn().expect("lock").expect("shared by turns");
    assert!(matches!(
        wait_for_claude_code_turns(&scene.data, profile_id, short),
        Err(ControlError::ClaudeCodeTurnsRunning)
    ));
    drop(first);
    assert!(matches!(
        wait_for_claude_code_turns(&scene.data, profile_id, short),
        Err(ControlError::ClaudeCodeTurnsRunning)
    ));
    drop(second);
    wait_for_claude_code_turns(&scene.data, profile_id, short).expect("every turn ended");

    let path = super::turns::turn_lock_path(&scene.data, profile_id);
    let revocation = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("lock file");
    revocation.try_lock().expect("exclusive");
    assert!(watch.lock_turn().expect("lock").is_none());
}

/// A wait that found a turn running succeeds once that last turn releases
/// its lock during the same wait.
#[test]
fn a_wait_succeeds_once_the_last_turn_ends_during_it() {
    let scene = Scene::new();
    let (profile_id, _, watch) = resolved_turn(&scene);
    let turn = watch.lock_turn().expect("lock").expect("free");
    let (contended, observed) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let seen = observed
            .recv_timeout(std::time::Duration::from_secs(30))
            .is_ok();
        drop(turn);
        seen
    });
    let mut signalled = false;
    super::turns::wait_for_turns_observed(
        &scene.data,
        profile_id,
        std::time::Duration::from_secs(30),
        || {
            if !signalled {
                signalled = true;
                let _ = contended.send(());
            }
        },
    )
    .expect("the wait ends once the last turn released its lock");
    assert!(signalled, "the wait first found the turn running");
    assert!(holder.join().expect("holder"), "released after contention");
}

/// A turn's record fails every wait once its lock is gone, as after its host
/// exited before the tree was observed ended, until the operator
/// acknowledges it; a cleared record leaves nothing. Acknowledging is
/// refused, as lock contention, while a turn holds the lock, and removes
/// every record of the connection and no other. A record fails a wait even
/// where no turn lock was ever made.
#[test]
fn a_turn_record_left_behind_fails_every_wait_until_acknowledged() {
    let scene = Scene::new();
    let (profile_id, _, watch) = resolved_turn(&scene);
    let short = std::time::Duration::from_millis(200);
    let turn = watch.lock_turn().expect("lock").expect("free");
    watch
        .record_turn()
        .expect("record")
        .clear()
        .expect("an observed end clears its record");
    let _left = watch.record_turn().expect("record");
    let _also_left = watch.record_turn().expect("a second turn's record");
    assert!(matches!(
        wait_for_claude_code_turns(&scene.data, profile_id, short),
        Err(ControlError::ClaudeCodeTurnsRunning)
    ));
    assert!(
        matches!(
            acknowledge_claude_code_cleanup(&scene.data, profile_id),
            Err(ControlError::ClaudeCodeTurnLockHeld)
        ),
        "refused while a turn holds the lock"
    );
    drop(turn);
    assert!(matches!(
        wait_for_claude_code_turns(&scene.data, profile_id, short),
        Err(ControlError::ClaudeCodeCleanupUnconfirmed(id)) if id == profile_id
    ));

    let other = Uuid::new_v4();
    let _other_left = ClaudeCodeWatch::for_tests(scene.data.clone(), other, &scene.root)
        .record_turn()
        .expect("record");
    assert!(acknowledge_claude_code_cleanup(&scene.data, profile_id).expect("acknowledged"));
    wait_for_claude_code_turns(&scene.data, profile_id, short).expect("acknowledged");
    assert!(
        !acknowledge_claude_code_cleanup(&scene.data, profile_id).expect("again"),
        "nothing is left to acknowledge"
    );
    assert!(
        matches!(
            wait_for_claude_code_turns(&scene.data, other, short),
            Err(ControlError::ClaudeCodeCleanupUnconfirmed(id)) if id == other
        ),
        "another connection's record stays"
    );
    assert!(acknowledge_claude_code_cleanup(&scene.data, other).expect("acknowledged"));
    wait_for_claude_code_turns(&scene.data, other, short).expect("acknowledged");
}

/// Acknowledging where no turn ever ran finds nothing, without making the
/// runtime folder.
#[test]
fn acknowledging_without_any_turn_finds_nothing() {
    let scene = Scene::new();
    let _store = scene.store();
    assert!(!acknowledge_claude_code_cleanup(&scene.data, Uuid::new_v4()).expect("nothing"));
    assert!(!scene.data.join(RUNTIME_PARENT).exists());
}

/// A removed connection's turn record keeps its sign-in folder reserved: a
/// new connection cannot record the same folder, or one around it, until the
/// record is acknowledged. A record that cannot be read reserves an unknown
/// folder, so every sign-in folder is refused until it is acknowledged.
#[test]
fn a_removed_connections_turn_record_reserves_its_sign_in_folder() {
    let scene = Scene::new();
    let (profile_id, grants, watch) = resolved_turn(&scene);
    let left = watch.record_turn().expect("record");
    let mut store = ControlStore::open(&scene.data).expect("store");
    for (_, grant_id) in grants {
        record_claude_code_revocation(&mut store, grant_id);
    }
    let removed = store
        .remove_provider_profile("claude", store.revision())
        .expect("removed");
    assert_eq!(removed.profile_id, profile_id);
    let events = store.entries.len();
    let below = scene.sign_in.join("below");
    fs::create_dir(&below).expect("folder below");
    let inside = ClaudeCodeDeclaration {
        name: "inside".to_owned(),
        program: scene.program.clone(),
        config_folder: below,
    };
    for declaration in [scene.declaration("again"), inside] {
        assert!(
            matches!(
                store.add_claude_code_profile(&declaration, store.revision()),
                Err(ControlError::ClaudeCodeSignInReserved(id)) if id == profile_id
            ),
            "{}",
            declaration.name
        );
    }
    assert_eq!(store.entries.len(), events);
    drop(store);
    assert!(acknowledge_claude_code_cleanup(&scene.data, profile_id).expect("acknowledged"));
    drop(left);

    let mut store = ControlStore::open(&scene.data).expect("store");
    store
        .add_claude_code_profile(&scene.declaration("again"), store.revision())
        .expect("the folder is free once acknowledged");
    let _ = store
        .remove_provider_profile("again", store.revision())
        .expect("removed");
    let unreadable = Uuid::new_v4();
    fs::write(
        scene.data.join(RUNTIME_PARENT).join(format!(
            "{}.{}.unconfirmed",
            unreadable.simple(),
            Uuid::new_v4().simple()
        )),
        b"not a record",
    )
    .expect("unreadable record");
    assert!(matches!(
        store.add_claude_code_profile(&scene.declaration("again"), store.revision()),
        Err(ControlError::ClaudeCodeTurnRecordUnreadable(id)) if id == unreadable
    ));
    assert!(acknowledge_claude_code_cleanup(&scene.data, unreadable).expect("acknowledged"));
    store
        .add_claude_code_profile(&scene.declaration("again"), store.revision())
        .expect("nothing is reserved");
}

#[cfg(not(windows))]
#[test]
fn other_platforms_refuse_claude_code_grants_without_recording_them() {
    let scene = Scene::new();
    let mut store = scene.store();
    store
        .add_claude_code_profile(&scene.declaration("claude"), store.revision())
        .expect("profile");
    let events = store.entries.len();
    let error = store
        .grant_claude_code("claude", &scene.relay, store.revision())
        .expect_err("no AppContainer");
    assert!(
        matches!(&error, ControlError::Container(cause) if cause.kind() == std::io::ErrorKind::Unsupported),
        "{error}"
    );
    assert_eq!(store.entries.len(), events);
}

#[cfg(windows)]
mod windows {
    use super::*;
    use omgcode_platform::container::{ContainerAccess, granted_access, profile_exists};

    fn access(path: &Path, profile_id: Uuid) -> Option<ContainerAccess> {
        granted_access(path, &claude_code_container(profile_id).expect("name")).expect("access")
    }

    #[test]
    fn grants_are_recorded_applied_and_revoked_with_the_profile_container() {
        let scene = Scene::new();
        let mut store = scene.store();
        let profile = store
            .add_claude_code_profile(&scene.declaration("claude"), store.revision())
            .expect("profile");
        let revision = store.revision();
        let granted = store
            .grant_claude_code("claude", &scene.relay, revision)
            .expect("grants");
        assert_eq!(
            granted
                .iter()
                .map(ClaudeCodeGrant::role)
                .collect::<Vec<_>>(),
            ClaudeCodeGrantRole::ALL
        );
        assert_eq!(store.revision(), revision + 4);
        let runtime = claude_code_runtime_folder(&scene.data, profile.id());
        let program_folder = scene.program.parent().expect("folder");
        assert_eq!(
            access(program_folder, profile.id()),
            Some(ContainerAccess::ReadExecute)
        );
        assert_eq!(
            access(&scene.sign_in, profile.id()),
            Some(ContainerAccess::ReadWrite)
        );
        assert_eq!(
            access(&runtime, profile.id()),
            Some(ContainerAccess::ReadExecute)
        );
        assert_eq!(
            access(&scene.relay, profile.id()),
            Some(ContainerAccess::ReadExecute)
        );
        assert_eq!(access(&scene.workspace, profile.id()), None);
        let name = claude_code_container(profile.id()).expect("name");
        assert!(profile_exists(&name).expect("profile"));

        // Granting again adds nothing: every role already has its folder.
        let again = store
            .grant_claude_code("claude", &scene.relay, store.revision())
            .expect("again");
        assert!(again.is_empty());
        assert_eq!(store.revision(), revision + 4);
        assert!(matches!(
            store.remove_provider_profile("claude", store.revision()),
            Err(ControlError::ProviderRefused(_))
        ));

        let revoked = store.revoke_claude_code_grants("claude").expect("revoke");
        assert_eq!(revoked.revoked, 4);
        assert!(revoked.profile_deleted);
        for path in [program_folder, &scene.sign_in, &runtime, &scene.relay] {
            assert_eq!(access(path, profile.id()), None, "{}", path.display());
        }
        assert!(!profile_exists(&name).expect("profile"));
        assert_eq!(
            store.revision(),
            revision + 4,
            "revocation keeps the revision"
        );
        assert!(store.claude_code_grants().is_empty());
        let removed = store
            .remove_provider_profile("claude", store.revision())
            .expect("removal after revocation");
        assert_eq!(removed.profile_id, profile.id());
    }

    #[test]
    fn a_relay_folder_that_holds_a_workspace_is_refused_before_recording() {
        let scene = Scene::new();
        let mut store = scene.store();
        store
            .add_claude_code_profile(&scene.declaration("claude"), store.revision())
            .expect("profile");
        let events = store.entries.len();
        let revision = store.revision();
        assert!(matches!(
            store.grant_claude_code("claude", &scene.root, revision),
            Err(ControlError::GrantRefused(
                "a Claude Code folder must not contain the development root"
            ))
        ));
        assert!(matches!(
            store.grant_claude_code("claude", &scene.data, revision),
            Err(ControlError::GrantRefused(
                "a Claude Code folder must not contain application storage"
            ))
        ));
        assert_eq!(store.entries.len(), events);
        assert_eq!(store.revision(), revision);
        assert!(store.claude_code_grants().is_empty());
    }

    /// A relay program beside the `claude` program shares its folder; the
    /// plan is refused as a whole, so no earlier role is granted.
    #[test]
    fn a_plan_with_a_shared_folder_is_refused_before_any_grant() {
        let scene = Scene::new();
        let mut store = scene.store();
        let profile = store
            .add_claude_code_profile(&scene.declaration("claude"), store.revision())
            .expect("profile");
        let events = store.entries.len();
        let revision = store.revision();
        let program_folder = scene.program.parent().expect("folder");
        assert!(matches!(
            store.grant_claude_code("claude", program_folder, revision),
            Err(ControlError::GrantRefused(
                "two roles of this connection name the same folder"
            ))
        ));
        assert_eq!(store.entries.len(), events);
        assert_eq!(store.revision(), revision);
        assert!(store.claude_code_grants().is_empty());
        for path in [program_folder, &scene.sign_in] {
            assert_eq!(access(path, profile.id()), None, "{}", path.display());
        }
        let name = claude_code_container(profile.id()).expect("name");
        assert!(!profile_exists(&name).expect("profile"));
    }

    /// A container profile left behind after the last revocation is deleted
    /// by a later revoke, although no grant remains.
    #[test]
    fn a_later_revoke_deletes_a_container_profile_that_has_no_grants() {
        let scene = Scene::new();
        let mut store = scene.store();
        let profile = store
            .add_claude_code_profile(&scene.declaration("claude"), store.revision())
            .expect("profile");
        let name = claude_code_container(profile.id()).expect("name");
        // The interrupted run: the last grant was revoked, the deletion was not.
        let relay = DirectoryRecord::capture(&scene.relay).expect("relay");
        let live = relay.live_path().expect("live path");
        omgcode_platform::container::grant(
            &live,
            relay.native_identity(),
            &name,
            ContainerAccess::ReadExecute,
        )
        .expect("creates the profile");
        omgcode_platform::container::revoke(&live, relay.native_identity(), &name)
            .expect("revokes");
        assert!(profile_exists(&name).expect("profile"));
        let revoked = store.revoke_claude_code_grants("claude").expect("revoke");
        assert_eq!(revoked.revoked, 0);
        assert!(revoked.profile_deleted);
        assert!(!profile_exists(&name).expect("profile"));
        let again = store.revoke_claude_code_grants("claude").expect("again");
        assert!(again.profile_deleted, "an absent profile counts as deleted");
    }

    /// A workspace container grant and a sign-in folder cannot overlap in
    /// either direction; an unrelated external folder is still granted.
    #[test]
    fn workspace_grants_and_sign_in_folders_stay_disjoint() {
        let scene = Scene::new();
        let mut store = scene.store();
        store
            .add_claude_code_profile(&scene.declaration("claude"), store.revision())
            .expect("profile");
        let workspace = store.workspaces().next().expect("workspace").id();
        let above = scene.data.with_file_name("above");
        let inside = above.join("sign-in");
        fs::create_dir_all(&inside).expect("nested folder");
        let declaration = ClaudeCodeDeclaration {
            name: "nested".to_owned(),
            program: scene.program.clone(),
            config_folder: inside.clone(),
        };
        store
            .add_claude_code_profile(&declaration, store.revision())
            .expect("nested profile");
        let below = inside.join("child");
        fs::create_dir(&below).expect("folder below a sign-in folder");
        let reason = "a container grant must not overlap a Claude Code sign-in folder";
        for folder in [&scene.sign_in, &above, &inside, &below] {
            assert!(
                matches!(
                    store.check_container_grants(
                        workspace,
                        std::slice::from_ref(folder),
                        ContainerGrantAccess::ReadExecute
                    ),
                    Err((0, ControlError::GrantRefused(refused))) if refused == reason
                ),
                "{}",
                folder.display()
            );
        }
        store
            .check_container_grants(
                workspace,
                std::slice::from_ref(&scene.relay),
                ContainerGrantAccess::ReadExecute,
            )
            .expect("an unrelated external folder");
    }

    /// A relay folder that equals, contains, or lies inside another
    /// connection's sign-in folder is refused before anything is recorded.
    #[test]
    fn a_relay_grant_overlapping_another_sign_in_folder_is_refused() {
        let scene = Scene::new();
        let mut store = scene.store();
        let outer = scene.data.with_file_name("outer");
        let sign_in = outer.join("sign-in");
        let below = sign_in.join("bin");
        let apart = scene.data.with_file_name("apart");
        for folder in [&below, &apart] {
            fs::create_dir_all(folder).expect("folder");
        }
        let first = ClaudeCodeDeclaration {
            name: "first".to_owned(),
            program: scene.program.clone(),
            config_folder: sign_in.clone(),
        };
        store
            .add_claude_code_profile(&first, store.revision())
            .expect("first profile");
        let second = ClaudeCodeDeclaration {
            name: "second".to_owned(),
            program: scene.program.clone(),
            config_folder: apart,
        };
        store
            .add_claude_code_profile(&second, store.revision())
            .expect("second profile");
        let events = store.entries.len();
        for (case, relay) in [
            ("contains", &outer),
            ("equal", &sign_in),
            ("inside", &below),
        ] {
            assert!(
                matches!(
                    store.grant_claude_code("second", relay, store.revision()),
                    Err(ControlError::GrantRefused(
                        "a Claude Code folder must not overlap another Claude Code connection's sign-in folder"
                    ))
                ),
                "{case}"
            );
        }
        assert_eq!(store.entries.len(), events);
        assert!(store.claude_code_grants().is_empty());
    }

    /// A revocation that fails after it recorded a withdrawal names the
    /// connection, so the caller still waits for its turns; a refusal
    /// before any withdrawal does not.
    #[test]
    fn a_revocation_that_fails_after_withdrawing_reports_the_withdrawal() {
        let scene = Scene::new();
        let mut store = scene.store();
        let profile = store
            .add_claude_code_profile(&scene.declaration("claude"), store.revision())
            .expect("profile");
        let refused = store
            .revoke_claude_code_grants("missing")
            .expect_err("unknown connection");
        assert!(refused.withdrawn.is_none());
        // Recorded grants without ACL changes: revoking an absent entry
        // succeeds, and the moved relay folder's revocation fails last.
        record_claude_code_grants(&mut store, &scene, profile.id());
        fs::rename(&scene.relay, scene.relay.with_file_name("relay-moved")).expect("move");
        let failure = store
            .revoke_claude_code_grants("claude")
            .expect_err("the relay revocation fails");
        assert_eq!(failure.withdrawn, Some(profile.id()), "{:?}", failure.error);
        assert!(store.claude_code_withdrawn(profile.id()).is_some());
        let again = store
            .revoke_claude_code_grants("claude")
            .expect_err("still fails");
        assert_eq!(
            again.withdrawn,
            Some(profile.id()),
            "the earlier withdrawal still counts"
        );
    }

    /// A withdrawal append whose sync failed may have written the event,
    /// which a later open replays and a running turn's watcher reads, so the
    /// failed revocation names the connection.
    #[test]
    fn a_withdrawal_append_that_failed_its_sync_names_the_connection() {
        let scene = Scene::new();
        let mut store = scene.store();
        let profile = store
            .add_claude_code_profile(&scene.declaration("claude"), store.revision())
            .expect("profile");
        record_claude_code_grants(&mut store, &scene, profile.id());
        store.fail_next_sync = true;
        let failure = store
            .revoke_claude_code_grants("claude")
            .expect_err("the append's sync fails");
        assert!(
            matches!(failure.error, ControlError::Io(_)),
            "{:?}",
            failure.error
        );
        assert_eq!(failure.withdrawn, Some(profile.id()));
        drop(store);
        let store = ControlStore::open(&scene.data).expect("store");
        assert_eq!(
            store.claude_code_withdrawn(profile.id()),
            Some("a grant of the Claude Code connection was revoked"),
            "the written event withdrew the connection"
        );
    }

    /// A retry after an earlier revocation revoked the last grant still
    /// counts that withdrawal: no grant is active then. Every grant active
    /// means none was withdrawn yet.
    #[test]
    fn a_retry_with_no_grant_left_counts_the_earlier_withdrawal() {
        let scene = Scene::new();
        let mut store = scene.store();
        let profile = store
            .add_claude_code_profile(&scene.declaration("claude"), store.revision())
            .expect("profile");
        let grants = record_claude_code_grants(&mut store, &scene, profile.id());
        let active = store.snapshot.claude_code_grants.clone();
        assert!(!store.claude_code_withdrawal_recorded(profile.id(), &active));
        for (_, grant_id) in grants {
            record_claude_code_revocation(&mut store, grant_id);
        }
        assert!(store.snapshot.claude_code_grants.is_empty());
        assert!(store.claude_code_withdrawal_recorded(profile.id(), &[]));
    }

    /// A granted sign-in folder that moved keeps every scope unproven and
    /// can be neither revoked nor removed. Once the same folder is back at
    /// its recorded path, revocation and then removal recover the settings.
    #[test]
    fn a_moved_granted_sign_in_folder_recovers_once_restored() {
        let scene = Scene::new();
        let mut store = scene.store();
        let profile = store
            .add_claude_code_profile(&scene.declaration("claude"), store.revision())
            .expect("profile");
        store
            .grant_claude_code("claude", &scene.relay, store.revision())
            .expect("grants");
        let apart = scene.data.with_file_name("apart");
        fs::create_dir(&apart).expect("separate root");
        let moved = scene.sign_in.with_file_name("claude-sign-in-moved");
        fs::rename(&scene.sign_in, &moved).expect("move the folder");

        assert!(matches!(
            store.set_development_root(&apart, store.revision()),
            Err(ControlError::GrantRefused(reason))
                if reason.starts_with("a Claude Code connection's sign-in folder changed since it was recorded")
        ));
        assert!(matches!(
            store.remove_provider_profile("claude", store.revision()),
            Err(ControlError::ProviderRefused(_))
        ));
        let failure = store
            .revoke_claude_code_grants("claude")
            .expect_err("the recorded folder is not at its path");
        assert_eq!(failure.withdrawn, Some(profile.id()), "{:?}", failure.error);

        fs::rename(&moved, &scene.sign_in).expect("restore the folder");
        let revoked = store
            .revoke_claude_code_grants("claude")
            .expect("revoked once restored");
        assert!(revoked.profile_deleted);
        let program_folder = scene.program.parent().expect("folder");
        let runtime = claude_code_runtime_folder(&scene.data, profile.id());
        for path in [program_folder, &scene.sign_in, &runtime, &scene.relay] {
            assert_eq!(access(path, profile.id()), None, "{}", path.display());
        }
        let _ = store
            .remove_provider_profile("claude", store.revision())
            .expect("removed after revocation");
        store
            .set_development_root(&apart, store.revision())
            .expect("the root changes once the record is gone");
    }
}
