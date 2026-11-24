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
//!   recorded local provider profile (`lm_studio` or `ollama_local`,
//!   ADR-0064) and the credential bound to it from protected OS storage. The
//!   environment variable is not read at all, and every failure ends the turn
//!   with a `credential` error. There is no fallback to no token, to another
//!   token, or to another endpoint. A profile that records no endpoint is
//!   refused with `endpoint_not_recorded`. An `ollama_local` profile is always
//!   `none`, so it never reads native storage.
//! - [`ConnectionSource::Active`] is the desktop default when no connection
//!   argument was given (ADR-0059 decision 7, ADR-0064). Each turn reads the
//!   active connection recorded in the settings store. When one is selected it
//!   is resolved exactly as [`ConnectionSource::Profile`], within the same
//!   open store, so the selection and the profile it names are read together.
//!   When none is selected, or no settings store exists, the default endpoint
//!   and the environment token apply, as for [`ConnectionSource::Environment`].
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
use omgcode_core::provider::ModelEndpoint;
use zeroize::Zeroizing;

use crate::error::{AppError, ErrorKind};
use crate::secrets::read_lm_token;

/// Where a desktop process gets the optional local server token. The launch
/// endpoint is held beside this value; it is the endpoint itself for
/// `Environment`, and the default for `Active`.
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
    /// The active connection recorded in the settings store, read again for
    /// each turn, or the launch endpoint and `OMGCODE_LM_TOKEN` when none is
    /// selected (ADR-0064).
    Active,
}

impl TokenSource {
    /// The connection source for one turn, given the endpoint this process was
    /// started with. A profile source ignores `launch_endpoint`: the profile's
    /// recorded endpoint is the only one it can use. An active source uses it
    /// only when no connection is selected.
    pub fn connection(&self, launch_endpoint: &str) -> ConnectionSource {
        match self {
            Self::Environment => ConnectionSource::Environment {
                endpoint: launch_endpoint.to_owned(),
            },
            Self::Profile(name) => ConnectionSource::Profile(name.clone()),
            Self::Active => ConnectionSource::Active {
                default_endpoint: launch_endpoint.to_owned(),
            },
        }
    }

