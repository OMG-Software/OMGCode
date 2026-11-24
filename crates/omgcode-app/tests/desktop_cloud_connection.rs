//! Adding Ollama Cloud and its API key, replacing the key, and removing a
//! connection with a stored key from the desktop `settings` window (ADR-0071
//! decision 5, ADR-0059 decision 6), against the real settings store and the
//! real native credential store. Keys are obviously fake. A `Vault` deletes
//! only the native items these tests created, including after a failed
//! assertion. No network is used: no turn is sent to the hosted service.

mod common;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use omgcode_app::TokenSource;
use omgcode_app::desktop::{
    Confirmation, ConnectionAddRequest, ConnectionOrigin, DesktopConfig, DesktopError,
    DesktopService, FolderPurpose, SettingsOverview, SettingsPrompt,
};
use omgcode_core::control::{ControlStore, CredentialError, CredentialId, CredentialStore};
use serde_json::{Value, json};
use uuid::Uuid;

use common::credential::Vault;
use common::{Fixture, NoCallServer, text_of, text_only, unused_endpoint, wire};

const KEY: &str = "test-key-not-real-71c2";
const NEXT_KEY: &str = "test-key-not-real-replaced-e5b0";
const CLOUD: &str = "https://ollama.com/v1";

/// Answers confirmations in order and records what each one said.
#[derive(Default)]
struct Answers {
    answers: RefCell<VecDeque<bool>>,
    shown: RefCell<Vec<Confirmation>>,
}

impl Answers {
    fn with(answers: &[bool]) -> Self {
        Self {
            answers: RefCell::new(answers.iter().copied().collect()),
            shown: RefCell::default(),
        }
    }

    fn shown(&self) -> Vec<Confirmation> {
        self.shown.take()
    }
}

impl SettingsPrompt for Answers {
    fn pick_folder(
        &self,
        _purpose: FolderPurpose,
        _start: Option<&std::path::Path>,
    ) -> Option<PathBuf> {
        panic!("no folder dialog is expected");
    }

    fn confirm(&self, confirmation: &Confirmation) -> bool {
        self.shown.borrow_mut().push(confirmation.clone());
        self.answers
            .borrow_mut()
            .pop_front()
            .expect("an unexpected confirmation")
    }
}

fn service(fixture: &Fixture) -> DesktopService {
    DesktopService::open(DesktopConfig {
        data_root: fixture.data.clone(),
        endpoint: unused_endpoint(),
        token_source: TokenSource::Active,
        stream: false,
    })
    .expect("open desktop service")
}

fn revision(fixture: &Fixture) -> u64 {
    ControlStore::open(&fixture.data)
        .expect("control store")
        .revision()
}

fn add(
    service: &DesktopService,
    fixture: &Fixture,
    body: Value,
) -> Result<SettingsOverview, DesktopError> {
    let mut request = body;
    request["revision"] = json!(revision(fixture));
    service.settings_add_connection(wire(request))
}

fn add_cloud(
    service: &DesktopService,
    fixture: &Fixture,
    name: &str,
    key: &str,
) -> Result<SettingsOverview, DesktopError> {
    add(
        service,
        fixture,
        json!({"name": name, "kind": "ollama_cloud", "api_key": key}),
    )
}

fn set_key(
    service: &DesktopService,
    fixture: &Fixture,
    name: &str,
    key: &str,
) -> Result<SettingsOverview, DesktopError> {
    service.settings_set_api_key(wire(
        json!({"revision": revision(fixture), "name": name, "api_key": key}),
    ))
}

fn remove(
    service: &DesktopService,
    fixture: &Fixture,
    name: &str,
    prompt: &Answers,
) -> Result<SettingsOverview, DesktopError> {
    service.settings_remove_connection(
        wire(json!({"revision": revision(fixture), "name": name})),
        prompt,
    )
}

fn code<T>(result: Result<T, DesktopError>) -> &'static str {
    result.err().expect("refused").code()
}

