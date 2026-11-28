//! Toolchain profiles generated from installed toolchains in the desktop
//! `settings` window, and their assignment to a chosen workspace (ADR-0103,
//! ADR-0107). A scripted prompt stands in for the native dialogs, as in
//! `desktop_settings_execution`, and a fixture host laid out like this
//! platform stands in for the computer.
//!
//! Workspaces are registered with core's primitive, which grants nothing. On
//! Windows, the one test that adds profiles to a workspace applies real
//! read-execute grants to the fixture host's temporary folders, as the CLI
//! tests do; dropping the fixture revokes them, which also deletes the
//! AppContainer profile.

mod common;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use omgcode_app::TokenSource;
use omgcode_app::desktop::{
    Confirmation, DesktopConfig, DesktopError, DesktopService, DiscoveredProfilesRequest,
    FolderPurpose, SettingsPrompt, ToolchainDiscovery, ToolchainDiscoveryRequest,
};
use omgcode_core::control::{
    ContainerGrantAccess, ContainerGrantState, ControlStore, ProfileDeclaration, ProfilePlatform,
};
use omgcode_core::toolchain_discovery::DiscoveryHost;
use serde_json::{Value, json};
use uuid::Uuid;

use common::{Fixture, json_of};

/// Answers confirmations in order; any folder dialog or an unscripted
/// confirmation fails the test.
#[derive(Default)]
struct ScriptedPrompt {
    answers: RefCell<VecDeque<bool>>,
    shown: RefCell<Vec<Confirmation>>,
    during_confirm: RefCell<Option<Box<dyn FnOnce()>>>,
}

impl ScriptedPrompt {
    fn answering(confirmed: bool) -> Self {
        let prompt = Self::default();
        prompt.answers.borrow_mut().push_back(confirmed);
        prompt
    }

    fn silent() -> Self {
        Self::default()
    }

    fn meanwhile(self, action: impl FnOnce() + 'static) -> Self {
        *self.during_confirm.borrow_mut() = Some(Box::new(action));
        self
    }

    fn shown(&self) -> Vec<Confirmation> {
        self.shown.take()
    }
}

impl SettingsPrompt for ScriptedPrompt {
    fn pick_folder(&self, _purpose: FolderPurpose, _start: Option<&Path>) -> Option<PathBuf> {
        panic!("discovery never opens a folder dialog");
    }

    fn confirm(&self, confirmation: &Confirmation) -> bool {
        self.shown.borrow_mut().push(confirmation.clone());
        if let Some(action) = self.during_confirm.borrow_mut().take() {
            action();
        }
        self.answers
            .borrow_mut()
            .pop_front()
            .expect("an unexpected confirmation")
    }
}

fn service(data: &Path) -> DesktopService {
    DesktopService::open(DesktopConfig {
        data_root: data.to_path_buf(),
        endpoint: "http://127.0.0.1:1234/v1".to_owned(),
        token_source: TokenSource::Environment,
        stream: false,
        claude_code: omgcode_app::desktop::ClaudeCodeHost::default(),
    })
    .expect("open desktop service")
}

fn current_revision(data: &Path) -> u64 {
    ControlStore::open_existing(data)
        .expect("open")
        .expect("store exists")
        .revision()
}

fn platform() -> ProfilePlatform {
    ProfilePlatform::current().expect("tests run on Linux or Windows")
}

fn program(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    }
}

/// A Go installation at `home` whose `VERSION` names `version`.
fn go_install(home: &Path, version: &str) {
    fs::create_dir_all(home.join("bin")).expect("go bin");
    fs::write(home.join("bin").join(program("go")), b"").expect("go program");
    fs::write(home.join("VERSION"), format!("go{version}\ntime x\n")).expect("go version");
}

/// A computer laid out like this platform, beside the fixture's data and
/// development roots, with Go 1.23.4 in the standard place and Go 1.22.0 in
/// the operator's `~/sdk`.
struct Host {
    base: PathBuf,
    newest: PathBuf,
    older: PathBuf,
}

impl Host {
    fn new(fixture: &Fixture) -> Self {
        let base = fixture.data.parent().expect("temporary root").join("host");
        let (newest, home) = match platform() {
            ProfilePlatform::Windows => {
                fs::create_dir_all(base.join("Windows").join("System32")).expect("System32");
                (
                    base.join("Program Files").join("Go"),
                    base.join("Users").join("operator"),
                )
            }
            ProfilePlatform::Linux => {
                fs::create_dir_all(base.join("usr").join("bin")).expect("usr/bin");
                (
                    base.join("usr").join("local").join("go"),
                    base.join("home").join("operator"),
                )
            }
        };
        let older = home.join("sdk").join("go1.22.0");
        go_install(&newest, "1.23.4");
        go_install(&older, "1.22.0");
        Self {
            base,
            newest,
            older,
        }
    }