    /// Whether the endpoint is fixed for the process. Only an active source
    /// can change between turns.
    pub const fn is_fixed(&self) -> bool {
        !matches!(self, Self::Active)
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
    /// The active connection recorded in the settings store, or
    /// `default_endpoint` with the environment token when none is selected.
    Active {
        /// The endpoint used when no connection is selected.
        default_endpoint: String,
    },
}

/// The provider and endpoint a turn would use, without its token.
///
/// It is shown to the operator and compared with what a window sent. Neither
/// value is a secret.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalEndpoint {
    /// The endpoint.
    pub endpoint: String,
    /// The selected profile's name and kind, or `None` for the launch or
    /// default endpoint.
    pub profile: Option<(String, ProviderKind)>,
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

/// The profile kind the model adapter reads `name` as. Every kind is served
/// by the one OpenAI-compatible adapter (ADR-0091), so this is the profile's
/// own kind; an unknown name is read as `lm_studio`, and core then refuses it
/// as an unknown profile.
fn local_adapter(control: &ControlStore, name: &str) -> ProviderKind {
    control
        .provider_profile(name)
        .map_or(ProviderKind::LmStudio, |status| status.profile().kind())
}

/// The endpoint a turn from `source` would use and the profile it comes from,
/// without reading any credential or environment token.
///
/// # Errors
///
/// For a profile or an active selection, a `credential` error with core's
/// cause code, as [`resolve_local_connection`] reports it. An environment
/// source never fails.
pub fn resolve_local_endpoint(
    data_root: &Path,
    source: &ConnectionSource,
) -> Result<LocalEndpoint, AppError> {
    let launch = |endpoint: &str| LocalEndpoint {
        endpoint: endpoint.to_owned(),
        profile: None,
    };
    let (control, name) = match source {
        ConnectionSource::Environment { endpoint } => return Ok(launch(endpoint)),
        ConnectionSource::Profile(name) => (open_settings(data_root, name)?, name.clone()),
        ConnectionSource::Active { default_endpoint } => {
            let Some(control) = open_settings_if_any(data_root)? else {
                return Ok(launch(default_endpoint));
            };
            let Some(name) = control
                .active_provider_profile()
                .map(|status| status.profile().name().to_owned())
            else {
                return Ok(launch(default_endpoint));
            };
            (control, name)
        }
    };
    let kind = local_adapter(&control, &name);
    let connection = control
        .provider_credential_binding(&name, kind)
        .map_err(|error| credential_error(format!("{}: {error}", error.code())))?;
    Ok(LocalEndpoint {
        endpoint: connection.endpoint().as_str().to_owned(),
        profile: Some((name, kind)),
    })
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
    resolve_connection_and_kind(data_root, source).map(|(connection, _)| connection)
}

/// [`resolve_local_connection`] together with the kind of the profile it used,
/// or `None` for an environment or default endpoint. The kind is read from the
/// same open settings store as the profile's endpoint and token, so a change to
/// the selection in between cannot pair one profile's connection with another
/// profile's kind.
///
/// # Errors
///
/// As [`resolve_local_connection`].
pub(crate) fn resolve_connection_and_kind(
    data_root: &Path,
    source: &ConnectionSource,
) -> Result<(LocalConnection, Option<ProviderKind>), AppError> {
    match source {
        ConnectionSource::Environment { endpoint } => Ok((environment_connection(endpoint)?, None)),
        ConnectionSource::Profile(name) => {
            let control = open_settings(data_root, name)?;
            let kind = local_adapter(&control, name);
            Ok((profile_connection(control, data_root, name)?, Some(kind)))
        }
        ConnectionSource::Active { default_endpoint } => {
            let Some(control) = open_settings_if_any(data_root)? else {
                return Ok((environment_connection(default_endpoint)?, None));
            };
            // The selection and the profile it names are read from one open
            // store, so a change in between cannot pair them wrongly.
            let selected = control
                .active_provider_profile()
                .map(|status| status.profile().name().to_owned());
            match selected {
                Some(name) => {
                    let kind = local_adapter(&control, &name);
                    Ok((profile_connection(control, data_root, &name)?, Some(kind)))
                }
                None => {
                    drop(control);
                    Ok((environment_connection(default_endpoint)?, None))
                }
            }
        }
    }
}

/// The launch or default endpoint with the environment's local server token.
fn environment_connection(endpoint: &str) -> Result<LocalConnection, AppError> {
    // The environment token is a local server's token. It is never sent to a
    // hosted endpoint, whose key comes from a bound credential only (ADR-0071,
    // ADR-0091).
    if ModelEndpoint::parse(endpoint).is_ok_and(|parsed| parsed.is_hosted()) {
        return Err(credential_error(
            "cloud_needs_profile: a hosted endpoint is used only through a provider profile with a stored key".to_owned(),
        ));
    }
    Ok(LocalConnection {
        endpoint: endpoint.to_owned(),
        token: read_lm_token()?.map(LmToken::new),
    })
}

/// One recorded profile resolved for a turn: its kind, its validated endpoint,
/// and its credential. Nothing else supplies any of the three.
///
/// `Debug` prints the kind and endpoint, which are not secret, and the
/// token's fixed redaction. The type has no `Serialize`.
#[derive(Debug)]
pub struct ProfileConnection {
    /// The profile's kind.
    pub kind: ProviderKind,
    /// The recorded endpoint, validated again by the shared rule.
    pub endpoint: ModelEndpoint,
    /// The bound credential, or `None` for a `none` profile. It is present
    /// for a hosted `api_key` profile with a bound credential. An `openai` or
    /// `anthropic` profile may record `subscription`, which carries no token
    /// and is refused before a credential is read.
    pub token: Option<LmToken>,
}

/// Resolve the named profile for one turn: its kind, validated endpoint, and
/// credential, read as [`resolve_local_connection`] reads a profile
/// (ADR-0047, ADR-0048, ADR-0071). Nothing is cached.
///
/// # Errors
///
/// A `credential` error with core's cause code for every refusal that
/// [`resolve_local_connection`] reports for a profile, and
/// `endpoint_invalid` if the recorded endpoint no longer passes the shared
/// rule.
pub fn resolve_profile_connection(
    data_root: &Path,
    name: &str,
) -> Result<ProfileConnection, AppError> {
    let control = open_settings(data_root, name)?;
    let kind = local_adapter(&control, name);
    let connection = profile_connection(control, data_root, name)?;
    let endpoint = ModelEndpoint::parse(&connection.endpoint).map_err(|_| {
        credential_error("endpoint_invalid: the recorded endpoint is not usable".to_owned())
    })?;
    Ok(ProfileConnection {
        kind,
        endpoint,
        token: connection.token,
    })
}

/// Check that a profile can be chosen as a connection now, for a front end
/// that offers a choice (ADR-0071): core's
/// [`ControlStore::check_provider_selectable`] on the existing settings store,
/// which for a hosted profile also needs a bound credential. No
/// native storage is read.
///
/// # Errors
///
/// A `credential` error when the settings store is missing or busy, or when
/// core refuses the profile.
pub fn check_profile_selectable(data_root: &Path, name: &str) -> Result<(), AppError> {
    profile_selectability(data_root, name).map_err(ProfileCheckError::into_app_error)
}

/// Why [`profile_selectability`] refused, as a type a caller can branch on
/// without reading the message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProfileCheckError {
    /// The profile is gone, or can no longer be chosen: no settings store
    /// exists, the name is unknown, or core refuses the profile (a hosted
    /// profile without a bound credential, for example).
    NotSelectable(AppError),
    /// The settings store is busy or unusable, so nothing is known about the
    /// profile.
    Unavailable(AppError),
}

