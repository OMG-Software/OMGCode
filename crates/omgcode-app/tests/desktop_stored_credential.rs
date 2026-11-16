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
    Fixture, MockServer, Step, session_request, settings_json, text, text_of, text_only,
    unused_endpoint, wait_finished, wire,
};

const STORED: &str = "fake-desktop-token-27ad90e1";
const ROTATED: &str = "fake-desktop-rotated-6e0c5b34";

fn service(fixture: &Fixture, endpoint: &str, profile: &str) -> DesktopService {
    DesktopService::open(DesktopConfig {
        data_root: fixture.data.clone(),
        endpoint: endpoint.to_owned(),
        token_source: TokenSource::Profile(profile.to_owned()),
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
    })))
}

#[test]
fn each_turn_reads_the_stored_credential_so_a_rotation_applies_to_the_next_turn() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile("lms", "lm_studio", "api_key");
    vault.bind("lms", STORED.as_bytes());
    let server = MockServer::start(vec![
        Step::reply(text("first")),
        Step::reply(text("second")),
    ]);
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
    vault.add_profile("lms", "lm_studio", "api_key");
    let id = vault.bind("lms", STORED.as_bytes());
    // Nothing listens: a request would fail as `provider`, not `credential`.
    let endpoint = unused_endpoint();
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
fn an_unknown_wrong_provider_or_unbound_profile_is_refused_before_a_turn() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile("openai-key", "openai", "api_key");
    vault.add_profile("unbound", "lm_studio", "api_key");
    vault.bind("openai-key", STORED.as_bytes());
    let endpoint = unused_endpoint();
    for (name, expected) in [
        ("missing", "unknown_profile:"),
        ("openai-key", "wrong_provider:"),
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
    vault.add_profile("open", "lm_studio", "none");
    let server = MockServer::start(vec![Step::reply(text("ok"))]);
    let service = service(&fixture, &server.endpoint, "open");
    let session = new_session(&service, workspace.id);
    send(&service, session, "hello", &server.endpoint).expect("turn");
    let done = wait_finished(&service, session);
    assert!(matches!(done.state, ChatState::Completed), "{done:?}");
    let (_, requests) = server.finish_recorded();
    assert_eq!(requests[0].header("authorization"), None);
}
