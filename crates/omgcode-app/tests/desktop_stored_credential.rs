//! The desktop service reads the local adapter's token from a stored provider
//! credential for each turn (ADR-0045, ADR-0047), against the real settings and
//! native credential stores. On Linux these need an unlocked Secret Service.
//! Test tokens are obviously fake, and a `Vault` deletes only what it stored.

use omgcode_app::TokenSource;
use omgcode_app::desktop::{ChatState, ChatStatus, DesktopConfig, DesktopService};
use serde_json::json;
use uuid::Uuid;

mod common;
use common::credential::Vault;
use common::{
    Fixture, MockServer, NoCallServer, Step, session_request, settings_json, text, text_of,
    text_only, unused_endpoint, wait_finished, wire,
};

const STORED: &str = "fake-desktop-token-27ad90e1";
const ROTATED: &str = "fake-desktop-rotated-6e0c5b34";

fn service(fixture: &Fixture, endpoint: &str, profile: &str) -> DesktopService {
    DesktopService::open(DesktopConfig {
        data_root: fixture.data.clone(),
        endpoint: endpoint.to_owned(),
        token_source: TokenSource::Profile(profile.to_owned()),
        // The scripted model server answers with plain JSON.
        stream: false,
        claude_code: omgcode_app::desktop::ClaudeCodeHost::default(),
    })
    .expect("open desktop service")
}

fn new_session(service: &DesktopService, workspace: Uuid) -> Uuid {
    service
        .session_create(wire(
            json!({"workspace_id": workspace, "title": "stored token"}),
        ))
        .expect("create session")
        .session_id
}

fn send(
    service: &DesktopService,
    session: Uuid,
    message: &str,
    endpoint: &str,
) -> Result<ChatStatus, omgcode_app::desktop::DesktopError> {
    service.chat_send(wire(json!({
        "session_id": session,
        "message": message,
        "settings": settings_json(endpoint),
        "skills": [],
    })))
}

#[test]
fn each_turn_reads_the_stored_credential_so_a_rotation_applies_to_the_next_turn() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let mut vault = Vault::new(&fixture.data);
    let server = MockServer::start(vec![
        Step::reply(text("first")),
        Step::reply(text("second")),
    ]);
    vault.add_profile_at("lms", "lm_studio", "api_key", &server.endpoint);
    vault.bind("lms", STORED.as_bytes());
    let service = service(&fixture, &server.endpoint, "lms");
    let session = new_session(&service, workspace.id);

    let started = send(&service, session, "hello", &server.endpoint).expect("first turn");
    assert!(matches!(started.state, ChatState::Running));
    let done = wait_finished(&service, session);
    assert!(matches!(done.state, ChatState::Completed), "{done:?}");
    let mut everything = text_of(&started) + &text_of(&done);

    // Rotate through the settings API between turns.
    vault.bind("lms", ROTATED.as_bytes());

    send(&service, session, "again", &server.endpoint).expect("second turn");
    let done = wait_finished(&service, session);
    assert!(matches!(done.state, ChatState::Completed), "{done:?}");
    everything += &text_of(&done);
    everything += &text_of(
        &service
            .session_transcript(wire(
                json!({"session_id": session, "after_seq": null, "limit": 200}),
            ))
            .expect("transcript"),
    );

    let (served, requests) = server.finish_recorded();
    assert_eq!(served, 2);
    assert_eq!(
        requests[0].header("authorization"),
        Some(format!("Bearer {STORED}"))
    );
    assert_eq!(
        requests[1].header("authorization"),
        Some(format!("Bearer {ROTATED}"))
    );
    for secret in [STORED, ROTATED] {
        assert!(!everything.contains(secret), "a response carries a token");
    }
}

#[test]
fn an_unreadable_credential_is_a_synchronous_credential_error_and_no_turn_starts() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let mut vault = Vault::new(&fixture.data);
    // Nothing listens: a request would fail as `provider`, not `credential`.
    let endpoint = unused_endpoint();
    vault.add_profile_at("lms", "lm_studio", "api_key", &endpoint);
    let id = vault.bind("lms", STORED.as_bytes());
    let service = service(&fixture, &endpoint, "lms");
    let session = new_session(&service, workspace.id);

    vault.delete_native(&id);
    let error = send(&service, session, "hello", &endpoint).expect_err("refused");
    assert_eq!(error.code(), "credential");
    assert!(
        error.message().starts_with("credential_not_found:"),
        "{}",
        error.message()
    );
    assert!(!text_of(&error).contains(STORED));

    // No thread started and no slot exists: the session stays idle.
    let status = service
        .chat_status(wire(session_request(session)))
        .expect("status");
    assert!(matches!(status.state, ChatState::Idle), "{status:?}");
    let transcript = service
        .session_transcript(wire(
            json!({"session_id": session, "after_seq": null, "limit": 200}),
        ))
        .expect("transcript");
    assert!(transcript.entries.is_empty(), "no turn was recorded");
}

#[test]
fn an_unknown_endpoint_less_or_unbound_profile_is_refused_before_a_turn() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile("openai-key", "openai", "api_key");
    let endpoint = unused_endpoint();
    vault.add_profile_at("unbound", "lm_studio", "api_key", &endpoint);
    vault.bind("openai-key", STORED.as_bytes());
    for (name, expected) in [
        ("missing", "unknown_profile:"),
        // A hosted profile recorded without an endpoint cannot be used (ADR-0092).
        ("openai-key", "endpoint_not_recorded:"),
        ("unbound", "not_bound:"),
    ] {
        let service = service(&fixture, &endpoint, name);
        let session = new_session(&service, workspace.id);
        let error = send(&service, session, "hello", &endpoint).expect_err("refused");
        assert_eq!(error.code(), "credential", "{name}");
        assert!(error.message().starts_with(expected), "{}", error.message());
        assert!(!text_of(&error).contains(STORED));
    }
}

