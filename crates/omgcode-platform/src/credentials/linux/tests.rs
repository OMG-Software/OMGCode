//! Native Secret Service tests. They need an unlocked default collection on
//! the session bus and fail, rather than skip, without one. The prompt
//! refusal tests instead use a peer-to-peer mock service, because a real
//! service cannot be made to answer with a prompt on demand.

use super::*;
use crate::credentials::tests::{exercise_contract, fresh_id, sentinel_secret};
use crate::credentials::{CredentialStore, CredentialStoreStatus};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn open() -> CredentialStore {
    CredentialStore::open(Path::new("/nonexistent-unused-root")).expect(
        "an unlocked Secret Service default collection is required; see docs/testing-standards.md",
    )
}

/// Remove every test item with these attributes, whatever its count,
/// through the prompt-refusing delete.
fn remove_matching(attributes: HashMap<&str, &str>) {
    let (connection, service) = connect().expect("connect");
    let collection = default_collection(&service).expect("default collection");
    for item in collection.search_items(attributes).expect("search") {
        delete_item(&connection, Some(SERVICE), &item.item_path).expect("delete test item");
    }
}

fn remove_all(id: &CredentialId) {
    remove_matching(attributes(id));
}

/// Attributes read back from an item, without the generic schema attribute
/// that gnome-keyring 46 may add to an item created without one, as
/// observed when its login keyring is saved. OMGCode never sends it.
fn sent_attributes(item: &Item<'_>) -> HashMap<String, String> {
    let mut attributes = item.get_attributes().expect("attributes");
    if attributes.get("xdg:schema").map(String::as_str) == Some("org.freedesktop.Secret.Generic") {
        attributes.remove("xdg:schema");
    }
    attributes
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
    // The final attributes are exactly the two expected ones: the pending
    // marker is gone. Only the service-added schema attribute is tolerated.
    let expected = HashMap::from([
        ("application".to_owned(), "omgcode".to_owned()),
        ("omgcode_credential".to_owned(), id.as_str().to_owned()),
    ]);
    assert_eq!(sent_attributes(item), expected);
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

    remove_matching(HashMap::from([(ID_ATTRIBUTE, id.as_str())]));
}

/// A placeholder left by an interrupted store is incomplete: it loads as
/// corrupt, never as its empty value, blocks reuse of its identifier, and
/// can be deleted.
#[test]
fn an_incomplete_placeholder_loads_as_corrupt_and_can_be_deleted() {
    let store = open();
    let id = fresh_id();
    {
        let (connection, service) = connect().expect("connect");
        let collection = default_collection(&service).expect("default collection");
        let created =
            create_pending_item(&connection, Some(SERVICE), &collection.collection_path, &id)
                .expect("placeholder");
        let items = collection.search_items(attributes(&id)).expect("search");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item_path, created);
        let expected = HashMap::from([
            ("application".to_owned(), "omgcode".to_owned()),
            ("omgcode_credential".to_owned(), id.as_str().to_owned()),
            ("omgcode_state".to_owned(), "pending".to_owned()),
        ]);
        assert_eq!(sent_attributes(&items[0]), expected);
        assert!(items[0].get_secret().expect("placeholder value").is_empty());
    }

    assert!(
        matches!(
            store.load(&id),
            Err(CredentialError::Corrupt("credential item is incomplete"))
        ),
        "a placeholder must not load"
    );
    assert!(matches!(
        store.store(&id, &sentinel_secret()),
        Err(CredentialError::AlreadyExists)
    ));
    store.delete(&id).expect("delete the placeholder");
    assert!(matches!(store.load(&id), Err(CredentialError::NotFound)));
}

/// What the mock service observed.
#[derive(Default)]
struct Record {
    prompted: AtomicBool,
    empty_placeholder: AtomicBool,
    session_closed: AtomicBool,
}

const MOCK_COLLECTION: &str = "/org/freedesktop/secrets/collection/mock";
const MOCK_SESSION: &str = "/org/freedesktop/secrets/session/mock";
const MOCK_ITEM: &str = "/org/freedesktop/secrets/collection/mock/1";

fn path(text: &'static str) -> OwnedObjectPath {
    ObjectPath::from_static_str_unchecked(text).into()
}

struct MockService;

#[zbus::interface(name = "org.freedesktop.Secret.Service")]
impl MockService {
    fn open_session(
        &self,
        algorithm: &str,
        _input: Value<'_>,
    ) -> zbus::fdo::Result<(Value<'static>, OwnedObjectPath)> {
        if algorithm == "plain" {
            Ok((Value::from(""), path(MOCK_SESSION)))
        } else {
            Err(zbus::fdo::Error::NotSupported(algorithm.to_owned()))
        }
    }
}

