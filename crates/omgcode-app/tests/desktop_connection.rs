//! Choosing the model connection in the desktop `settings` window (ADR-0059
//! decision 7, ADR-0062): recording a local LM Studio or Ollama connection,
//! selecting it after a native confirmation, and the next chat turn using it.
//! A scripted prompt stands in for the native confirmation, and loopback
//! servers stand in for the model servers.

mod common;

use std::cell::RefCell;
use std::collections::VecDeque;

use omgcode_app::TokenSource;
use omgcode_app::desktop::{
    ChatState, ChatStatus, Confirmation, ConnectionSourceView, DesktopConfig, DesktopError,
    DesktopService, FolderPurpose, SettingsOverview, SettingsPrompt,
};
use omgcode_app::{ConnectionSource, resolve_local_connection, resolve_local_endpoint};
use omgcode_core::control::{ControlStore, ProviderKind};
use serde_json::json;
use uuid::Uuid;

use common::credential::Vault;
use common::{
    Fixture, MockServer, NoCallServer, Step, settings_json, text, text_only, unused_endpoint,
    wait_finished, wire,
};

/// Answers confirmations in order and records what each one said. A folder
/// dialog or a confirmation beyond the script fails the test.
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
    ) -> Option<std::path::PathBuf> {
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

/// A service started without a connection argument: the active connection
/// chosen in `settings` applies, and `default` while none is chosen.
fn active_service(fixture: &Fixture, default: &str) -> DesktopService {
    DesktopService::open(DesktopConfig {
        data_root: fixture.data.clone(),
        endpoint: default.to_owned(),
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
    name: &str,
    kind: &str,
    endpoint: &str,
) -> Result<SettingsOverview, DesktopError> {
    service.settings_add_local_connection(wire(json!({
        "revision": revision(fixture),
        "name": name,
        "kind": kind,
        "endpoint": endpoint,
    })))
}

fn select(
    service: &DesktopService,
    fixture: &Fixture,
    profile: Option<&str>,
    prompt: &Answers,
) -> Result<SettingsOverview, DesktopError> {
    service.settings_select_connection(
        wire(json!({ "revision": revision(fixture), "profile": profile })),
        prompt,
    )
}

fn send(
    service: &DesktopService,
    session: Uuid,
    endpoint: &str,
) -> Result<ChatStatus, DesktopError> {
    service.chat_send(wire(json!({
        "session_id": session,
        "message": "hello",
        "settings": settings_json(endpoint),
        "skills": [],
    })))
}

fn code<T>(result: Result<T, DesktopError>) -> &'static str {
    result.err().expect("refused").code()
}

/// The whole path the dropdown drives: an Ollama connection is recorded,
/// chosen after a confirmation that names it, refused for a window that
/// still shows the old endpoint, and then carries the next turn without any
/// token.
#[test]
fn an_ollama_connection_chosen_in_settings_carries_the_next_turn() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let default = NoCallServer::start();
    let ollama = MockServer::start(vec![Step::reply(text("from ollama"))]);
    let service = active_service(&fixture, &default.endpoint);

    let overview = service.settings_overview().expect("overview");
    assert_eq!(overview.connection.source, ConnectionSourceView::Active);
    assert!(overview.connection.selectable);
    assert_eq!(overview.connection.profile, None);
    assert_eq!(overview.connection.endpoint, default.endpoint);

    let overview = add(
        &service,
        &fixture,
        "ollama",
        "ollama_local",
        &ollama.endpoint,
    )
    .expect("record an Ollama connection");
    let profile = &overview.provider_profiles[0];
    assert_eq!(profile.kind, "ollama_local");
    assert_eq!(profile.auth, "none");
    assert!(profile.selectable);
    assert!(!profile.active);
    assert_eq!(
        service.chat_defaults().settings.endpoint,
        default.endpoint,
        "recording a connection changes nothing"
    );

    let prompt = Answers::with(&[true]);
    let overview = select(&service, &fixture, Some("ollama"), &prompt).expect("select");
    let shown = prompt.shown();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].title, "Choose the model connection");
    assert!(
        shown[0]
            .message
            .contains("to Ollama through the connection ollama")
    );
    assert!(
        shown[0].message.contains(&ollama.endpoint),
        "{}",
        shown[0].message
    );
    assert_eq!(overview.connection.profile.as_deref(), Some("ollama"));
    assert_eq!(overview.connection.kind, Some("ollama_local"));
    assert_eq!(overview.connection.endpoint, ollama.endpoint);
    assert!(overview.provider_profiles[0].active);

    let session = service
        .session_create(wire(json!({"workspace_id": workspace.id, "title": "t"})))
        .expect("session")
        .session_id;
    // A window that still shows the default endpoint is refused, unsent.
    assert_eq!(
        code(send(&service, session, &default.endpoint)),
        "connection_changed"
    );
    let endpoint = service.chat_defaults().settings.endpoint;
    assert_eq!(endpoint, ollama.endpoint);
    send(&service, session, &endpoint).expect("turn starts");
    let done = wait_finished(&service, session);
    assert!(matches!(done.state, ChatState::Completed), "{done:?}");

    let (served, recorded) = ollama.finish_recorded();
    assert_eq!(served, 1);
    assert!(recorded[0].head.starts_with("POST /v1/chat/completions "));
    assert_eq!(recorded[0].header("authorization"), None);
    default.assert_untouched();
}

