//! Registry credential bindings against the real control journal and the real
//! native credential store. On Linux these need an unlocked Secret Service
//! default collection and fail without one.

use super::*;
use std::fs;
use std::path::{Path, PathBuf};

use crate::control::{
    FILE_NAME, PackageProfileDeclaration, ProviderProfileDeclaration,
    credential as provider_binding,
};
use crate::test_support::private_data_root;

const SENTINEL: &[u8] = b"omgcode-registry-sentinel-token-77c1";
const REGISTRY: &str = "https://registry.example.com/api/";
const CDN: &str = "https://cdn.example.com/files/";

struct Fixture {
    data: PathBuf,
    credentials: CredentialStore,
    _directory: tempfile::TempDir,
}

impl Fixture {
    fn store(&self) -> ControlStore {
        ControlStore::open(&self.data).expect("control store")
    }
}

/// A private data root with one two-origin package profile and one `api_key`
/// provider profile.
fn fixture() -> Fixture {
    let directory = tempfile::tempdir().expect("temporary directory");
    let data = directory.path().join("data");
    private_data_root(&data);
    let mut store = ControlStore::open(&data).expect("control store");
    let revision = store.revision();
    store
        .add_package_profile(
            &PackageProfileDeclaration {
                name: "registries".into(),
                origins: vec![REGISTRY.into(), CDN.into()],
            },
            revision,
        )
        .expect("package profile");
    let revision = store.revision();
    store
        .add_provider_profile(
            &ProviderProfileDeclaration {
                name: "keys".into(),
                kind: "openai".into(),
                auth: "api_key".into(),
                endpoint: None,
            },
            revision,
        )
        .expect("provider profile");
    drop(store);
    let credentials = CredentialStore::open(&data)
        .expect("a native credential store is required (on Linux, an unlocked Secret Service)");
    Fixture {
        data,
        credentials,
        _directory: directory,
    }
}

fn secret(bytes: &[u8]) -> Secret {
    Secret::new(bytes.to_vec()).expect("secret")
}