struct MockSession(Arc<Record>);

#[zbus::interface(name = "org.freedesktop.Secret.Session")]
impl MockSession {
    fn close(&self) {
        self.0.session_closed.store(true, Ordering::SeqCst);
    }
}

/// Every new item needs confirmation.
struct MockCollection(Arc<Record>);

#[zbus::interface(name = "org.freedesktop.Secret.Collection")]
impl MockCollection {
    fn create_item(
        &self,
        _properties: HashMap<String, OwnedValue>,
        secret: (OwnedObjectPath, Vec<u8>, Vec<u8>, String),
        _replace: bool,
    ) -> (OwnedObjectPath, OwnedObjectPath) {
        let (session, _parameters, value, _content_type) = secret;
        self.0.empty_placeholder.store(
            session.as_str() == MOCK_SESSION && value.is_empty(),
            Ordering::SeqCst,
        );
        (path("/"), path("/prompt/1"))
    }
}

/// Every deletion needs confirmation.
struct MockItem;

#[zbus::interface(name = "org.freedesktop.Secret.Item")]
impl MockItem {
    fn delete(&self) -> OwnedObjectPath {
        path("/prompt/2")
    }
}

struct MockPrompt(Arc<Record>);

#[zbus::interface(name = "org.freedesktop.Secret.Prompt")]
impl MockPrompt {
    fn prompt(&self, _window_id: &str) {
        self.0.prompted.store(true, Ordering::SeqCst);
    }

    fn dismiss(&self) {}
}

/// A client connection to a mock Secret Service over a socket pair. Fields
/// drop in order: the client, the server, then the runtime driving both.
struct Mock {
    client: Connection,
    record: Arc<Record>,
    _server: zbus::Connection,
    _runtime: tokio::runtime::Runtime,
}

fn mock() -> Mock {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("mock runtime");
    let record = Arc::new(Record::default());
    let (server, client) = runtime.block_on(async {
        let (server_stream, client_stream) = tokio::net::UnixStream::pair().expect("socket pair");
        let server = zbus::connection::Builder::unix_stream(server_stream)
            .server(zbus::Guid::generate())
            .expect("server GUID")
            .p2p()
            .serve_at(SERVICE_PATH, MockService)
            .expect("service")
            .serve_at(MOCK_SESSION, MockSession(Arc::clone(&record)))
            .expect("session")
            .serve_at(MOCK_COLLECTION, MockCollection(Arc::clone(&record)))
            .expect("collection")
            .serve_at(MOCK_ITEM, MockItem)
            .expect("item")
            .serve_at("/prompt/1", MockPrompt(Arc::clone(&record)))
            .expect("create prompt")
            .serve_at("/prompt/2", MockPrompt(Arc::clone(&record)))
            .expect("delete prompt")
            .build();
        let server = tokio::spawn(server);
        let client = zbus::connection::Builder::unix_stream(client_stream)
            .p2p()
            .build()
            .await
            .expect("mock client");
        let server = server
            .await
            .expect("mock server task")
            .expect("mock server");
        (server, client)
    });
    Mock {
        client: Connection::from(client),
        record,
        _server: server,
        _runtime: runtime,
    }
}

/// A `CreateItem` that answers with a prompt is refused; the prompt is
/// never run, the plain session carried only an empty value, and it closed.
#[test]
fn a_create_prompt_is_refused_without_being_run() {
    let mock = mock();
    let result = create_pending_item(&mock.client, None, &path(MOCK_COLLECTION), &fresh_id());
    assert!(
        matches!(result, Err(CredentialError::PromptRequired)),
        "{result:?}"
    );
    assert!(
        !mock.record.prompted.load(Ordering::SeqCst),
        "Prompt was called"
    );
    assert!(mock.record.empty_placeholder.load(Ordering::SeqCst));
    assert!(mock.record.session_closed.load(Ordering::SeqCst));
}

/// A `Delete` that answers with a prompt is refused; the prompt is never run.
#[test]
fn a_delete_prompt_is_refused_without_being_run() {
    let mock = mock();
    let result = delete_item(&mock.client, None, &path(MOCK_ITEM));
    assert!(
        matches!(result, Err(CredentialError::PromptRequired)),
        "{result:?}"
    );
    assert!(
        !mock.record.prompted.load(Ordering::SeqCst),
        "Prompt was called"
    );
}
