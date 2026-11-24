//! The local adapter's endpoint and token from a stored provider profile
//! (ADR-0047, ADR-0048), against the real settings store, the real native
//! credential store, and the scripted loopback model server.
//!
//! On Linux these need an unlocked Secret Service default collection and fail
//! without one. Every item a test stores is deleted when its `Vault` drops. Test
//! tokens are obviously fake.

use std::fs;

use omgcode_app::{
    AppError, ChatHooks, ChatSettings, ChatTurnInput, ConnectionSource, ErrorKind, LmToken,
    ProfileCheckError, check_profile_selectable, create_session, profile_selectability,
    read_lm_token, redact_known_secret, resolve_local_connection, resolve_profile_connection,
    run_chat_turn,
};
use omgcode_core::control::{ControlStore, ProviderKind};
use serde_json::json;

mod common;
use common::credential::Vault;
use common::{Fixture, MockServer, NoCallServer, Step, text, text_only};

const STORED: &str = "fake-stored-token-4c19e7a2";
const ROTATED: &str = "fake-rotated-token-90b3d1f6";
/// An endpoint for a profile whose test sends no request.
const RECORDED: &str = "http://127.0.0.1:1/v1";
/// The one hosted endpoint (ADR-0071); no test connects to it.
const CLOUD: &str = "https://ollama.com/v1";

fn profile(name: &str) -> ConnectionSource {
    ConnectionSource::Profile(name.to_owned())
}

/// The token of a resolution, so each test can keep asserting on it alone.
fn resolve_token(
    data: &std::path::Path,
    source: &ConnectionSource,
) -> Result<Option<LmToken>, AppError> {
    resolve_local_connection(data, source).map(|connection| connection.token)
}

/// The failure of a resolution, which must never expose `STORED`.
fn refusal(fixture: &Fixture, source: &ConnectionSource) -> (String, String) {
    let error = resolve_local_connection(&fixture.data, source)
        .map(|connection| {
            (
                connection.endpoint,
                connection.token.map(|token| token.expose().to_owned()),
            )
        })
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
    let source = ConnectionSource::Environment {
        endpoint: "http://127.0.0.1:4321/v1".to_owned(),
    };
    let connection = resolve_local_connection(&fixture.data, &source).expect("environment");
    assert_eq!(connection.endpoint, "http://127.0.0.1:4321/v1");
    assert_eq!(
        connection.token.map(|token| token.expose().to_owned()),
        read_lm_token().expect("environment token")
    );
    assert!(!fixture.data.join("control-v1.jsonl").exists());
}

#[test]
fn a_profile_source_returns_its_endpoint_and_the_stored_token_and_none_for_a_keyless_profile() {
    let fixture = Fixture::new();
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile_at("lms", "lm_studio", "api_key", RECORDED);
    vault.add_profile_at("open", "lm_studio", "none", "http://[::1]:2/v1/");
    vault.bind("lms", STORED.as_bytes());

    let connection = resolve_local_connection(&fixture.data, &profile("LMS")).expect("stored");
    assert_eq!(connection.endpoint, RECORDED, "exactly the recorded text");
    let token = connection.token.expect("a token");
    assert_eq!(token.expose(), STORED);
    assert_eq!(format!("{token:?}"), "LmToken(<redacted>)");

    // A `none` profile selects no token at all, and still has its endpoint.
    let open = resolve_local_connection(&fixture.data, &profile("open")).expect("none profile");
    assert_eq!(open.endpoint, "http://[::1]:2/v1/");
    assert!(open.token.is_none());
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
    vault.add_profile_at("lms", "lm_studio", "api_key", RECORDED);
    vault.add_profile_at("unbound", "lm_studio", "api_key", RECORDED);
    vault.add_profile("openai-key", "openai", "api_key");
    vault.add_profile_at(
        "plan",
        "anthropic",
        "subscription",
        "https://api.anthropic.com/v1",
    );
    vault.bind("openai-key", STORED.as_bytes());
    let lost = vault.bind("lms", STORED.as_bytes());

    let (_, message) = refusal(&fixture, &profile("missing"));
    assert!(message.starts_with("unknown_profile:"), "{message}");
    // Every kind is served by the one adapter (ADR-0091), so an OpenAI key
    // recorded before endpoints existed is refused for its missing endpoint,
    // before the key is read, and a subscription is never used.
    let (_, message) = refusal(&fixture, &profile("openai-key"));
    assert!(message.starts_with("endpoint_not_recorded:"), "{message}");
    let (_, message) = refusal(&fixture, &profile("plan"));
    assert!(
        message.starts_with("subscription_unsupported:"),
        "{message}"
    );
    let (_, message) = refusal(&fixture, &profile("unbound"));
    assert!(message.starts_with("not_bound:"), "{message}");

    // The native item disappears while its binding stays recorded.
    vault.delete_native(&lost);
    let (_, message) = refusal(&fixture, &profile("lms"));
    assert!(message.starts_with("credential_not_found:"), "{message}");
    assert!(message.contains(lost.as_str()), "{message}");
}

