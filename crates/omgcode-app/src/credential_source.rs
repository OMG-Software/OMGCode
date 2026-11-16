//! The one place the local adapter's endpoint and server token are obtained
//! (ADR-0047, ADR-0048).
//!
//! The operator chooses a [`ConnectionSource`] once, outside any model or
//! webview influence. [`resolve_local_connection`] then returns the endpoint
//! and the optional token for one turn from that one source, so a stored token
//! can never be paired with an endpoint from somewhere else:
//!
//! - [`ConnectionSource::Environment`] keeps the prototype behaviour: the
//!   endpoint is the one given (the command line or its default) and the token
//!   is the `OMGCODE_LM_TOKEN` variable, through [`read_lm_token`].
//! - [`ConnectionSource::Profile`] reads the endpoint recorded with a
//!   recorded `lm_studio` provider profile and the credential bound to it from
//!   protected OS storage. The environment variable is not read at all, and
//!   every failure ends the turn with a `credential` error. There is no
//!   fallback to no token, to another token, or to another endpoint. A profile
//!   that records no endpoint is refused with `endpoint_not_recorded`.
//!
//! Order of operations for a profile, with the settings store open throughout so
//! a rotation cannot delete the item between the binding lookup and the load:
//!
//! 1. open the existing settings store;
//! 2. `provider_credential_binding`, which refuses an unknown profile, a profile
//!    of another provider, a profile with no recorded endpoint, a
//!    `subscription` profile and an unbound `api_key` profile, and which
//!    touches no native storage;
//! 3. a `none` profile ends here with its endpoint and no token, so it works on
//!    a host whose protected store is unavailable;
//! 4. only for a bound `api_key` profile, open the native credential store and
//!    call `load_provider_credential`;
//! 5. close both stores before returning, so none is held while a model request
//!    is in flight. Nothing is cached: each turn resolves again.

use std::fmt;
use std::path::Path;

use omgcode_core::control::{
    ControlError, ControlStore, CredentialStore, LoadedProviderConnection,
    ProviderCredentialUseError, ProviderKind, Secret,
};
use zeroize::Zeroizing;

use crate::error::{AppError, ErrorKind};
use crate::secrets::read_lm_token;

/// Where a desktop process gets the optional local server token. The endpoint
/// of a desktop process is fixed at launch and held beside this value.
///
/// A profile name is an operator-chosen label, not a secret, so this type has
/// ordinary `Debug` output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TokenSource {
    /// The `OMGCODE_LM_TOKEN` process environment variable.
    Environment,
    /// The credential bound to the named recorded provider profile, whose own
    /// recorded endpoint is the only endpoint it can use.
    Profile(String),
}

impl TokenSource {
    /// The connection source for one turn, given the endpoint this process was
    /// started with. A profile source ignores `launch_endpoint`: the profile's
    /// recorded endpoint is the only one it can use.
    pub fn connection(&self, launch_endpoint: &str) -> ConnectionSource {
        match self {
            Self::Environment => ConnectionSource::Environment {
                endpoint: launch_endpoint.to_owned(),
            },
            Self::Profile(name) => ConnectionSource::Profile(name.clone()),
        }
    }
}

/// Where a front end gets the local adapter's endpoint and optional token.
///
/// A profile name and an endpoint are not secrets, so this type has ordinary
/// `Debug` output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionSource {
    /// The given endpoint (the command line or its default), with the token
    /// from the `OMGCODE_LM_TOKEN` process environment variable.
    Environment {
        /// The endpoint. It is validated when the clients are built.
        endpoint: String,
    },
    /// The endpoint and the credential recorded for the named provider
    /// profile. Nothing else supplies either.
    Profile(String),
}

/// The local server token, held only while one turn needs it.
///
/// The text zeroes itself when dropped. The type has no `Serialize`, and its
/// `Debug` output is a fixed redaction.
pub struct LmToken(Zeroizing<String>);

impl LmToken {
    /// Take ownership of an environment token. The caller's `String` is moved,
    /// not copied.
    pub(crate) fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    /// Borrow the token text for a request header or for redaction. It must not
    /// be logged, serialized, or placed in an error.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for LmToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LmToken(<redacted>)")
    }
}

/// The endpoint and the optional token for one turn, from one source.
///
/// `Debug` prints the endpoint, which is not secret, and the token's fixed
/// redaction. The type has no `Serialize`.
#[derive(Debug)]
pub struct LocalConnection {
    /// The endpoint every request of this turn uses.
    pub endpoint: String,
    /// The optional server token, zeroed on drop.
    pub token: Option<LmToken>,
}

/// Obtain the endpoint and token for one turn from `source`.
///
/// `data_root` locates the settings store and the credential store; it is not
/// read for [`ConnectionSource::Environment`]. A `none` profile gives its
/// endpoint and no token.
///
/// # Errors
///
/// [`ConnectionSource::Environment`] fails exactly as [`read_lm_token`] does.
/// [`ConnectionSource::Profile`] fails with a `credential` error when no
/// settings store exists, when another process holds it (no retry here), when
/// core refuses the profile (including `endpoint_not_recorded`), when the
/// native store fails, or when the stored secret is not non-empty UTF-8 text.
/// The message begins with a stable cause code.
pub fn resolve_local_connection(
    data_root: &Path,
    source: &ConnectionSource,
) -> Result<LocalConnection, AppError> {
    match source {
        ConnectionSource::Environment { endpoint } => Ok(LocalConnection {
            endpoint: endpoint.clone(),
            token: read_lm_token()?.map(LmToken::new),
        }),
        ConnectionSource::Profile(name) => {
            let (endpoint, secret) = load_profile_connection(data_root, name)?;
            let token = match secret {
                Some(secret) => Some(token_from_secret(&secret)?),
                None => None,
            };
            Ok(LocalConnection { endpoint, token })
        }
    }
}

