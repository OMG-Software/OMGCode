use super::*;
use std::fs;

use crate::control::{ControlError, ControlStore, FILE_NAME};

fn store() -> (tempfile::TempDir, std::path::PathBuf, ControlStore) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let data = directory.path().join("data");
    let store = ControlStore::open(&data).expect("store");
    (directory, data, store)
}

fn declaration(name: &str, kind: &str, auth: &str) -> ProviderProfileDeclaration {
    ProviderProfileDeclaration {
        name: name.into(),
        kind: kind.into(),
        auth: auth.into(),
        endpoint: None,
    }
}

/// Profiles are recorded with revisions, survive replay, and are removed.
#[test]
fn provider_profiles_are_recorded_replayed_and_removed() {
    let (_directory, data, mut store) = store();
    let profile = store
        .add_provider_profile(&declaration("work", "openai", "subscription"), 0)
        .expect("profile");
    assert_eq!(store.revision(), 1);
    let status = store.provider_profile("WORK").expect("case-insensitive");
    assert_eq!(status.profile().id(), profile.id());
    assert_eq!(status.profile().kind(), ProviderKind::OpenAi);
    assert_eq!(status.profile().auth(), ProviderAuth::Subscription);
    assert_eq!(status.revision(), 1);
    let encoded = serde_json::to_value(store.snapshot()).expect("snapshot");
    assert_eq!(encoded["provider_profiles"][0]["name"], "work");
    assert_eq!(encoded["provider_profiles"][0]["kind"], "openai");
    assert_eq!(encoded["provider_profiles"][0]["auth"], "subscription");
    drop(store);

    let mut store = ControlStore::open(&data).expect("replay");
    assert_eq!(store.provider_profiles().len(), 1);
    assert!(matches!(
        store.add_provider_profile(&declaration("WORK", "anthropic", "api_key"), 1),
        Err(ControlError::ProviderRefused(_))
    ));
    assert_eq!(
        store
            .remove_provider_profile("work", 1)
            .expect("remove")
            .profile_id,
        profile.id()
    );
    assert_eq!(store.revision(), 2);
    assert!(store.provider_profiles().is_empty());
    assert!(matches!(
        store.remove_provider_profile("work", 2),
        Err(ControlError::UnknownProvider)
    ));
    let encoded = serde_json::to_value(store.snapshot()).expect("snapshot");
    assert!(encoded.get("provider_profiles").is_none());
}

/// A provider and authentication pair must be compatible, and a profile
/// cannot be duplicated or use an unknown spelling.
#[test]
fn invalid_provider_profiles_are_refused_without_a_record() {
    let (_directory, data, mut store) = store();
    let cases = [
        ("openai", "none"),
        ("anthropic", "none"),
        ("ollama_local", "api_key"),
        ("ollama_cloud", "subscription"),
        ("open_router", "subscription"),
        ("lm_studio", "subscription"),
        ("unknown", "none"),
        ("openai", "password"),
    ];
    for (kind, auth) in cases {
        let result = store.add_provider_profile(&declaration("bad", kind, auth), 0);
        assert!(
            matches!(result, Err(ControlError::ProviderRefused(_))),
            "{kind}/{auth}: {result:?}"
        );
    }
    assert_eq!(store.revision(), 0);
    assert!(store.provider_profiles().is_empty());

    store
        .add_provider_profile(&declaration("work", "openai", "api_key"), 0)
        .expect("profile");
    let result = store.add_provider_profile(&declaration("work", "anthropic", "api_key"), 1);
    assert!(matches!(result, Err(ControlError::ProviderRefused(_))));
    drop(store);
    let text = fs::read_to_string(data.join(FILE_NAME)).expect("journal text");
    assert_eq!(text.matches("provider_profile_added").count(), 1, "{text}");
}

/// Local providers use `none`; cloud key providers require an API key.
#[test]
fn provider_authentication_pairs_are_supported() {
    let (_directory, _data, mut store) = store();
    for (index, (kind, auth)) in [
        ("openai", "subscription"),
        ("anthropic", "api_key"),
        ("ollama_local", "none"),
        ("ollama_cloud", "api_key"),
        ("open_router", "api_key"),
        ("lm_studio", "none"),
        ("lm_studio", "api_key"),
    ]
    .into_iter()
    .enumerate()
    {
        let revision = store.revision();
        store
            .add_provider_profile(&declaration(&format!("p{index}"), kind, auth), revision)
            .unwrap_or_else(|error| panic!("{kind}/{auth}: {error}"));
    }
}