/// The identifier of the credential bound to `name`, if any.
fn bound(fixture: &Fixture, name: &str) -> Option<CredentialId> {
    ControlStore::open(&fixture.data)
        .expect("control store")
        .provider_profile(name)
        .and_then(|status| status.credential().map(|binding| binding.id().clone()))
}

/// The native item's bytes, or `None` when it does not exist.
fn native(fixture: &Fixture, id: &CredentialId) -> Option<Vec<u8>> {
    match CredentialStore::open(&fixture.data)
        .expect("native store")
        .load(id)
    {
        Ok(secret) => Some(secret.expose().to_vec()),
        Err(CredentialError::NotFound) => None,
        Err(error) => panic!("unexpected native store failure: {error}"),
    }
}

/// Assert that no file under `root` holds `key`: the settings journal and
/// every other persisted file. The native store keeps the key outside the
/// data directory (Credential Manager or the Secret Service).
fn assert_no_file_holds(root: &Path, key: &str) {
    let mut pending = vec![root.to_path_buf()];
    let mut files = 0;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read data directory") {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files += 1;
                let bytes = fs::read(&path).expect("read persisted file");
                assert!(
                    !bytes
                        .windows(key.len())
                        .any(|window| window == key.as_bytes()),
                    "{} holds the key",
                    path.display()
                );
            }
        }
    }
    assert!(files > 0, "the settings journal was not found");
}

fn profile_count(fixture: &Fixture) -> usize {
    ControlStore::open(&fixture.data)
        .expect("control store")
        .provider_profiles()
        .len()
}

/// Adding Ollama Cloud records the profile with the fixed endpoint, stores and
/// binds the key, and makes the profile selectable. Nothing returned holds
/// the key.
#[test]
fn adding_ollama_cloud_records_the_profile_and_binds_the_key() {
    let fixture = Fixture::new();
    let service = service(&fixture);
    let mut vault = Vault::new(&fixture.data);

    let overview = add_cloud(&service, &fixture, "cloud", KEY).expect("add");
    let id = bound(&fixture, "cloud").expect("a bound credential");
    vault.track(id.clone());
    assert_eq!(native(&fixture, &id).as_deref(), Some(KEY.as_bytes()));

    let profile = overview
        .provider_profiles
        .iter()
        .find(|profile| profile.name == "cloud")
        .expect("listed");
    assert_eq!(profile.kind, "ollama_cloud");
    assert_eq!(profile.auth, "api_key");
    assert_eq!(profile.endpoint.as_deref(), Some(CLOUD));
    assert!(profile.credential_bound);
    assert!(profile.selectable);
    assert!(!profile.active);
    ControlStore::open(&fixture.data)
        .expect("control store")
        .check_provider_selectable("cloud")
        .expect("selectable");

    // No response, and no journal the desktop reads, holds the key.
    assert!(!text_of(&overview).contains(KEY), "overview holds the key");
    let again = service.settings_overview().expect("overview");
    assert!(!text_of(&again).contains(KEY));
    assert_no_file_holds(&fixture.data, KEY);
}

/// Each named hosted provider is added with only a name and a key: Rust
/// supplies its one documented endpoint, stores and binds the key, and the
/// key can be replaced later (ADR-0091).
#[test]
fn adding_a_named_hosted_provider_records_its_fixed_endpoint() {
    let fixture = Fixture::new();
    let service = service(&fixture);
    let mut vault = Vault::new(&fixture.data);
    for (name, kind, endpoint) in [
        ("claude", "anthropic", "https://api.anthropic.com/v1"),
        ("gpt", "openai", "https://api.openai.com/v1"),
        ("deep", "deepseek", "https://api.deepseek.com"),
        ("glm", "z_ai", "https://api.z.ai/api/paas/v4"),
        ("grok", "xai", "https://api.x.ai/v1"),
    ] {
        let overview = add(
            &service,
            &fixture,
            json!({"name": name, "kind": kind, "api_key": KEY}),
        )
        .unwrap_or_else(|error| panic!("{kind}: {error}"));
        let id = bound(&fixture, name).expect("a bound credential");
        vault.track(id.clone());
        let profile = overview
            .provider_profiles
            .iter()
            .find(|profile| profile.name == name)
            .expect("listed");
        assert_eq!(profile.kind, kind);
        assert_eq!(profile.auth, "api_key");
        assert_eq!(profile.endpoint.as_deref(), Some(endpoint));
        assert!(profile.credential_bound && profile.selectable, "{kind}");
        assert!(!text_of(&overview).contains(KEY), "{kind}");
    }
    set_key(&service, &fixture, "deep", NEXT_KEY).expect("replace the DeepSeek key");
    let replaced = bound(&fixture, "deep").expect("bound");
    vault.track(replaced.clone());
    assert_eq!(
        native(&fixture, &replaced).as_deref(),
        Some(NEXT_KEY.as_bytes())
    );
    assert_no_file_holds(&fixture.data, KEY);
    assert_no_file_holds(&fixture.data, NEXT_KEY);
}