    fn discovery(&self) -> DiscoveryHost {
        DiscoveryHost::rooted_at(platform(), &self.base)
    }
}

fn discover(fixture: &Fixture, host: &Host, workspace: Option<Uuid>) -> ToolchainDiscovery {
    service(&fixture.data)
        .settings_discover_toolchains(
            ToolchainDiscoveryRequest {
                workspace_id: workspace,
            },
            &host.discovery(),
        )
        .expect("discovered")
}

fn digests_of(discovery: &ToolchainDiscovery) -> Vec<String> {
    discovery
        .proposals
        .iter()
        .map(|proposal| proposal.digest.clone())
        .collect()
}

fn add_request(revision: u64, digests: Vec<String>) -> DiscoveredProfilesRequest {
    DiscoveredProfilesRequest {
        revision,
        digests,
        workspace_id: None,
    }
}

/// Register one workspace with core's primitive, which grants nothing;
/// returns its identifier.
fn with_workspace(fixture: &Fixture) -> Uuid {
    let project = fixture.development.join("project");
    fs::create_dir_all(&project).expect("project");
    let mut store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("exists");
    let revision = store.revision();
    *store
        .register_workspace(&project, revision)
        .expect("registered")
        .id()
        .as_uuid()
}

fn profile_count(data: &Path) -> usize {
    ControlStore::open_existing(data)
        .expect("open")
        .expect("exists")
        .profiles()
        .len()
}

#[test]
fn discovery_proposes_installed_toolchains_and_records_nothing() {
    let fixture = Fixture::new();
    let host = Host::new(&fixture);
    let before = current_revision(&fixture.data);

    let discovery = discover(&fixture, &host, None);

    let names: Vec<&str> = discovery
        .proposals
        .iter()
        .map(|proposal| proposal.name.as_str())
        .collect();
    assert_eq!(names, ["go", "go-1.22.0"]);
    let newest = &discovery.proposals[0];
    assert_eq!(newest.family_label, "Go");
    assert_eq!(newest.version.as_deref(), Some("1.23.4"));
    assert_eq!(newest.digest.len(), 64);
    assert_eq!(newest.refusal, None);
    assert!(!newest.recorded);
    // The installation and the system part, which every proposal lists. On
    // Linux `usr/local/go` sits inside the system part's `usr`, so core drops
    // the nested root and `usr` alone covers the installation.
    let installation = host.newest.display().to_string();
    let roots: Vec<&str> = newest
        .read_only_roots
        .iter()
        .map(|root| root.folder.as_str())
        .collect();
    let expected = match platform() {
        ProfilePlatform::Windows => vec![
            installation.clone(),
            host.base
                .join("Windows")
                .join("System32")
                .display()
                .to_string(),
        ],
        ProfilePlatform::Linux => vec![host.base.join("usr").display().to_string()],
    };
    assert_eq!(roots, expected, "{newest:?}");
    let variables: Vec<(&str, &str)> = newest
        .environment
        .iter()
        .map(|variable| (variable.name.as_str(), variable.value.as_str()))
        .collect();
    assert!(
        variables.contains(&("GOTOOLCHAIN", "local")),
        "{variables:?}"
    );
    assert!(
        variables.contains(&("GOROOT", installation.as_str())),
        "{variables:?}"
    );
    assert!(!newest.notes.is_empty());
    // No workspace was selected, so no root reports container access.
    let wire_view = json_of(&discovery);
    for root in wire_view["proposals"][0]["read_only_roots"]
        .as_array()
        .expect("roots")
    {
        assert_eq!(root["grant"], "not_applicable");
    }
    assert_eq!(wire_view["workspace_grant_active"], Value::Null);
    assert_eq!(wire_view["omitted"], 0);
    assert_eq!(discovery.overview.revision, before);
    assert_eq!(current_revision(&fixture.data), before);
    assert_eq!(profile_count(&fixture.data), 0);
}

