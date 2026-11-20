//! The provider and model chosen per session (ADR-0069 decisions 6 to 11): the
//! recorded choice, its native confirmation, its re-check on every turn, and
//! the per-session model list. A scripted prompt stands in for the native
//! confirmation, and loopback servers stand in for the model servers.

mod common;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;

use omgcode_app::TokenSource;
use omgcode_app::desktop::{
    ChatState, ChatStatus, Confirmation, ConnectionOrigin, ConnectionStatus, DesktopConfig,
    DesktopError, DesktopService, FolderPurpose, SettingsPrompt,
};
use omgcode_core::control::ControlStore;
use serde_json::json;
use uuid::Uuid;

use common::credential::Vault;
use common::{
    Fixture, MockServer, NoCallServer, Step, settings_json, text, text_only, unused_endpoint,
    wait_finished, wire,
};

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

fn open(fixture: &Fixture, default: &str, source: TokenSource) -> DesktopService {
    DesktopService::open(DesktopConfig {
        data_root: fixture.data.clone(),
        endpoint: default.to_owned(),
        token_source: source,
        stream: false,
    })
    .expect("open desktop service")
}

fn active(fixture: &Fixture, default: &str) -> DesktopService {
    open(fixture, default, TokenSource::Active)
}

fn session(service: &DesktopService, fixture_workspace: Uuid) -> Uuid {
    service
        .session_create(wire(
            json!({"workspace_id": fixture_workspace, "title": "t"}),
        ))
        .expect("session")
        .session_id
}

fn status(service: &DesktopService, session: Option<Uuid>) -> ConnectionStatus {
    service
        .connection_status(wire(json!({ "session_id": session })))
        .expect("status")
}

fn choose(
    service: &DesktopService,
    session: Uuid,
    profile: Option<&str>,
    prompt: &Answers,
) -> Result<ConnectionStatus, DesktopError> {
    service.connection_select(
        wire(json!({ "session_id": session, "profile": profile })),
        prompt,
    )
}

