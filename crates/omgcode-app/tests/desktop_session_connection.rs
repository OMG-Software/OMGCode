//! The provider and model chosen per session (ADR-0071 decisions 6 to 11): the
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
    DesktopError, DesktopService, FolderPurpose, ReasoningEffort, SessionEffortSelectRequest,
    SettingsPrompt,
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
        claude_code: omgcode_app::desktop::ClaudeCodeHost::default(),
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
    prompt: &dyn SettingsPrompt,
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

fn effort(
    service: &DesktopService,
    session: Uuid,
    effort: Option<&str>,
) -> Result<ConnectionStatus, DesktopError> {
    service.session_effort_select(wire(
        json!({ "session_id": session, "reasoning_effort": effort }),
    ))
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
    // The composer sends the model the status names for the session.
    let mut settings = settings_json(&ollama.endpoint);
    settings["model"] = json!(chosen.model);
    service
        .chat_send(wire(json!({
            "session_id": first,
            "message": "hello",
            "settings": settings,
            "skills": [],
        })))
        .expect("turn starts");
    let done = wait_finished(&service, first);
    assert!(matches!(done.state, ChatState::Completed), "{done:?}");
    let (served, recorded) = ollama.finish_recorded();
    assert_eq!(served, 2);
    assert!(recorded[0].head.starts_with("GET /v1/models "));
    assert!(recorded[1].head.starts_with("POST /v1/chat/completions "));
    let request: serde_json::Value =
        serde_json::from_str(&recorded[1].body).expect("chat request body");
    assert_eq!(request["model"], "lfm2.5:latest");
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

/// Runs `change` while the dialog is open, as the trusted CLI could, then
/// answers Yes.
struct ChangedMeanwhile<F: Fn()>(F);

impl<F: Fn()> SettingsPrompt for ChangedMeanwhile<F> {
    fn pick_folder(
        &self,
        _purpose: FolderPurpose,
        _start: Option<&std::path::Path>,
    ) -> Option<PathBuf> {
        panic!("no folder dialog is expected");
    }

    fn confirm(&self, _confirmation: &Confirmation) -> bool {
        (self.0)();
        true
    }
}

/// A Yes holds only for what the dialog named. A profile replaced during the
/// dialog under the same name and endpoint by another provider kind records
/// nothing, although the recorded choice (name and endpoint) is unchanged.
#[test]
fn a_profile_replaced_by_another_kind_during_the_dialog_records_nothing() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let server = NoCallServer::start();
    let service = active(&fixture, &unused_endpoint());
    let vault = Vault::new(&fixture.data);
    vault.add_profile_at("local", "lm_studio", "none", &server.endpoint);
    let id = session(&service, workspace.id);

    let prompt = ChangedMeanwhile(|| {
        vault.remove_profile("local");
        vault.add_profile_at("local", "ollama_local", "none", &server.endpoint);
    });
    let refused = service.connection_select(
        wire(json!({ "session_id": id, "profile": "local" })),
        &prompt,
    );
    assert_eq!(code(refused), "stale_revision");
    assert!(!record_file(&fixture).exists());
    assert_eq!(status(&service, Some(id)).origin, ConnectionOrigin::Default);

    // Chosen again, the dialog names the provider that is recorded now.
    let prompt = Answers::with(&[true]);
    choose(&service, id, Some("local"), &prompt).expect("choose again");
    let shown = prompt.shown();
    assert_eq!(shown.len(), 1);
    assert!(shown[0].message.contains("Ollama"), "{}", shown[0].message);
    server.assert_untouched();
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

/// A named hosted provider and an operator-typed OpenAI-compatible service are
/// offered once their keys are bound. Each dialog names the provider and the
/// host that receives the turn, and the typed address is called out as typed
/// (ADR-0092). Nothing is sent: choosing records the session's choice only.
#[test]
fn choosing_a_hosted_provider_names_its_host_and_flags_a_typed_address() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let service = active(&fixture, &unused_endpoint());
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile_at("deep", "deepseek", "api_key", "https://api.deepseek.com");
    vault.add_profile_at(
        "custom",
        "openai_compatible",
        "api_key",
        "https://provider-api.example.org/v1",
    );
    let id = session(&service, workspace.id);
    assert!(status(&service, Some(id)).choices.is_empty());
    vault.bind("deep", b"fake-deepseek-key-for-the-test");
    vault.bind("custom", b"fake-custom-key-for-the-test");
    let view = status(&service, Some(id));
    let kinds: Vec<&str> = view.choices.iter().map(|choice| choice.kind).collect();
    assert_eq!(kinds, ["deepseek", "openai_compatible"]);

    let prompt = Answers::with(&[true]);
    let chosen = choose(&service, id, Some("deep"), &prompt).expect("choose deepseek");
    let message = &prompt.shown()[0].message;
    assert!(
        message.contains("to DeepSeek through the connection deep"),
        "{message}"
    );
    assert!(
        message.contains("leave this computer for api.deepseek.com."),
        "{message}"
    );
    assert!(!message.contains("typed"), "{message}");
    assert!(!message.contains("fake-deepseek-key"));
    assert_eq!(chosen.connection.kind, Some("deepseek"));

    let prompt = Answers::with(&[true]);
    let chosen = choose(&service, id, Some("custom"), &prompt).expect("choose custom");
    let message = &prompt.shown()[0].message;
    assert!(
        message.contains("to an OpenAI-compatible service through the connection custom"),
        "{message}"
    );
    assert!(
        message.contains("https://provider-api.example.org/v1"),
        "{message}"
    );
    assert!(
        message.contains("leave this computer for provider-api.example.org."),
        "{message}"
    );
    assert!(message.contains("This address was typed"), "{message}");
    assert!(!message.contains("fake-custom-key"));
    assert_eq!(chosen.connection.kind, Some("openai_compatible"));
    assert_eq!(
        chosen.connection.endpoint,
        "https://provider-api.example.org/v1"
    );
}

