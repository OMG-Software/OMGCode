//! Credential bindings against the real control journal and the real native
//! credential store. On Linux these need an unlocked Secret Service default
//! collection and fail without one.

use super::*;
use std::fs;
use std::path::{Path, PathBuf};

use crate::control::{FILE_NAME, ProviderProfileDeclaration};

const SENTINEL: &[u8] = b"omgcode-core-sentinel-secret-41d2e8";

/// A private data root with a prepared control journal and an open native
/// store. Fields drop in order, so the native store closes before the
/// temporary directory is removed. Tests bind the fixture first and open
/// their control store after it, so that store also closes first.
struct Fixture {
    data: PathBuf,
    credentials: CredentialStore,
    _directory: tempfile::TempDir,
}

impl Fixture {
    /// Open the fixture's control journal.
    fn store(&self) -> ControlStore {
        ControlStore::open(&self.data).expect("control store")
    }
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
    drop(store);
    let credentials = CredentialStore::open(&data)
        .expect("a native credential store is required (on Linux, an unlocked Secret Service)");
    Fixture {
        data,
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
    let fixture = fixture();
    let (data, credentials) = (&fixture.data, &fixture.credentials);
    let mut store = fixture.store();
    let revision = store.revision();
    let set = store
        .set_provider_credential(credentials, "KEYS", &secret(SENTINEL), revision)
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

    let bytes = journal(data);
    assert!(!contains(&bytes, SENTINEL), "the journal holds no secret");
    assert!(contains(&bytes, b"provider_credential_bound"));
    assert!(contains(&bytes, set.binding.id().as_str().as_bytes()));

    // Replay restores the binding; removal is revisioned.
    let mut store = ControlStore::open(data).expect("replay");
    assert_eq!(
        store
            .provider_profile("keys")
            .expect("profile")
            .credential(),
        Some(&set.binding)
    );
    assert!(matches!(
        store.remove_provider_credential(credentials, "keys", revision),
        Err(ProviderCredentialError::Control(
            ControlError::Conflict { .. }
        ))
    ));
    let removal = store
        .remove_provider_credential(credentials, "keys", revision + 1)
        .expect("remove credential");
    assert_eq!(&removal.removed, set.binding.id());
    assert!(removal.orphan.is_none());
    assert!(matches!(
        credentials.load(set.binding.id()),
        Err(CredentialError::NotFound)
    ));
    assert!(matches!(
        store.remove_provider_credential(credentials, "keys", revision + 2),
        Err(ProviderCredentialError::Control(
            ControlError::ProviderRefused(_)
        ))
    ));
    drop(store);

    let store = ControlStore::open(data).expect("replay removal");
    assert!(
        store
            .provider_profile("keys")
            .expect("profile")
            .credential()
            .is_none()
    );
    assert_eq!(store.revision(), revision + 2);
    drop(store);
    let bytes = journal(data);
    assert!(contains(&bytes, b"provider_credential_removed"));
    assert!(!contains(&bytes, SENTINEL));
}

/// Rotation binds a fresh identifier and deletes the replaced native item.
#[test]
fn rotation_binds_a_new_identifier_and_deletes_the_old_item() {
    let fixture = fixture();
    let credentials = &fixture.credentials;
    let mut store = fixture.store();
    let revision = store.revision();
    let first = store
        .set_provider_credential(credentials, "keys", &secret(b"first-key"), revision)
        .expect("first");
    let second = store
        .set_provider_credential(credentials, "keys", &secret(b"second-key"), revision + 1)
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
        .remove_provider_credential(credentials, "keys", revision + 2)
        .expect("cleanup");
    assert!(removal.orphan.is_none());
}

/// Stale revisions, unknown profiles, and non-API-key profiles are refused
/// before any native item is stored.
#[test]
fn refused_bindings_store_nothing_and_record_nothing() {
    let fixture = fixture();
    let (data, credentials) = (&fixture.data, &fixture.credentials);
    let mut store = fixture.store();
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
        let result = store.set_provider_credential(credentials, name, &secret(SENTINEL), expected);
        assert!(
            matches!(result, Err(ProviderCredentialError::Control(_))),
            "{name}: {result:?}"
        );
    }
    let plan = store.set_provider_credential(credentials, "plan", &secret(SENTINEL), revision);
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
        ControlStore::open(data).expect("replay").revision(),
        revision
    );
    // On Windows the credential directory is created only by a store, so its
    // absence shows that no native write happened.
    #[cfg(windows)]
    assert!(!data.join("credentials-v1").exists());
}