/// A hosted API-key profile added without an endpoint could never be used,
/// and its endpoint cannot be added later. A named kind records its one
/// documented endpoint instead, `openai_compatible` is refused, and a
/// subscription or local profile keeps recording none (ADR-0092).
#[test]
fn a_hosted_api_key_profile_added_without_an_endpoint_records_its_fixed_one() {
    let (_directory, _data, mut store) = store();
    for kind in ProviderKind::ALL
        .into_iter()
        .filter(|kind| kind.is_hosted())
    {
        let revision = store.revision();
        let added = store.add_provider_profile(
            &declaration(kind.as_str(), kind.as_str(), "api_key"),
            revision,
        );
        match kind.fixed_endpoint() {
            Some(fixed) => {
                let profile = added.unwrap_or_else(|error| panic!("{kind:?}: {error}"));
                assert_eq!(
                    profile.endpoint().map(ProviderEndpoint::as_str),
                    Some(fixed)
                );
            }
            None => {
                assert_eq!(kind, ProviderKind::OpenAiCompatible);
                assert_eq!(
                    refusal(added),
                    "an openai_compatible profile needs an endpoint"
                );
                assert_eq!(store.revision(), revision);
            }
        }
    }
    let revision = store.revision();
    let plan = store
        .add_provider_profile(&declaration("plan", "openai", "subscription"), revision)
        .expect("subscription");
    assert_eq!(plan.endpoint(), None);
    let revision = store.revision();
    let local = store
        .add_provider_profile(&declaration("local", "lm_studio", "api_key"), revision)
        .expect("local");
    assert_eq!(local.endpoint(), None);
}

const ENDPOINT: &str = "http://127.0.0.1:1234/v1";

fn with_endpoint(
    name: &str,
    kind: &str,
    auth: &str,
    endpoint: Option<&str>,
) -> ProviderProfileDeclaration {
    ProviderProfileDeclaration {
        endpoint: endpoint.map(str::to_owned),
        ..declaration(name, kind, auth)
    }
}