fn model(
    service: &DesktopService,
    session: Uuid,
    model: &str,
) -> Result<ConnectionStatus, DesktopError> {
    service.session_model_select(wire(json!({ "session_id": session, "model": model })))
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

fn record_file(fixture: &Fixture) -> PathBuf {
    fixture
        .data
        .join("desktop-v1")
        .join("session-connections.json")
}

fn add_local(fixture: &Fixture, name: &str, kind: &str, endpoint: &str) {
    Vault::new(&fixture.data).add_profile_at(name, kind, "none", endpoint);
}

fn model_list(ids: &[&str]) -> serde_json::Value {
    let data: Vec<_> = ids.iter().map(|id| json!({ "id": id })).collect();
    json!({ "object": "list", "data": data })
}

/// The whole path: a choice is confirmed, recorded for one session only, clears
/// the model, and carries that session's next turn and model list, while
/// another session keeps following the default.
#[test]
fn a_session_choice_carries_that_sessions_turns_and_model_list() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let default = NoCallServer::start();
    let ollama = MockServer::start(vec![
        Step::reply(model_list(&["lfm2.5:latest", "kimi-k3:cloud"])),
        Step::reply(text("from ollama")),
    ]);
    let service = active(&fixture, &default.endpoint);
    add_local(&fixture, "ollama", "ollama_local", &ollama.endpoint);
    let first = session(&service, workspace.id);
    let second = session(&service, workspace.id);

    // Before any choice a session follows the default, and the saved model
    // shows nothing.
    let before = status(&service, Some(first));
    assert_eq!(before.origin, ConnectionOrigin::Default);
    assert_eq!(before.connection.endpoint, default.endpoint);
    assert_eq!(before.model, "");
    assert!(!before.stale);
    assert!(!record_file(&fixture).exists());

    // A model alone records nothing about the provider.
    let chosen = model(&service, first, "pre-chosen").expect("model only");
    assert_eq!(chosen.origin, ConnectionOrigin::Default);
    assert_eq!(chosen.model, "pre-chosen");

    // Yes records the provider and clears the model.
    let prompt = Answers::with(&[true]);
    let chosen = choose(&service, first, Some("ollama"), &prompt).expect("choose");
    let shown = prompt.shown();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].title, "Choose the model connection");
    assert!(shown[0].message.contains("this session's chat turns"));
    assert!(
        shown[0]
            .message
            .contains("to Ollama through the connection ollama")
    );
    assert!(shown[0].message.contains(&ollama.endpoint));
    assert!(!shown[0].message.contains("ollama.com"));
    assert_eq!(chosen.origin, ConnectionOrigin::Session);
    assert_eq!(chosen.connection.profile.as_deref(), Some("ollama"));
    assert_eq!(chosen.connection.kind, Some("ollama_local"));
    assert_eq!(chosen.model, "", "the model belongs to one server");
    let active_names: Vec<&str> = chosen
        .choices
        .iter()
        .filter(|choice| choice.active)
        .map(|choice| choice.name.as_str())
        .collect();
    assert_eq!(active_names, ["ollama"]);
    let text_on_disk = fs::read_to_string(record_file(&fixture)).expect("record file");
    assert!(text_on_disk.contains("\"version\": 1"));
    assert!(text_on_disk.contains(&ollama.endpoint));

    // The other session, and the default, are untouched.
    let other = status(&service, Some(second));
    assert_eq!(other.origin, ConnectionOrigin::Default);
    assert_eq!(other.connection.endpoint, default.endpoint);
    assert_eq!(status(&service, None).connection.endpoint, default.endpoint);

    // The list is that session's; a model is recorded without a dialog.
    let listed = service
        .chat_models(wire(json!({ "session_id": first })))
        .expect("list");
    assert_eq!(listed.session_id, first);
    assert_eq!(listed.endpoint, ollama.endpoint);
    assert_eq!(listed.kind, Some("ollama_local"));
    assert_eq!(listed.models.len(), 2);
    let chosen = model(&service, first, "lfm2.5:latest").expect("model");
    assert_eq!(chosen.model, "lfm2.5:latest");
    assert_eq!(chosen.connection.profile.as_deref(), Some("ollama"));

    // The session's turn goes to its own server; an old endpoint is refused.
    assert_eq!(
        code(send(&service, first, &default.endpoint)),
        "connection_changed"
    );
    send(&service, first, &ollama.endpoint).expect("turn starts");
    let done = wait_finished(&service, first);
    assert!(matches!(done.state, ChatState::Completed), "{done:?}");
    let (served, recorded) = ollama.finish_recorded();
    assert_eq!(served, 2);
    assert!(recorded[0].head.starts_with("GET /v1/models "));
    assert!(recorded[1].head.starts_with("POST /v1/chat/completions "));
    default.assert_untouched();
}

/// A launch argument wins for every session, whatever the file holds.
#[test]
fn a_launch_argument_wins_over_a_recorded_session_choice() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let launch = MockServer::start(vec![Step::reply(model_list(&["only-here"]))]);
    let ollama = NoCallServer::start();
    let service = active(&fixture, &unused_endpoint());
    add_local(&fixture, "ollama", "ollama_local", &ollama.endpoint);
    let id = session(&service, workspace.id);
    choose(&service, id, Some("ollama"), &Answers::with(&[true])).expect("choose");
    model(&service, id, "kept").expect("model");

    let fixed = open(&fixture, &launch.endpoint, TokenSource::Environment);
    let view = status(&fixed, Some(id));
    assert_eq!(view.origin, ConnectionOrigin::Launch);
    assert_eq!(view.connection.endpoint, launch.endpoint);
    assert!(!view.connection.selectable);
    assert!(view.choices.is_empty());
    assert_eq!(view.model, "kept", "a recorded model still shows");
    let listed = fixed
        .chat_models(wire(json!({ "session_id": id })))
        .expect("list");
    assert_eq!(listed.endpoint, launch.endpoint);
    assert_eq!(listed.kind, None);
    let prompt = Answers::with(&[]);
    assert_eq!(
        code(choose(&fixed, id, Some("ollama"), &prompt)),
        "connection_fixed"
    );
    assert!(prompt.shown().is_empty());
    let (served, _) = launch.finish_recorded();
    assert_eq!(served, 1);
    ollama.assert_untouched();
}