/// A binding write whose sync fails may already be on disk, so the new item
/// is kept and named, and the replaced item is not deleted. A recorded
/// binding never names an item that OMGCode deleted.
#[test]
fn an_uncertain_binding_write_keeps_the_new_item_and_names_it() {
    let fixture = fixture();
    let (data, credentials) = (&fixture.data, &fixture.credentials);
    let mut store = fixture.store();
    let revision = store.revision();
    let first = store
        .set_provider_credential(credentials, "keys", &secret(b"first-key"), revision)
        .expect("first");
    store.fail_next_sync = true;
    let result =
        store.set_provider_credential(credentials, "keys", &secret(SENTINEL), revision + 1);
    let (kept, replaced) = match result {
        Err(ProviderCredentialError::BindingUncertain {
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
        b"first-key"
    );
    drop(store);

    // The injected failure follows a completed write, so the binding is on
    // disk, and it names the kept item.
    let mut store = fixture.store();
    let binding = store
        .provider_profile("keys")
        .expect("profile")
        .credential()
        .cloned()
        .expect("uncertain write landed");
    assert_eq!(binding.id(), &kept);
    assert_eq!(
        credentials.load(binding.id()).expect("bound item").expose(),
        SENTINEL
    );

    let revision = store.revision();
    let removal = store
        .remove_provider_credential(credentials, "keys", revision)
        .expect("remove the kept binding");
    assert!(removal.orphan.is_none());
    credentials
        .delete(first.binding.id())
        .expect("delete the replaced item by hand");
    drop(store);
    assert!(!contains(&journal(data), SENTINEL));
}

/// A binding append refused before any write records nothing and deletes
/// the new item.
#[test]
fn a_binding_refused_before_any_write_deletes_the_new_item() {
    let fixture = fixture();
    let (data, credentials) = (&fixture.data, &fixture.credentials);
    let mut store = fixture.store();
    let revision = store.revision();
    let entries = store.entries.len();
    // A full journal refuses the append before anything is written.
    store.bytes = super::super::MAX_BYTES;
    let result = store.set_provider_credential(credentials, "keys", &secret(SENTINEL), revision);
    assert!(
        matches!(
            result,
            Err(ProviderCredentialError::Control(ControlError::LimitReached))
        ),
        "{result:?}"
    );
    assert_eq!(store.entries.len(), entries);
    drop(store);
    let store = fixture.store();
    assert_eq!(store.revision(), revision, "nothing was recorded");
    assert!(
        store
            .provider_profile("keys")
            .expect("profile")
            .credential()
            .is_none()
    );
    // This fixture's private root is the only place the item could be, so
    // an empty credential directory shows it was deleted. The Linux Secret
    // Service is shared with parallel tests, so there the deletion is covered
    // only by the classification test below.
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

/// Only a failed journal write or sync leaves a binding uncertain; every
/// refusal before a write is definite.
#[test]
fn only_a_failed_journal_write_is_an_uncertain_binding() {
    assert!(binding_write_uncertain(&ControlError::Io(
        std::io::Error::other("sync failed")
    )));
    for error in [
        ControlError::Invalid("invalid provider credential binding"),
        ControlError::Busy,
        ControlError::Conflict {
            expected: 1,
            actual: 2,
        },
        ControlError::LimitReached,
        ControlError::Poisoned,
        ControlError::UnknownProvider,
        ControlError::ProviderRefused("refused"),
    ] {
        assert!(!binding_write_uncertain(&error), "{error}");
    }
}

/// Store failures that may follow a created item attempt cleanup; refusals
/// before any write do not.
#[test]
fn only_store_failures_after_creation_may_leave_an_item() {
    for (error, may_leave) in [
        (CredentialError::Invalid("refused"), false),
        (CredentialError::TooLarge, false),
        (CredentialError::AlreadyExists, false),
        (CredentialError::Io(std::io::Error::other("sync")), true),
        (CredentialError::Corrupt("partial"), true),
        (CredentialError::Unavailable("gone".to_owned()), true),
        (CredentialError::Locked, true),
        (CredentialError::NotFound, true),
        (CredentialError::PromptRequired, true),
    ] {
        assert_eq!(store_may_leave_item(&error), may_leave, "{error}");
    }
}

/// A store failure whose item cannot be deleted keeps its identifier.
#[test]
fn a_store_failure_whose_cleanup_fails_names_the_identifier() {
    let id = CredentialId::parse(&Uuid::new_v4().simple().to_string()).expect("id");
    let io = || CredentialError::Io(std::io::Error::other("directory sync failed"));
    for cleanup in [Ok(()), Err(CredentialError::NotFound)] {
        let outcome = store_cleanup_outcome(id.clone(), io(), cleanup);
        assert!(
            matches!(
                outcome,
                ProviderCredentialError::Credential(CredentialError::Io(_))
            ),
            "{outcome:?}"
        );
    }
    match store_cleanup_outcome(id.clone(), io(), Err(CredentialError::Locked)) {
        ProviderCredentialError::Unstored {
            error: CredentialError::Io(_),
            credential_id,
            cleanup: CredentialError::Locked,
        } => assert_eq!(credential_id, id),
        other => panic!("expected an unstored orphan: {other:?}"),
    }
}

/// A store failure deletes an item it may have left; a refusal before any
/// write deletes nothing. The native failure itself cannot be induced, so an
/// item stored under the identifier stands in for one a failed store left.
#[test]
fn a_store_failure_deletes_an_item_it_may_have_left() {
    let fixture = fixture();
    let credentials = &fixture.credentials;
    let id = CredentialId::parse(&Uuid::new_v4().simple().to_string()).expect("id");
    credentials
        .store(&id, &secret(SENTINEL))
        .expect("left item");
    let error = store_failure(
        credentials,
        id.clone(),
        CredentialError::Io(std::io::Error::other("directory sync failed")),
    );
    assert!(
        matches!(
            error,
            ProviderCredentialError::Credential(CredentialError::Io(_))
        ),
        "{error:?}"
    );
    assert!(matches!(
        credentials.load(&id),
        Err(CredentialError::NotFound)
    ));
    // An item that is already absent is not an orphan.
    let error = store_failure(credentials, id.clone(), CredentialError::Corrupt("partial"));
    assert!(
        matches!(
            error,
            ProviderCredentialError::Credential(CredentialError::Corrupt(_))
        ),
        "{error:?}"
    );

    credentials
        .store(&id, &secret(SENTINEL))
        .expect("existing item");
    let error = store_failure(credentials, id.clone(), CredentialError::AlreadyExists);
    assert!(
        matches!(
            error,
            ProviderCredentialError::Credential(CredentialError::AlreadyExists)
        ),
        "{error:?}"
    );
    assert_eq!(
        credentials.load(&id).expect("not deleted").expose(),
        SENTINEL
    );
    credentials.delete(&id).expect("cleanup");
}

/// Removing a profile removes its binding in the same event, and the caller
/// deletes the native item afterwards.
#[test]
fn profile_removal_ends_its_binding_in_the_same_event() {
    let fixture = fixture();
    let (data, credentials) = (&fixture.data, &fixture.credentials);
    let mut store = fixture.store();
    let revision = store.revision();
    let set = store
        .set_provider_credential(credentials, "keys", &secret(SENTINEL), revision)
        .expect("set");
    let removal = store
        .remove_provider_profile("keys", revision + 1)
        .expect("remove profile");
    assert_eq!(removal.credential.as_ref(), Some(&set.binding));
    assert_eq!(store.revision(), revision + 2);
    let binding = removal.credential.expect("bound credential");
    assert!(delete_unbound_credential(credentials, &binding).is_none());
    assert!(matches!(
        credentials.load(binding.id()),
        Err(CredentialError::NotFound)
    ));
    // An absent item is not an orphan.
    assert!(delete_unbound_credential(credentials, &binding).is_none());
    drop(store);

    let store = ControlStore::open(data).expect("replay");
    assert!(store.provider_profile("keys").is_none());
    drop(store);
    let text = String::from_utf8(journal(data)).expect("journal text");
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
    let fixture = fixture();
    let credentials = &fixture.credentials;
    let foreign = match credentials.backend() {
        CredentialBackend::WindowsDpapi => CredentialBackend::LinuxSecretService,
        CredentialBackend::LinuxSecretService => CredentialBackend::WindowsDpapi,
    };
    let binding = CredentialBinding {
        id: CredentialId::parse(&Uuid::new_v4().simple().to_string()).expect("id"),
        backend: foreign,
    };
    let orphan = delete_unbound_credential(credentials, &binding).expect("orphan");
    assert_eq!(&orphan.id, binding.id());
    assert!(matches!(orphan.error, CredentialError::Unavailable(_)));
}

/// Replay refuses credential events that break the binding rules.
#[test]
fn replay_rejects_invalid_credential_events() {
    let fixture = fixture();
    let store = fixture.store();
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