#[test]
fn the_view_sends_no_path_back_and_requests_carry_only_digests() {
    let fixture = Fixture::new();
    let host = Host::new(&fixture);
    let discovery = discover(&fixture, &host, None);
    let digest = discovery.proposals[0].digest.clone();

    // A request that names a folder, a root, or a variable is refused.
    for refused in [
        json!({ "revision": 1, "digests": [digest], "workspace_id": null, "roots": ["x"] }),
        json!({ "revision": 1, "digests": [digest], "workspace_id": null, "environment": [] }),
    ] {
        assert!(serde_json::from_value::<DiscoveredProfilesRequest>(refused).is_err());
    }
}

#[test]
fn selected_proposals_are_added_after_one_confirmation_that_lists_everything() {
    let fixture = Fixture::new();
    let host = Host::new(&fixture);
    let discovery = discover(&fixture, &host, None);
    let before = discovery.overview.revision;
    let prompt = ScriptedPrompt::answering(true);

    let after = service(&fixture.data)
        .settings_add_discovered_profiles(
            add_request(before, digests_of(&discovery)),
            &host.discovery(),
            &prompt,
        )
        .expect("added");

    let shown = prompt.shown();
    assert_eq!(shown.len(), 1, "one confirmation for the batch");
    let message = &shown[0].message;
    for expected in [
        "2 toolchain profile(s)",
        "go (Go, 1.23.4)",
        "go-1.22.0 (Go, 1.22.0)",
        "Read-only roots:",
        "PATH entries:",
        "GOTOOLCHAIN=local",
        "GOROOT=",
        "go1.22.0",
        "ran no program",
        "grants nothing",
        "plain text",
    ] {
        assert!(message.contains(expected), "{expected} in {message}");
    }
    // One record per profile, at consecutive revisions.
    assert_eq!(after.overview.revision, before + 2);
    let store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("exists");
    let newest = store.profile("go").expect("go recorded").profile();
    let goroot = newest
        .environment()
        .iter()
        .find(|variable| variable.name() == "GOROOT")
        .expect("GOROOT");
    assert_eq!(Path::new(goroot.value()), host.newest);
    let older = store
        .profile("go-1.22.0")
        .expect("older recorded")
        .profile();
    assert!(
        older.environment().iter().any(
            |variable| variable.name() == "GOROOT" && Path::new(variable.value()) == host.older
        )
    );
    // The refreshed list shows both as recorded, and core now refuses them.
    for proposal in &after.proposals {
        assert!(proposal.recorded, "{proposal:?}");
        assert!(proposal.refusal.is_some(), "{proposal:?}");
    }
}

#[test]
fn a_declined_confirmation_records_nothing() {
    let fixture = Fixture::new();
    let host = Host::new(&fixture);
    let discovery = discover(&fixture, &host, None);
    let before = discovery.overview.revision;
    let prompt = ScriptedPrompt::answering(false);

    let error = service(&fixture.data)
        .settings_add_discovered_profiles(
            add_request(before, digests_of(&discovery)),
            &host.discovery(),
            &prompt,
        )
        .expect_err("declined");

    assert_eq!(error.code(), "cancelled");
    assert_eq!(current_revision(&fixture.data), before);
    assert_eq!(profile_count(&fixture.data), 0);
}

#[test]
fn a_stale_revision_or_a_bad_selection_is_refused_before_any_dialog() {
    let fixture = Fixture::new();
    let host = Host::new(&fixture);
    let discovery = discover(&fixture, &host, None);
    let before = discovery.overview.revision;
    let digests = digests_of(&discovery);

    for (request, code) in [
        (add_request(before + 1, digests.clone()), "stale_revision"),
        (add_request(before, Vec::new()), "invalid_request"),
        (
            add_request(before, vec![digests[0].clone(), digests[0].clone()]),
            "invalid_request",
        ),
        (
            add_request(before, vec!["ab".repeat(32).to_uppercase()]),
            "invalid_request",
        ),
    ] {
        let error = service(&fixture.data)
            .settings_add_discovered_profiles(request, &host.discovery(), &ScriptedPrompt::silent())
            .expect_err("refused");
        assert_eq!(error.code(), code, "{error:?}");
    }
    assert_eq!(profile_count(&fixture.data), 0);
}