/// Load the profile's endpoint and secret while the settings store is open,
/// then close both stores.
fn load_profile_connection(
    data_root: &Path,
    name: &str,
) -> Result<(String, Option<Secret>), AppError> {
    let control = match ControlStore::open_existing(data_root) {
        Ok(Some(control)) => control,
        Ok(None) => {
            return Err(credential_error(format!(
                "no settings store exists, so provider profile {name} cannot be used"
            )));
        }
        Err(ControlError::Busy) => {
            return Err(credential_error(
                "the settings store is in use by another process; try again".to_owned(),
            ));
        }
        Err(error) => {
            return Err(credential_error(format!(
                "settings_unusable: the settings store could not be opened: {error}"
            )));
        }
    };
    // Every refusal and the `none` profile are decided here, with no native
    // store access, so they report their own cause on any host.
    let keyless_endpoint = match control.provider_credential_binding(name, ProviderKind::LmStudio) {
        Ok(connection) if connection.binding().is_none() => {
            Some(connection.endpoint().as_str().to_owned())
        }
        Ok(_) => None,
        Err(error) => return Err(credential_error(format!("{}: {error}", error.code()))),
    };
    if let Some(endpoint) = keyless_endpoint {
        return Ok((endpoint, None));
    }
    // The settings store stays open across the load, so a rotation cannot
    // delete the item between the binding lookup and the read. The native
    // store is released at the end of this block, before the settings store.
    let loaded = {
        let credentials = CredentialStore::open(data_root).map_err(|error| {
            credential_error(format!(
                "{}: {error}",
                ProviderCredentialUseError::native_code(&error)
            ))
        })?;
        control.load_provider_credential(&credentials, name, ProviderKind::LmStudio)
    };
    drop(control);
    loaded
        .map(LoadedProviderConnection::into_parts)
        .map_err(|error| credential_error(format!("{}: {error}", error.code())))
}

/// Convert a stored secret without an intermediate un-zeroed copy.
///
/// The bytes are validated in place and copied once into a zeroing string; the
/// [`Secret`] zeroes its own buffer when it drops. A failed conversion never
/// builds an owned error value from the bytes.
fn token_from_secret(secret: &Secret) -> Result<LmToken, AppError> {
    let text = std::str::from_utf8(secret.expose()).map_err(|_| {
        credential_error("credential_invalid: the stored credential is not UTF-8 text".to_owned())
    })?;
    if text.is_empty() {
        return Err(credential_error(
            "credential_invalid: the stored credential is empty".to_owned(),
        ));
    }
    Ok(LmToken(Zeroizing::new(text.to_owned())))
}

fn credential_error(message: String) -> AppError {
    AppError::new(ErrorKind::Credential, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_contains_the_token() {
        let token = LmToken::new("fake-token-value".to_owned());
        assert_eq!(token.expose(), "fake-token-value");
        assert_eq!(format!("{token:?}"), "LmToken(<redacted>)");
        assert_eq!(format!("{token:#?}"), "LmToken(<redacted>)");
        let connection = LocalConnection {
            endpoint: "http://127.0.0.1:1234/v1".to_owned(),
            token: Some(token),
        };
        let printed = format!("{connection:?}");
        assert!(!printed.contains("fake-token-value"), "{printed}");
        assert!(printed.contains("http://127.0.0.1:1234/v1"), "{printed}");
    }

    #[test]
    fn a_secret_must_be_non_empty_utf8() {
        let secret = Secret::new(b"fake-token".to_vec()).expect("secret");
        assert_eq!(
            token_from_secret(&secret).expect("token").expose(),
            "fake-token"
        );
        let secret = Secret::new(vec![0xff, 0xfe, 0x41]).expect("secret");
        let error = token_from_secret(&secret).expect_err("refused");
        assert_eq!(error.code(), "credential");
        assert!(error.message().starts_with("credential_invalid"));
    }

    #[test]
    fn a_profile_source_has_plain_debug_output() {
        let source = TokenSource::Profile("lms".to_owned());
        assert_eq!(format!("{source:?}"), "Profile(\"lms\")");
        let source = ConnectionSource::Profile("lms".to_owned());
        assert_eq!(format!("{source:?}"), "Profile(\"lms\")");
    }

    /// A profile source never takes the launch endpoint, so no endpoint from
    /// elsewhere can sit beside a selected profile.
    #[test]
    fn a_profile_token_source_ignores_the_launch_endpoint() {
        assert_eq!(
            TokenSource::Environment.connection("http://127.0.0.1:1/v1"),
            ConnectionSource::Environment {
                endpoint: "http://127.0.0.1:1/v1".to_owned()
            }
        );
        assert_eq!(
            TokenSource::Profile("lms".to_owned()).connection("http://127.0.0.1:1/v1"),
            ConnectionSource::Profile("lms".to_owned())
        );
    }
}