fn refusal(result: Result<ProviderProfile, ControlError>) -> &'static str {
    match result {
        Err(ControlError::ProviderRefused(message)) => message,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// An `lm_studio` profile records the exact endpoint text, which survives
/// replay and appears in the serialized profile and status.
#[test]
fn an_lm_studio_profile_records_its_endpoint_exactly() {
    let (_directory, data, mut store) = store();
    for (index, text) in [ENDPOINT, "http://[::1]:4321/v1/"].into_iter().enumerate() {
        let revision = store.revision();
        let profile = store
            .add_provider_profile(
                &with_endpoint(&format!("p{index}"), "lm_studio", "api_key", Some(text)),
                revision,
            )
            .expect("profile");
        assert_eq!(profile.endpoint().map(ProviderEndpoint::as_str), Some(text));
    }
    let status = store.provider_profile("P0").expect("profile");
    assert_eq!(
        status.profile().endpoint().map(ProviderEndpoint::as_str),
        Some(ENDPOINT)
    );
    let encoded = serde_json::to_value(store.snapshot()).expect("snapshot");
    assert_eq!(encoded["provider_profiles"][0]["endpoint"], ENDPOINT);
    assert_eq!(
        encoded["provider_profiles"][1]["endpoint"], "http://[::1]:4321/v1/",
        "stored without normalisation"
    );
    drop(store);
    let store = ControlStore::open(&data).expect("replay");
    assert_eq!(
        store
            .provider_profile("p1")
            .and_then(|status| status.profile().endpoint())
            .map(ProviderEndpoint::as_str),
        Some("http://[::1]:4321/v1/")
    );
}

/// The profile uses the adapter's own rule: every destination the provider
/// module refuses is refused at add time, and nothing is recorded.
#[test]
fn every_endpoint_the_adapter_refuses_is_refused_at_add_time() {
    let (_directory, _data, mut store) = store();
    for text in crate::provider::REFUSED_ENDPOINTS {
        let message = refusal(
            store.add_provider_profile(&with_endpoint("bad", "lm_studio", "none", Some(text)), 0),
        );
        assert!(
            message.contains("local provider endpoint"),
            "{text:?}: {message}"
        );
    }
    let long = format!("http://127.0.0.1:{}1234/v1", "0".repeat(300));
    assert!(
        crate::provider::validate_base_url(&long).is_ok(),
        "only the length bound refuses this one"
    );
    let message = refusal(
        store.add_provider_profile(&with_endpoint("bad", "lm_studio", "none", Some(&long)), 0),
    );
    assert!(message.contains("256 bytes"), "{message}");
    assert_eq!(MAX_PROVIDER_ENDPOINT_BYTES, 256);
    assert_eq!(store.revision(), 0, "a refusal records nothing");
    let exact = format!(
        "http://127.0.0.1:{}1234/v1",
        "0".repeat(MAX_PROVIDER_ENDPOINT_BYTES - ENDPOINT.len())
    );
    assert_eq!(exact.len(), MAX_PROVIDER_ENDPOINT_BYTES);
    store
        .add_provider_profile(&with_endpoint("edge", "lm_studio", "none", Some(&exact)), 0)
        .expect("exactly 256 bytes is accepted");
}

#[test]
fn a_refused_endpoint_writes_no_event() {
    let (_directory, data, mut store) = store();
    let _ = refusal(store.add_provider_profile(
        &with_endpoint("bad", "lm_studio", "none", Some("https://127.0.0.1:1/v1")),
        0,
    ));
    drop(store);
    let text = fs::read_to_string(data.join(FILE_NAME)).expect("journal text");
    assert_eq!(text.matches("provider_profile_added").count(), 0, "{text}");
}

/// A named hosted kind refuses a loopback endpoint, and `openai_compatible`
/// refuses one too: it is HTTPS only (ADR-0092).
#[test]
fn hosted_kinds_refuse_a_loopback_endpoint() {
    let (_directory, _data, mut store) = store();
    for (kind, auth) in [
        ("openai", "api_key"),
        ("anthropic", "subscription"),
        ("open_router", "api_key"),
        ("deepseek", "api_key"),
    ] {
        let message = refusal(
            store.add_provider_profile(&with_endpoint("other", kind, auth, Some(ENDPOINT)), 0),
        );
        assert!(message.contains("one documented endpoint"), "{kind}");
    }
    let message = refusal(store.add_provider_profile(
        &with_endpoint("other", "openai_compatible", "api_key", Some(ENDPOINT)),
        0,
    ));
    assert!(message.contains("openai_compatible endpoint"), "{message}");
    assert_eq!(store.revision(), 0);
}

/// A profile without an endpoint still works and writes exactly the bytes it
/// wrote before endpoints existed.
#[test]
fn a_profile_without_an_endpoint_keeps_its_pre_endpoint_bytes() {
    let (_directory, data, mut store) = store();
    let profile = store
        .add_provider_profile(&declaration("legacy", "lm_studio", "none"), 0)
        .expect("profile");
    assert!(profile.endpoint().is_none());
    let encoded = serde_json::to_string(&profile).expect("profile JSON");
    assert_eq!(
        encoded,
        format!(
            r#"{{"id":"{}","name":"legacy","kind":"lm_studio","auth":"none"}}"#,
            profile.id()
        )
    );
    let status = serde_json::to_value(store.snapshot()).expect("snapshot");
    assert!(status["provider_profiles"][0].get("endpoint").is_none());
    drop(store);
    let text = fs::read_to_string(data.join(FILE_NAME)).expect("journal text");
    let event = text
        .lines()
        .find(|line| line.contains("provider_profile_added"))
        .expect("event");
    assert!(!event.contains("endpoint"), "{event}");
}

/// A pre-endpoint profile record deserializes with no endpoint and
/// re-serializes to the identical text.
#[test]
fn a_pre_endpoint_profile_record_round_trips() {
    let text = r#"{"id":"0b7f2c2e-6f0a-4f6c-8f7e-1d2f3a4b5c6d","name":"legacy","kind":"lm_studio","auth":"api_key"}"#;
    let profile: ProviderProfile = serde_json::from_str(text).expect("pre-change record");
    assert!(profile.endpoint().is_none());
    assert_eq!(serde_json::to_string(&profile).expect("JSON"), text);
}

/// A journal written without endpoints replays, and re-serializing its
/// events reproduces the same bytes.
#[test]
fn a_pre_endpoint_journal_replays_and_reserializes_identically() {
    let (_directory, data, mut store) = store();
    store
        .add_provider_profile(&declaration("legacy", "lm_studio", "api_key"), 0)
        .expect("profile");
    drop(store);
    let bytes = fs::read(data.join(FILE_NAME)).expect("journal");
    let copy = tempfile::tempdir().expect("copy");
    let copied = copy.path().join("data");
    drop(ControlStore::open(&copied).expect("create the directory"));
    fs::write(copied.join(FILE_NAME), &bytes).expect("fixture");
    let store = ControlStore::open(&copied).expect("replay");
    assert!(
        store
            .provider_profile("legacy")
            .expect("profile")
            .profile()
            .endpoint()
            .is_none()
    );
    let mut again = Vec::new();
    for entry in &store.entries {
        again.extend(serde_json::to_vec(entry).expect("event JSON"));
        again.push(b'\n');
    }
    assert_eq!(again, bytes);
}

/// Replay applies the validation: a hand-edited journal cannot introduce an
/// endpoint the command would refuse, or any unknown field.
#[test]
fn replay_rejects_invalid_endpoints_and_unknown_fields() {
    let (_directory, data, mut store) = store();
    store
        .add_provider_profile(
            &with_endpoint("edited", "lm_studio", "none", Some(ENDPOINT)),
            0,
        )
        .expect("profile");
    store
        .add_provider_profile(&declaration("plain", "openai", "subscription"), 1)
        .expect("profile");
    drop(store);
    let original = fs::read_to_string(data.join(FILE_NAME)).expect("journal");
    assert!(original.contains(ENDPOINT));
    let cases = [
        (ENDPOINT, "http://localhost:1234/v1"),
        (ENDPOINT, "https://127.0.0.1:1234/v1"),
        (ENDPOINT, "http://192.0.2.1:1234/v1"),
        (
            r#""kind":"openai","auth":"subscription""#,
            r#""kind":"openai","auth":"subscription","endpoint":"http://127.0.0.1:1234/v1""#,
        ),
        (r#""kind":"lm_studio""#, r#""kind":"lm_studio","extra":1"#),
    ];
    for (from, to) in cases {
        assert!(original.contains(from), "{from}");
        let edited = original.replacen(from, to, 1);
        let directory = tempfile::tempdir().expect("copy");
        let path = directory.path().join("data");
        drop(ControlStore::open(&path).expect("directory"));
        fs::write(path.join(FILE_NAME), edited).expect("edited fixture");
        let result = ControlStore::open(&path);
        assert!(
            matches!(result, Err(ControlError::Invalid(_))),
            "{to}: {:?}",
            result.err()
        );
    }
}

const OLLAMA_ENDPOINT: &str = "http://127.0.0.1:11434/v1";

/// An `ollama_local` profile records an endpoint under the same loopback rule
/// as `lm_studio` (ADR-0064), and is refused exactly where `lm_studio` is.
#[test]
fn an_ollama_local_profile_records_a_loopback_endpoint_under_the_shared_rule() {
    let (_directory, data, mut store) = store();
    let profile = store
        .add_provider_profile(
            &with_endpoint("ollama", "ollama_local", "none", Some(OLLAMA_ENDPOINT)),
            0,
        )
        .expect("profile");
    assert_eq!(
        profile.endpoint().map(ProviderEndpoint::as_str),
        Some(OLLAMA_ENDPOINT)
    );
    assert!(profile.kind().is_local());
    for text in crate::provider::REFUSED_ENDPOINTS {
        let message = refusal(
            store
                .add_provider_profile(&with_endpoint("bad", "ollama_local", "none", Some(text)), 1),
        );
        assert!(
            message.contains("local provider endpoint"),
            "{text:?}: {message}"
        );
    }
    // Ollama has no credential: `api_key` stays refused with an endpoint too.
    let _ = refusal(store.add_provider_profile(
        &with_endpoint("keyed", "ollama_local", "api_key", Some(OLLAMA_ENDPOINT)),
        1,
    ));
    assert_eq!(store.revision(), 1);
    drop(store);
    let store = ControlStore::open(&data).expect("replay");
    assert_eq!(
        store
            .provider_profile("ollama")
            .and_then(|status| status.profile().endpoint())
            .map(ProviderEndpoint::as_str),
        Some(OLLAMA_ENDPOINT)
    );
}

/// Only LM Studio and Ollama on this computer are local kinds.
#[test]
fn only_lm_studio_and_ollama_local_are_local_kinds() {
    for kind in ProviderKind::ALL {
        if !matches!(kind, ProviderKind::LmStudio | ProviderKind::OllamaLocal) {
            assert!(!kind.is_local(), "{kind:?}");
        }
    }
    assert!(ProviderKind::LmStudio.is_local());
    assert!(ProviderKind::OllamaLocal.is_local());
}

/// The active connection is recorded, survives replay, and returns to the
/// default endpoint when cleared or when its profile is removed.
#[test]
fn the_active_connection_is_selected_replayed_and_cleared() {
    let (_directory, data, mut store) = store();
    store
        .add_provider_profile(
            &with_endpoint("lms", "lm_studio", "none", Some(ENDPOINT)),
            0,
        )
        .expect("lm studio");
    store
        .add_provider_profile(
            &with_endpoint("ollama", "ollama_local", "none", Some(OLLAMA_ENDPOINT)),
            1,
        )
        .expect("ollama");
    assert!(store.active_provider_profile().is_none());
    store
        .select_active_provider(Some("OLLAMA"), 2)
        .expect("select ollama");
    assert_eq!(store.revision(), 3);
    assert_eq!(
        store
            .active_provider_profile()
            .map(|status| status.profile().name()),
        Some("ollama")
    );
    let encoded = serde_json::to_value(store.snapshot()).expect("snapshot");
    assert!(encoded.get("active_provider").is_some(), "{encoded}");
    drop(store);

    let mut store = ControlStore::open(&data).expect("replay");
    assert_eq!(
        store
            .active_provider_profile()
            .map(|status| status.profile().name()),
        Some("ollama")
    );
    store
        .select_active_provider(Some("lms"), 3)
        .expect("switch to lm studio");
    store.select_active_provider(None, 4).expect("clear");
    assert!(store.active_provider_profile().is_none());
    let encoded = serde_json::to_value(store.snapshot()).expect("snapshot");
    assert!(encoded.get("active_provider").is_none(), "{encoded}");

    // Removing the selected profile clears the selection in the same event.
    store
        .select_active_provider(Some("ollama"), 5)
        .expect("select again");
    let removal = store.remove_provider_profile("ollama", 6).expect("remove");
    assert!(removal.credential.is_none());
    assert!(store.active_provider_profile().is_none());
    drop(store);
    let store = ControlStore::open(&data).expect("replay after removal");
    assert!(store.active_provider_profile().is_none());
    assert_eq!(store.revision(), 7);
}

/// A profile that is not local, records no endpoint, or is unknown cannot be
/// selected, an unchanged selection is refused, and none of these records
/// anything.
#[test]
fn an_unselectable_or_unchanged_connection_is_refused_without_a_record() {
    let (_directory, data, mut store) = store();
    store
        .add_provider_profile(&declaration("cloud", "openai", "subscription"), 0)
        .expect("cloud");
    store
        .add_provider_profile(&declaration("bare", "lm_studio", "none"), 1)
        .expect("no endpoint");
    store
        .add_provider_profile(
            &with_endpoint("ollama", "ollama_local", "none", Some(OLLAMA_ENDPOINT)),
            2,
        )
        .expect("ollama");
    for name in ["cloud", "bare"] {
        assert!(
            matches!(
                store.select_active_provider(Some(name), 3),
                Err(ControlError::ProviderRefused(_))
            ),
            "{name}"
        );
    }
    assert!(matches!(
        store.select_active_provider(Some("missing"), 3),
        Err(ControlError::UnknownProvider)
    ));
    assert!(matches!(
        store.select_active_provider(None, 3),
        Err(ControlError::ProviderRefused(_))
    ));
    assert!(matches!(
        store.select_active_provider(Some("ollama"), 2),
        Err(ControlError::Conflict { .. })
    ));
    store
        .select_active_provider(Some("ollama"), 3)
        .expect("select");
    assert!(matches!(
        store.select_active_provider(Some("ollama"), 4),
        Err(ControlError::ProviderRefused(_))
    ));
    assert_eq!(store.revision(), 4);
    drop(store);
    let text = fs::read_to_string(data.join(FILE_NAME)).expect("journal text");
    assert_eq!(
        text.matches("active_provider_selected").count(),
        1,
        "{text}"
    );
}

/// Replay refuses a selection that names a missing or unselectable profile,
/// or that changes nothing.
#[test]
fn replay_rejects_an_invalid_active_connection() {
    let (_directory, data, mut store) = store();
    let cloud = store
        .add_provider_profile(&declaration("cloud", "openai", "subscription"), 0)
        .expect("cloud");
    let ollama = store
        .add_provider_profile(
            &with_endpoint("ollama", "ollama_local", "none", Some(OLLAMA_ENDPOINT)),
            1,
        )
        .expect("ollama");
    store
        .select_active_provider(Some("ollama"), 2)
        .expect("select");
    drop(store);
    let original = fs::read_to_string(data.join(FILE_NAME)).expect("journal");
    // Only the selection event names a profile with "profile_id"; the
    // profile events use "id".
    let selected = format!(r#""profile_id":"{}""#, ollama.id());
    assert_eq!(original.matches(&selected).count(), 1, "{original}");
    let cases = [
        format!(r#""profile_id":"{}""#, cloud.id()),
        format!(r#""profile_id":"{}""#, Uuid::new_v4()),
        r#""profile_id":null"#.to_owned(),
    ];
    for replacement in cases {
        let edited = original.replacen(&selected, &replacement, 1);
        let directory = tempfile::tempdir().expect("copy");
        let path = directory.path().join("data");
        drop(ControlStore::open(&path).expect("directory"));
        fs::write(path.join(FILE_NAME), edited).expect("edited fixture");
        let result = ControlStore::open(&path);
        assert!(
            matches!(result, Err(ControlError::Invalid(_))),
            "{replacement}: {:?}",
            result.err()
        );
    }
}

const CLOUD_ENDPOINT: &str = "https://ollama.com/v1";

/// An `ollama_cloud` profile accepts exactly the fixed cloud endpoint, with or
/// without one trailing slash, stores it as given, and replays it (ADR-0071).
#[test]
fn an_ollama_cloud_profile_records_exactly_the_cloud_endpoint_and_replays_it() {
    let (_directory, data, mut store) = store();
    for (revision, (name, endpoint)) in [
        ("cloud", CLOUD_ENDPOINT),
        ("slash", "https://ollama.com/v1/"),
    ]
    .into_iter()
    .enumerate()
    {
        let profile = store
            .add_provider_profile(
                &with_endpoint(name, "ollama_cloud", "api_key", Some(endpoint)),
                u64::try_from(revision).expect("revision"),
            )
            .expect("cloud profile");
        assert_eq!(
            profile.endpoint().map(ProviderEndpoint::as_str),
            Some(endpoint)
        );
    }
    drop(store);
    let store = ControlStore::open(&data).expect("replay");
    let status = store.provider_profile("slash").expect("profile");
    assert_eq!(
        status.profile().endpoint().map(ProviderEndpoint::as_str),
        Some("https://ollama.com/v1/")
    );
    assert_eq!(status.profile().kind(), ProviderKind::OllamaCloud);
    assert_eq!(status.profile().auth(), ProviderAuth::ApiKey);
}

/// Every other endpoint text is refused for `ollama_cloud` and writes no event.
#[test]
fn an_ollama_cloud_profile_refuses_every_other_endpoint() {
    let (_directory, _data, mut store) = store();
    let long = format!("https://ollama.com/v1/{}", "a".repeat(300));
    let refused = [
        "http://ollama.com/v1",
        "https://ollama.com",
        "https://ollama.com/",
        "https://ollama.com/v1//",
        "https://ollama.com/v1/extra",
        "https://ollama.com/v2",
        "https://ollama.com:443/v1",
        "https://ollama.com:8443/v1",
        "https://www.ollama.com/v1",
        "https://ollama.com.evil.example/v1",
        "https://user@ollama.com/v1",
        "https://user:pass@ollama.com/v1",
        "https://ollama.com/v1?x=1",
        "https://ollama.com/v1#fragment",
        "https://OLLAMA.COM/v1",
        "https://ollama.com/V1",
        "HTTPS://ollama.com/v1",
        " https://ollama.com/v1",
        "https://ollama.com/v1 ",
        "https://ollama.com/v1\u{7}",
        "https://127.0.0.1:1234/v1",
        ENDPOINT,
        "",
        long.as_str(),
    ];
    for endpoint in refused {
        let message = refusal(store.add_provider_profile(
            &with_endpoint("cloud", "ollama_cloud", "api_key", Some(endpoint)),
            0,
        ));
        assert!(
            message.contains("one documented endpoint") || message.contains("longer than 256"),
            "{endpoint:?}: {message}"
        );
    }
    assert_eq!(store.revision(), 0);
    // The cloud endpoint is refused for every other kind.
    for (kind, auth) in [
        ("lm_studio", "none"),
        ("ollama_local", "none"),
        ("openai", "api_key"),
        ("open_router", "api_key"),
    ] {
        assert!(
            matches!(
                store.add_provider_profile(
                    &with_endpoint("other", kind, auth, Some(CLOUD_ENDPOINT)),
                    0
                ),
                Err(ControlError::ProviderRefused(_))
            ),
            "{kind}"
        );
    }
    // A subscription is still refused for ollama_cloud.
    assert!(matches!(
        store.add_provider_profile(
            &with_endpoint(
                "cloud",
                "ollama_cloud",
                "subscription",
                Some(CLOUD_ENDPOINT)
            ),
            0
        ),
        Err(ControlError::ProviderRefused(_))
    ));
    assert_eq!(store.revision(), 0);
}

/// Replay applies the cloud rule, so an edited journal cannot point an
/// `ollama_cloud` profile at another host.
#[test]
fn replay_rejects_an_ollama_cloud_profile_with_another_endpoint() {
    let (_directory, data, mut store) = store();
    store
        .add_provider_profile(
            &with_endpoint("cloud", "ollama_cloud", "api_key", Some(CLOUD_ENDPOINT)),
            0,
        )
        .expect("profile");
    drop(store);
    let original = fs::read_to_string(data.join(FILE_NAME)).expect("journal");
    assert!(original.contains(CLOUD_ENDPOINT));
    for replacement in [
        "https://evil.example/v1",
        "http://ollama.com/v1",
        "http://127.0.0.1:1234/v1",
    ] {
        let edited = original.replacen(CLOUD_ENDPOINT, replacement, 1);
        let directory = tempfile::tempdir().expect("copy");
        let path = directory.path().join("data");
        drop(ControlStore::open(&path).expect("directory"));
        fs::write(path.join(FILE_NAME), edited).expect("edited fixture");
        let result = ControlStore::open(&path);
        assert!(
            matches!(result, Err(ControlError::Invalid(_))),
            "{replacement}: {:?}",
            result.err()
        );
    }
}

/// An `ollama_cloud` profile with the cloud endpoint is selectable once a
/// credential is bound; replay checks the endpoint rule only (ADR-0071).
#[test]
fn an_ollama_cloud_profile_is_selectable_and_replays_without_a_credential_check() {
    let (_directory, data, mut store) = store();
    store
        .add_provider_profile(
            &with_endpoint("cloud", "ollama_cloud", "api_key", Some(CLOUD_ENDPOINT)),
            0,
        )
        .expect("cloud");
    store
        .add_provider_profile(&declaration("bare", "ollama_cloud", "api_key"), 1)
        .expect("no endpoint");
    assert!(matches!(
        store.select_active_provider(Some("cloud"), 2),
        Err(ControlError::ProviderRefused(message)) if message.contains("bound credential")
    ));
    assert!(matches!(
        store.select_active_provider(Some("bare"), 2),
        Err(ControlError::ProviderRefused(_))
    ));
    assert_eq!(store.revision(), 2);
    // Binding needs the native store, which these tests do not open, so record
    // a binding in the snapshot as the selection check reads it.
    let binding = crate::control::CredentialBinding::new(
        crate::control::CredentialId::parse(&Uuid::new_v4().simple().to_string()).expect("id"),
        crate::control::CredentialBackend::WindowsDpapi,
    );
    store
        .snapshot
        .provider_profiles
        .iter_mut()
        .find(|status| status.profile().name() == "cloud")
        .expect("profile")
        .credential = Some(binding);
    store
        .select_active_provider(Some("cloud"), 2)
        .expect("select a bound cloud profile");
    assert_eq!(
        store
            .active_provider_profile()
            .map(|status| status.profile().kind()),
        Some(ProviderKind::OllamaCloud)
    );
    drop(store);
    // Replay has no credential store: the selection replays on the endpoint rule.
    let store = ControlStore::open(&data).expect("replay");
    assert_eq!(
        store
            .active_provider_profile()
            .map(|status| status.profile().name()),
        Some("cloud")
    );
}

/// Every kind except the two local ones is hosted, and every hosted kind but
/// `openai_compatible` has one fixed HTTPS endpoint (ADR-0092). Each spelling
/// round-trips through `parse`.
#[test]
fn every_hosted_kind_but_openai_compatible_has_one_fixed_https_endpoint() {
    use crate::provider::OutputLimitField;

    // Listed in full, so a new kind fails to compile until its field is chosen.
    const fn documented_output_field(kind: ProviderKind) -> OutputLimitField {
        match kind {
            ProviderKind::OpenAi
            | ProviderKind::Moonshot
            | ProviderKind::MiniMax
            | ProviderKind::XAi => OutputLimitField::MaxCompletionTokens,
            ProviderKind::LmStudio
            | ProviderKind::OllamaLocal
            | ProviderKind::OllamaCloud
            | ProviderKind::Anthropic
            | ProviderKind::Google
            | ProviderKind::Meta
            | ProviderKind::Tencent
            | ProviderKind::ThinkingMachines
            | ProviderKind::OpenRouter
            | ProviderKind::ZAi
            | ProviderKind::DeepSeek
            | ProviderKind::OpenAiCompatible => OutputLimitField::MaxTokens,
        }
    }

    assert_eq!(ProviderKind::ALL.len(), 16);
    for kind in ProviderKind::ALL {
        assert_eq!(ProviderKind::parse(kind.as_str()), Some(kind));
        assert_eq!(
            ProviderKind::parse(&kind.as_str().to_ascii_uppercase()),
            Some(kind)
        );
        assert_eq!(kind.is_hosted(), !kind.is_local(), "{kind:?}");
        match kind.fixed_endpoint() {
            Some(endpoint) => {
                assert!(kind.is_hosted(), "{kind:?}");
                assert!(endpoint.starts_with("https://"), "{kind:?}");
                // The kind's endpoint must be a row of the client's fixed table,
                // not a URL that only passes the typed-endpoint rule, and carry
                // the output field ADR-0092 records for that provider.
                let (_, field) = crate::provider::fixed_endpoint(endpoint)
                    .unwrap_or_else(|| panic!("{kind:?}: endpoint missing from the fixed table"));
                assert_eq!(field, documented_output_field(kind), "{kind:?}");
                assert!(
                    crate::provider::ModelEndpoint::parse(endpoint)
                        .is_ok_and(|parsed| parsed.is_hosted() && parsed.output_field() == field),
                    "{kind:?}"
                );
            }
            None => assert!(
                kind.is_local() || kind == ProviderKind::OpenAiCompatible,
                "{kind:?}"
            ),
        }
    }
    assert_eq!(ProviderKind::parse("grok"), None);
    // No two kinds share an endpoint, so a key's audience names one provider.
    let mut endpoints: Vec<&str> = ProviderKind::ALL
        .iter()
        .filter_map(|kind| kind.fixed_endpoint())
        .collect();
    let count = endpoints.len();
    endpoints.sort_unstable();
    endpoints.dedup();
    assert_eq!(endpoints.len(), count);
}

/// Each named hosted kind records exactly its fixed endpoint with `api_key`,
/// replays it, and refuses another provider's endpoint, a loopback endpoint,
/// and an operator-typed one.
#[test]
fn each_named_hosted_kind_records_only_its_own_endpoint() {
    let (_directory, data, mut store) = store();
    let mut revision = 0;
    for kind in ProviderKind::ALL {
        let Some(endpoint) = kind.fixed_endpoint() else {
            continue;
        };
        for wrong in [
            "https://provider-api.example.org/v1",
            ENDPOINT,
            if kind == ProviderKind::OpenAi {
                CLOUD_ENDPOINT
            } else {
                crate::provider::OPENAI_ENDPOINT
            },
        ] {
            let message = refusal(store.add_provider_profile(
                &with_endpoint("wrong", kind.as_str(), "api_key", Some(wrong)),
                revision,
            ));
            assert!(
                message.contains("one documented endpoint"),
                "{kind:?} {wrong}: {message}"
            );
        }
        let name = kind.as_str().replace('_', "-");
        store
            .add_provider_profile(
                &with_endpoint(&name, kind.as_str(), "api_key", Some(endpoint)),
                revision,
            )
            .unwrap_or_else(|error| panic!("{kind:?}: {error}"));
        revision += 1;
        if !matches!(kind, ProviderKind::OpenAi | ProviderKind::Anthropic) {
            assert!(
                matches!(
                    store.add_provider_profile(
                        &with_endpoint("sub", kind.as_str(), "subscription", Some(endpoint)),
                        revision,
                    ),
                    Err(ControlError::ProviderRefused(_))
                ),
                "{kind:?}"
            );
        }
    }
    assert_eq!(store.revision(), revision);
    drop(store);
    let store = ControlStore::open(&data).expect("replay");
    for kind in ProviderKind::ALL {
        if let Some(endpoint) = kind.fixed_endpoint() {
            let status = store
                .provider_profile(&kind.as_str().replace('_', "-"))
                .expect("replayed profile");
            assert_eq!(status.profile().kind(), kind);
            assert_eq!(
                status.profile().endpoint().map(ProviderEndpoint::as_str),
                Some(endpoint)
            );
        }
    }
}

/// An `openai_compatible` profile records the HTTPS base URL the operator
/// typed, exactly, and refuses plain HTTP, user information, queries, and text
/// the URL parser would rewrite (ADR-0092). Replay applies the same rule.
#[test]
fn an_openai_compatible_profile_records_an_https_base_url_under_the_custom_rule() {
    let (_directory, data, mut store) = store();
    let typed = "https://provider-api.example.org/v1";
    store
        .add_provider_profile(
            &with_endpoint("custom", "openai_compatible", "api_key", Some(typed)),
            0,
        )
        .expect("custom profile");
    for refused in [
        "http://provider-api.example.org/v1",
        "https://Provider-API.example.org/v1",
        "https://user@provider-api.example.org/v1",
        "https://provider-api.example.org/v1?key=1",
        "https://provider-api.example.org:443/v1",
        "https://provider-api.example.org/a/../v1",
        ENDPOINT,
        "",
    ] {
        let message = refusal(store.add_provider_profile(
            &with_endpoint("other", "openai_compatible", "api_key", Some(refused)),
            1,
        ));
        assert!(
            message.contains("openai_compatible endpoint"),
            "{refused:?}: {message}"
        );
    }
    for auth in ["none", "subscription"] {
        assert!(matches!(
            store.add_provider_profile(
                &with_endpoint("other", "openai_compatible", auth, Some(typed)),
                1
            ),
            Err(ControlError::ProviderRefused(_))
        ));
    }
    // Without a bound key it cannot be chosen.
    assert!(matches!(
        store.select_active_provider(Some("custom"), 1),
        Err(ControlError::ProviderRefused(message)) if message.contains("bound credential")
    ));
    assert_eq!(store.revision(), 1);
    drop(store);
    let original = fs::read_to_string(data.join(FILE_NAME)).expect("journal");
    let store = ControlStore::open(&data).expect("replay");
    assert_eq!(
        store
            .provider_profile("custom")
            .and_then(|status| status.profile().endpoint())
            .map(ProviderEndpoint::as_str),
        Some(typed)
    );
    drop(store);
    for replacement in [
        "http://provider-api.example.org/v1",
        "https://user@provider-api.example.org/v1",
    ] {
        let edited = original.replacen(typed, replacement, 1);
        let directory = tempfile::tempdir().expect("copy");
        let path = directory.path().join("data");
        drop(ControlStore::open(&path).expect("directory"));
        fs::write(path.join(FILE_NAME), edited).expect("edited fixture");
        assert!(
            matches!(ControlStore::open(&path), Err(ControlError::Invalid(_))),
            "{replacement}"
        );
    }
}