/// A profile recorded without an endpoint is refused whatever its
/// authentication mode, before any native store access: the bound credential's
/// native item is gone, so a load would have reported that instead.
#[test]
fn a_profile_without_an_endpoint_is_refused_with_endpoint_not_recorded() {
    let fixture = Fixture::new();
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile("old-none", "lm_studio", "none");
    vault.add_profile("old-key", "lm_studio", "api_key");
    vault.add_profile("old-unbound", "lm_studio", "api_key");
    let id = vault.bind("old-key", STORED.as_bytes());
    vault.delete_native(&id);
    for name in ["old-none", "old-key", "old-unbound", "OLD-KEY"] {
        let (_, message) = refusal(&fixture, &profile(name));
        assert!(
            message.starts_with("endpoint_not_recorded:"),
            "{name}: {message}"
        );
        assert!(message.contains("no recorded endpoint"), "{message}");
    }
}

#[test]
fn a_busy_settings_store_is_refused_without_a_retry() {
    let fixture = Fixture::new();
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile_at("lms", "lm_studio", "api_key", RECORDED);
    vault.bind("lms", STORED.as_bytes());

    let holder = ControlStore::open(&fixture.data).expect("another holder");
    let (_, message) = refusal(&fixture, &profile("lms"));
    assert!(message.contains("in use by another process"), "{message}");
    drop(holder);

    // Released, the same resolution succeeds.
    assert!(
        resolve_token(&fixture.data, &profile("lms"))
            .expect("resolves")
            .is_some()
    );
}

#[test]
fn a_stored_secret_that_is_not_utf8_text_is_refused() {
    let fixture = Fixture::new();
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile_at("lms", "lm_studio", "api_key", RECORDED);
    vault.bind("lms", &[0xff, 0xfe, 0x41]);
    let (_, message) = refusal(&fixture, &profile("lms"));
    assert!(message.starts_with("credential_invalid:"), "{message}");
}

#[test]
fn the_stored_token_is_sent_as_the_bearer_and_recorded_nowhere() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let mut vault = Vault::new(&fixture.data);
    let server = MockServer::start(vec![Step::reply(text(&format!(
        "I will not repeat {STORED}"
    )))]);
    vault.add_profile_at("lms", "lm_studio", "api_key", &server.endpoint);
    vault.bind("lms", STORED.as_bytes());
    let connection = resolve_local_connection(&fixture.data, &profile("lms")).expect("connection");
    let settings = ChatSettings::new(connection.endpoint, "fixture", 131_072);
    let token = connection.token;
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
            skills: &[],
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