/// A recorded profile that is removed, or recorded again with another endpoint,
/// refuses the turn and the list until the operator chooses again.
#[test]
fn a_removed_or_changed_profile_makes_the_choice_stale() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let first = NoCallServer::start();
    let second = NoCallServer::start();
    let service = active(&fixture, &unused_endpoint());
    let vault = Vault::new(&fixture.data);
    vault.add_profile_at("ollama", "ollama_local", "none", &first.endpoint);
    let id = session(&service, workspace.id);
    choose(&service, id, Some("ollama"), &Answers::with(&[true])).expect("choose");
    assert!(!status(&service, Some(id)).stale);

    vault.remove_profile("ollama");
    let view = status(&service, Some(id));
    assert!(view.stale);
    assert_eq!(view.origin, ConnectionOrigin::Session);
    assert_eq!(view.connection.profile.as_deref(), Some("ollama"));
    assert_eq!(
        code(send(&service, id, &first.endpoint)),
        "session_connection_stale"
    );
    assert_eq!(
        code(service.chat_models(wire(json!({ "session_id": id })))),
        "session_connection_stale"
    );

    // Added again with another endpoint: the same name does not carry over.
    vault.add_profile_at("ollama", "ollama_local", "none", &second.endpoint);
    let view = status(&service, Some(id));
    assert!(view.stale);
    assert!(view.choices.iter().all(|choice| !choice.active));
    assert_eq!(
        code(send(&service, id, &second.endpoint)),
        "session_connection_stale"
    );

    // Choosing it again is a change, confirmed again, and works.
    let prompt = Answers::with(&[true]);
    let fresh = choose(&service, id, Some("ollama"), &prompt).expect("choose again");
    assert_eq!(prompt.shown().len(), 1);
    assert!(!fresh.stale);
    assert_eq!(fresh.connection.endpoint, second.endpoint);
    first.assert_untouched();
    second.assert_untouched();
}

/// Every refusal comes before a dialog, and a declined dialog records nothing.
#[test]
fn a_refused_or_declined_choice_records_nothing() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let service = active(&fixture, &unused_endpoint());
    let vault = Vault::new(&fixture.data);
    vault.add_profile_at(
        "ollama",
        "ollama_local",
        "none",
        "http://127.0.0.1:11434/v1",
    );
    vault.add_profile("bare", "lm_studio", "none");
    vault.add_profile("hosted", "openai", "api_key");
    vault.add_profile_at(
        "unkeyed-cloud",
        "ollama_cloud",
        "api_key",
        "https://ollama.com/v1",
    );
    let id = session(&service, workspace.id);

    // Unselectable, unknown, an unchanged choice (the default), and an
    // unknown session are refused without a dialog.
    let prompt = Answers::with(&[]);
    for name in [
        Some("bare"),
        Some("hosted"),
        Some("unkeyed-cloud"),
        Some("missing"),
        None,
    ] {
        assert_eq!(
            code(choose(&service, id, name, &prompt)),
            "control",
            "{name:?}"
        );
    }
    assert_eq!(
        code(choose(&service, Uuid::new_v4(), Some("ollama"), &prompt)),
        "not_found"
    );
    assert!(prompt.shown().is_empty());
    assert!(!record_file(&fixture).exists());

    // No leaves the file absent and the view unchanged.
    let prompt = Answers::with(&[false]);
    assert_eq!(
        code(choose(&service, id, Some("ollama"), &prompt)),
        "cancelled"
    );
    assert_eq!(prompt.shown().len(), 1);
    assert!(!record_file(&fixture).exists());
    assert_eq!(status(&service, Some(id)).origin, ConnectionOrigin::Default);

    // Choosing it twice is refused the second time, with no dialog; the
    // unselectable profiles are not offered.
    choose(&service, id, Some("ollama"), &Answers::with(&[true])).expect("choose");
    let prompt = Answers::with(&[]);
    assert_eq!(
        code(choose(&service, id, Some("ollama"), &prompt)),
        "control"
    );
    assert!(prompt.shown().is_empty());
    let names: Vec<String> = status(&service, Some(id))
        .choices
        .into_iter()
        .map(|choice| choice.name)
        .collect();
    assert_eq!(names, ["ollama"]);

    // The request names a session and a profile and nothing else.
    for bad in [
        json!({ "session_id": id, "profile": "ollama", "endpoint": "http://evil" }),
        json!({ "profile": "ollama" }),
    ] {
        assert!(
            serde_json::from_value::<omgcode_app::desktop::ConnectionChooseRequest>(bad).is_err()
        );
    }
}