/// An OpenAI-compatible connection records the HTTPS base URL the operator
/// typed, exactly, with its key. Plain HTTP and other text its rule refuses
/// leave no profile and no stored key (ADR-0091).
#[test]
fn adding_an_openai_compatible_connection_records_the_typed_endpoint() {
    let fixture = Fixture::new();
    let service = service(&fixture);
    let mut vault = Vault::new(&fixture.data);
    let typed = "https://provider-api.example.org/v1";
    for refused in [
        "http://provider-api.example.org/v1",
        "https://user@provider-api.example.org/v1",
        "https://provider-api.example.org/v1?key=1",
    ] {
        let error = add(
            &service,
            &fixture,
            json!({"name": "custom", "kind": "openai_compatible", "endpoint": refused, "api_key": KEY}),
        )
        .expect_err(refused);
        assert_eq!(error.code(), "control", "{refused}");
        assert!(!format!("{error} {error:?}").contains(KEY));
    }
    assert_eq!(profile_count(&fixture), 0);

    let overview = add(
        &service,
        &fixture,
        json!({"name": "custom", "kind": "openai_compatible", "endpoint": typed, "api_key": KEY}),
    )
    .expect("add");
    vault.track(bound(&fixture, "custom").expect("a bound credential"));
    let profile = overview
        .provider_profiles
        .iter()
        .find(|profile| profile.name == "custom")
        .expect("listed");
    assert_eq!(profile.kind, "openai_compatible");
    assert_eq!(profile.endpoint.as_deref(), Some(typed));
    assert!(profile.credential_bound && profile.selectable);
    assert_no_file_holds(&fixture.data, KEY);
}

