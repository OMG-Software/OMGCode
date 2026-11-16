//! Credentials as freedesktop Secret Service items in the default collection.
//!
//! Each credential is one item whose attributes name this application and the
//! credential identifier. The secret crosses the session bus only in a
//! Diffie-Hellman session with AES (the `secret-service` crate's Rust crypto).
//! Exactly one item must match an identifier; more than one is corrupt. Any
//! process of the same user can read these items. See ADR-0042.
//!
//! OMGCode never unlocks a collection or runs a prompt. A locked collection is
//! reported as locked before any call that could prompt. The crate's blocking
//! `create_item` and `Item::delete` would run a returned prompt and wait for
//! it without a bound, so this module makes those two calls itself over the
//! same bus connection. When the service answers with a prompt, the prompt is
//! dismissed without being shown and the call fails with
//! [`CredentialError::PromptRequired`].
//!
//! A credential is created in three steps, so the secret never travels in a
//! plain session:
//!
//! 1. A raw `CreateItem` in a short plain session makes a placeholder item
//!    with an empty value and an extra `omgcode_state=pending` attribute.
//! 2. The crate's `Item::set_secret` sends the secret through its encrypted
//!    session. `SetSecret` returns no prompt.
//! 3. The crate's `Item::set_attributes` replaces the attributes with the
//!    final two, removing the pending marker. A property write returns no
//!    prompt either.
//!
//! An item that still carries the pending marker is incomplete and loads as
//! corrupt, never as its placeholder value. A failed step 2 or 3 deletes the
//! placeholder through the raw delete; when that also fails, the item keeps
//! its identifier, so the caller can delete or report it.

use std::collections::HashMap;

use secret_service::blocking::{Collection, Item, SecretService};
use secret_service::{EncryptionType, Error};
use zbus::blocking::Connection;
use zbus::message::{Flags, Message};
use zbus::zvariant::{Dict, ObjectPath, OwnedObjectPath, OwnedValue, Value};

use super::{CredentialBackend, CredentialError, CredentialId, CredentialStoreStatus, Secret};

/// The application attribute and its value.
pub(super) const APPLICATION_ATTRIBUTE: &str = "application";
pub(super) const APPLICATION: &str = "omgcode";
/// The attribute holding the credential identifier.
pub(super) const ID_ATTRIBUTE: &str = "omgcode_credential";
/// The attribute that marks an item whose secret is not yet stored.
const STATE_ATTRIBUTE: &str = "omgcode_state";
/// The value of [`STATE_ATTRIBUTE`] on a placeholder item.
const PENDING: &str = "pending";
/// The stored secret's content type.
pub(super) const CONTENT_TYPE: &str = "application/octet-stream";

/// The Secret Service's well-known bus name.
const SERVICE: &str = "org.freedesktop.secrets";
/// The service object path.
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE_INTERFACE: &str = "org.freedesktop.Secret.Service";
const COLLECTION_INTERFACE: &str = "org.freedesktop.Secret.Collection";
const ITEM_INTERFACE: &str = "org.freedesktop.Secret.Item";
const SESSION_INTERFACE: &str = "org.freedesktop.Secret.Session";
const PROMPT_INTERFACE: &str = "org.freedesktop.Secret.Prompt";
const LABEL_PROPERTY: &str = "org.freedesktop.Secret.Item.Label";
const ATTRIBUTES_PROPERTY: &str = "org.freedesktop.Secret.Item.Attributes";
/// The path the protocol returns for "no prompt" and "no object".
const NO_OBJECT: &str = "/";

/// A Secret Service store. Each operation opens its own connection and
/// encrypted session.
pub(crate) struct SecretServiceStore {
    _private: (),
}

impl SecretServiceStore {
    /// Check that the service answers and its default collection is unlocked.
    pub(super) fn open() -> Result<Self, CredentialError> {
        let (_connection, service) = connect()?;
        let collection = default_collection(&service)?;
        require_unlocked(&collection)?;
        Ok(Self { _private: () })
    }