#[test]
fn a_selection_made_before_the_installations_changed_is_refused() {
    let fixture = Fixture::new();
    let host = Host::new(&fixture);
    let discovery = discover(&fixture, &host, None);
    // The newest installation is upgraded after the list was made.
    go_install(&host.newest, "1.23.5");

    let error = service(&fixture.data)
        .settings_add_discovered_profiles(
            add_request(discovery.overview.revision, digests_of(&discovery)),
            &host.discovery(),
            &ScriptedPrompt::silent(),
        )
        .expect_err("changed");

    assert_eq!(error.code(), "discovery_changed");
    assert_eq!(profile_count(&fixture.data), 0);
}

#[test]
fn a_proposal_core_would_refuse_shows_its_reason_and_is_refused_before_any_dialog() {
    let fixture = Fixture::new();
    let host = Host::new(&fixture);
    {
        let mut store = ControlStore::open_existing(&fixture.data)
            .expect("open")
            .expect("exists");
        let revision = store.revision();
        store
            .add_profile(
                &ProfileDeclaration {
                    name: "GO".into(),
                    read_only_roots: vec![host.older.clone()],
                    path_entries: Vec::new(),
                    environment: Vec::new(),
                },
                revision,
            )
            .expect("an operator's own profile");
    }

    let discovery = discover(&fixture, &host, None);
    let taken = &discovery.proposals[0];
    assert_eq!(taken.name, "go");
    assert!(taken.recorded, "names compare case-insensitively");
    let reason = taken.refusal.as_deref().expect("refused");
    assert!(reason.contains("already"), "{reason}");
    assert_eq!(discovery.proposals[1].refusal, None);

    let error = service(&fixture.data)
        .settings_add_discovered_profiles(
            add_request(discovery.overview.revision, digests_of(&discovery)),
            &host.discovery(),
            &ScriptedPrompt::silent(),
        )
        .expect_err("refused");
    assert_eq!(error.code(), "control");
    assert!(error.message().starts_with("go: "), "{}", error.message());
    assert_eq!(profile_count(&fixture.data), 1);
}

#[test]
fn a_change_made_while_the_confirmation_is_open_is_refused_as_stale() {
    let fixture = Fixture::new();
    let host = Host::new(&fixture);
    let discovery = discover(&fixture, &host, None);
    let data = fixture.data.clone();
    let root = host.older.clone();
    let prompt = ScriptedPrompt::answering(true).meanwhile(move || {
        let mut store = ControlStore::open_existing(&data)
            .expect("open")
            .expect("exists");
        let revision = store.revision();
        store
            .add_profile(
                &ProfileDeclaration {
                    name: "intervening".into(),
                    read_only_roots: vec![root],
                    path_entries: Vec::new(),
                    environment: Vec::new(),
                },
                revision,
            )
            .expect("an intervening change");
    });

    let error = service(&fixture.data)
        .settings_add_discovered_profiles(
            add_request(discovery.overview.revision, digests_of(&discovery)),
            &host.discovery(),
            &prompt,
        )
        .expect_err("stale");

    assert_eq!(error.code(), "stale_revision");
    assert_eq!(profile_count(&fixture.data), 1, "only the intervening one");
}

#[test]
fn a_batch_that_stops_partway_says_how_many_were_recorded() {
    let fixture = Fixture::new();
    let host = Host::new(&fixture);
    // 31 of the 32 profiles allowed: each proposal passes its own check, but
    // only the first of two fits.
    {
        let mut store = ControlStore::open_existing(&fixture.data)
            .expect("open")
            .expect("exists");
        for index in 0..31 {
            let revision = store.revision();
            store
                .add_profile(
                    &ProfileDeclaration {
                        name: format!("filler-{index}"),
                        read_only_roots: vec![host.older.clone()],
                        path_entries: Vec::new(),
                        environment: Vec::new(),
                    },
                    revision,
                )
                .expect("filler");
        }
    }
    let discovery = discover(&fixture, &host, None);
    assert!(
        discovery
            .proposals
            .iter()
            .all(|proposal| proposal.refusal.is_none())
    );

    let error = service(&fixture.data)
        .settings_add_discovered_profiles(
            add_request(discovery.overview.revision, digests_of(&discovery)),
            &host.discovery(),
            &ScriptedPrompt::answering(true),
        )
        .expect_err("the second does not fit");

    assert_eq!(error.code(), "control");
    assert!(
        error
            .message()
            .starts_with("1 of 2 profiles were recorded before this failure"),
        "{}",
        error.message()
    );
    assert_eq!(profile_count(&fixture.data), 32);
}

