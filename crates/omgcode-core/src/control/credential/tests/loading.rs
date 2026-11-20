//! Reading a bound credential for use (ADR-0047) against the real control
//! journal and the real native credential store.

use super::*;

const ENDPOINT: &str = "http://127.0.0.1:4242/v1";

/// Add an `lm_studio` profile with the given authentication mode and the
/// endpoint every selected profile needs (ADR-0048).
fn add_lm_studio(store: &mut ControlStore, name: &str, auth: &str) {
    add_lm_studio_with(store, name, auth, Some(ENDPOINT));
}

/// Add an `lm_studio` profile that may record no endpoint, as one recorded
/// before endpoints existed.
fn add_lm_studio_with(store: &mut ControlStore, name: &str, auth: &str, endpoint: Option<&str>) {
    let revision = store.revision();
    store
        .add_provider_profile(
            &ProviderProfileDeclaration {
                name: name.into(),
                kind: "lm_studio".into(),
                auth: auth.into(),
                endpoint: endpoint.map(str::to_owned),
            },
            revision,
        )
        .expect("lm_studio profile");
}

/// Give a recorded profile an endpoint without the add path, to reach the
/// checks that follow the endpoint check for a kind that cannot record one.
fn force_endpoint(store: &mut ControlStore, name: &str) {
    store
        .snapshot
        .provider_profiles
        .iter_mut()
        .find(|status| status.profile().name() == name)
        .expect("profile")
        .profile
        .endpoint = Some(ProviderEndpoint(ENDPOINT.to_owned()));
}

fn lm_studio(
    store: &ControlStore,
    credentials: &CredentialStore,
    name: &str,
) -> Result<Option<Secret>, ProviderCredentialUseError> {
    store
        .load_provider_credential(credentials, name, ProviderKind::LmStudio)
        .map(|connection| connection.into_parts().1)
}

/// A use of a stored credential returns exactly the stored bytes, records
/// nothing, and follows a rotation to the new secret.
#[test]
fn a_load_returns_the_stored_bytes_records_nothing_and_follows_rotation() {
    let fixture = fixture();
    let (data, credentials) = (&fixture.data, &fixture.credentials);
    let mut store = fixture.store();
    add_lm_studio(&mut store, "lms", "api_key");
    let revision = store.revision();
    let set = store
        .set_provider_credential(credentials, "lms", &secret(SENTINEL), revision)
        .expect("set");
    let revision = store.revision();
    let entries = store.entries.len();
    let length = store.bytes;
    // The journal is locked while the store is open, so compare bytes through
    // the store's own accounting and, after a reopen, through the file.
    let loaded = lm_studio(&store, credentials, "LMS")
        .expect("load")
        .expect("a secret");
    assert_eq!(loaded.expose(), SENTINEL);
    assert_eq!(format!("{loaded:?}"), "Secret(<redacted>)");
    assert_eq!(store.revision(), revision, "a read changes no revision");
    assert_eq!(store.entries.len(), entries);
    assert_eq!(store.bytes, length, "a read appends nothing");
    drop(store);
    let bytes_before = journal(data);
    assert_eq!(bytes_before.len(), length);
    let store = fixture.store();
    // The lookup is case-insensitive, like every profile lookup.
    assert!(lm_studio(&store, credentials, "lms").is_ok());
    drop(store);
    assert_eq!(journal(data), bytes_before, "a read appends nothing");
    let mut store = fixture.store();

    let rotated = store
        .set_provider_credential(credentials, "lms", &secret(b"rotated-key"), revision)
        .expect("rotate");
    assert_ne!(rotated.binding.id(), set.binding.id());
    let loaded = lm_studio(&store, credentials, "lms")
        .expect("load")
        .expect("a secret");
    assert_eq!(loaded.expose(), b"rotated-key");

    let revision = store.revision();
    let removal = store
        .remove_provider_credential(credentials, "lms", revision)
        .expect("cleanup");
    assert!(removal.orphan.is_none());
}

