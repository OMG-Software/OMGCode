//! Toolchain profiles and container grants generated from installed
//! toolchains in the desktop `settings` window (ADR-0103). A scripted prompt
//! stands in for the native dialogs, as in `desktop_settings_execution`, and a
//! fixture host laid out like this platform stands in for the computer.
//!
//! No test applies a container grant: that changes a folder's access-control
//! list and creates an AppContainer profile on the host. The grant path is
//! tested up to its confirmation on Windows, and its platform refusal
//! elsewhere.

mod common;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use omgcode_app::TokenSource;
use omgcode_app::desktop::{
    Confirmation, DesktopConfig, DesktopError, DesktopService, DiscoveredGrantsRequest,
    DiscoveredProfilesRequest, FolderPurpose, SettingsPrompt, SettingsRevisionRequest,
    ToolchainDiscovery, ToolchainDiscoveryRequest,
};
use omgcode_core::control::{ControlStore, ProfileDeclaration, ProfilePlatform};
use omgcode_core::toolchain_discovery::DiscoveryHost;
use serde_json::{Value, json};
use uuid::Uuid;

use common::{Fixture, json_of, wire};

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

/// A folder dialog stands in for registering a workspace.
struct FolderPrompt(PathBuf);

impl SettingsPrompt for FolderPrompt {
    fn pick_folder(&self, _purpose: FolderPurpose, _start: Option<&Path>) -> Option<PathBuf> {
        Some(self.0.clone())
    }

    fn confirm(&self, _confirmation: &Confirmation) -> bool {
        true
    }
}

fn service(data: &Path) -> DesktopService {
    DesktopService::open(DesktopConfig {
        data_root: data.to_path_buf(),
        endpoint: "http://127.0.0.1:1234/v1".to_owned(),
        token_source: TokenSource::Environment,
        stream: false,
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

/// Register one workspace through the service; returns its identifier.
fn with_workspace(fixture: &Fixture) -> Uuid {
    let project = fixture.development.join("project");
    fs::create_dir_all(&project).expect("project");
    let request: SettingsRevisionRequest =
        wire(json!({ "revision": current_revision(&fixture.data) }));
    service(&fixture.data)
        .settings_register_workspace(request, &FolderPrompt(project))
        .expect("registered")
        .workspaces[0]
        .workspace_id
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
    // The installation and the system part, which every proposal lists.
    assert_eq!(newest.read_only_roots.len(), 2);
    let installation = host.newest.display().to_string();
    assert!(
        newest
            .read_only_roots
            .iter()
            .any(|root| root.folder == installation),
        "{newest:?}"
    );
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

#[cfg(windows)]
#[test]
fn on_windows_a_grant_confirmation_names_the_workspace_and_every_folder() {
    let fixture = Fixture::new();
    let host = Host::new(&fixture);
    let workspace = with_workspace(&fixture);
    let discovery = discover(&fixture, &host, Some(workspace));
    let before = discovery.overview.revision;
    let prompt = ScriptedPrompt::answering(false);

    let error = service(&fixture.data)
        .settings_grant_discovered_roots(
            DiscoveredGrantsRequest {
                revision: before,
                workspace_id: workspace,
                digests: digests_of(&discovery),
            },
            &host.discovery(),
            &prompt,
        )
        .expect_err("declined");

    assert_eq!(error.code(), "cancelled");
    let shown = prompt.shown();
    assert_eq!(shown.len(), 1);
    let message = &shown[0].message;
    for expected in [
        "project",
        "System32",
        "Go",
        "go1.22.0",
        "read and execute",
        "access-control list",
    ] {
        assert!(message.contains(expected), "{expected} in {message}");
    }
    // System32 is listed once although both proposals need it.
    assert_eq!(message.matches("System32").count(), 1, "{message}");
    let store = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("exists");
    assert!(store.container_grants().is_empty());
    assert_eq!(store.revision(), before);
}

#[cfg(windows)]
#[test]
fn on_windows_a_grant_for_an_unregistered_workspace_is_refused_before_any_dialog() {
    let fixture = Fixture::new();
    let host = Host::new(&fixture);
    let discovery = discover(&fixture, &host, None);

    let error = service(&fixture.data)
        .settings_grant_discovered_roots(
            DiscoveredGrantsRequest {
                revision: discovery.overview.revision,
                workspace_id: Uuid::new_v4(),
                digests: digests_of(&discovery),
            },
            &host.discovery(),
            &ScriptedPrompt::silent(),
        )
        .expect_err("unknown");

    assert_eq!(error.code(), "unknown_workspace");
}

#[cfg(not(windows))]
#[test]
fn off_windows_grants_are_refused_and_roots_need_none() {
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

    let error = service(&fixture.data)
        .settings_grant_discovered_roots(
            DiscoveredGrantsRequest {
                revision: discovery.overview.revision,
                workspace_id: workspace,
                digests: digests_of(&discovery),
            },
            &host.discovery(),
            &ScriptedPrompt::silent(),
        )
        .expect_err("unsupported");

    assert_eq!(error.code(), "unsupported_platform");
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
