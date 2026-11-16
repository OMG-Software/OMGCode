//! Native Secret Service tests. They need an unlocked default collection on
//! the session bus and fail, rather than skip, without one.

use super::*;
use crate::credentials::tests::{exercise_contract, fresh_id, sentinel_secret};
use crate::credentials::{CredentialStore, CredentialStoreStatus};
use std::path::Path;

fn open() -> CredentialStore {
    CredentialStore::open(Path::new("/nonexistent-unused-root")).expect(
        "an unlocked Secret Service default collection is required; see docs/testing-standards.md",
    )
}

/// Remove every test item with this identifier, whatever its count.
fn remove_all(id: &CredentialId) {
    let service = SecretService::connect(EncryptionType::Dh).expect("connect");
    let collection = service
        .get_default_collection()
        .expect("default collection");
    for item in collection.search_items(attributes(id)).expect("search") {
        item.delete().expect("delete test item");
    }
}

#[test]
fn secret_service_store_meets_the_native_contract() {
    let store = open();
    assert_eq!(
        store.backend(),
        crate::credentials::CredentialBackend::LinuxSecretService
    );
    assert_eq!(
        crate::credentials::probe(),
        CredentialStoreStatus::Available(crate::credentials::CredentialBackend::LinuxSecretService)
    );
    exercise_contract(&store);
}

#[test]
fn stored_items_have_exactly_the_specified_shape() {
    let store = open();
    let id = fresh_id();
    let secret = sentinel_secret();
    store.store(&id, &secret).expect("store");

    let service = SecretService::connect(EncryptionType::Dh).expect("connect");
    let collection = service
        .get_default_collection()
        .expect("default collection");
    let items = collection.search_items(attributes(&id)).expect("search");
    assert_eq!(items.len(), 1);
    let item = &items[0];
    let expected = HashMap::from([
        ("application".to_owned(), "omgcode".to_owned()),
        ("omgcode_credential".to_owned(), id.as_str().to_owned()),
    ]);
    // gnome-keyring 46 may add its generic schema attribute to an item that
    // was created without one, as observed when its login keyring is saved.
    // Only that service-added attribute is tolerated; OMGCode sends exactly
    // the two expected attributes.
    let mut attributes = item.get_attributes().expect("attributes");
    if attributes.get("xdg:schema").map(String::as_str) == Some("org.freedesktop.Secret.Generic") {
        attributes.remove("xdg:schema");
    }
    assert_eq!(attributes, expected);
    assert_eq!(
        item.get_label().expect("label"),
        format!("OMGCode credential {id}")
    );
    // The store sends `application/octet-stream`, but gnome-keyring 46 does
    // not retain content types and reports `text/plain` for every item, so
    // the read-back type is not asserted.
    assert_eq!(item.get_secret().expect("secret"), secret.expose());
    drop(items);
    remove_all(&id);
}

#[test]
fn duplicate_items_for_one_identifier_are_corrupt() {
    let store = open();
    let id = fresh_id();
    store.store(&id, &sentinel_secret()).expect("store");

    // Create a second item with the same attributes directly through the
    // crate, as another client could.
    {
        let service = SecretService::connect(EncryptionType::Dh).expect("connect");
        let collection = service
            .get_default_collection()
            .expect("default collection");
        collection
            .create_item(
                &label(&id),
                attributes(&id),
                b"another-value",
                false,
                CONTENT_TYPE,
            )
            .expect("second item");
        assert_eq!(
            collection
                .search_items(attributes(&id))
                .expect("search")
                .len(),
            2
        );
    }

    assert!(matches!(store.load(&id), Err(CredentialError::Corrupt(_))));
    assert!(matches!(
        store.delete(&id),
        Err(CredentialError::Corrupt(_))
    ));
    assert!(matches!(
        store.store(&id, &sentinel_secret()),
        Err(CredentialError::AlreadyExists)
    ));
    remove_all(&id);
}

#[test]
fn an_item_from_another_application_is_not_matched() {
    let store = open();
    let id = fresh_id();
    {
        let service = SecretService::connect(EncryptionType::Dh).expect("connect");
        let collection = service
            .get_default_collection()
            .expect("default collection");
        collection
            .create_item(
                "foreign",
                HashMap::from([
                    (APPLICATION_ATTRIBUTE, "another-application"),
                    (ID_ATTRIBUTE, id.as_str()),
                ]),
                b"foreign-value",
                false,
                CONTENT_TYPE,
            )
            .expect("foreign item");
    }
    assert!(matches!(store.load(&id), Err(CredentialError::NotFound)));

    let service = SecretService::connect(EncryptionType::Dh).expect("connect");
    let collection = service
        .get_default_collection()
        .expect("default collection");
    for item in collection
        .search_items(HashMap::from([(ID_ATTRIBUTE, id.as_str())]))
        .expect("search")
    {
        item.delete().expect("delete foreign item");
    }
}