fn journal(data: &Path) -> Vec<u8> {
    fs::read(data.join(FILE_NAME)).expect("journal bytes")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn new_id() -> CredentialId {
    CredentialId::parse(&Uuid::new_v4().simple().to_string()).expect("id")
}

fn origin(text: &str) -> PackageOrigin {
    parse_origin(text).expect("origin")
}

fn profile_id(store: &ControlStore) -> Uuid {
    store
        .package_profile("registries")
        .expect("profile")
        .profile()
        .id()
}

/// Binding, rotating, and unbinding are revisioned, replayed, reported, and
/// never put secret bytes in the journal.
#[test]
fn bindings_are_recorded_rotated_replayed_and_removed_without_secret_bytes() {
    let fixture = fixture();
    let (data, credentials) = (&fixture.data, &fixture.credentials);
    let mut store = fixture.store();
    let revision = store.revision();
    // A differently spelled origin resolves to the declared one.
    let set = store
        .set_package_credential(
            credentials,
            "REGISTRIES",
            "https://Registry.Example.com:443/api",
            PackageAuth::Bearer,
            &secret(SENTINEL),
            revision,
        )
        .expect("set credential");
    assert!(set.replaced.is_none() && set.orphan.is_none());
    assert_eq!(set.origin, "https://registry.example.com:443/api/");
    assert_eq!(store.revision(), revision + 1);
    assert_eq!(
        credentials.load(set.binding.id()).expect("stored").expose(),
        SENTINEL
    );
    let status = store.package_profile("registries").expect("profile");
    let bound = status.credential_for(&origin(REGISTRY)).expect("bound");
    assert_eq!(bound.auth(), PackageAuth::Bearer);
    assert_eq!(bound.binding(), &set.binding);
    assert!(status.credential_for(&origin(CDN)).is_none());
    let encoded = serde_json::to_value(store.snapshot()).expect("snapshot");
    let shown = &encoded["package_profiles"][0]["credentials"][0];
    assert_eq!(shown["origin"], "https://registry.example.com:443/api/");
    assert_eq!(shown["auth"], "bearer");
    assert_eq!(shown["binding"]["credential_id"], set.binding.id().as_str());
    assert_eq!(shown["binding"]["backend"], credentials.backend().as_str());

    // Rotation: new identifier, new auth, old item deleted after the bind.
    let rotated = store
        .set_package_credential(
            credentials,
            "registries",
            REGISTRY,
            PackageAuth::Basic,
            &secret(b"alice:hunter2-sentinel"),
            revision + 1,
        )
        .expect("rotate");
    assert_eq!(rotated.replaced.as_ref(), Some(set.binding.id()));
    assert!(rotated.orphan.is_none());
    assert_ne!(rotated.binding.id(), set.binding.id());
    assert!(matches!(
        credentials.load(set.binding.id()),
        Err(CredentialError::NotFound)
    ));
    drop(store);

    let mut store = fixture.store();
    let status = store.package_profile("registries").expect("replayed");
    assert_eq!(status.credentials().len(), 1);
    assert_eq!(status.credentials()[0].auth(), PackageAuth::Basic);
    assert_eq!(status.credentials()[0].binding(), &rotated.binding);

    let revision = store.revision();
    let removal = store
        .remove_package_credential(credentials, "registries", REGISTRY, revision)
        .expect("remove");
    assert_eq!(&removal.removed, rotated.binding.id());
    assert!(removal.orphan.is_none());
    assert_eq!(store.revision(), revision + 1);
    assert!(matches!(
        credentials.load(rotated.binding.id()),
        Err(CredentialError::NotFound)
    ));
    assert!(matches!(
        store.remove_package_credential(credentials, "registries", REGISTRY, revision + 1),
        Err(PackageCredentialError::Control(
            ControlError::PackageProfileRefused(_)
        ))
    ));
    drop(store);

    let store = fixture.store();
    assert!(
        store
            .package_profile("registries")
            .expect("profile")
            .credentials()
            .is_empty()
    );
    let encoded = serde_json::to_value(store.snapshot()).expect("snapshot");
    assert!(encoded["package_profiles"][0].get("credentials").is_none());
    drop(store);
    let bytes = journal(data);
    assert!(!contains(&bytes, SENTINEL));
    assert!(!contains(&bytes, b"hunter2"));
}

/// Stale revisions, unknown profiles and origins, and invalid secrets are
/// refused before any native write.
#[test]
fn refused_bindings_store_nothing_and_record_nothing() {
    let fixture = fixture();
    let (data, credentials) = (&fixture.data, &fixture.credentials);
    let mut store = fixture.store();
    let entries = store.entries.len();
    let bytes = store.bytes;
    let revision = store.revision();
    let cases = [
        ("registries", REGISTRY, revision - 1),
        ("missing", REGISTRY, revision),
        ("registries", "https://other.example.com/", revision),
        // Same host, but a path the profile does not declare.
        (
            "registries",
            "https://registry.example.com/other/",
            revision,
        ),
        // Not an origin at all.
        (
            "registries",
            "https://registry.example.com/api/?x=1",
            revision,
        ),
        (
            "registries",
            "https://user:pw@registry.example.com/api/",
            revision,
        ),
    ];
    for (name, origin_text, expected) in cases {
        let result = store.set_package_credential(
            credentials,
            name,
            origin_text,
            PackageAuth::Bearer,
            &secret(SENTINEL),
            expected,
        );
        assert!(
            matches!(result, Err(PackageCredentialError::Control(_))),
            "{name} {origin_text}: {result:?}"
        );
    }
    assert_eq!(store.revision(), revision);
    assert_eq!(store.entries.len(), entries, "nothing was recorded");
    assert_eq!(store.bytes, bytes);
    drop(store);
    // On Windows the credential directory is created only by a store write,
    // so its absence shows that no native write happened.
    #[cfg(windows)]
    assert!(!data.join("credentials-v1").exists());
    #[cfg(not(windows))]
    let _ = data;
}

/// Secret validation: visible ASCII only, no controls, no edge whitespace,
/// bearer without any space, basic with a nonempty user and a colon.
#[test]
fn invalid_secrets_are_refused_before_anything_is_stored() {
    // Failure messages name the case index and scheme only: secret bytes and
    // validation results never reach test output.
    for (case, (auth, bytes)) in [
        (PackageAuth::Bearer, b"tok\ten".as_slice()),
        (PackageAuth::Bearer, b"tok\ren"),
        (PackageAuth::Bearer, b"tok\nen"),
        (PackageAuth::Bearer, b"tok\0en"),
        (PackageAuth::Bearer, b"tok\x7fen"),
        (PackageAuth::Bearer, "tok\u{e9}en".as_bytes()),
        (PackageAuth::Bearer, b" token"),
        (PackageAuth::Bearer, b"token "),
        (PackageAuth::Bearer, b"to ken"),
        (PackageAuth::Basic, b"nocolon"),
        (PackageAuth::Basic, b":password"),
        (PackageAuth::Basic, b" alice:pw"),
        (PackageAuth::Basic, b"alice:pw\n"),
        (PackageAuth::Basic, b"alice:pw "),
        (PackageAuth::Basic, "alice:p\u{e9}".as_bytes()),
    ]
    .into_iter()
    .enumerate()
    {
        let refused = matches!(
            validate_package_secret(auth, &secret(bytes)),
            Err(ControlError::PackageProfileRefused(_))
        );
        assert!(refused, "invalid case {case} ({auth:?}) was not refused");
    }
    for (case, (auth, bytes)) in [
        (PackageAuth::Bearer, b"abc.DEF_123-~+/=".as_slice()),
        (PackageAuth::Basic, b"alice:pass word"),
        (PackageAuth::Basic, b"a:"),
        (PackageAuth::Basic, b"a:b:c"),
    ]
    .into_iter()
    .enumerate()
    {
        let accepted = validate_package_secret(auth, &secret(bytes)).is_ok();
        assert!(accepted, "valid case {case} ({auth:?}) was refused");
    }
    // The largest accepted secret is valid; one more byte is refused by the
    // secret type itself.
    let largest = vec![b'a'; crate::control::MAX_SECRET_BYTES];
    validate_package_secret(PackageAuth::Bearer, &secret(&largest)).expect("largest");
    assert!(Secret::new(vec![b'a'; crate::control::MAX_SECRET_BYTES + 1]).is_err());

    // The refusal comes before the native store, the journal, and any item.
    let fixture = fixture();
    let (data, credentials) = (&fixture.data, &fixture.credentials);
    let mut store = fixture.store();
    let revision = store.revision();
    let entries = store.entries.len();
    let result = store.set_package_credential(
        credentials,
        "registries",
        REGISTRY,
        PackageAuth::Basic,
        &secret(b"no-colon-sentinel"),
        revision,
    );
    assert!(
        matches!(
            &result,
            Err(PackageCredentialError::Control(
                ControlError::PackageProfileRefused(message)
            )) if !message.contains("sentinel")
        ),
        "{result:?}"
    );
    assert_eq!(store.entries.len(), entries);
    drop(store);
    #[cfg(windows)]
    assert!(!data.join("credentials-v1").exists());
    #[cfg(not(windows))]
    let _ = data;
}

/// A binding write whose sync fails may already be on disk, so the new item
/// is kept and named, and the replaced item is not deleted.
#[test]
fn an_uncertain_binding_write_keeps_the_new_item_and_names_it() {
    let fixture = fixture();
    let (data, credentials) = (&fixture.data, &fixture.credentials);
    let mut store = fixture.store();
    let revision = store.revision();
    let first = store
        .set_package_credential(
            credentials,
            "registries",
            REGISTRY,
            PackageAuth::Bearer,
            &secret(b"first-token"),
            revision,
        )
        .expect("first");
    store.fail_next_sync = true;
    let result = store.set_package_credential(
        credentials,
        "registries",
        REGISTRY,
        PackageAuth::Bearer,
        &secret(SENTINEL),
        revision + 1,
    );
    let (kept, replaced) = match result {
        Err(PackageCredentialError::BindingUncertain {
            error: ControlError::Io(_),
            credential_id,
            replaced,
        }) => (credential_id, replaced),
        other => panic!("expected an uncertain binding: {other:?}"),
    };
    assert_eq!(replaced.as_ref(), Some(first.binding.id()));
    assert_eq!(credentials.load(&kept).expect("kept").expose(), SENTINEL);
    assert_eq!(
        credentials
            .load(first.binding.id())
            .expect("replaced item kept")
            .expose(),
        b"first-token"
    );
    drop(store);

    // The injected failure follows a completed write, so it is on disk.
    let mut store = fixture.store();
    let binding = store
        .package_profile("registries")
        .expect("profile")
        .credential_for(&origin(REGISTRY))
        .expect("uncertain write landed")
        .binding()
        .clone();
    assert_eq!(binding.id(), &kept);
    let revision = store.revision();
    let removal = store
        .remove_package_credential(credentials, "registries", REGISTRY, revision)
        .expect("remove the kept binding");
    assert!(removal.orphan.is_none());
    credentials
        .delete(first.binding.id())
        .expect("delete the replaced item by hand");
    drop(store);
    assert!(!contains(&journal(data), SENTINEL));
}

/// A binding append refused before any write records nothing and deletes the
/// new item.
#[test]
fn a_binding_refused_before_any_write_deletes_the_new_item() {
    let fixture = fixture();
    let (data, credentials) = (&fixture.data, &fixture.credentials);
    let mut store = fixture.store();
    let revision = store.revision();
    let entries = store.entries.len();
    // A full journal refuses the append before anything is written.
    store.bytes = crate::control::MAX_BYTES;
    let result = store.set_package_credential(
        credentials,
        "registries",
        REGISTRY,
        PackageAuth::Bearer,
        &secret(SENTINEL),
        revision,
    );
    assert!(
        matches!(
            result,
            Err(PackageCredentialError::Control(ControlError::LimitReached))
        ),
        "{result:?}"
    );
    assert_eq!(store.entries.len(), entries);
    drop(store);
    let store = fixture.store();
    assert_eq!(store.revision(), revision, "nothing was recorded");
    assert!(
        store
            .package_profile("registries")
            .expect("profile")
            .credentials()
            .is_empty()
    );
    #[cfg(windows)]
    assert_eq!(
        fs::read_dir(data.join("credentials-v1"))
            .expect("credential directory")
            .count(),
        0
    );
    #[cfg(not(windows))]
    let _ = data;
}

/// The native store failure categories map through the shared helpers to the
/// package error without losing the orphan identifier.
#[test]
fn shared_store_failures_keep_their_package_categories() {
    let id = new_id();
    let unstored = PackageCredentialError::from(ProviderCredentialError::Unstored {
        error: CredentialError::Io(std::io::Error::other("directory sync failed")),
        credential_id: id.clone(),
        cleanup: CredentialError::Locked,
    });
    assert!(
        matches!(&unstored, PackageCredentialError::Unstored { credential_id, .. } if *credential_id == id),
        "{unstored:?}"
    );
    let uncertain = PackageCredentialError::from(ProviderCredentialError::BindingUncertain {
        error: ControlError::Io(std::io::Error::other("sync failed")),
        credential_id: id.clone(),
        replaced: None,
    });
    let message = uncertain.to_string();
    assert!(message.contains(id.as_str()), "{message}");
    assert!(message.contains("package-credential-remove"), "{message}");
    assert!(!message.contains("provider-"), "{message}");
}

/// Removing a profile ends every one of its bindings in the same event.
#[test]
fn profile_removal_ends_its_bindings_in_the_same_event() {
    let fixture = fixture();
    let (data, credentials) = (&fixture.data, &fixture.credentials);
    let mut store = fixture.store();
    let mut revision = store.revision();
    let mut bound = Vec::new();
    for (origin_text, auth) in [(REGISTRY, PackageAuth::Bearer), (CDN, PackageAuth::Basic)] {
        let secret_bytes: &[u8] = if auth == PackageAuth::Basic {
            b"user:pw-sentinel"
        } else {
            SENTINEL
        };
        let set = store
            .set_package_credential(
                credentials,
                "registries",
                origin_text,
                auth,
                &secret(secret_bytes),
                revision,
            )
            .expect("set");
        bound.push(set.binding);
        revision += 1;
    }
    let removal = store
        .remove_package_profile("registries", revision)
        .expect("remove profile");
    assert_eq!(removal.credentials.len(), 2);
    for binding in &removal.credentials {
        assert!(bound.contains(binding));
        assert!(delete_unbound_credential(credentials, binding).is_none());
        assert!(matches!(
            credentials.load(binding.id()),
            Err(CredentialError::NotFound)
        ));
    }
    assert_eq!(store.revision(), revision + 1);
    drop(store);

    let store = ControlStore::open(data).expect("replay");
    assert!(store.package_profile("registries").is_none());
    drop(store);
    let text = String::from_utf8(journal(data)).expect("journal text");
    let removed = text
        .lines()
        .find(|line| line.contains("package_profile_removed"))
        .expect("removal event");
    for binding in &bound {
        assert!(removed.contains(binding.id().as_str()), "{removed}");
    }
    assert!(!text.contains("sentinel"));
}

/// A profile removal without bindings keeps the earlier journal shape, and an
/// earlier journal line (no `credential_ids`) still parses and replays.
#[test]
fn earlier_journals_without_the_new_fields_still_replay() {
    let fixture = fixture();
    let data = &fixture.data;
    let mut store = fixture.store();
    let revision = store.revision();
    let _ = store
        .remove_package_profile("registries", revision)
        .expect("remove unbound profile");
    drop(store);
    let text = String::from_utf8(journal(data)).expect("journal text");
    let removed = text
        .lines()
        .find(|line| line.contains("package_profile_removed"))
        .expect("removal event");
    assert!(!removed.contains("credential_ids"), "{removed}");
    assert!(
        ControlStore::open(data)
            .expect("replay")
            .package_profiles()
            .is_empty()
    );

    let earlier: Action = serde_json::from_value(serde_json::json!({
        "type": "package_profile_removed",
        "payload": { "profile_id": Uuid::new_v4() },
    }))
    .expect("earlier removal parses");
    assert!(matches!(
        earlier,
        Action::PackageProfileRemoved { credential_ids, .. } if credential_ids.is_empty()
    ));
    let start: crate::control::PackageFetchStart = serde_json::from_value(serde_json::json!({
        "fetch_id": Uuid::new_v4(),
        "workspace_id": Uuid::new_v4(),
        "profile_id": Uuid::new_v4(),
        "profile_name": "registries",
        "profile_revision": 2,
        "url_sha256": "0".repeat(64),
        "output_name": "pkg.bin",
        "max_bytes": 10,
        "timeout_ms": 1000,
    }))
    .expect("earlier fetch start parses");
    assert!(start.credential_id.is_none());
}

/// A load returns the bound secret and its scheme for the matched origin,
/// nothing for an unbound origin, and an error for a binding it cannot read.
#[test]
fn a_load_is_tied_to_the_matched_origin_and_never_falls_back() {
    let fixture = fixture();
    let credentials = &fixture.credentials;
    let mut store = fixture.store();
    let revision = store.revision();
    let set = store
        .set_package_credential(
            credentials,
            "registries",
            REGISTRY,
            PackageAuth::Basic,
            &secret(b"alice:pw"),
            revision,
        )
        .expect("set");
    let id = profile_id(&store);
    let loaded = store
        .load_package_credential(credentials, id, &origin(REGISTRY))
        .expect("load")
        .expect("bound");
    assert_eq!(loaded.auth(), PackageAuth::Basic);
    assert_eq!(loaded.credential_id(), set.binding.id());
    assert_eq!(loaded.secret().expose(), b"alice:pw");
    assert!(!format!("{loaded:?}").contains("alice"));
    // Another origin of the same profile has no binding and gets no secret.
    assert!(
        store
            .load_package_credential(credentials, id, &origin(CDN))
            .expect("load")
            .is_none()
    );
    assert!(matches!(
        store.load_package_credential(credentials, Uuid::new_v4(), &origin(REGISTRY)),
        Err(PackageCredentialUseError::UnknownProfile)
    ));

    // A bound item missing from the native store is an error, not `None`.
    credentials.delete(set.binding.id()).expect("delete item");
    assert!(matches!(
        store.load_package_credential(credentials, id, &origin(REGISTRY)),
        Err(PackageCredentialUseError::Store {
            error: CredentialError::NotFound,
            ..
        })
    ));

    // A binding recorded by another backend is never read from this one.
    let foreign = match credentials.backend() {
        CredentialBackend::WindowsDpapi => CredentialBackend::LinuxSecretService,
        CredentialBackend::LinuxSecretService => CredentialBackend::WindowsDpapi,
    };
    store.snapshot.package_profiles[0].credentials[0].binding =
        CredentialBinding::new(new_id(), foreign);
    assert!(matches!(
        store.load_package_credential(credentials, id, &origin(REGISTRY)),
        Err(PackageCredentialUseError::OtherBackend { .. })
    ));
}

fn bound_action(profile_id: Uuid, origin_text: &str, id: &CredentialId) -> Action {
    Action::PackageCredentialBound {
        profile_id,
        origin: origin(origin_text).render(),
        auth: PackageAuth::Bearer,
        binding: CredentialBinding::new(id.clone(), CredentialBackend::WindowsDpapi),
    }
}

/// Replay refuses package-credential events that break the binding rules.
#[test]
fn replay_rejects_invalid_package_credential_events() {
    let fixture = fixture();
    let store = fixture.store();
    let profile = profile_id(&store);
    let provider = store.provider_profile("keys").expect("keys").profile().id();
    let id = new_id();

    // An unknown profile, an undeclared origin, and a non-canonical spelling.
    let mut state = store.snapshot.clone();
    for action in [
        bound_action(Uuid::new_v4(), REGISTRY, &id),
        bound_action(profile, "https://other.example.com/", &id),
        Action::PackageCredentialBound {
            profile_id: profile,
            origin: "https://registry.example.com/api".to_owned(),
            auth: PackageAuth::Bearer,
            binding: CredentialBinding::new(id.clone(), CredentialBackend::WindowsDpapi),
        },
    ] {
        assert!(
            matches!(
                apply(&mut state.clone(), &action),
                Err(ControlError::Invalid(_))
            ),
            "{action:?}"
        );
    }

    apply(&mut state, &bound_action(profile, REGISTRY, &id)).expect("valid binding");
    // The same identifier cannot be rebound to the origin, shared with
    // another origin, or shared with a provider profile.
    for action in [
        bound_action(profile, REGISTRY, &id),
        bound_action(profile, CDN, &id),
    ] {
        assert!(
            matches!(
                apply(&mut state.clone(), &action),
                Err(ControlError::Invalid(_))
            ),
            "{action:?}"
        );
    }
    assert!(matches!(
        provider_binding::apply(
            &mut state.clone(),
            &Action::ProviderCredentialBound {
                profile_id: provider,
                binding: CredentialBinding::new(id.clone(), CredentialBackend::WindowsDpapi),
            }
        ),
        Err(ControlError::Invalid(_))
    ));
    // The reverse: an identifier a provider holds cannot be bound to an origin.
    let mut with_provider = store.snapshot.clone();
    let provider_id = new_id();
    provider_binding::apply(
        &mut with_provider,
        &Action::ProviderCredentialBound {
            profile_id: provider,
            binding: CredentialBinding::new(provider_id.clone(), CredentialBackend::WindowsDpapi),
        },
    )
    .expect("provider binding");
    assert!(matches!(
        apply(
            &mut with_provider,
            &bound_action(profile, CDN, &provider_id)
        ),
        Err(ControlError::Invalid(_))
    ));
    // A different identifier rotates the origin; at most one stays bound.
    let rotated = new_id();
    apply(&mut state, &bound_action(profile, REGISTRY, &rotated)).expect("rotation");
    assert_eq!(state.package_profiles[0].credentials.len(), 1);

    // A removal must name the recorded origin and identifier.
    let removed =
        |origin_text: &str, credential_id: &CredentialId| Action::PackageCredentialRemoved {
            profile_id: profile,
            origin: origin(origin_text).render(),
            credential_id: credential_id.clone(),
        };
    for action in [
        removed(REGISTRY, &id),
        removed(CDN, &rotated),
        Action::PackageCredentialRemoved {
            profile_id: Uuid::new_v4(),
            origin: origin(REGISTRY).render(),
            credential_id: rotated.clone(),
        },
    ] {
        assert!(
            matches!(
                apply(&mut state.clone(), &action),
                Err(ControlError::Invalid(_))
            ),
            "{action:?}"
        );
    }
    let mut after = state.clone();
    apply(&mut after, &removed(REGISTRY, &rotated)).expect("matching removal");
    assert!(after.package_profiles[0].credentials.is_empty());

    // A profile removal must name exactly its bound credentials.
    for credential_ids in [vec![], vec![id.clone()], vec![rotated.clone(), id.clone()]] {
        assert!(
            matches!(
                super::super::apply(
                    &mut state.clone(),
                    &Action::PackageProfileRemoved {
                        profile_id: profile,
                        credential_ids,
                    },
                    state.revision,
                ),
                Err(ControlError::Invalid(_))
            ),
            "profile removal must match its bindings"
        );
    }
    super::super::apply(
        &mut state.clone(),
        &Action::PackageProfileRemoved {
            profile_id: profile,
            credential_ids: vec![rotated],
        },
        state.revision,
    )
    .expect("matching profile removal");
}

/// A tampered journal is refused on open, and an unknown action is refused by
/// an older reader (the action enum denies unknown variants).
#[test]
fn a_tampered_journal_with_a_bad_binding_is_refused_on_open() {
    let fixture = fixture();
    let data = &fixture.data;
    let store = fixture.store();
    let profile = profile_id(&store);
    drop(store);
    // Append a well-formed event that binds an undeclared origin.
    let mut entry: serde_json::Value = serde_json::from_slice(
        journal(data)
            .split(|byte| *byte == b'\n')
            .rfind(|line| !line.is_empty())
            .expect("last line"),
    )
    .expect("entry");
    entry["sequence"] = serde_json::json!(entry["sequence"].as_u64().expect("sequence") + 1);
    entry["revision"] = serde_json::json!(entry["revision"].as_u64().expect("revision") + 1);
    entry["event_id"] = serde_json::json!(Uuid::new_v4());
    entry["action"] =
        serde_json::to_value(bound_action(profile, REGISTRY, &new_id())).expect("action");
    entry["action"]["origin"] = serde_json::json!("https://evil.example.com:443/api/");
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(data.join(FILE_NAME))
        .expect("journal");
    use std::io::Write as _;
    writeln!(file, "{entry}").expect("append");
    drop(file);
    assert!(
        matches!(ControlStore::open(data), Err(ControlError::Invalid(_))),
        "a binding for an undeclared origin must not replay"
    );
}