#[test]
fn an_unknown_workspace_is_refused() {
    let fixture = Fixture::new();
    let host = Host::new(&fixture);

    let error: DesktopError = service(&fixture.data)
        .settings_discover_toolchains(
            ToolchainDiscoveryRequest {
                workspace_id: Some(Uuid::new_v4()),
            },
            &host.discovery(),
        )
        .expect_err("unknown");

    assert_eq!(error.code(), "unknown_workspace");
}

#[test]
fn adding_with_an_unknown_workspace_is_refused_before_any_dialog() {
    let fixture = Fixture::new();
    let host = Host::new(&fixture);
    let discovery = discover(&fixture, &host, None);
    let before = discovery.overview.revision;

    let error = service(&fixture.data)
        .settings_add_discovered_profiles(
            DiscoveredProfilesRequest {
                revision: before,
                digests: digests_of(&discovery),
                workspace_id: Some(Uuid::new_v4()),
            },
            &host.discovery(),
            &ScriptedPrompt::silent(),
        )
        .expect_err("unknown");

    // Refused before the dialog and before any profile, so the window never
    // reports a failure for a change that was recorded.
    assert_eq!(error.code(), "unknown_workspace");
    assert_eq!(profile_count(&fixture.data), 0);
    assert_eq!(current_revision(&fixture.data), before);
}

#[cfg(windows)]
#[test]
fn on_windows_each_root_says_what_the_workspace_container_needs() {
    let fixture = Fixture::new();
    let host = Host::new(&fixture);
    let workspace = with_workspace(&fixture);

    let discovery = discover(&fixture, &host, Some(workspace));

    assert_eq!(discovery.workspace_id, Some(workspace));
    // The workspace has no recorded read-write grant on its own folder.
    assert_eq!(discovery.workspace_grant_active, Some(false));
    let wire_view = json_of(&discovery);
    for proposal in wire_view["proposals"].as_array().expect("proposals") {
        for root in proposal["read_only_roots"].as_array().expect("roots") {
            // The fixture folders are this account's own, and nothing grants
            // the container access to them.
            assert_eq!(root["grant"], "needs_grant", "{root}");
        }
    }
}

#[test]
fn adding_with_a_workspace_names_what_it_grants_in_the_one_confirmation() {
    let fixture = Fixture::new();
    let host = Host::new(&fixture);
    let workspace = with_workspace(&fixture);
    let discovery = discover(&fixture, &host, Some(workspace));
    let before = discovery.overview.revision;
    let prompt = ScriptedPrompt::answering(false);

    let error = service(&fixture.data)
        .settings_add_discovered_profiles(
            DiscoveredProfilesRequest {
                revision: before,
                digests: digests_of(&discovery),
                workspace_id: Some(workspace),
            },
            &host.discovery(),
            &prompt,
        )
        .expect_err("declined");

    assert_eq!(error.code(), "cancelled");
    let shown = prompt.shown();
    assert_eq!(shown.len(), 1, "one confirmation for profiles and grants");
    let message = &shown[0].message;
    for expected in [
        "2 toolchain profile(s)",
        "project",
        "assigned to the workspace",
    ] {
        assert!(message.contains(expected), "{expected} in {message}");
    }
    if cfg!(windows) {
        for expected in [
            "System32",
            "go1.22.0",
            "read and execute",
            "access-control list",
            "AppContainer",
        ] {
            assert!(message.contains(expected), "{expected} in {message}");
        }
        // Each grant is listed once, although both proposals need System32;
        // the roots list it once per proposal.
        let grants = message
            .split("read and execute access to everything beneath it")
            .nth(1)
            .expect("a grant list");
        assert_eq!(grants.matches("System32").count(), 1, "{message}");
    } else {
        assert!(!message.contains("access-control list"), "{message}");
    }
    let store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("exists");
    assert!(store.container_grants().is_empty());
    assert!(store.profiles().is_empty());
    assert_eq!(store.revision(), before);
    drop(store);
    fixture
        .revoke_grants()
        .expect("every container grant is revoked");
}

