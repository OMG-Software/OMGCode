//! ChatGPT subscription records against the real control journal and the
//! real native credential store. On Linux these need an unlocked Secret
//! Service default collection, like the other credential tests.

use super::*;
use std::path::PathBuf;

use omgcode_platform::credentials::Zeroizing;

use crate::control::{ProviderProfileDeclaration, Secret};
use crate::provider::chatgpt::{
    PLAN_SCOPE, TokenSet, companion_credential_id, load_current_record,
};
use crate::test_support::private_data_root;

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

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().expect("temporary directory");
    let data = directory.path().join("data");
    private_data_root(&data);
    let mut store = ControlStore::open(&data).expect("control store");
    for (name, kind, auth, endpoint) in [
        ("chatgpt", "openai", "subscription", Some(OPENAI_ENDPOINT)),
        ("second", "openai", "subscription", Some(OPENAI_ENDPOINT)),
        ("no-endpoint", "openai", "subscription", None),
        ("keys", "openai", "api_key", Some(OPENAI_ENDPOINT)),
        ("claude", "anthropic", "subscription", None),
    ] {
        let revision = store.revision();
        store
            .add_provider_profile(
                &ProviderProfileDeclaration {
                    name: name.into(),
                    kind: kind.into(),
                    auth: auth.into(),
                    endpoint: endpoint.map(str::to_owned),
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

fn signed_in(client: &str, subject: &str) -> SignedIn {
    SignedIn {
        client_id: client.to_owned(),
        subject: subject.to_owned(),
        email: Some("person@example.test".to_owned()),
        tokens: TokenSet {
            access_token: Zeroizing::new("access-token".to_owned()),
            refresh_token: Zeroizing::new("refresh-token".to_owned()),
            scopes: vec![PLAN_SCOPE.to_owned()],
            expires_at: 4_000_000_000,
        },
    }
}

fn sign_in(fixture: &Fixture, name: &str, client: &str, subject: &str) -> ProviderCredentialSet {
    let mut store = fixture.store();
    let request = store.subscription_sign_in_request(name).expect("request");
    let revision = store.revision();
    store
        .record_provider_subscription(
            &fixture.credentials,
            name,
            &request,
            &signed_in(client, subject),
            revision,
        )
        .expect("recorded sign-in")
}

fn sign_out(fixture: &Fixture, name: &str) {
    let mut store = fixture.store();
    let revision = store.revision();
    let removal = store
        .sign_out_provider_subscription(&fixture.credentials, name, revision)
        .expect("sign-out");
    assert!(removal.orphans.is_empty());
}

#[test]
fn only_a_signed_out_chatgpt_profile_can_start_a_sign_in() {
    let fixture = fixture();
    let store = fixture.store();
    let request = store
        .subscription_sign_in_request("ChatGPT")
        .expect("request");
    assert!(request.client_id.is_none() && request.subject.is_none());
    assert!(crate::provider::chatgpt::valid_host_id(&request.host_id));
    for name in ["no-endpoint", "keys", "claude"] {
        assert!(matches!(
            store.subscription_sign_in_request(name),
            Err(ControlError::ProviderRefused(_))
        ));
    }
    assert!(matches!(
        store.subscription_sign_in_request("missing"),
        Err(ControlError::UnknownProvider)
    ));
}

#[test]
fn a_recorded_sign_in_stores_generation_one_and_survives_replay() {
    let fixture = fixture();
    let set = sign_in(&fixture, "chatgpt", "oaiapp_one", "subject-1");
    let store = fixture.store();
    let connection = store.provider_subscription("chatgpt").expect("connection");
    assert_eq!(connection.binding, set.binding);
    assert_eq!(connection.client_id, "oaiapp_one");
    assert_eq!(connection.endpoint, OPENAI_ENDPOINT);
    let status = store.provider_profile("chatgpt").expect("profile");
    let registration = status.subscription().expect("registration");
    assert_eq!(registration.account_label(), Some("person@example.test"));
    let record = load_current_record(
        &fixture.credentials,
        connection.binding.id(),
        "oaiapp_one",
        "subject-1",
    )
    .expect("record");
    assert_eq!(record.generation(), 1);
    assert_eq!(record.tokens.access_token.as_str(), "access-token");
    // The journal holds no token.
    let journal = std::fs::read(fixture.data.join(crate::control::FILE_NAME)).unwrap_or_default();
    let text = String::from_utf8_lossy(&journal);
    assert!(!text.contains("access-token") && !text.contains("refresh-token"));
    // A signed-in profile cannot sign in again or be given an API key.
    assert!(matches!(
        store.subscription_sign_in_request("chatgpt"),
        Err(ControlError::ProviderRefused(_))
    ));
    drop(store);
    let mut store = fixture.store();
    let revision = store.revision();
    let secret = Secret::new(b"sk-test".to_vec()).expect("secret");
    assert!(
        store
            .set_provider_credential(&fixture.credentials, "chatgpt", &secret, revision)
            .is_err()
    );
    drop(store);
    sign_out(&fixture, "chatgpt");
}

#[test]
fn only_a_signed_in_chatgpt_profile_can_be_chosen() {
    let fixture = fixture();
    let refused = |store: &ControlStore, name: &str| {
        matches!(
            store.check_provider_selectable(name),
            Err(ControlError::ProviderRefused(_))
        )
    };
    let store = fixture.store();
    assert!(refused(&store, "chatgpt"), "signed out");
    assert!(refused(&store, "no-endpoint"), "no endpoint");
    drop(store);
    let _ = sign_in(&fixture, "chatgpt", "oaiapp_one", "subject-1");
    let mut store = fixture.store();
    assert!(store.check_provider_selectable("chatgpt").is_ok());
    let revision = store.revision();
    store
        .select_active_provider(Some("chatgpt"), revision)
        .expect("a signed-in plan connection is chosen");
    drop(store);
    sign_out(&fixture, "chatgpt");
    assert!(refused(&fixture.store(), "chatgpt"), "signed out again");
}

#[test]
fn sign_out_deletes_both_slots_and_keeps_the_registration() {
    let fixture = fixture();
    let set = sign_in(&fixture, "chatgpt", "oaiapp_one", "subject-1");
    let companion = companion_credential_id(set.binding.id());
    let next = Secret::new(b"{}".to_vec()).expect("secret");
    fixture
        .credentials
        .store(&companion, &next)
        .expect("companion");

    let mut store = fixture.store();
    let revision = store.revision();
    assert!(matches!(
        store.remove_provider_profile("chatgpt", revision),
        Err(ControlError::ProviderRefused(_))
    ));
    assert!(matches!(
        store.remove_provider_credential(&fixture.credentials, "chatgpt", revision),
        Err(ProviderCredentialError::Control(
            ControlError::ProviderRefused(_)
        ))
    ));
    assert!(matches!(
        store.sign_out_provider_subscription(&fixture.credentials, "keys", revision),
        Err(ProviderCredentialError::Control(
            ControlError::ProviderRefused(_)
        ))
    ));
    drop(store);
    sign_out(&fixture, "chatgpt");
    assert!(matches!(
        fixture.credentials.load(set.binding.id()),
        Err(CredentialError::NotFound)
    ));
    assert!(matches!(
        fixture.credentials.load(&companion),
        Err(CredentialError::NotFound)
    ));
    let store = fixture.store();
    assert!(matches!(
        store.provider_subscription("chatgpt"),
        Err(ProviderCredentialUseError::NotBound { .. })
    ));
    // Each refusal names its real cause.
    let refused = |name| store.provider_subscription(name).expect_err(name).code();
    assert_eq!(refused("keys"), "not_subscription");
    assert_eq!(refused("no-endpoint"), "endpoint_not_recorded");
    assert_eq!(refused("claude"), "wrong_provider");
    assert_eq!(refused("missing"), "unknown_profile");
    let request = store
        .subscription_sign_in_request("chatgpt")
        .expect("again");
    assert_eq!(request.client_id.as_deref(), Some("oaiapp_one"));
    assert_eq!(request.subject.as_deref(), Some("subject-1"));
}

#[test]
fn every_sign_in_shares_one_host_and_a_registration_cannot_change() {
    let fixture = fixture();
    let _ = sign_in(&fixture, "chatgpt", "oaiapp_one", "subject-1");
    let first_host = fixture
        .store()
        .provider_profile("chatgpt")
        .and_then(|status| {
            status
                .subscription()
                .map(|known| known.host_id().to_owned())
        })
        .expect("host");
    let request = fixture
        .store()
        .subscription_sign_in_request("second")
        .expect("second request");
    assert_eq!(request.host_id, first_host);
    sign_out(&fixture, "chatgpt");

    // Replay refuses a sign-in that changes the recorded account.
    let mut store = fixture.store();
    let request = store
        .subscription_sign_in_request("chatgpt")
        .expect("request");
    let revision = store.revision();
    let changed = store.record_provider_subscription(
        &fixture.credentials,
        "chatgpt",
        &request,
        &signed_in("oaiapp_one", "someone-else"),
        revision,
    );
    assert!(matches!(
        changed,
        Err(ProviderCredentialError::Control(ControlError::Invalid(_)))
    ));
    // A stale revision is refused before any native write.
    let stale = store.record_provider_subscription(
        &fixture.credentials,
        "chatgpt",
        &request,
        &signed_in("oaiapp_one", "subject-1"),
        revision.saturating_sub(1),
    );
    assert!(matches!(
        stale,
        Err(ProviderCredentialError::Control(
            ControlError::Conflict { .. }
        ))
    ));
}