/// A declined confirmation records nothing, and a connection fixed by a
/// launch argument cannot be chosen: no dialog is shown for it.
#[test]
fn a_declined_or_fixed_choice_changes_nothing() {
    let fixture = Fixture::new();
    let service = active_service(&fixture, &unused_endpoint());
    add(
        &service,
        &fixture,
        "ollama",
        "ollama_local",
        "http://127.0.0.1:11434/v1",
    )
    .expect("record");
    let before = revision(&fixture);
    let prompt = Answers::with(&[false]);
    assert_eq!(
        code(select(&service, &fixture, Some("ollama"), &prompt)),
        "cancelled"
    );
    assert_eq!(prompt.shown().len(), 1);
    assert_eq!(revision(&fixture), before);

    let fixed = DesktopService::open(DesktopConfig {
        data_root: fixture.data.clone(),
        endpoint: unused_endpoint(),
        token_source: TokenSource::Environment,
        stream: false,
    })
    .expect("fixed service");
    let prompt = Answers::with(&[]);
    assert_eq!(
        code(select(&fixed, &fixture, Some("ollama"), &prompt)),
        "connection_fixed"
    );
    let overview = fixed.settings_overview().expect("overview");
    assert_eq!(
        overview.connection.source,
        ConnectionSourceView::Environment
    );
    assert!(!overview.connection.selectable);
    assert_eq!(revision(&fixture), before);
}

/// Choices core would refuse are refused before any dialog: an unknown name,
/// a cloud profile, a profile without an endpoint, and the current choice.
#[test]
fn an_unusable_choice_is_refused_before_any_dialog() {
    let fixture = Fixture::new();
    let service = active_service(&fixture, &unused_endpoint());
    let vault = Vault::new(&fixture.data);
    vault.add_profile("cloud", "openai", "api_key");
    vault.add_profile("bare", "lm_studio", "none");
    let prompt = Answers::with(&[]);
    for name in [Some("missing"), Some("cloud"), Some("bare"), None] {
        assert_eq!(
            code(select(&service, &fixture, name, &prompt)),
            "control",
            "{name:?}"
        );
    }
    assert!(prompt.shown().is_empty());
}