/// A typed address with a non-default port is named with that port, as the
/// settings window shows it: `host:8443` can be a different service from
/// `host`, and the dialog must name the one that receives the key (ADR-0092).
#[test]
fn choosing_a_typed_address_with_a_port_names_the_port() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let service = active(&fixture, &unused_endpoint());
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile_at(
        "custom",
        "openai_compatible",
        "api_key",
        "https://llm.internal.example:8443/v1",
    );
    vault.bind("custom", b"fake-custom-key-for-the-test");
    let id = session(&service, workspace.id);

    let prompt = Answers::with(&[true]);
    choose(&service, id, Some("custom"), &prompt).expect("choose custom");
    let message = &prompt.shown()[0].message;
    assert!(
        message.contains("leave this computer for llm.internal.example:8443."),
        "{message}"
    );
    assert!(!message.contains("fake-custom-key"));
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

/// An effort is recorded per session without a dialog, kept by a model
/// choice, omitted from the file when none is chosen, and sent with the
/// session's turn (ADR-0102 decisions 7 and 8).
#[test]
fn an_effort_is_recorded_per_session_and_sent_with_its_turn() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let default = MockServer::start(vec![Step::reply(text("thought hard"))]);
    let service = active(&fixture, &default.endpoint);
    let id = session(&service, workspace.id);
    let other = session(&service, workspace.id);

    // A loopback connection takes every effort; none is chosen yet.
    let before = status(&service, Some(id));
    assert_eq!(before.reasoning_effort, None);
    assert_eq!(before.reasoning_efforts, ReasoningEffort::ALL);

    let chosen = effort(&service, id, Some("high")).expect("effort");
    assert_eq!(chosen.reasoning_effort, Some(ReasoningEffort::High));
    assert_eq!(chosen.origin, ConnectionOrigin::Default);
    assert_eq!(chosen.model, "", "an effort leaves the model as it was");
    let on_disk = fs::read_to_string(record_file(&fixture)).expect("record file");
    assert!(
        on_disk.contains("\"reasoning_effort\": \"high\""),
        "{on_disk}"
    );
    assert_eq!(status(&service, Some(other)).reasoning_effort, None);

    // A model choice keeps the effort; clearing it removes the field.
    let kept = model(&service, id, "fixture").expect("model");
    assert_eq!(kept.reasoning_effort, Some(ReasoningEffort::High));
    assert_eq!(
        effort(&service, id, Some("xhigh"))
            .expect("change")
            .reasoning_effort,
        Some(ReasoningEffort::XHigh)
    );

    // Only the seven names, and no other field, are accepted.
    for request in [
        json!({ "session_id": id, "reasoning_effort": "extreme" }),
        json!({ "session_id": id, "reasoning_effort": "High" }),
        json!({ "session_id": id, "reasoning_effort": "low", "endpoint": "x" }),
    ] {
        assert!(
            serde_json::from_value::<SessionEffortSelectRequest>(request.clone()).is_err(),
            "{request}"
        );
    }
    assert_eq!(
        code(effort(&service, Uuid::new_v4(), Some("low"))),
        "not_found"
    );

    // The turn sends the effort the composer names.
    let mut settings = settings_json(&default.endpoint);
    settings["reasoning_effort"] = json!("xhigh");
    service
        .chat_send(wire(json!({
            "session_id": id,
            "message": "hello",
            "settings": settings,
            "skills": [],
        })))
        .expect("turn starts");
    let done = wait_finished(&service, id);
    assert!(matches!(done.state, ChatState::Completed), "{done:?}");
    let (served, recorded) = default.finish_recorded();
    assert_eq!(served, 1);
    let request: serde_json::Value =
        serde_json::from_str(&recorded[0].body).expect("chat request body");
    assert_eq!(request["reasoning_effort"], "xhigh");

    let cleared = effort(&service, id, None).expect("clear");
    assert_eq!(cleared.reasoning_effort, None);
    let on_disk = fs::read_to_string(record_file(&fixture)).expect("record file");
    assert!(!on_disk.contains("reasoning_effort"), "{on_disk}");
}

