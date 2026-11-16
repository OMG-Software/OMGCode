//! The local adapter's token from a stored provider credential (ADR-0047),
//! against the real settings store, the real native credential store, and the
//! scripted loopback model server.
//!
//! On Linux these need an unlocked Secret Service default collection and fail
//! without one. Every item a test stores is deleted when its `Vault` drops. Test
//! tokens are obviously fake.

use std::fs;

use omgcode_app::{
    ChatHooks, ChatSettings, ChatTurnInput, ErrorKind, LmToken, TokenSource, create_session,
    read_lm_token, redact_known_secret, resolve_lm_token, run_chat_turn,
};
use omgcode_core::control::ControlStore;
use serde_json::json;

mod common;
use common::credential::Vault;
use common::{Fixture, MockServer, Step, text, text_only};

const STORED: &str = "fake-stored-token-4c19e7a2";
const ROTATED: &str = "fake-rotated-token-90b3d1f6";

fn profile(name: &str) -> TokenSource {
    TokenSource::Profile(name.to_owned())
}

/// The failure of a resolution, which must never expose `STORED`.
fn refusal(fixture: &Fixture, source: &TokenSource) -> (String, String) {
    let error = resolve_lm_token(&fixture.data, source)
        .map(|token| token.map(|token| token.expose().to_owned()))
        .expect_err("the source is refused");
    assert_eq!(error.kind(), ErrorKind::Credential);
    assert_eq!(error.code(), "credential");
    for text in [error.message().to_owned(), format!("{error:?}")] {
        assert!(!text.contains(STORED), "{text}");
    }
    (error.code().to_owned(), error.message().to_owned())
}

#[test]
fn the_environment_source_is_the_existing_environment_read() {
    // No settings store exists, and none is needed or touched.
    let fixture = Fixture::bare();
    let resolved = resolve_lm_token(&fixture.data, &TokenSource::Environment)
        .map(|token| token.map(|token| token.expose().to_owned()));
    assert_eq!(resolved, read_lm_token());
}

#[test]
fn a_profile_source_returns_the_stored_token_and_none_for_a_keyless_profile() {
    let fixture = Fixture::new();
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile("lms", "lm_studio", "api_key");
    vault.add_profile("open", "lm_studio", "none");
    vault.bind("lms", STORED.as_bytes());

    let token = resolve_lm_token(&fixture.data, &profile("LMS"))
        .expect("stored token")
        .expect("a token");
    assert_eq!(token.expose(), STORED);
    assert_eq!(format!("{token:?}"), "LmToken(<redacted>)");

    // A `none` profile selects no token at all.
    assert!(
        resolve_lm_token(&fixture.data, &profile("open"))
            .expect("none profile")
            .is_none()
    );
}

#[test]
fn every_refusal_is_a_credential_error_with_its_cause_code() {
    let fixture = Fixture::bare();
    // No settings store exists.
    let (_, message) = refusal(&fixture, &profile("lms"));
    assert!(message.contains("no settings store exists"), "{message}");
    assert!(
        !fixture.data.join("control-v1.jsonl").exists(),
        "resolving creates no settings store"
    );

    let fixture = Fixture::new();
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile("lms", "lm_studio", "api_key");
    vault.add_profile("unbound", "lm_studio", "api_key");
    vault.add_profile("openai-key", "openai", "api_key");
    vault.add_profile("plan", "anthropic", "subscription");
    vault.bind("openai-key", STORED.as_bytes());
    let lost = vault.bind("lms", STORED.as_bytes());

    let (_, message) = refusal(&fixture, &profile("missing"));
    assert!(message.starts_with("unknown_profile:"), "{message}");
    // An OpenAI key is never read for the local adapter.
    let (_, message) = refusal(&fixture, &profile("openai-key"));
    assert!(message.starts_with("wrong_provider:"), "{message}");
    let (_, message) = refusal(&fixture, &profile("plan"));
    assert!(message.starts_with("wrong_provider:"), "{message}");
    let (_, message) = refusal(&fixture, &profile("unbound"));
    assert!(message.starts_with("not_bound:"), "{message}");

    // The native item disappears while its binding stays recorded.
    vault.delete_native(&lost);
    let (_, message) = refusal(&fixture, &profile("lms"));
    assert!(message.starts_with("credential_not_found:"), "{message}");
    assert!(message.contains(lost.as_str()), "{message}");
}