/// Every refused request leaves no profile, no stored secret, and no change of
/// revision, and no error repeats the key.
#[test]
fn a_refused_add_records_nothing_and_never_repeats_the_key() {
    let fixture = Fixture::new();
    let service = service(&fixture);
    let before = revision(&fixture);
    let oversized = "k".repeat(16 * 1024 + 1);
    let cases = [
        (
            "missing key",
            json!({"name": "c", "kind": "ollama_cloud"}),
            KEY,
        ),
        (
            "key for a local kind",
            json!({"name": "l", "kind": "ollama_local", "endpoint": "http://127.0.0.1:11434/v1", "api_key": KEY}),
            KEY,
        ),
        (
            "endpoint for the cloud",
            json!({"name": "c", "kind": "ollama_cloud", "endpoint": "https://example.com/v1", "api_key": KEY}),
            KEY,
        ),
        (
            "oversized key",
            json!({"name": "c", "kind": "ollama_cloud", "api_key": oversized}),
            "kkkkkkkk",
        ),
        (
            "empty key",
            json!({"name": "c", "kind": "ollama_cloud", "api_key": ""}),
            "unused",
        ),
        (
            "control character in the key",
            json!({"name": "c", "kind": "ollama_cloud", "api_key": format!("{KEY}\n")}),
            KEY,
        ),
        (
            "local kind without an endpoint",
            json!({"name": "l", "kind": "lm_studio"}),
            KEY,
        ),
        (
            "endpoint for a named hosted provider",
            json!({"name": "d", "kind": "deepseek", "endpoint": "https://example.com/v1", "api_key": KEY}),
            KEY,
        ),
        (
            "missing key for a named hosted provider",
            json!({"name": "d", "kind": "anthropic"}),
            KEY,
        ),
        (
            "missing endpoint for an OpenAI-compatible service",
            json!({"name": "o", "kind": "openai_compatible", "api_key": KEY}),
            KEY,
        ),
        (
            "missing key for an OpenAI-compatible service",
            json!({"name": "o", "kind": "openai_compatible", "endpoint": "https://provider-api.example.org/v1"}),
            KEY,
        ),
    ];
    for (label, body, secret) in cases {
        let error = add(&service, &fixture, body).expect_err(label);
        assert_eq!(error.code(), "invalid_request", "{label}");
        let shown = format!(
            "{error} {error:?} {}",
            serde_json::to_string(&error).expect("json")
        );
        // The failure message names the case only: printing `shown` would
        // write the key to the test log in exactly the case this catches.
        assert!(
            !shown.contains(secret),
            "{label}: the refusal repeats the key"
        );
    }
    assert_eq!(revision(&fixture), before);
    assert_eq!(profile_count(&fixture), 0);

    // A duplicate name is refused by core before any key is stored, and the
    // first profile keeps its own credential.
    add_cloud(&service, &fixture, "cloud", KEY).expect("first add");
    let mut vault = Vault::new(&fixture.data);
    let first = bound(&fixture, "cloud").expect("bound");
    vault.track(first.clone());
    let recorded = revision(&fixture);
    let error = add_cloud(&service, &fixture, "cloud", NEXT_KEY).expect_err("duplicate");
    assert_eq!(error.code(), "control");
    assert!(!format!("{error} {error:?}").contains(NEXT_KEY));
    assert_eq!(revision(&fixture), recorded);
    assert_eq!(bound(&fixture, "cloud"), Some(first.clone()));
    assert_eq!(native(&fixture, &first).as_deref(), Some(KEY.as_bytes()));
    assert_eq!(profile_count(&fixture), 1);

    // A stale revision is refused before anything is recorded.
    let stale = service.settings_add_connection(wire(json!({
        "revision": recorded - 1,
        "name": "other",
        "kind": "ollama_cloud",
        "api_key": NEXT_KEY,
    })));
    assert_eq!(code(stale), "stale_revision");
    assert_eq!(profile_count(&fixture), 1);
}

/// A request type never prints the key.
#[test]
fn a_request_never_prints_its_key() {
    let request: ConnectionAddRequest = wire(json!({
        "revision": 1,
        "name": "cloud",
        "kind": "ollama_cloud",
        "api_key": KEY,
    }));
    assert!(!format!("{request:?}").contains(KEY));
    let set: omgcode_app::desktop::ApiKeySetRequest =
        wire(json!({"revision": 1, "name": "cloud", "api_key": KEY}));
    assert!(!format!("{set:?}").contains(KEY));
}

/// Setting a key stores the new item, binds it, and deletes the old one. It
/// refuses a local profile and an unknown name, and shows no dialog.
#[test]
fn setting_a_key_replaces_the_stored_item() {
    let fixture = Fixture::new();
    let service = service(&fixture);
    let mut vault = Vault::new(&fixture.data);
    add_cloud(&service, &fixture, "cloud", KEY).expect("add");
    let old = bound(&fixture, "cloud").expect("bound");
    vault.track(old.clone());

    let overview = set_key(&service, &fixture, "cloud", NEXT_KEY).expect("set");
    let new = bound(&fixture, "cloud").expect("still bound");
    vault.track(new.clone());
    assert_ne!(new, old);
    assert_eq!(native(&fixture, &new).as_deref(), Some(NEXT_KEY.as_bytes()));
    assert_eq!(native(&fixture, &old), None, "the replaced item is deleted");
    assert!(!text_of(&overview).contains(NEXT_KEY));
    assert_no_file_holds(&fixture.data, KEY);
    assert_no_file_holds(&fixture.data, NEXT_KEY);

    // A local profile, an unknown name, a bad key, and a stale revision.
    service
        .settings_add_connection(wire(json!({
            "revision": revision(&fixture),
            "name": "lms",
            "kind": "lm_studio",
            "endpoint": "http://127.0.0.1:1234/v1",
        })))
        .expect("local profile");
    let before = revision(&fixture);
    assert_eq!(
        code(set_key(&service, &fixture, "lms", KEY)),
        "invalid_request"
    );
    assert_eq!(code(set_key(&service, &fixture, "missing", KEY)), "control");
    assert_eq!(
        code(set_key(&service, &fixture, "cloud", "")),
        "invalid_request"
    );
    let stale = service.settings_set_api_key(wire(
        json!({"revision": before - 1, "name": "cloud", "api_key": KEY}),
    ));
    assert_eq!(code(stale), "stale_revision");
    assert_eq!(revision(&fixture), before);
    assert_eq!(bound(&fixture, "cloud"), Some(new));
    assert_eq!(bound(&fixture, "lms"), None);
}