    pub(super) fn store(&self, id: &CredentialId, secret: &Secret) -> Result<(), CredentialError> {
        let (connection, service) = connect()?;
        let collection = default_collection(&service)?;
        // Creating an item in a locked collection would ask for an unlock
        // prompt, so refuse first.
        require_unlocked(&collection)?;
        if !collection
            .search_items(attributes(id))
            .map_err(service_error)?
            .is_empty()
        {
            return Err(CredentialError::AlreadyExists);
        }
        let item =
            create_pending_item(&connection, Some(SERVICE), &collection.collection_path, id)?;
        complete_pending_item(&collection, id, &item, secret)
            .map_err(|error| remove_placeholder(&connection, Some(SERVICE), &item, error))
    }

    pub(super) fn load(&self, id: &CredentialId) -> Result<Secret, CredentialError> {
        let (_connection, service) = connect()?;
        let collection = default_collection(&service)?;
        require_unlocked(&collection)?;
        let items = collection
            .search_items(attributes(id))
            .map_err(service_error)?;
        let item = single_item(&items)?;
        if item.is_locked().map_err(service_error)? {
            return Err(CredentialError::Locked);
        }
        // A placeholder holds no secret; never return its value.
        if item
            .get_attributes()
            .map_err(service_error)?
            .contains_key(STATE_ATTRIBUTE)
        {
            return Err(CredentialError::Corrupt("credential item is incomplete"));
        }
        let bytes = item.get_secret().map_err(service_error)?;
        Secret::new(bytes)
            .map_err(|_| CredentialError::Corrupt("stored credential is empty or too large"))
    }

    pub(super) fn delete(&self, id: &CredentialId) -> Result<(), CredentialError> {
        let (connection, service) = connect()?;
        let collection = default_collection(&service)?;
        require_unlocked(&collection)?;
        let items = collection
            .search_items(attributes(id))
            .map_err(service_error)?;
        let item = single_item(&items)?;
        if item.is_locked().map_err(service_error)? {
            return Err(CredentialError::Locked);
        }
        delete_item(&connection, Some(SERVICE), &item.item_path)
    }
}

/// Report availability without creating, unlocking, or reading anything.
pub(super) fn probe() -> CredentialStoreStatus {
    let result = connect().and_then(|(_connection, service)| {
        let collection = default_collection(&service)?;
        collection.is_locked().map_err(service_error)
    });
    match result {
        Ok(false) => CredentialStoreStatus::Available(CredentialBackend::LinuxSecretService),
        Ok(true) | Err(CredentialError::Locked) => {
            CredentialStoreStatus::Locked(CredentialBackend::LinuxSecretService)
        }
        Err(CredentialError::Unavailable(reason)) => CredentialStoreStatus::Unavailable(reason),
        Err(error) => CredentialStoreStatus::Unavailable(error.to_string()),
    }
}

/// The exact attributes of one stored credential's item.
pub(super) fn attributes(id: &CredentialId) -> HashMap<&str, &str> {
    HashMap::from([
        (APPLICATION_ATTRIBUTE, APPLICATION),
        (ID_ATTRIBUTE, id.as_str()),
    ])
}

/// The attributes of a placeholder item: the final ones plus the marker.
fn pending_attributes(id: &CredentialId) -> HashMap<&str, &str> {
    let mut attributes = attributes(id);
    attributes.insert(STATE_ATTRIBUTE, PENDING);
    attributes
}

pub(super) fn label(id: &CredentialId) -> String {
    format!("OMGCode credential {id}")
}

/// Connect to the session bus and open the crate's encrypted session on the
/// same connection, so raw calls and crate calls reach the same service.
fn connect<'a>() -> Result<(Connection, SecretService<'a>), CredentialError> {
    let unavailable = |error: &dyn std::fmt::Display| {
        CredentialError::Unavailable(format!(
            "the Secret Service on the session bus did not answer: {error}"
        ))
    };
    let connection = Connection::session().map_err(|error| unavailable(&error))?;
    let service = SecretService::connect_with_existing(EncryptionType::Dh, connection.clone())
        .map_err(|error| unavailable(&error))?;
    Ok((connection, service))
}

/// The default collection. A missing alias is unavailable, never created.
fn default_collection<'a>(
    service: &'a SecretService<'a>,
) -> Result<Collection<'a>, CredentialError> {
    service
        .get_default_collection()
        .map_err(|error| match error {
            Error::NoResult => CredentialError::Unavailable(
                "the Secret Service has no default collection".to_owned(),
            ),
            error => service_error(error),
        })
}

fn require_unlocked(collection: &Collection<'_>) -> Result<(), CredentialError> {
    if collection.is_locked().map_err(service_error)? {
        Err(CredentialError::Locked)
    } else {
        Ok(())
    }
}