/// An Ollama Cloud profile is offered once its key is bound, and its dialog says
/// that prompts and tool output leave this computer.
#[test]
fn choosing_ollama_cloud_names_the_hosted_service_in_the_dialog() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let service = active(&fixture, &unused_endpoint());
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile_at("cloud", "ollama_cloud", "api_key", "https://ollama.com/v1");
    let id = session(&service, workspace.id);
    assert!(status(&service, Some(id)).choices.is_empty());
    vault.bind("cloud", b"fake-cloud-key-for-the-test");

    let view = status(&service, Some(id));
    assert_eq!(view.choices.len(), 1);
    assert_eq!(view.choices[0].kind, "ollama_cloud");
    let prompt = Answers::with(&[true]);
    let chosen = choose(&service, id, Some("cloud"), &prompt).expect("choose");
    let shown = prompt.shown();
    assert_eq!(shown.len(), 1);
    let message = &shown[0].message;
    assert!(message.contains("to Ollama Cloud through the connection cloud"));
    assert!(message.contains("https://ollama.com/v1"));
    assert!(
        message.contains("leave this computer for ollama.com"),
        "{message}"
    );
    assert!(message.contains("Its stored credential is sent with each request."));
    assert!(!message.contains("fake-cloud-key"));
    assert_eq!(chosen.connection.kind, Some("ollama_cloud"));
    assert_eq!(chosen.connection.endpoint, "https://ollama.com/v1");
    assert!(
        !fs::read_to_string(record_file(&fixture))
            .expect("record file")
            .contains("fake-cloud-key")
    );
}

/// A model name passes the rule `chat_send` applies, and a model alone keeps
/// the session following the default connection.
#[test]
fn a_model_is_validated_and_never_pins_a_provider() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let default = NoCallServer::start();
    let service = active(&fixture, &default.endpoint);
    let id = session(&service, workspace.id);

    for bad in ["a\nb", "tab\there", &"m".repeat(257)] {
        assert_eq!(code(model(&service, id, bad)), "invalid_request", "{bad:?}");
    }
    assert_eq!(code(model(&service, Uuid::new_v4(), "m")), "not_found");
    assert!(!record_file(&fixture).exists());

    let chosen = model(&service, id, "google/gemma-4-e4b").expect("model");
    assert_eq!(chosen.model, "google/gemma-4-e4b");
    assert_eq!(chosen.origin, ConnectionOrigin::Default);
    assert_eq!(model(&service, id, "").expect("clear").model, "");
    assert!(
        serde_json::from_value::<omgcode_app::desktop::SessionModelSelectRequest>(
            json!({ "session_id": id, "model": "m", "endpoint": "x" })
        )
        .is_err()
    );

    // The session follows the global selection made later in `settings`.
    let other = MockServer::start(vec![Step::reply(model_list(&["m"]))]);
    add_local(&fixture, "lms", "lm_studio", &other.endpoint);
    let mut control = ControlStore::open(&fixture.data).expect("store");
    let revision = control.revision();
    control
        .select_active_provider(Some("lms"), revision)
        .expect("global selection");
    drop(control);
    let followed = status(&service, Some(id));
    assert_eq!(followed.connection.endpoint, other.endpoint);
    let listed = service
        .chat_models(wire(json!({ "session_id": id })))
        .expect("list");
    assert_eq!(listed.endpoint, other.endpoint);
    assert_eq!(listed.kind, Some("lm_studio"));
    other.finish();
    default.assert_untouched();
}

/// An unreadable file is reported, treated as empty, and replaced by the next
/// recorded choice; chat is never blocked by it.
#[test]
fn a_damaged_file_is_reported_and_never_blocks_chat() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let default = MockServer::start(vec![Step::reply(text("fine"))]);
    let service = active(&fixture, &default.endpoint);
    let id = session(&service, workspace.id);
    let path = record_file(&fixture);
    fs::create_dir_all(path.parent().expect("directory")).expect("directory");
    fs::write(&path, "{ not json").expect("damage");

    let view = status(&service, Some(id));
    assert!(view.session_choices_unreadable);
    assert_eq!(view.origin, ConnectionOrigin::Default);
    send(&service, id, &default.endpoint).expect("turn starts");
    let done = wait_finished(&service, id);
    assert!(matches!(done.state, ChatState::Completed), "{done:?}");

    let repaired = model(&service, id, "m").expect("replaces the file");
    assert!(!repaired.session_choices_unreadable);
    assert!(fs::read_to_string(&path).expect("file").contains("\"m\""));
    let (served, _) = default.finish_recorded();
    assert_eq!(served, 1);
}
