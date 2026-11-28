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
    assert!(!revoked.profile_deleted);
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
        for folder in [&scene.root, &scene.data] {
            assert!(
                store
                    .grant_claude_code("claude", folder, store.revision())
                    .is_err(),
                "{}",
                folder.display()
            );
        }
        assert_eq!(store.entries.len(), events);
        assert!(store.claude_code_grants().is_empty());
    }
}