/// A turn that uses the resolved connection reaches the profile's endpoint
/// with the stored bearer, and no other listener sees a request.
#[test]
fn a_turn_with_a_profile_sends_to_the_profile_endpoint_and_nowhere_else() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let mut vault = Vault::new(&fixture.data);
    let profile_server = MockServer::start(vec![Step::reply(text("from the profile endpoint"))]);
    let other = NoCallServer::start();
    assert_ne!(profile_server.endpoint, other.endpoint);
    vault.add_profile_at("lms", "lm_studio", "api_key", &profile_server.endpoint);
    vault.bind("lms", STORED.as_bytes());

    let connection = resolve_local_connection(&fixture.data, &profile("lms")).expect("connection");
    assert_eq!(connection.endpoint, profile_server.endpoint);
    let settings = ChatSettings::new(connection.endpoint, "fixture", 131_072);
    let mut journal = create_session(&fixture.data, &workspace.path, "endpoint").expect("session");
    run_chat_turn(
        &mut journal,
        &fixture.data,
        &ChatTurnInput {
            message: "hello",
            settings: &settings,
            token: connection.token.as_ref().map(LmToken::expose),
            skills: &[],
        },
        &mut ChatHooks::default(),
    )
    .expect("turn");
    drop(journal);

    let (served, requests) = profile_server.finish_recorded();
    assert_eq!(served, 1);
    assert_eq!(
        requests[0].header("authorization"),
        Some(format!("Bearer {STORED}"))
    );
    other.assert_untouched();
}

#[test]
fn a_rotated_credential_is_used_by_the_next_resolution() {
    let fixture = Fixture::new();
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile_at("lms", "lm_studio", "api_key", RECORDED);
    vault.bind("lms", STORED.as_bytes());
    let first = resolve_token(&fixture.data, &profile("lms"))
        .expect("first")
        .expect("token");
    assert_eq!(first.expose(), STORED);
    vault.bind("lms", ROTATED.as_bytes());
    let second = resolve_token(&fixture.data, &profile("lms"))
        .expect("second")
        .expect("token");
    assert_eq!(second.expose(), ROTATED);
}

/// On Windows the native store refuses a data root that grants access to a
/// principal other than the current user, the system, and administrators, so
/// such a root makes `CredentialStore::open` fail. The early outcomes are
/// decided from the settings store alone and must still report their own
/// cause, and a `none` profile must still resolve.
///
/// The test grants the built-in Users group read access itself. Whether a
/// temporary directory inherits such a grant depends on the host: it does on
/// some machines and not on others, so the inherited ACL alone proves nothing.
#[cfg(windows)]
#[test]
fn early_outcomes_need_no_native_store() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let data = temporary.path().join("application-data");
    fs::create_dir(&data).expect("data root");
    // S-1-5-32-545 is the built-in Users group, named by its identifier so
    // the command does not depend on the display language.
    let granted = std::process::Command::new("icacls")
        .arg(&data)
        .args(["/grant", "*S-1-5-32-545:(RX)"])
        .output()
        .expect("icacls");
    assert!(granted.status.success(), "icacls fixture failed");
    let vault = Vault::new(&data);
    vault.add_profile_at("open", "lm_studio", "none", RECORDED);
    vault.add_profile_at("unbound", "lm_studio", "api_key", RECORDED);
    vault.add_profile("old-key", "lm_studio", "api_key");
    vault.add_profile("openai-key", "openai", "api_key");

    // Prove the premise: the native store cannot open under this root.
    let premise = omgcode_core::control::CredentialStore::open(&data).err();
    assert!(
        premise.is_some_and(|error| error.to_string().contains("untrusted principal")),
        "the native store must refuse this data root"
    );

    let open = resolve_local_connection(&data, &profile("open")).expect("none profile");
    assert_eq!(open.endpoint, RECORDED);
    assert!(open.token.is_none());
    for (name, expected) in [
        ("missing", "unknown_profile:"),
        ("openai-key", "endpoint_not_recorded:"),
        ("old-key", "endpoint_not_recorded:"),
        ("unbound", "not_bound:"),
    ] {
        let error = resolve_token(&data, &profile(name)).expect_err("refused");
        assert_eq!(error.code(), "credential");
        assert!(error.message().starts_with(expected), "{}", error.message());
    }
}