#[test]
fn a_busy_settings_store_is_refused_without_a_retry() {
    let fixture = Fixture::new();
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile("lms", "lm_studio", "api_key");
    vault.bind("lms", STORED.as_bytes());

    let holder = ControlStore::open(&fixture.data).expect("another holder");
    let (_, message) = refusal(&fixture, &profile("lms"));
    assert!(message.contains("in use by another process"), "{message}");
    drop(holder);

    // Released, the same resolution succeeds.
    assert!(
        resolve_lm_token(&fixture.data, &profile("lms"))
            .expect("resolves")
            .is_some()
    );
}

#[test]
fn a_stored_secret_that_is_not_utf8_text_is_refused() {
    let fixture = Fixture::new();
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile("lms", "lm_studio", "api_key");
    vault.bind("lms", &[0xff, 0xfe, 0x41]);
    let (_, message) = refusal(&fixture, &profile("lms"));
    assert!(message.starts_with("credential_invalid:"), "{message}");
}

#[test]
fn the_stored_token_is_sent_as_the_bearer_and_recorded_nowhere() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile("lms", "lm_studio", "api_key");
    vault.bind("lms", STORED.as_bytes());
    let server = MockServer::start(vec![Step::reply(text(&format!(
        "I will not repeat {STORED}"
    )))]);
    let settings = ChatSettings::new(server.endpoint.clone(), "fixture", 131_072);
    let token: Option<LmToken> =
        resolve_lm_token(&fixture.data, &profile("lms")).expect("stored token");
    // The CLI removes the token from a title before it creates the session.
    let title = redact_known_secret(
        &format!("title {STORED}"),
        token.as_ref().map(LmToken::expose),
    );
    let mut journal = create_session(&fixture.data, &workspace.path, &title).expect("session");
    let path = journal.path().to_owned();

    let output = run_chat_turn(
        &mut journal,
        &fixture.data,
        &ChatTurnInput {
            message: &format!("please use {STORED} here"),
            settings: &settings,
            token: token.as_ref().map(LmToken::expose),
        },
        &mut ChatHooks::default(),
    )
    .expect("turn");
    drop(journal);

    let (served, requests) = server.finish_recorded();
    assert_eq!(served, 1);
    assert_eq!(
        requests[0].header("authorization"),
        Some(format!("Bearer {STORED}"))
    );
    assert!(
        !requests[0].body.contains(STORED),
        "the request body carries no token"
    );
    assert!(requests[0].body.contains("[REDACTED]"));

    let returned = serde_json::to_string(&output).expect("output serializes");
    assert!(!returned.contains(STORED), "{returned}");
    assert!(returned.contains("[REDACTED]"), "{returned}");

    let bytes = fs::read(&path).expect("journal");
    let journal_text = String::from_utf8_lossy(&bytes);
    assert!(!journal_text.contains(STORED), "the journal holds no token");
    assert!(journal_text.contains("[REDACTED]"));
    // The journal names neither the profile nor the credential.
    assert!(!journal_text.contains("lms\""));

    // The control journal never held the secret either.
    let control = fs::read(fixture.data.join("control-v1.jsonl")).expect("control journal");
    assert!(
        !String::from_utf8_lossy(&control).contains(STORED),
        "the settings journal holds no token"
    );
}

#[test]
fn a_rotated_credential_is_used_by_the_next_resolution() {
    let fixture = Fixture::new();
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile("lms", "lm_studio", "api_key");
    vault.bind("lms", STORED.as_bytes());
    let first = resolve_lm_token(&fixture.data, &profile("lms"))
        .expect("first")
        .expect("token");
    assert_eq!(first.expose(), STORED);
    vault.bind("lms", ROTATED.as_bytes());
    let second = resolve_lm_token(&fixture.data, &profile("lms"))
        .expect("second")
        .expect("token");
    assert_eq!(second.expose(), ROTATED);
}