/// Exactly one item may match an identifier.
fn single_item<'a, 'b>(items: &'b [Item<'a>]) -> Result<&'b Item<'a>, CredentialError> {
    match items {
        [] => Err(CredentialError::NotFound),
        [item] => Ok(item),
        _ => Err(CredentialError::Corrupt(
            "more than one item has this identifier",
        )),
    }
}

/// Steps 2 and 3 of a store: find the placeholder, send the secret through
/// the encrypted session, then remove the pending marker. Neither call can
/// return a prompt: in `secret-service` 5.2.0 both are a plain method or
/// property call on the item proxy.
fn complete_pending_item(
    collection: &Collection<'_>,
    id: &CredentialId,
    created: &OwnedObjectPath,
    secret: &Secret,
) -> Result<(), CredentialError> {
    let items = collection
        .search_items(attributes(id))
        .map_err(service_error)?;
    let item = single_item(&items)?;
    let marked = item
        .get_attributes()
        .map_err(service_error)?
        .get(STATE_ATTRIBUTE)
        .is_some_and(|state| state == PENDING);
    if item.item_path != *created || !marked {
        return Err(CredentialError::Corrupt(
            "the created placeholder item was not found",
        ));
    }
    item.set_secret(secret.expose(), CONTENT_TYPE)
        .map_err(service_error)?;
    item.set_attributes(attributes(id)).map_err(service_error)
}

/// Delete a placeholder after a failed store and report the original
/// failure. If the deletion also fails, the item still carries the
/// identifier, so the caller can try again and report it.
fn remove_placeholder(
    connection: &Connection,
    destination: Option<&str>,
    item: &OwnedObjectPath,
    error: CredentialError,
) -> CredentialError {
    match delete_item(connection, destination, item) {
        Ok(()) | Err(CredentialError::NotFound) => error,
        Err(cleanup) => match error {
            CredentialError::Io(error) => CredentialError::Io(std::io::Error::new(
                error.kind(),
                format!("{error}; the placeholder item could not be deleted: {cleanup}"),
            )),
            // Other categories carry fixed text. The caller's own cleanup
            // attempt reports a remaining item with its identifier.
            error => error,
        },
    }
}

/// Step 1 of a store: create the placeholder item through a plain session
/// that carries only its empty value. A prompt is refused, never run.
fn create_pending_item(
    connection: &Connection,
    destination: Option<&str>,
    collection: &OwnedObjectPath,
    id: &CredentialId,
) -> Result<OwnedObjectPath, CredentialError> {
    let session = open_plain_session(connection, destination)?;
    let created = create_item(connection, destination, collection, &session, id);
    let closed = close_session(connection, destination, &session);
    match (created, closed) {
        (Ok(item), Ok(())) => Ok(item),
        (Ok(item), Err(error)) => Err(remove_placeholder(connection, destination, &item, error)),
        (Err(error), _) => Err(error),
    }
}

/// `Service.OpenSession("plain", "")`. The session carries only the empty
/// placeholder value, never a secret.
fn open_plain_session(
    connection: &Connection,
    destination: Option<&str>,
) -> Result<OwnedObjectPath, CredentialError> {
    let reply = connection
        .call_method(
            destination,
            SERVICE_PATH,
            Some(SERVICE_INTERFACE),
            "OpenSession",
            &("plain", Value::from("")),
        )
        .map_err(bus_error)?;
    let (_output, session): (OwnedValue, OwnedObjectPath) =
        reply.body().deserialize().map_err(bus_error)?;
    Ok(session)
}

/// `Session.Close` for a session this module opened.
fn close_session(
    connection: &Connection,
    destination: Option<&str>,
    session: &OwnedObjectPath,
) -> Result<(), CredentialError> {
    connection
        .call_method(destination, session, Some(SESSION_INTERFACE), "Close", &())
        .map(drop)
        .map_err(bus_error)
}