/// A hosted `subscription` profile (kept for replay, ADR-0091) never takes a
/// key: the request is refused before the core is asked, records nothing, and
/// leaves the profile unbound, so it can never be chosen.
#[test]
fn a_subscription_profile_is_refused_a_key() {
    let fixture = Fixture::new();
    let service = service(&fixture);
    let vault = Vault::new(&fixture.data);
    vault.add_profile_at(
        "plan",
        "anthropic",
        "subscription",
        "https://api.anthropic.com/v1",
    );
    let before = revision(&fixture);
    let refused = set_key(&service, &fixture, "plan", KEY).expect_err("refused");
    assert_eq!(refused.code(), "invalid_request");
    assert!(refused.message().contains("uses an API key"), "{refused:?}");
    assert!(!text_of(&refused).contains(KEY));
    assert_eq!(revision(&fixture), before);
    assert_eq!(bound(&fixture, "plan"), None);
    assert_no_file_holds(&fixture.data, KEY);
}

/// Removing a profile with a stored key asks first, says the key is deleted,
/// and on Yes removes the profile and the native item. No keeps both.
#[test]
fn removing_a_bound_profile_asks_and_deletes_the_key() {
    let fixture = Fixture::new();
    let service = service(&fixture);
    let mut vault = Vault::new(&fixture.data);
    add_cloud(&service, &fixture, "cloud", KEY).expect("add");
    let id = bound(&fixture, "cloud").expect("bound");
    vault.track(id.clone());

    let before = revision(&fixture);
    let declined = Answers::with(&[false]);
    assert_eq!(
        code(remove(&service, &fixture, "cloud", &declined)),
        "cancelled"
    );
    let shown = declined.shown();
    assert_eq!(shown.len(), 1);
    assert!(
        shown[0].message.contains("stored API key"),
        "{}",
        shown[0].message
    );
    assert!(shown[0].message.contains("cloud"));
    assert!(!shown[0].message.contains(KEY));
    assert_eq!(revision(&fixture), before);
    assert_eq!(bound(&fixture, "cloud"), Some(id.clone()));
    assert_eq!(native(&fixture, &id).as_deref(), Some(KEY.as_bytes()));

    let accepted = Answers::with(&[true]);
    let overview = remove(&service, &fixture, "cloud", &accepted).expect("remove");
    assert_eq!(accepted.shown().len(), 1);
    assert!(overview.provider_profiles.is_empty());
    assert_eq!(native(&fixture, &id), None, "the native item is deleted");
    assert_eq!(profile_count(&fixture), 0);
}

