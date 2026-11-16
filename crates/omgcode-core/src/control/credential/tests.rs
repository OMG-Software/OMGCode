//! Credential bindings against the real control journal and the real native
//! credential store. On Linux these need an unlocked Secret Service default
//! collection and fail without one.

use super::*;
use std::fs;
use std::path::{Path, PathBuf};

use crate::control::{FILE_NAME, ProviderProfileDeclaration};

const SENTINEL: &[u8] = b"omgcode-core-sentinel-secret-41d2e8";

struct Fixture {
    data: PathBuf,
    store: ControlStore,
    credentials: CredentialStore,
    _directory: tempfile::TempDir,
}

/// A data root the Windows private store accepts, holding a control journal
/// with one profile per authentication mode.
fn fixture() -> Fixture {
    let directory = tempfile::tempdir().expect("temporary directory");
    let data = directory.path().join("data");
    private_data_root(&data);
    let mut store = ControlStore::open(&data).expect("control store");
    for (name, kind, auth) in [
        ("keys", "openai", "api_key"),
        ("other", "open_router", "api_key"),
        ("plan", "anthropic", "subscription"),
        ("local", "ollama_local", "none"),
    ] {
        let revision = store.revision();
        store
            .add_provider_profile(
                &ProviderProfileDeclaration {
                    name: name.into(),
                    kind: kind.into(),
                    auth: auth.into(),
                },
                revision,
            )
            .expect("profile");
    }
    let credentials = CredentialStore::open(&data)
        .expect("a native credential store is required (on Linux, an unlocked Secret Service)");
    Fixture {
        data,
        store,
        credentials,
        _directory: directory,
    }
}