/// `Collection.CreateItem` with an empty value and the pending marker,
/// never replacing an existing item. A prompt is refused, never run.
fn create_item(
    connection: &Connection,
    destination: Option<&str>,
    collection: &OwnedObjectPath,
    session: &OwnedObjectPath,
    id: &CredentialId,
) -> Result<OwnedObjectPath, CredentialError> {
    let label = label(id);
    let attributes: Dict<'_, '_> = pending_attributes(id).into();
    let properties: HashMap<&str, Value<'_>> = HashMap::from([
        (LABEL_PROPERTY, Value::from(label.as_str())),
        (ATTRIBUTES_PROPERTY, Value::from(attributes)),
    ]);
    let session_path: ObjectPath<'_> = session.into();
    let placeholder = (
        session_path,
        Vec::<u8>::new(),
        Vec::<u8>::new(),
        "text/plain",
    );
    let reply = connection
        .call_method(
            destination,
            collection,
            Some(COLLECTION_INTERFACE),
            "CreateItem",
            &(properties, placeholder, false),
        )
        .map_err(bus_error)?;
    let (item, prompt): (OwnedObjectPath, OwnedObjectPath) =
        reply.body().deserialize().map_err(bus_error)?;
    if item.as_str() == NO_OBJECT {
        refuse_prompt(connection, destination, &prompt)?;
        return Err(CredentialError::Io(std::io::Error::other(
            "the Secret Service created no item and returned no prompt",
        )));
    }
    // An item path together with a prompt is not a documented reply. The
    // item exists, so keep it; the prompt is still never run.
    if prompt.as_str() != NO_OBJECT {
        dismiss_prompt(connection, destination, &prompt);
    }
    Ok(item)
}

/// `Item.Delete`. A returned prompt is refused, never run.
fn delete_item(
    connection: &Connection,
    destination: Option<&str>,
    item: &OwnedObjectPath,
) -> Result<(), CredentialError> {
    let reply = connection
        .call_method(destination, item, Some(ITEM_INTERFACE), "Delete", &())
        .map_err(bus_error)?;
    let prompt: OwnedObjectPath = reply.body().deserialize().map_err(bus_error)?;
    refuse_prompt(connection, destination, &prompt)
}

/// Accept the "no prompt" path. Any other path is dismissed, without being
/// shown, and refused.
fn refuse_prompt(
    connection: &Connection,
    destination: Option<&str>,
    prompt: &OwnedObjectPath,
) -> Result<(), CredentialError> {
    if prompt.as_str() == NO_OBJECT {
        return Ok(());
    }
    dismiss_prompt(connection, destination, prompt);
    Err(CredentialError::PromptRequired)
}

/// Ask the service to discard a prompt, best effort. The message expects no
/// reply, so a service that never answers cannot block the caller.
fn dismiss_prompt(connection: &Connection, destination: Option<&str>, prompt: &OwnedObjectPath) {
    let message = Message::method_call(prompt, "Dismiss").and_then(|builder| {
        let builder = builder
            .interface(PROMPT_INTERFACE)?
            .with_flags(Flags::NoReplyExpected)?;
        match destination {
            Some(destination) => builder.destination(destination)?.build(&()),
            None => builder.build(&()),
        }
    });
    // The caller refuses the request whether or not the service drops the
    // prompt, so a failure to build or send the dismissal changes nothing
    // and is deliberately not reported over that refusal.
    if let Ok(message) = message {
        let _ = connection.send(&message);
    }
}

/// Keep lock, absence, and availability categories distinct from other bus
/// errors.
fn bus_error(error: zbus::Error) -> CredentialError {
    if let zbus::Error::MethodError(name, _, _) = &error {
        match name.as_str() {
            "org.freedesktop.Secret.Error.IsLocked" => return CredentialError::Locked,
            "org.freedesktop.Secret.Error.NoSuchObject"
            | "org.freedesktop.DBus.Error.UnknownObject" => return CredentialError::NotFound,
            "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner" => {
                return CredentialError::Unavailable(
                    "the Secret Service is not available on the session bus".to_owned(),
                );
            }
            _ => {}
        }
    }
    CredentialError::Io(std::io::Error::other(error.to_string()))
}

/// Keep lock and availability categories distinct from other service errors.
fn service_error(error: Error) -> CredentialError {
    match error {
        Error::Locked => CredentialError::Locked,
        Error::Unavailable => CredentialError::Unavailable(
            "the Secret Service is not available on the session bus".to_owned(),
        ),
        Error::Prompt | Error::PromptDisconnected => CredentialError::PromptRequired,
        Error::Crypto(reason) => CredentialError::Corrupt(reason),
        error => CredentialError::Io(std::io::Error::other(error.to_string())),
    }
}

#[cfg(test)]
mod tests;