/// The desktop launch check and the service both accept a `none` profile
/// without any native store. The shell's own startup code needs `omgcode-core`
/// to build the settings fixture, so this runs at the service level.
#[test]
fn a_keyless_profile_passes_the_launch_check_and_opens_the_service() {
    use omgcode_app::TokenSource;
    use omgcode_app::desktop::{DesktopConfig, DesktopService};
    let fixture = Fixture::new();
    let vault = Vault::new(&fixture.data);
    vault.add_profile_at("open", "lm_studio", "none", "http://127.0.0.1:1/v1");
    let launch =
        resolve_local_connection(&fixture.data, &profile("open")).expect("the launch check");
    assert!(launch.token.is_none());
    DesktopService::open(DesktopConfig {
        data_root: fixture.data.clone(),
        endpoint: launch.endpoint,
        token_source: TokenSource::Profile("open".to_owned()),
        stream: true,
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
    let server = MockServer::start(vec![Step::reply(tool_call(
        "call-1",
        "workspace_read_file",
        &json!({"path": "note.txt", "note": STORED}),
    ))]);
    vault.add_profile_at("lms", "lm_studio", "api_key", &server.endpoint);
    vault.bind("lms", STORED.as_bytes());
    let connection = resolve_local_connection(&fixture.data, &profile("lms")).expect("connection");
    let settings = ChatSettings::new(connection.endpoint, "fixture", 131_072);
    let token = connection.token;
    let mut journal = create_session(&fixture.data, &workspace.path, "tool call").expect("session");
    let path = journal.path().to_owned();
    let result = run_chat_turn(
        &mut journal,
        &fixture.data,
        &ChatTurnInput {
            message: "read it",
            settings: &settings,
            token: token.as_ref().map(LmToken::expose),
            skills: &[],
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

/// An `ollama_cloud` profile resolves like an `lm_studio` one: its fixed
/// endpoint as a validated type and the stored key, and a key bound to a
/// local profile is never taken for it (ADR-0071). No request is sent.
#[test]
fn an_ollama_cloud_profile_resolves_its_endpoint_and_stored_key() {
    let fixture = Fixture::new();
    let mut vault = Vault::new(&fixture.data);
    vault.add_profile_at("cloud", "ollama_cloud", "api_key", CLOUD);
    vault.add_profile_at("unbound", "ollama_cloud", "api_key", CLOUD);
    vault.add_profile_at("lms", "lm_studio", "api_key", RECORDED);
    vault.bind("cloud", STORED.as_bytes());
    vault.bind("lms", ROTATED.as_bytes());

    let connection = resolve_local_connection(&fixture.data, &profile("CLOUD")).expect("cloud");
    assert_eq!(connection.endpoint, CLOUD);
    assert_eq!(connection.token.expect("a key").expose(), STORED);

    let resolved = resolve_profile_connection(&fixture.data, "cloud").expect("profile");
    assert_eq!(resolved.kind, ProviderKind::OllamaCloud);
    assert!(resolved.endpoint.is_hosted());
    assert_eq!(resolved.token.expect("a key").expose(), STORED);
    let local = resolve_profile_connection(&fixture.data, "lms").expect("local");
    assert_eq!(local.kind, ProviderKind::LmStudio);
    assert!(!local.endpoint.is_hosted());

    let (_, message) = refusal(&fixture, &profile("unbound"));
    assert!(message.starts_with("not_bound:"), "{message}");
    assert!(check_profile_selectable(&fixture.data, "cloud").is_ok());
    assert!(check_profile_selectable(&fixture.data, "unbound").is_err());
    // The typed check separates a profile that cannot be chosen from an
    // unusable store, without reading a message.
    assert!(profile_selectability(&fixture.data, "cloud").is_ok());
    for name in ["unbound", "missing"] {
        assert!(
            matches!(
                profile_selectability(&fixture.data, name),
                Err(ProfileCheckError::NotSelectable(_))
            ),
            "{name}"
        );
    }
}

/// The environment token is never sent to the hosted endpoint, so the cloud
/// endpoint without a profile is refused before the variable is read.
#[test]
fn the_cloud_endpoint_is_refused_without_a_profile() {
    let fixture = Fixture::bare();
    let source = ConnectionSource::Environment {
        endpoint: CLOUD.to_owned(),
    };
    let (_, message) = refusal(&fixture, &source);
    assert!(message.starts_with("cloud_needs_profile:"), "{message}");
}