#[test]
fn adding_with_a_workspace_assigns_the_profiles_and_grants_their_folders() {
    let fixture = Fixture::new();
    let host = Host::new(&fixture);
    let workspace = with_workspace(&fixture);
    let discovery = discover(&fixture, &host, Some(workspace));

    let after = service(&fixture.data)
        .settings_add_discovered_profiles(
            DiscoveredProfilesRequest {
                revision: discovery.overview.revision,
                digests: digests_of(&discovery),
                workspace_id: Some(workspace),
            },
            &host.discovery(),
            &ScriptedPrompt::answering(true),
        )
        .expect("added and assigned");

    assert_eq!(after.overview.workspaces[0].profiles, ["go", "go-1.22.0"]);
    let store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("exists");
    let grants: Vec<_> = store
        .container_grants()
        .iter()
        .map(|status| {
            let grant = status.grant();
            assert_eq!(*grant.workspace_id().as_uuid(), workspace);
            assert_eq!(grant.access(), ContainerGrantAccess::ReadExecute);
            assert_eq!(status.state(), ContainerGrantState::Active, "{status:?}");
            grant.directory_display()
        })
        .collect();
    if cfg!(windows) {
        // System32 once, and each Go installation's own folder.
        assert_eq!(grants.len(), 3, "{grants:?}");
        assert_eq!(
            grants
                .iter()
                .filter(|grant| grant.ends_with("System32"))
                .count(),
            1,
            "{grants:?}"
        );
        // The refreshed list shows every root as readable now.
        for proposal in json_of(&after)["proposals"].as_array().expect("proposals") {
            for root in proposal["read_only_roots"].as_array().expect("roots") {
                assert_eq!(root["grant"], "readable", "{root}");
            }
        }
    } else {
        assert!(grants.is_empty(), "{grants:?}");
    }
    drop(store);
    fixture
        .revoke_grants()
        .expect("every container grant is revoked");
}

#[cfg(windows)]
#[test]
fn on_windows_adding_a_proposal_inside_the_development_root_to_a_workspace_grants_nothing() {
    let fixture = Fixture::new();
    let host = Host::new(&fixture);
    let workspace = with_workspace(&fixture);
    // A Go that the operator's GOROOT names inside the development root,
    // beside the workspace rather than in it.
    let inside = fixture.development.join("tools").join("go");
    go_install(&inside, "1.24.0");
    let request = service(&fixture.data)
        .settings_discover_toolchains(
            ToolchainDiscoveryRequest {
                workspace_id: Some(workspace),
            },
            &host.discovery().with_goroot(inside.clone()),
        )
        .expect("discovered");
    let shown_root = inside.display().to_string();
    let proposal = request
        .proposals
        .iter()
        .find(|proposal| {
            proposal
                .read_only_roots
                .iter()
                .any(|root| root.folder == shown_root)
        })
        .expect("the GOROOT proposal");
    // Core would refuse it as a profile, and the list says so.
    assert!(proposal.refusal.is_some(), "{proposal:?}");
    let before = request.overview.revision;

    let error = service(&fixture.data)
        .settings_add_discovered_profiles(
            DiscoveredProfilesRequest {
                revision: before,
                digests: vec![proposal.digest.clone()],
                workspace_id: Some(workspace),
            },
            &host.discovery().with_goroot(inside),
            &ScriptedPrompt::silent(),
        )
        .expect_err("core refuses the profile");

    assert_eq!(error.code(), "control");
    assert!(
        error.message().contains("development root"),
        "{}",
        error.message()
    );
    let store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("exists");
    assert!(store.container_grants().is_empty());
    assert_eq!(store.revision(), before);
    drop(store);
    fixture
        .revoke_grants()
        .expect("every container grant is revoked");
}

#[cfg(not(windows))]
#[test]
fn off_windows_roots_need_no_grant() {
    let fixture = Fixture::new();
    let host = Host::new(&fixture);
    let workspace = with_workspace(&fixture);
    let discovery = discover(&fixture, &host, Some(workspace));
    assert_eq!(discovery.workspace_grant_active, None);
    let wire_view = json_of(&discovery);
    for root in wire_view["proposals"][0]["read_only_roots"]
        .as_array()
        .expect("roots")
    {
        assert_eq!(root["grant"], "not_applicable");
    }
}

#[test]
fn a_host_laid_out_for_the_other_platform_is_refused() {
    let fixture = Fixture::new();
    let other = match platform() {
        ProfilePlatform::Windows => ProfilePlatform::Linux,
        ProfilePlatform::Linux => ProfilePlatform::Windows,
    };
    let base = fixture.data.parent().expect("root").join("other");
    fs::create_dir_all(&base).expect("other host");

    let error = service(&fixture.data)
        .settings_discover_toolchains(
            ToolchainDiscoveryRequest::default(),
            &DiscoveryHost::rooted_at(other, &base),
        )
        .expect_err("refused");

    assert_eq!(error.code(), "unsupported_platform");
}