impl ProfileCheckError {
    /// The underlying application error, with its `credential` code.
    #[must_use]
    pub fn into_app_error(self) -> AppError {
        match self {
            Self::NotSelectable(error) | Self::Unavailable(error) => error,
        }
    }
}

/// As [`check_profile_selectable`], with a typed refusal: a profile that is
/// gone or cannot be chosen is [`ProfileCheckError::NotSelectable`], and a
/// busy or unusable settings store is [`ProfileCheckError::Unavailable`].
///
/// # Errors
///
/// See [`ProfileCheckError`].
pub fn profile_selectability(data_root: &Path, name: &str) -> Result<(), ProfileCheckError> {
    let control = open_settings_if_any(data_root)
        .map_err(ProfileCheckError::Unavailable)?
        .ok_or_else(|| {
            ProfileCheckError::NotSelectable(credential_error(format!(
                "no settings store exists, so provider profile {name} cannot be used"
            )))
        })?;
    control
        .check_provider_selectable(name)
        .map(|_| ())
        .map_err(|error| {
            ProfileCheckError::NotSelectable(credential_error(format!("not_selectable: {error}")))
        })
}

/// Resolve one profile's endpoint and token from an open settings store.
fn profile_connection(
    control: ControlStore,
    data_root: &Path,
    name: &str,
) -> Result<LocalConnection, AppError> {
    let (endpoint, secret) = load_profile_connection(control, data_root, name)?;
    let token = match secret {
        Some(secret) => Some(token_from_secret(&secret)?),
        None => None,
    };
    Ok(LocalConnection { endpoint, token })
}

/// Open the existing settings store for a named profile. A missing store is a
/// `credential` error, because the profile cannot exist.
fn open_settings(data_root: &Path, name: &str) -> Result<ControlStore, AppError> {
    open_settings_if_any(data_root)?.ok_or_else(|| {
        credential_error(format!(
            "no settings store exists, so provider profile {name} cannot be used"
        ))
    })
}

/// Open the existing settings store, or `None` when none exists. Another
/// holder is waited for briefly ([`ControlStore::open_existing_waiting`]), and
/// refused if it still holds the store after that.
fn open_settings_if_any(data_root: &Path) -> Result<Option<ControlStore>, AppError> {
    match ControlStore::open_existing_waiting(data_root) {
        Ok(control) => Ok(control),
        Err(ControlError::Busy) => Err(credential_error(
            "the settings store is in use by another process; try again".to_owned(),
        )),
        Err(error) => Err(credential_error(format!(
            "settings_unusable: the settings store could not be opened: {error}"
        ))),
    }
}

/// Load the profile's endpoint and secret while the settings store is open,
/// then close both stores.
fn load_profile_connection(
    control: ControlStore,
    data_root: &Path,
    name: &str,
) -> Result<(String, Option<Secret>), AppError> {
    let adapter = local_adapter(&control, name);
    // Every refusal and the `none` profile are decided here, with no native
    // store access, so they report their own cause on any host.
    let keyless_endpoint = match control.provider_credential_binding(name, adapter) {
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
        control.load_provider_credential(&credentials, name, adapter)
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

    /// Only an active source can change between turns, and it carries the
    /// launch endpoint only as its default.
    #[test]
    fn an_active_source_is_not_fixed_and_keeps_the_launch_endpoint_as_its_default() {
        assert!(TokenSource::Environment.is_fixed());
        assert!(TokenSource::Profile("lms".to_owned()).is_fixed());
        assert!(!TokenSource::Active.is_fixed());
        assert_eq!(
            TokenSource::Active.connection("http://127.0.0.1:1/v1"),
            ConnectionSource::Active {
                default_endpoint: "http://127.0.0.1:1/v1".to_owned()
            }
        );
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