/// Core validates what the window adds: a non-loopback endpoint, a bad name,
/// and an unknown kind are refused and record nothing.
#[test]
fn an_invalid_connection_is_refused_by_core_or_the_wire() {
    let fixture = Fixture::new();
    let service = active_service(&fixture, &unused_endpoint());
    let before = revision(&fixture);
    for (name, endpoint) in [
        ("remote", "http://192.0.2.1:11434/v1"),
        ("named", "http://localhost:11434/v1"),
        ("bad name", "http://127.0.0.1:11434/v1"),
    ] {
        assert_eq!(
            code(add(&service, &fixture, name, "ollama_local", endpoint)),
            "control",
            "{name}"
        );
    }
    assert_eq!(revision(&fixture), before);
    let unknown_kind = serde_json::from_value::<omgcode_app::desktop::LocalConnectionAddRequest>(
        json!({"revision": before, "name": "x", "kind": "openai", "endpoint": "http://127.0.0.1:1/v1"}),
    );
    assert!(
        unknown_kind.is_err(),
        "only the two local kinds can be added"
    );
}

/// Removing the active connection returns to the default endpoint. A profile
/// with a stored credential is left to the trusted CLI.
#[test]
fn removing_the_active_connection_returns_to_the_default() {
    let fixture = Fixture::new();
    let default = unused_endpoint();
    let service = active_service(&fixture, &default);
    add(
        &service,
        &fixture,
        "lms",
        "lm_studio",
        "http://127.0.0.1:1234/v1",
    )
    .expect("record");
    select(&service, &fixture, Some("lms"), &Answers::with(&[true])).expect("select");
    let overview = service
        .settings_remove_connection(wire(json!({
            "revision": revision(&fixture),
            "name": "lms",
        })))
        .expect("remove");
    assert!(overview.provider_profiles.is_empty());
    assert_eq!(overview.connection.profile, None);
    assert_eq!(overview.connection.endpoint, default);
    assert_eq!(service.chat_defaults().settings.endpoint, default);

    let mut vault = Vault::new(&fixture.data);
    vault.add_profile_at("keyed", "lm_studio", "api_key", "http://127.0.0.1:1234/v1");
    vault.bind("keyed", b"fake-connection-token-41c9");
    assert_eq!(
        code(service.settings_remove_connection(wire(json!({
            "revision": revision(&fixture),
            "name": "keyed",
        })))),
        "credential_bound"
    );
    assert!(
        ControlStore::open(&fixture.data)
            .expect("store")
            .provider_profile("keyed")
            .is_some()
    );
}

/// The active source resolves the default endpoint with no settings store, a
/// selected Ollama profile with no token, and reads the selection again for
/// each call.
#[test]
fn the_active_source_resolves_the_selection_each_time() {
    let fixture = Fixture::bare();
    let source = ConnectionSource::Active {
        default_endpoint: "http://127.0.0.1:1234/v1".to_owned(),
    };
    let endpoint = resolve_local_endpoint(&fixture.data, &source).expect("no store");
    assert_eq!(endpoint.endpoint, "http://127.0.0.1:1234/v1");
    assert_eq!(endpoint.profile, None);

    let mut control = ControlStore::open(&fixture.data).expect("store");
    control
        .add_provider_profile(
            &omgcode_core::control::ProviderProfileDeclaration {
                name: "ollama".into(),
                kind: "ollama_local".into(),
                auth: "none".into(),
                endpoint: Some("http://127.0.0.1:11434/v1".into()),
            },
            0,
        )
        .expect("profile");
    control
        .select_active_provider(Some("ollama"), 1)
        .expect("select");
    drop(control);

    let endpoint = resolve_local_endpoint(&fixture.data, &source).expect("selected");
    assert_eq!(endpoint.endpoint, "http://127.0.0.1:11434/v1");
    assert_eq!(
        endpoint.profile,
        Some(("ollama".to_owned(), ProviderKind::OllamaLocal))
    );
    let connection = resolve_local_connection(&fixture.data, &source).expect("connection");
    assert_eq!(connection.endpoint, "http://127.0.0.1:11434/v1");
    assert!(connection.token.is_none());
    // The CLI's and the desktop's `--provider-profile` accept an Ollama
    // profile too.
    let named = resolve_local_connection(
        &fixture.data,
        &ConnectionSource::Profile("ollama".to_owned()),
    )
    .expect("named profile");
    assert_eq!(named.endpoint, "http://127.0.0.1:11434/v1");
}