/// Windows temporary directories inherit grants the private store refuses,
/// so give the data root an owner-only ACL, as an installed profile has.
fn private_data_root(data: &Path) {
    fs::create_dir(data).expect("data root");
    #[cfg(windows)]
    {
        use std::process::Command;
        let output = Command::new("whoami")
            .args(["/user", "/fo", "csv", "/nh"])
            .output()
            .expect("whoami");
        assert!(output.status.success(), "whoami failed");
        let text = String::from_utf8(output.stdout).expect("UTF-8");
        let sid = text
            .trim()
            .rsplit(',')
            .next()
            .expect("SID column")
            .trim_matches('"')
            .to_owned();
        let status = Command::new("icacls")
            .arg(data)
            .args(["/inheritance:r", "/grant:r", &format!("*{sid}:(OI)(CI)F")])
            .output()
            .expect("icacls");
        assert!(status.status.success(), "icacls failed");
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

/// Binding and unbinding are revisioned, replayed, reported, and never put
/// secret bytes in the journal.
#[test]
fn bindings_are_recorded_replayed_and_removed_without_secret_bytes() {
    let Fixture {
        data,
        mut store,
        credentials,
        _directory,
    } = fixture();
    let revision = store.revision();
    let set = store
        .set_provider_credential(&credentials, "KEYS", &secret(SENTINEL), revision)
        .expect("set credential");
    assert!(set.replaced.is_none());
    assert!(set.orphan.is_none());
    assert_eq!(set.binding.backend(), credentials.backend());
    assert_eq!(store.revision(), revision + 1);
    assert_eq!(
        credentials.load(set.binding.id()).expect("stored").expose(),
        SENTINEL
    );

    let status = store.provider_profile("keys").expect("profile");
    assert_eq!(status.credential(), Some(&set.binding));
    let encoded = serde_json::to_value(store.snapshot()).expect("snapshot");
    let shown = &encoded["provider_profiles"][0]["credential"];
    assert_eq!(shown["state"], "bound");
    assert_eq!(shown["id"], set.binding.id().as_str());
    assert_eq!(shown["backend"], credentials.backend().as_str());
    assert_eq!(
        encoded["provider_profiles"][1]["credential"],
        serde_json::json!({"state": "none"})
    );
    drop(store);

    let bytes = journal(&data);
    assert!(!contains(&bytes, SENTINEL), "the journal holds no secret");
    assert!(contains(&bytes, b"provider_credential_bound"));
    assert!(contains(&bytes, set.binding.id().as_str().as_bytes()));

    // Replay restores the binding; removal is revisioned.
    let mut store = ControlStore::open(&data).expect("replay");
    assert_eq!(
        store
            .provider_profile("keys")
            .expect("profile")
            .credential(),
        Some(&set.binding)
    );
    assert!(matches!(
        store.remove_provider_credential(&credentials, "keys", revision),
        Err(ProviderCredentialError::Control(
            ControlError::Conflict { .. }
        ))
    ));
    let removal = store
        .remove_provider_credential(&credentials, "keys", revision + 1)
        .expect("remove credential");
    assert_eq!(&removal.removed, set.binding.id());
    assert!(removal.orphan.is_none());
    assert!(matches!(
        credentials.load(set.binding.id()),
        Err(CredentialError::NotFound)
    ));
    assert!(matches!(
        store.remove_provider_credential(&credentials, "keys", revision + 2),
        Err(ProviderCredentialError::Control(
            ControlError::ProviderRefused(_)
        ))
    ));
    drop(store);

    let store = ControlStore::open(&data).expect("replay removal");
    assert!(
        store
            .provider_profile("keys")
            .expect("profile")
            .credential()
            .is_none()
    );
    assert_eq!(store.revision(), revision + 2);
    drop(store);
    let bytes = journal(&data);
    assert!(contains(&bytes, b"provider_credential_removed"));
    assert!(!contains(&bytes, SENTINEL));
}

/// Rotation binds a fresh identifier and deletes the replaced native item.
#[test]
fn rotation_binds_a_new_identifier_and_deletes_the_old_item() {
    let Fixture {
        mut store,
        credentials,
        ..
    } = fixture();
    let revision = store.revision();
    let first = store
        .set_provider_credential(&credentials, "keys", &secret(b"first-key"), revision)
        .expect("first");
    let second = store
        .set_provider_credential(&credentials, "keys", &secret(b"second-key"), revision + 1)
        .expect("second");
    assert_ne!(first.binding.id(), second.binding.id());
    assert_eq!(second.replaced.as_ref(), Some(first.binding.id()));
    assert!(second.orphan.is_none());
    assert!(matches!(
        credentials.load(first.binding.id()),
        Err(CredentialError::NotFound)
    ));
    assert_eq!(
        credentials.load(second.binding.id()).expect("new").expose(),
        b"second-key"
    );
    let removal = store
        .remove_provider_credential(&credentials, "keys", revision + 2)
        .expect("cleanup");
    assert!(removal.orphan.is_none());
}

/// Stale revisions, unknown profiles, and non-API-key profiles are refused
/// before any native item is stored.
#[test]
fn refused_bindings_store_nothing_and_record_nothing() {
    let Fixture {
        data,
        mut store,
        credentials,
        _directory,
    } = fixture();
    let entries = store.entries.len();
    let bytes = store.bytes;
    let revision = store.revision();
    let cases = [
        ("keys", revision - 1),
        ("missing", revision),
        ("plan", revision),
        ("local", revision),
    ];
    for (name, expected) in cases {
        let result = store.set_provider_credential(&credentials, name, &secret(SENTINEL), expected);
        assert!(
            matches!(result, Err(ProviderCredentialError::Control(_))),
            "{name}: {result:?}"
        );
    }
    let plan = store.set_provider_credential(&credentials, "plan", &secret(SENTINEL), revision);
    assert!(
        matches!(
            &plan,
            Err(ProviderCredentialError::Control(ControlError::ProviderRefused(message)))
                if message.contains("subscription")
        ),
        "{plan:?}"
    );
    assert_eq!(store.revision(), revision);
    assert_eq!(store.entries.len(), entries, "nothing was recorded");
    assert_eq!(store.bytes, bytes);
    drop(store);
    assert_eq!(
        ControlStore::open(&data).expect("replay").revision(),
        revision
    );
    // On Windows the credential directory is created only by a store, so its
    // absence shows that no native write happened.
    #[cfg(windows)]
    assert!(!data.join("credentials-v1").exists());
}

/// A journal write that fails after the native store deletes the new item,
/// so no secret survives without the operator seeing an error.
#[test]
fn a_failed_binding_write_deletes_the_new_native_item() {
    let Fixture {
        data,
        mut store,
        credentials,
        _directory,
    } = fixture();
    let revision = store.revision();
    store.fail_next_sync = true;
    let result = store.set_provider_credential(&credentials, "keys", &secret(SENTINEL), revision);
    assert!(
        matches!(
            result,
            Err(ProviderCredentialError::Control(ControlError::Io(_)))
        ),
        "{result:?}"
    );
    drop(store);

    // The injected failure follows a completed write, so the binding is on
    // disk. Its item was deleted: the documented uncertain-write outcome is a
    // binding that loads as not found, never an unbound secret.
    let store = ControlStore::open(&data).expect("replay");
    let binding = store
        .provider_profile("keys")
        .expect("profile")
        .credential()
        .cloned()
        .expect("uncertain write landed");
    assert!(matches!(
        credentials.load(binding.id()),
        Err(CredentialError::NotFound)
    ));
    drop(store);
    assert!(!contains(&journal(&data), SENTINEL));
}

/// Removing a profile removes its binding in the same event, and the caller
/// deletes the native item afterwards.
#[test]
fn profile_removal_ends_its_binding_in_the_same_event() {
    let Fixture {
        data,
        mut store,
        credentials,
        _directory,
    } = fixture();
    let revision = store.revision();
    let set = store
        .set_provider_credential(&credentials, "keys", &secret(SENTINEL), revision)
        .expect("set");
    let removal = store
        .remove_provider_profile("keys", revision + 1)
        .expect("remove profile");
    assert_eq!(removal.credential.as_ref(), Some(&set.binding));
    assert_eq!(store.revision(), revision + 2);
    let binding = removal.credential.expect("bound credential");
    assert!(delete_unbound_credential(&credentials, &binding).is_none());
    assert!(matches!(
        credentials.load(binding.id()),
        Err(CredentialError::NotFound)
    ));
    // An absent item is not an orphan.
    assert!(delete_unbound_credential(&credentials, &binding).is_none());
    drop(store);

    let store = ControlStore::open(&data).expect("replay");
    assert!(store.provider_profile("keys").is_none());
    drop(store);
    let text = String::from_utf8(journal(&data)).expect("journal text");
    let removed = text
        .lines()
        .find(|line| line.contains("provider_profile_removed"))
        .expect("removal event");
    assert!(removed.contains(binding.id().as_str()), "{removed}");
}

/// A binding recorded by another backend is reported as an orphan, never
/// deleted from this store.
#[test]
fn another_backends_credential_is_an_orphan() {
    let Fixture { credentials, .. } = fixture();
    let foreign = match credentials.backend() {
        CredentialBackend::WindowsDpapi => CredentialBackend::LinuxSecretService,
        CredentialBackend::LinuxSecretService => CredentialBackend::WindowsDpapi,
    };
    let binding = CredentialBinding {
        id: CredentialId::parse(&Uuid::new_v4().simple().to_string()).expect("id"),
        backend: foreign,
    };
    let orphan = delete_unbound_credential(&credentials, &binding).expect("orphan");
    assert_eq!(&orphan.id, binding.id());
    assert!(matches!(orphan.error, CredentialError::Unavailable(_)));
}

/// Replay refuses credential events that break the binding rules.
#[test]
fn replay_rejects_invalid_credential_events() {
    let Fixture { store, .. } = fixture();
    let profile = |name: &str| {
        store
            .provider_profile(name)
            .expect("profile")
            .profile()
            .id()
    };
    let keys = profile("keys");
    let other = profile("other");
    let plan = profile("plan");
    let local = profile("local");
    let binding = CredentialBinding {
        id: CredentialId::parse(&Uuid::new_v4().simple().to_string()).expect("id"),
        backend: CredentialBackend::WindowsDpapi,
    };
    let bound = |profile_id| Action::ProviderCredentialBound {
        profile_id,
        binding: binding.clone(),
    };

    // Unknown, subscription, and keyless profiles cannot hold a credential.
    for profile_id in [Uuid::new_v4(), plan, local] {
        let mut state = store.snapshot.clone();
        assert!(
            matches!(
                apply(&mut state, &bound(profile_id)),
                Err(ControlError::Invalid(_))
            ),
            "{profile_id}"
        );
    }

    let mut state = store.snapshot.clone();
    apply(&mut state, &bound(keys)).expect("valid binding");
    // The same identifier cannot be rebound or shared with another profile.
    assert!(matches!(
        apply(&mut state.clone(), &bound(keys)),
        Err(ControlError::Invalid(_))
    ));
    assert!(matches!(
        apply(&mut state.clone(), &bound(other)),
        Err(ControlError::Invalid(_))
    ));
    // A removal must name the recorded binding.
    let other_id = CredentialId::parse(&Uuid::new_v4().simple().to_string()).expect("id");
    assert!(matches!(
        apply(
            &mut state.clone(),
            &Action::ProviderCredentialRemoved {
                profile_id: keys,
                credential_id: other_id.clone(),
            }
        ),
        Err(ControlError::Invalid(_))
    ));
    assert!(matches!(
        apply(
            &mut state.clone(),
            &Action::ProviderCredentialRemoved {
                profile_id: other,
                credential_id: binding.id.clone(),
            }
        ),
        Err(ControlError::Invalid(_))
    ));
    // A profile removal must name exactly its bound credential.
    for credential_id in [None, Some(other_id)] {
        assert!(matches!(
            super::super::provider::apply(
                &mut state.clone(),
                &Action::ProviderProfileRemoved {
                    profile_id: keys,
                    credential_id,
                },
                state.revision,
            ),
            Err(ControlError::Invalid(_))
        ));
    }
    assert!(matches!(
        super::super::provider::apply(
            &mut state.clone(),
            &Action::ProviderProfileRemoved {
                profile_id: other,
                credential_id: Some(binding.id.clone()),
            },
            state.revision,
        ),
        Err(ControlError::Invalid(_))
    ));
    super::super::provider::apply(
        &mut state.clone(),
        &Action::ProviderProfileRemoved {
            profile_id: keys,
            credential_id: Some(binding.id.clone()),
        },
        state.revision,
    )
    .expect("matching profile removal");
}

/// Recorded identifiers and backends are validated when a journal is parsed.
#[test]
fn recorded_bindings_reject_malformed_identifiers_and_backends() {
    let id = Uuid::new_v4().simple().to_string();
    let valid = serde_json::json!({"credential_id": id, "backend": "windows_dpapi"});
    let binding: CredentialBinding = serde_json::from_value(valid).expect("valid binding");
    assert_eq!(binding.id().as_str(), id);
    for invalid in [
        serde_json::json!({"credential_id": id.to_uppercase(), "backend": "windows_dpapi"}),
        serde_json::json!({"credential_id": "../escape", "backend": "windows_dpapi"}),
        serde_json::json!({"credential_id": id, "backend": "plaintext"}),
        serde_json::json!({"credential_id": id, "backend": "windows_dpapi", "secret": "x"}),
    ] {
        assert!(
            serde_json::from_value::<CredentialBinding>(invalid.clone()).is_err(),
            "{invalid}"
        );
    }
}