/// Removing the active bound profile also says where new turns go.
#[test]
fn removing_the_active_cloud_profile_names_the_default() {
    let fixture = Fixture::new();
    let service = service(&fixture);
    let default = service.chat_defaults().settings.endpoint;
    let mut vault = Vault::new(&fixture.data);
    add_cloud(&service, &fixture, "cloud", KEY).expect("add");
    vault.track(bound(&fixture, "cloud").expect("bound"));
    service
        .settings_select_connection(
            wire(json!({"revision": revision(&fixture), "profile": "cloud"})),
            &Answers::with(&[true]),
        )
        .expect("select");

    let prompt = Answers::with(&[true]);
    let overview = remove(&service, &fixture, "cloud", &prompt).expect("remove");
    let shown = prompt.shown();
    assert_eq!(shown.len(), 1);
    let message = &shown[0].message;
    assert!(message.contains("stored API key"), "{message}");
    assert!(message.contains("default connection"), "{message}");
    assert!(message.contains(&default), "{message}");
    assert_eq!(overview.connection.profile, None);
}

/// A session that chose the profile refuses its next turn as stale once the
/// profile is removed, and nothing is sent.
#[test]
fn a_session_that_chose_a_removed_cloud_profile_becomes_stale() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let service = service(&fixture);
    let mut vault = Vault::new(&fixture.data);
    add_cloud(&service, &fixture, "cloud", KEY).expect("add");
    vault.track(bound(&fixture, "cloud").expect("bound"));
    let id: Uuid = service
        .session_create(wire(json!({"workspace_id": workspace.id, "title": "t"})))
        .expect("session")
        .session_id;
    let chosen = service
        .connection_select(
            wire(json!({"session_id": id, "profile": "cloud"})),
            &Answers::with(&[true]),
        )
        .expect("choose");
    assert!(!chosen.stale);

    remove(&service, &fixture, "cloud", &Answers::with(&[true])).expect("remove");
    let view = service
        .connection_status(wire(json!({"session_id": id})))
        .expect("status");
    assert!(view.stale);
    assert_eq!(view.origin, ConnectionOrigin::Session);
    let server = NoCallServer::start();
    let sent = service.chat_send(wire(json!({
        "session_id": id,
        "message": "hello",
        "settings": common::settings_json(&server.endpoint),
        "skills": [],
    })));
    assert_eq!(code(sent), "session_connection_stale");
    server.assert_untouched();
}

/// A data folder that grants another account access is reported with its own
/// code, and the operator can remove the grant after a native confirmation
/// that names it (ADR-0073). Declining changes nothing.
#[cfg(windows)]
#[test]
fn a_shared_data_folder_is_reported_and_repaired_only_after_confirmation() {
    // S-1-5-32-545 is the built-in Users group, named by SID so the test does
    // not depend on the display language.
    const USERS: &str = "S-1-5-32-545";
    let fixture = Fixture::new();
    let granted = std::process::Command::new("icacls")
        .arg(&fixture.data)
        .args(["/grant", &format!("*{USERS}:(OI)(CI)(RX)")])
        .output()
        .expect("icacls");
    assert!(granted.status.success(), "icacls fixture failed");
    let service = service(&fixture);
    let mut vault = Vault::new(&fixture.data);

    let refused = add_cloud(&service, &fixture, "cloud", KEY);
    assert_eq!(code(refused), "credential_storage_not_private");
    assert_eq!(profile_count(&fixture), 0, "nothing is recorded");

    let declined = Answers::with(&[false]);
    assert_eq!(
        code(service.settings_repair_data_folder(&declined)),
        "cancelled"
    );
    let shown = declined.shown();
    assert_eq!(shown.len(), 1);
    assert!(
        shown[0].message.contains(&format!("({USERS})")),
        "{}",
        shown[0].message
    );
    let still_refused = add_cloud(&service, &fixture, "cloud", KEY);
    assert_eq!(code(still_refused), "credential_storage_not_private");

    let confirmed = Answers::with(&[true]);
    service
        .settings_repair_data_folder(&confirmed)
        .expect("repair after confirmation");
    assert_eq!(confirmed.shown().len(), 1);
    add_cloud(&service, &fixture, "cloud", KEY).expect("add after repair");
    vault.track(bound(&fixture, "cloud").expect("a bound credential"));

    // Nothing is left to remove, so no dialog is shown.
    service
        .settings_repair_data_folder(&Answers::with(&[]))
        .expect("a private folder needs no repair");
}