/// Each refusal is typed and carries a stable code. With the native item
/// deleted, a wrong-provider answer proves no load happened, because a load
/// would have reported the missing item instead.
#[test]
fn refusals_are_typed_and_precede_any_native_load() {
    let fixture = fixture();
    let credentials = &fixture.credentials;
    let mut store = fixture.store();
    add_lm_studio(&mut store, "lms-none", "none");
    add_lm_studio(&mut store, "lms-unbound", "api_key");

    let error = lm_studio(&store, credentials, "missing").expect_err("unknown");
    assert!(matches!(
        error,
        ProviderCredentialUseError::UnknownProfile { .. }
    ));
    assert_eq!(error.code(), "unknown_profile");

    // An `openai` key is never read for the LM Studio adapter. Its native item
    // is gone, so a load would have reported not-found instead.
    let revision = store.revision();
    let set = store
        .set_provider_credential(credentials, "keys", &secret(SENTINEL), revision)
        .expect("openai key");
    credentials.delete(set.binding.id()).expect("delete item");
    let error = lm_studio(&store, credentials, "keys").expect_err("wrong provider");
    assert!(
        matches!(
            error,
            ProviderCredentialUseError::WrongProvider {
                expected: ProviderKind::LmStudio,
                actual: ProviderKind::OpenAi,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(error.code(), "wrong_provider");
    assert!(error.to_string().contains("openai"), "{error}");
    // The kind check precedes the authentication check, so a keyless profile
    // of another kind is refused too.
    assert_eq!(
        lm_studio(&store, credentials, "local")
            .expect_err("kind")
            .code(),
        "wrong_provider"
    );

    // A `none` profile of the asking kind yields no credential.
    assert!(
        lm_studio(&store, credentials, "lms-none")
            .expect("none")
            .is_none()
    );

    // No LM Studio profile can be a subscription, so ask as the provider
    // whose recorded profile is one. That kind cannot record an endpoint, so
    // the endpoint check (which precedes the authentication check) refuses it
    // first; `force_endpoint` then reaches the subscription refusal.
    let error = store
        .load_provider_credential(credentials, "plan", ProviderKind::Anthropic)
        .expect_err("no endpoint");
    assert_eq!(error.code(), "endpoint_not_recorded");
    force_endpoint(&mut store, "plan");
    let error = store
        .load_provider_credential(credentials, "plan", ProviderKind::Anthropic)
        .expect_err("subscription");
    assert!(matches!(
        error,
        ProviderCredentialUseError::SubscriptionUnsupported { .. }
    ));
    assert_eq!(error.code(), "subscription_unsupported");

    // An `api_key` profile with no binding is an error, never `None`.
    let error = lm_studio(&store, credentials, "lms-unbound").expect_err("unbound");
    assert!(matches!(error, ProviderCredentialUseError::NotBound { .. }));
    assert_eq!(error.code(), "not_bound");

    // A binding by another backend is refused before any load.
    let foreign = match credentials.backend() {
        CredentialBackend::WindowsDpapi => CredentialBackend::LinuxSecretService,
        CredentialBackend::LinuxSecretService => CredentialBackend::WindowsDpapi,
    };
    let foreign_id = CredentialId::parse(&Uuid::new_v4().simple().to_string()).expect("id");
    store
        .snapshot
        .provider_profiles
        .iter_mut()
        .find(|status| status.profile().name() == "lms-unbound")
        .expect("profile")
        .credential = Some(CredentialBinding {
        id: foreign_id.clone(),
        backend: foreign,
    });
    let error = lm_studio(&store, credentials, "lms-unbound").expect_err("backend");
    assert_eq!(error.code(), "other_backend");
    assert!(error.to_string().contains(foreign_id.as_str()), "{error}");
}

/// A binding whose native item is gone reports its category and identifier.
#[test]
fn a_missing_native_item_is_a_typed_not_found() {
    let fixture = fixture();
    let credentials = &fixture.credentials;
    let mut store = fixture.store();
    add_lm_studio(&mut store, "lms", "api_key");
    let revision = store.revision();
    let set = store
        .set_provider_credential(credentials, "lms", &secret(SENTINEL), revision)
        .expect("set");
    credentials.delete(set.binding.id()).expect("delete item");
    let error = lm_studio(&store, credentials, "lms").expect_err("not found");
    assert!(
        matches!(
            &error,
            ProviderCredentialUseError::Store {
                error: CredentialError::NotFound,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(error.code(), "credential_not_found");
    let text = error.to_string();
    assert!(text.contains(set.binding.id().as_str()), "{text}");
    assert!(text.contains("lms"), "{text}");
    assert!(!text.contains("omgcode-core-sentinel"), "{text}");
    assert!(!format!("{error:?}").contains("omgcode-core-sentinel"));
}

/// An `ollama_cloud` profile is a stored-credential adapter beside
/// `lm_studio` (ADR-0069): it returns its recorded endpoint with the secret,
/// is refused for the wrong adapter in both directions, and is refused with
/// the typed cause when it records no endpoint or has no binding.
#[test]
fn an_ollama_cloud_profile_binds_and_loads_like_lm_studio() {
    const CLOUD: &str = "https://ollama.com/v1";
    let fixture = fixture();
    let credentials = &fixture.credentials;
    let mut store = fixture.store();
    let cloud = |name: &str, endpoint: Option<&str>| ProviderProfileDeclaration {
        name: name.into(),
        kind: "ollama_cloud".into(),
        auth: "api_key".into(),
        endpoint: endpoint.map(str::to_owned),
    };
    for declaration in [
        cloud("cloud", Some(CLOUD)),
        cloud("cloud-unbound", Some(CLOUD)),
        cloud("cloud-bare", None),
    ] {
        let revision = store.revision();
        store
            .add_provider_profile(&declaration, revision)
            .expect("cloud profile");
    }
    add_lm_studio(&mut store, "lms", "api_key");
    let ask = |store: &ControlStore, name: &str, adapter: ProviderKind| {
        store
            .provider_credential_binding(name, adapter)
            .map(|connection| {
                (
                    connection.endpoint().as_str().to_owned(),
                    connection.binding().is_some(),
                )
            })
    };

    // Refusals decide before any native access, in the documented order.
    let error = ask(&store, "cloud-bare", ProviderKind::OllamaCloud).expect_err("no endpoint");
    assert_eq!(error.code(), "endpoint_not_recorded");
    let error = ask(&store, "cloud-unbound", ProviderKind::OllamaCloud).expect_err("unbound");
    assert_eq!(error.code(), "not_bound");
    let error = ask(&store, "cloud", ProviderKind::LmStudio).expect_err("wrong adapter");
    assert_eq!(error.code(), "wrong_provider");
    let error = ask(&store, "lms", ProviderKind::OllamaCloud).expect_err("wrong adapter");
    assert_eq!(error.code(), "wrong_provider");

    let revision = store.revision();
    let set = store
        .set_provider_credential(credentials, "cloud", &secret(SENTINEL), revision)
        .expect("set");
    assert_eq!(
        ask(&store, "cloud", ProviderKind::OllamaCloud).expect("binding"),
        (CLOUD.to_owned(), true)
    );
    let loaded = store
        .load_provider_credential(credentials, "cloud", ProviderKind::OllamaCloud)
        .expect("load");
    let (endpoint, secret_value) = loaded.into_parts();
    assert_eq!(endpoint, CLOUD);
    assert_eq!(
        store
            .provider_profile("cloud")
            .and_then(|status| status.credential()),
        Some(&set.binding)
    );
    assert_eq!(secret_value.expect("a secret").expose(), SENTINEL);
    // The LM Studio adapter still cannot read the cloud profile's key.
    assert_eq!(
        lm_studio(&store, credentials, "cloud")
            .expect_err("wrong adapter")
            .code(),
        "wrong_provider"
    );
    // A bound cloud profile is selectable; an unbound one is not.
    assert!(store.check_provider_selectable("cloud").is_ok());
    assert!(matches!(
        store.check_provider_selectable("cloud-unbound"),
        Err(ControlError::ProviderRefused(_))
    ));
    assert!(matches!(
        store.check_provider_selectable("cloud-bare"),
        Err(ControlError::ProviderRefused(_))
    ));
    let revision = store.revision();
    let removal = store
        .remove_provider_credential(credentials, "cloud", revision)
        .expect("cleanup");
    assert!(removal.orphan.is_none());
}

/// A poisoned store refuses a read before it looks at any profile.
#[test]
fn a_poisoned_store_refuses_a_read() {
    let fixture = fixture();
    let mut store = fixture.store();
    add_lm_studio(&mut store, "lms", "none");
    store.poisoned = true;
    let error = lm_studio(&store, &fixture.credentials, "lms").expect_err("poisoned");
    assert!(matches!(
        error,
        ProviderCredentialUseError::Settings(ControlError::Poisoned)
    ));
    assert_eq!(error.code(), "settings_unusable");
}

/// Native categories map to distinct, published codes.
#[test]
fn use_error_codes_are_stable() {
    let id = CredentialId::parse(&Uuid::new_v4().simple().to_string()).expect("id");
    let store_error = |error| ProviderCredentialUseError::Store {
        profile: "p".into(),
        credential_id: id.clone(),
        error,
    };
    let cases = [
        (CredentialError::NotFound, "credential_not_found"),
        (CredentialError::Locked, "credential_locked"),
        (
            CredentialError::Unavailable("x".into()),
            "credential_unavailable",
        ),
        (CredentialError::Corrupt("x"), "credential_corrupt"),
        (
            CredentialError::PromptRequired,
            "credential_prompt_required",
        ),
        (
            CredentialError::Io(std::io::Error::other("x")),
            "credential_io",
        ),
        (CredentialError::TooLarge, "credential_refused"),
    ];
    for (error, code) in cases {
        assert_eq!(store_error(error).code(), code);
    }
}

/// The binding check decides every early outcome with no native store open:
/// nothing here constructs a `CredentialStore`. Each case that would hit two
/// refusals proves the order: unknown profile, kind, endpoint, then the
/// authentication checks.
#[test]
fn the_binding_check_decides_without_any_native_store() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let data = directory.path().join("data");
    private_data_root(&data);
    let mut store = ControlStore::open(&data).expect("control store");
    add_lm_studio(&mut store, "lms-key", "api_key");
    add_lm_studio(&mut store, "lms-none", "none");
    add_lm_studio(&mut store, "lms-unbound", "api_key");
    // Recorded without an endpoint, as before endpoints existed. The
    // api_key one is also unbound, so it would hit two refusals.
    add_lm_studio_with(&mut store, "old-none", "none", None);
    add_lm_studio_with(&mut store, "old-key", "api_key", None);
    for (name, kind, auth) in [
        ("keys", "openai", "api_key"),
        ("plan", "anthropic", "subscription"),
    ] {
        let revision = store.revision();
        store
            .add_provider_profile(
                &ProviderProfileDeclaration {
                    name: name.into(),
                    kind: kind.into(),
                    auth: auth.into(),
                    endpoint: None,
                },
                revision,
            )
            .expect("profile");
    }
    let binding = CredentialBinding {
        id: CredentialId::parse(&Uuid::new_v4().simple().to_string()).expect("id"),
        backend: CredentialBackend::WindowsDpapi,
    };
    store
        .snapshot
        .provider_profiles
        .iter_mut()
        .find(|status| status.profile().name() == "lms-key")
        .expect("profile")
        .credential = Some(binding.clone());
    let revision = store.revision();
    let entries = store.entries.len();

    {
        let check = |name: &str, adapter| store.provider_credential_binding(name, adapter);
        let code = |name: &str, adapter| check(name, adapter).expect_err("refused").code();
        assert_eq!(code("missing", ProviderKind::LmStudio), "unknown_profile");
        assert_eq!(code("keys", ProviderKind::LmStudio), "wrong_provider");
        assert_eq!(code("lms-none", ProviderKind::OpenAi), "wrong_provider");
        // The kind check precedes the endpoint check: these profiles have no
        // endpoint and are also for another kind.
        assert_eq!(code("old-none", ProviderKind::OpenAi), "wrong_provider");
        assert_eq!(code("old-key", ProviderKind::OpenAi), "wrong_provider");
        // The endpoint check precedes every authentication check: `none`,
        // `api_key` that is unbound, and `subscription` all report it.
        assert_eq!(
            code("old-none", ProviderKind::LmStudio),
            "endpoint_not_recorded"
        );
        assert_eq!(
            code("old-key", ProviderKind::LmStudio),
            "endpoint_not_recorded"
        );
        assert_eq!(
            code("plan", ProviderKind::Anthropic),
            "endpoint_not_recorded"
        );
        let error = check("OLD-KEY", ProviderKind::LmStudio).expect_err("refused");
        let text = error.to_string();
        assert!(text.contains("old-key"), "{text}");
        assert!(text.contains("no recorded endpoint"), "{text}");
        assert!(text.contains("add it again with an endpoint"), "{text}");
        assert_eq!(code("lms-unbound", ProviderKind::LmStudio), "not_bound");

        let none = check("LMS-NONE", ProviderKind::LmStudio).expect("none");
        assert_eq!(none.endpoint().as_str(), ENDPOINT);
        assert!(none.binding().is_none());
        let bound = check("lms-key", ProviderKind::LmStudio).expect("bound");
        assert_eq!(bound.endpoint().as_str(), ENDPOINT);
        assert_eq!(bound.binding(), Some(&binding));
    }
    // The authentication checks follow once an endpoint exists.
    force_endpoint(&mut store, "plan");
    assert_eq!(
        store
            .provider_credential_binding("plan", ProviderKind::Anthropic)
            .expect_err("subscription")
            .code(),
        "subscription_unsupported"
    );
    assert_eq!(store.revision(), revision);
    assert_eq!(store.entries.len(), entries, "a check records nothing");

    store.poisoned = true;
    assert_eq!(
        store
            .provider_credential_binding("lms-none", ProviderKind::LmStudio)
            .expect_err("poisoned")
            .code(),
        "settings_unusable"
    );
}

/// The loaded connection carries the endpoint the decision returned, exactly
/// as recorded, for both a keyless and a keyed profile, and a recorded
/// endpoint is not a secret while the secret never prints.
#[test]
fn a_load_returns_the_recorded_endpoint_with_the_secret() {
    let fixture = fixture();
    let credentials = &fixture.credentials;
    let mut store = fixture.store();
    add_lm_studio(&mut store, "lms-none", "none");
    add_lm_studio_with(&mut store, "lms", "api_key", Some("http://[::1]:4343/v1/"));
    add_lm_studio_with(&mut store, "old", "api_key", None);
    let revision = store.revision();
    let _ = store
        .set_provider_credential(credentials, "lms", &secret(SENTINEL), revision)
        .expect("set");
    let before = store.revision();

    let connection = store
        .load_provider_credential(credentials, "LMS", ProviderKind::LmStudio)
        .expect("load");
    assert_eq!(connection.endpoint(), "http://[::1]:4343/v1/");
    assert_eq!(connection.secret().expect("secret").expose(), SENTINEL);
    let printed = format!("{connection:?}");
    assert!(printed.contains("http://[::1]:4343/v1/"), "{printed}");
    assert!(printed.contains("<redacted>"), "{printed}");
    assert!(!printed.contains("omgcode-core-sentinel"), "{printed}");
    let (endpoint, secret_value) = connection.into_parts();
    assert_eq!(endpoint, "http://[::1]:4343/v1/");
    assert_eq!(secret_value.expect("secret").expose(), SENTINEL);

    let none = store
        .load_provider_credential(credentials, "lms-none", ProviderKind::LmStudio)
        .expect("none");
    assert_eq!(none.endpoint(), ENDPOINT);
    assert!(none.secret().is_none());

    // No endpoint: refused before the native store is read, even though a
    // keyed profile would otherwise report that nothing is bound.
    let error = store
        .load_provider_credential(credentials, "old", ProviderKind::LmStudio)
        .expect_err("no endpoint");
    assert_eq!(error.code(), "endpoint_not_recorded");
    assert_eq!(store.revision(), before);

    let revision = store.revision();
    let removal = store
        .remove_provider_credential(credentials, "lms", revision)
        .expect("cleanup");
    assert!(removal.orphan.is_none());
}