#[test]
fn a_keyless_profile_sends_no_authorization_header() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let vault = Vault::new(&fixture.data);
    let server = MockServer::start(vec![Step::reply(text("ok"))]);
    vault.add_profile_at("open", "lm_studio", "none", &server.endpoint);
    let service = service(&fixture, &server.endpoint, "open");
    let session = new_session(&service, workspace.id);
    send(&service, session, "hello", &server.endpoint).expect("turn");
    let done = wait_finished(&service, session);
    assert!(matches!(done.state, ChatState::Completed), "{done:?}");
    let (_, requests) = server.finish_recorded();
    assert_eq!(requests[0].header("authorization"), None);
}

#[test]
fn a_profile_without_an_endpoint_is_refused_before_a_turn() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile("old-none", "lm_studio", "none");
    vault.add_profile("old-key", "lm_studio", "api_key");
    vault.bind("old-key", STORED.as_bytes());
    let endpoint = unused_endpoint();
    for name in ["old-none", "old-key"] {
        let service = service(&fixture, &endpoint, name);
        let session = new_session(&service, workspace.id);
        let error = send(&service, session, "hello", &endpoint).expect_err("refused");
        assert_eq!(error.code(), "credential", "{name}");
        assert!(
            error.message().starts_with("endpoint_not_recorded:"),
            "{}",
            error.message()
        );
        assert!(!text_of(&error).contains(STORED));
    }
}

/// The service's fixed endpoint is the profile's, and a profile removed and
/// added again with another endpoint while the service runs ends the next turn
/// synchronously: no thread starts and neither listener is contacted.
#[test]
fn a_profile_replaced_with_another_endpoint_refuses_the_next_turn() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let mut vault = Vault::new(&fixture.data);
    let server = MockServer::start(vec![Step::reply(text("first"))]);
    let replacement = NoCallServer::start();
    vault.add_profile_at("lms", "lm_studio", "api_key", &server.endpoint);
    vault.bind("lms", STORED.as_bytes());
    // What the shell would resolve once at launch and fix for the process.
    let launch = omgcode_app::resolve_local_connection(
        &fixture.data,
        &omgcode_app::ConnectionSource::Profile("lms".to_owned()),
    )
    .expect("the launch check");
    assert_eq!(launch.endpoint, server.endpoint);
    let service = service(&fixture, &launch.endpoint, "lms");
    assert_eq!(service.chat_defaults().settings.endpoint, server.endpoint);
    let session = new_session(&service, workspace.id);

    send(&service, session, "hello", &launch.endpoint).expect("first turn");
    let done = wait_finished(&service, session);
    assert!(matches!(done.state, ChatState::Completed), "{done:?}");

    // Replace the profile through the settings API: same name, new endpoint.
    vault.remove_profile("lms");
    vault.add_profile_at("lms", "lm_studio", "api_key", &replacement.endpoint);
    vault.bind("lms", ROTATED.as_bytes());

    let error = send(&service, session, "again", &launch.endpoint).expect_err("refused");
    assert_eq!(error.code(), "credential");
    assert!(
        error.message().starts_with("endpoint_changed:"),
        "{}",
        error.message()
    );
    assert!(error.message().contains("restart"), "{}", error.message());
    let rendered = text_of(&error);
    assert!(!rendered.contains(STORED) && !rendered.contains(ROTATED));

    // No thread started: the finished slot of the first turn is untouched, and
    // the interface still shows the launch endpoint.
    let status = service
        .chat_status(wire(session_request(session)))
        .expect("status");
    assert!(matches!(status.state, ChatState::Completed), "{status:?}");
    assert_eq!(service.chat_defaults().settings.endpoint, server.endpoint);

    let (served, requests) = server.finish_recorded();
    assert_eq!(served, 1, "only the first turn reached the launch endpoint");
    assert_eq!(
        requests[0].header("authorization"),
        Some(format!("Bearer {STORED}"))
    );
    replacement.assert_untouched();
}

/// The model list (ADR-0066) reads the stored credential as a turn does and
/// sends it only to the profile's endpoint; an unreadable one is a
/// `credential` error before anything is sent. No result carries the token.
#[test]
fn the_model_list_sends_the_stored_credential_and_never_returns_it() {
    let fixture = Fixture::new();
    let mut vault = Vault::new(&fixture.data);
    let server = MockServer::start(vec![Step::reply(json!({"data": [{"id": "m"}]}))]);
    vault.add_profile_at("lms", "lm_studio", "api_key", &server.endpoint);
    let id = vault.bind("lms", STORED.as_bytes());
    let service = service(&fixture, &server.endpoint, "lms");
    let workspace = fixture.workspace("project", text_only());
    let session = new_session(&service, workspace.id);

    let listed = service
        .chat_models(wire(session_request(session)))
        .expect("list");
    assert_eq!(listed.endpoint, server.endpoint);
    assert!(!text_of(&listed).contains(STORED));
    let (served, requests) = server.finish_recorded();
    assert_eq!(served, 1);
    assert!(requests[0].head.starts_with("GET /v1/models "));
    assert_eq!(
        requests[0].header("authorization"),
        Some(format!("Bearer {STORED}"))
    );

    vault.delete_native(&id);
    let error = service
        .chat_models(wire(session_request(session)))
        .expect_err("refused");
    assert_eq!(error.code(), "credential");
    assert!(!text_of(&error).contains(STORED));
}