/// On Windows the native store refuses a data root that is not private, so a
/// root left with its inherited ACL makes `CredentialStore::open` fail. The
/// early outcomes are decided from the settings store alone and must still
/// report their own cause, and a `none` profile must still resolve.
#[cfg(windows)]
#[test]
fn early_outcomes_need_no_native_store() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let data = temporary.path().join("application-data");
    fs::create_dir(&data).expect("data root with its inherited ACL");
    let vault = Vault::new(&data);
    vault.add_profile("open", "lm_studio", "none");
    vault.add_profile("unbound", "lm_studio", "api_key");
    vault.add_profile("openai-key", "openai", "api_key");

    // Prove the premise: the native store cannot open under this root.
    let premise = omgcode_core::control::CredentialStore::open(&data).err();
    assert!(
        premise.is_some_and(|error| error.to_string().contains("untrusted principal")),
        "the native store must refuse this data root"
    );

    assert!(
        resolve_lm_token(&data, &profile("open"))
            .expect("none profile")
            .is_none()
    );
    for (name, expected) in [
        ("missing", "unknown_profile:"),
        ("openai-key", "wrong_provider:"),
        ("unbound", "not_bound:"),
    ] {
        let error = resolve_lm_token(&data, &profile(name)).expect_err("refused");
        assert_eq!(error.code(), "credential");
        assert!(error.message().starts_with(expected), "{}", error.message());
    }
}

/// The desktop launch check and the service both accept a `none` profile
/// without any native store. The shell's own startup code needs `omgcode-core`
/// to build the settings fixture, so this runs at the service level.
#[test]
fn a_keyless_profile_passes_the_launch_check_and_opens_the_service() {
    use omgcode_app::desktop::{DesktopConfig, DesktopService};
    let fixture = Fixture::new();
    let vault = Vault::new(&fixture.data);
    vault.add_profile("open", "lm_studio", "none");
    let source = profile("open");
    assert!(
        resolve_lm_token(&fixture.data, &source)
            .expect("the launch check")
            .is_none()
    );
    DesktopService::open(DesktopConfig {
        data_root: fixture.data.clone(),
        endpoint: "http://127.0.0.1:1/v1".to_owned(),
        token_source: source,
    })
    .expect("the service opens");
}

/// The stored token is refused inside a tool call exactly like the
/// environment token: the call is not run and the turn records no token.
#[test]
fn a_tool_call_containing_the_stored_token_is_refused() {
    use common::{read_tools, tool_call};
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", read_tools());
    fs::write(workspace.path.join("note.txt"), "note\n").expect("note");
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile("lms", "lm_studio", "api_key");
    vault.bind("lms", STORED.as_bytes());
    let server = MockServer::start(vec![Step::reply(tool_call(
        "call-1",
        "workspace_read_file",
        &json!({"path": "note.txt", "note": STORED}),
    ))]);
    let settings = ChatSettings::new(server.endpoint.clone(), "fixture", 131_072);
    let token = resolve_lm_token(&fixture.data, &profile("lms")).expect("token");
    let mut journal = create_session(&fixture.data, &workspace.path, "tool call").expect("session");
    let path = journal.path().to_owned();
    let result = run_chat_turn(
        &mut journal,
        &fixture.data,
        &ChatTurnInput {
            message: "read it",
            settings: &settings,
            token: token.as_ref().map(LmToken::expose),
        },
        &mut ChatHooks::default(),
    );
    drop(journal);
    assert_eq!(server.finish(), 1, "the call was made, then refused");
    let rendered = match &result {
        Ok(output) => serde_json::to_string(output).expect("output"),
        Err(error) => format!("{error}"),
    };
    assert!(!rendered.contains(STORED), "{rendered}");
    let error = result.as_ref().expect_err("the turn must not complete");
    assert!(
        error
            .message()
            .contains("tool call containing the configured server token"),
        "{error}"
    );
    let bytes = fs::read(&path).expect("journal");
    assert!(
        !String::from_utf8_lossy(&bytes).contains(STORED),
        "the journal holds no token"
    );
}