/// A connection whose provider documents no effort offers none and refuses
/// one, on save and on send; choosing a provider clears the session's effort
/// (ADR-0102 decisions 2, 7, and 8).
#[test]
fn a_connection_without_effort_refuses_one_and_a_provider_change_clears_it() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let default = NoCallServer::start();
    let service = active(&fixture, &default.endpoint);
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile_at("deep", "deepseek", "api_key", "https://api.deepseek.com");
    vault.bind("deep", b"fake-deepseek-key-for-the-test");
    let id = session(&service, workspace.id);
    effort(&service, id, Some("medium")).expect("effort on the default");

    let prompt = Answers::with(&[true]);
    let chosen = choose(&service, id, Some("deep"), &prompt).expect("choose deepseek");
    assert_eq!(chosen.reasoning_effort, None, "a provider choice clears it");
    assert!(chosen.reasoning_efforts.is_empty());
    assert_eq!(code(effort(&service, id, Some("low"))), "invalid_request");
    assert_eq!(
        effort(&service, id, None).expect("none").reasoning_effort,
        None
    );

    // The send is refused before the turn starts; nothing reaches the network.
    let mut settings = settings_json(&chosen.connection.endpoint);
    settings["reasoning_effort"] = json!("low");
    let refused = service.chat_send(wire(json!({
        "session_id": id,
        "message": "hello",
        "settings": settings,
        "skills": [],
    })));
    assert_eq!(code(refused), "invalid_request");
    assert_eq!(
        status(&service, Some(id)).reasoning_effort,
        None,
        "a refused send records nothing"
    );
    default.assert_untouched();
}

/// Answers Yes, but first tries an effort choice for the same session while
/// the provider confirmation is open, and keeps what that returned.
struct EffortDuringConfirmation<'a> {
    service: &'a DesktopService,
    session: Uuid,
    refused: RefCell<Option<&'static str>>,
}

impl SettingsPrompt for EffortDuringConfirmation<'_> {
    fn pick_folder(
        &self,
        _purpose: FolderPurpose,
        _start: Option<&std::path::Path>,
    ) -> Option<PathBuf> {
        panic!("no folder dialog is expected");
    }

    fn confirm(&self, _confirmation: &Confirmation) -> bool {
        let attempt = effort(self.service, self.session, Some("high"));
        self.refused.replace(Some(code(attempt)));
        true
    }
}

/// An effort choice runs as one settings change, as a provider choice does,
/// so a provider choice cannot land between its check and its save: while a
/// confirmation is open it is refused with `settings_change_running`, and it
/// works again once the provider choice ends (ADR-0102).
#[test]
fn an_effort_choice_is_refused_while_a_provider_choice_runs() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let default = NoCallServer::start();
    let service = active(&fixture, &default.endpoint);
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile_at("deep", "deepseek", "api_key", "https://api.deepseek.com");
    vault.bind("deep", b"fake-deepseek-key-for-the-test");
    let id = session(&service, workspace.id);

    let prompt = EffortDuringConfirmation {
        service: &service,
        session: id,
        refused: RefCell::new(None),
    };
    let chosen = choose(&service, id, Some("deep"), &prompt).expect("choose deepseek");
    assert_eq!(prompt.refused.take(), Some("settings_change_running"));
    assert_eq!(chosen.reasoning_effort, None);
    assert!(chosen.reasoning_efforts.is_empty());
    // The guard is released: the next choice is checked against DeepSeek.
    assert_eq!(code(effort(&service, id, Some("high"))), "invalid_request");
    assert_eq!(status(&service, Some(id)).reasoning_effort, None);
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
