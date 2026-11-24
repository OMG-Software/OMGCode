//! Sign in with ChatGPT and keep the plan's rotating tokens (ADR-0093).
//!
//! Only trusted code starts a sign-in: the CLI and the desktop `settings`
//! window. The flow is OpenAI's documented browser flow for agents: a dynamic
//! client registration on the first sign-in, PKCE with S256, a random `state`
//! and `nonce`, and a loopback callback on `127.0.0.1`. The ID token is
//! verified against OpenAI's published keys and then dropped. A completed
//! sign-in shows only that the account granted the plan scope; it is not proof
//! that a plan-backed request will be accepted.
//!
//! No token, code, or verifier is ever written to an error, a log, or a
//! journal. Every error here carries a fixed text and a stable code.

mod callback;
mod id_token;
mod store;

pub use store::{
    ChatGptTokens, SessionLock, SlotStore, StoredTokens, companion_credential_id,
    initial_token_record, load_current_record, revoke_current, store_next_record,
};

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use omgcode_platform::credentials::Zeroizing;
use reqwest::redirect::Policy;
use reqwest::{Client, Url};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{CONNECT_TIMEOUT, ProviderError, read_error_body, send_request};
use crate::storage::parse_strict_json;
use crate::turn::CancelToken;

/// The issuer every ID token must name.
pub const AUTH_ISSUER: &str = "https://auth.openai.com";
const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
const REVOKE_URL: &str = "https://auth.openai.com/api/accounts/oauth/revoke";
const JWKS_URL: &str = "https://auth.openai.com/.well-known/jwks.json";
/// The protected resource the plan's tokens are issued for.
pub const RESOURCE: &str = "https://api.openai.com/v1";
/// The scopes a sign-in requests.
pub const SCOPE: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
/// The scope that lets the tokens call the API on the ChatGPT plan.
pub const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
/// The client a first sign-in names so OpenAI registers a new one.
const DYNAMIC_CLIENT: &str = "dynamic_agent_client";
/// The name OpenAI shows the account owner for a new registration.
const AGENT_NAME: &str = "OMGCode";
/// The loopback path OpenAI redirects to.
const CALLBACK_PATH: &str = "/auth/callback";
/// The longest the browser step may take.
const SIGN_IN_WAIT: Duration = Duration::from_secs(5 * 60);
/// The deadline for one token, key, or revocation request.
const AUTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest lifetime accepted for an access token.
const MAX_EXPIRES_IN: u64 = 24 * 60 * 60;
/// The longest accepted token, client id, or subject.
const MAX_TOKEN_BYTES: usize = 8 * 1024;
const MAX_IDENTIFIER_BYTES: usize = 256;
/// OAuth error codes that mean the refresh token cannot be used again.
const REFUSED_REFRESH_CODES: &[&str] = &[
    "invalid_grant",
    "invalid_refresh_token",
    "token_expired",
    "refresh_token_expired",
    "refresh_token_invalidated",
    "refresh_token_reused",
];

/// The sign-in service's addresses. Production code can only use
/// [`AuthEndpoints::openai`]; tests point every address at a loopback server.
#[derive(Clone, Debug)]
pub struct AuthEndpoints {
    authorize: Url,
    token: Url,
    revoke: Url,
    jwks: Url,
    issuer: String,
    https_only: bool,
}

impl AuthEndpoints {
    /// OpenAI's documented sign-in addresses.
    ///
    /// # Panics
    ///
    /// Never: the addresses are constants that parse.
    #[must_use]
    pub fn openai() -> Self {
        let parse = |text: &str| Url::parse(text).expect("a constant sign-in address parses");
        Self {
            authorize: parse(AUTHORIZE_URL),
            token: parse(TOKEN_URL),
            revoke: parse(REVOKE_URL),
            jwks: parse(JWKS_URL),
            issuer: AUTH_ISSUER.to_owned(),
            https_only: true,
        }
    }

    /// Every address on one loopback test server, with OpenAI's issuer.
    #[cfg(test)]
    pub(crate) fn loopback(base: &str) -> Self {
        let parse = |path: &str| Url::parse(&format!("{base}{path}")).expect("test address");
        Self {
            authorize: parse("/api/accounts/authorize"),
            token: parse("/api/accounts/oauth/token"),
            revoke: parse("/api/accounts/oauth/revoke"),
            jwks: parse("/.well-known/jwks.json"),
            issuer: AUTH_ISSUER.to_owned(),
            https_only: false,
        }
    }
}

/// Why a sign-in, refresh, or revocation failed. The texts are fixed and
/// never quote the service, a parameter, or a token.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum SignInError {
    /// The operator cancelled.
    #[error("the ChatGPT sign-in was cancelled")]
    Cancelled,
    /// No callback arrived within five minutes.
    #[error("the ChatGPT sign-in was not completed within five minutes")]
    TimedOut,
    /// The system browser could not be started.
    #[error("the system browser could not be opened for the ChatGPT sign-in")]
    Browser,
    /// The loopback listener could not be started.
    #[error("the ChatGPT sign-in could not listen on 127.0.0.1")]
    Listener,
    /// The account owner refused the sign-in.
    #[error("the ChatGPT sign-in was refused in the browser")]
    AccessDenied,
    /// The callback's `state` differed from the one this attempt sent.
    #[error("the ChatGPT sign-in callback did not match this attempt")]
    StateMismatch,
    /// The callback was malformed.
    #[error("the ChatGPT sign-in callback was not valid: {0}")]
    Callback(&'static str),
    /// A returning registration came back with another client.
    #[error("the ChatGPT sign-in returned a different client registration")]
    ClientMismatch,
    /// A returning registration came back with another account.
    #[error("the ChatGPT sign-in returned a different account; add another profile for it")]
    IdentityMismatch,
    /// The tokens do not carry the plan scope.
    #[error("the ChatGPT sign-in did not grant use of the plan")]
    PlanUsageNotGranted,
    /// The ID token failed validation.
    #[error("the ChatGPT ID token was not valid: {0}")]
    IdToken(&'static str),
    /// The service answered with a failure status.
    #[error("the ChatGPT sign-in service returned HTTP status {0}")]
    HttpStatus(u16),
    /// The service's answer was not the documented shape.
    #[error("the ChatGPT sign-in service returned an invalid response")]
    Malformed,
    /// The service could not be reached.
    #[error("the ChatGPT sign-in service could not be reached")]
    Transport,
    /// The stored session was refused; the operator must sign in again.
    #[error("the ChatGPT session has ended; sign in again")]
    SignInRequired,
    /// The protected token store or the session lock could not be used.
    #[error("the ChatGPT session's protected token store could not be used")]
    TokenStore,
}

impl SignInError {
    /// A stable code for front ends and tests.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Browser => "browser_unavailable",
            Self::Listener => "listener_unavailable",
            Self::AccessDenied => "access_denied",
            Self::StateMismatch => "state_mismatch",
            Self::Callback(_) => "invalid_callback",
            Self::ClientMismatch => "client_mismatch",
            Self::IdentityMismatch => "identity_mismatch",
            Self::PlanUsageNotGranted => "plan_usage_not_granted",
            Self::IdToken(_) => "invalid_id_token",
            Self::HttpStatus(_) => "http_status",
            Self::Malformed => "malformed_response",
            Self::Transport => "transport",
            Self::SignInRequired => "signin_required",
            Self::TokenStore => "token_store_unavailable",
        }
    }
}

impl From<SignInError> for ProviderError {
    fn from(error: SignInError) -> Self {
        match error {
            SignInError::Cancelled => Self::Cancelled,
            SignInError::SignInRequired => Self::SignInRequired,
            _ => Self::AccessTokenUnavailable,
        }
    }
}

/// What a sign-in starts from: a returning registration's client and
/// account, and the host identifier this data root sends.
#[derive(Clone, Debug)]
pub struct SignInRequest {
    /// The client OpenAI issued at the first sign-in, if any.
    pub client_id: Option<String>,
    /// The account subject recorded at the first sign-in, if any.
    pub subject: Option<String>,
    /// `urn:uuid:` and a random UUIDv4, the same for every sign-in here.
    pub host_id: String,
}

/// One token pair. It zeroes both tokens on drop.
pub struct TokenSet {
    /// The bearer token for API requests.
    pub access_token: Zeroizing<String>,
    /// The single-use refresh token.
    pub refresh_token: Zeroizing<String>,
    /// The granted scopes.
    pub scopes: Vec<String>,
    /// When the access token expires, in seconds since the Unix epoch.
    pub expires_at: u64,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TokenSet")
            .field("scopes", &self.scopes)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// A completed sign-in: the registration, the verified account, and tokens.
#[derive(Debug)]
pub struct SignedIn {
    /// The issued client.
    pub client_id: String,
    /// The ID token's verified `sub`.
    pub subject: String,
    /// The ID token's email, kept only as a display label.
    pub email: Option<String>,
    /// The tokens to store.
    pub tokens: TokenSet,
}

/// Run the browser sign-in.
///
/// `open_browser` receives the authorization address and starts the system
/// browser; the CLI also prints it. The address carries no secret beyond
/// this attempt's `state`, `nonce`, and PKCE challenge.
///
/// # Errors
///
/// See [`SignInError`]. Nothing is stored by this function.
pub fn sign_in(
    endpoints: &AuthEndpoints,
    request: &SignInRequest,
    open_browser: &mut dyn FnMut(&str) -> Result<(), SignInError>,
    cancel: &CancelToken,
) -> Result<SignedIn, SignInError> {
    sign_in_within(endpoints, request, open_browser, SIGN_IN_WAIT, cancel)
}

/// [`sign_in`] with a chosen wait, so tests can time out quickly.
pub(crate) fn sign_in_within(
    endpoints: &AuthEndpoints,
    request: &SignInRequest,
    open_browser: &mut dyn FnMut(&str) -> Result<(), SignInError>,
    wait: Duration,
    cancel: &CancelToken,
) -> Result<SignedIn, SignInError> {
    if cancel.is_cancelled() {
        return Err(SignInError::Cancelled);
    }
    let listener = callback::CallbackListener::bind()?;
    let redirect_uri = listener.redirect_uri();
    let state = random_text()?;
    let nonce = random_text()?;
    let verifier = random_text()?;
    let address = authorize_address(endpoints, request, &redirect_uri, &state, &nonce, &verifier);
    open_browser(address.as_str())?;
    let callback = listener.wait(&state, request.client_id.as_deref(), wait, cancel)?;
    let client_id = callback.client_id;
    let form = form_body(&[
        ("grant_type", "authorization_code"),
        ("client_id", &client_id),
        ("code", &callback.code),
        ("code_verifier", &verifier),
        ("redirect_uri", &redirect_uri),
        ("resource", RESOURCE),
    ]);
    let (tokens, id_token) = on_auth_runtime(async {
        let client = auth_client(endpoints)?;
        let (status, body) = post_form(&client, &endpoints.token, form, cancel).await?;
        let (tokens, id_token) = parse_code_exchange(status, body.as_deref(), now_seconds())?;
        Ok((tokens, id_token))
    })?;
    let keys = on_auth_runtime(async {
        let client = auth_client(endpoints)?;
        get_json(&client, &endpoints.jwks, cancel).await
    })?;
    let identity = id_token::validate(
        &id_token,
        &keys,
        &endpoints.issuer,
        &client_id,
        &nonce,
        now_seconds(),
    )?;
    if request
        .subject
        .as_deref()
        .is_some_and(|recorded| recorded != identity.subject)
    {
        return Err(SignInError::IdentityMismatch);
    }
    Ok(SignedIn {
        client_id,
        subject: identity.subject,
        email: identity.email,
        tokens,
    })
}

/// Exchange a refresh token for a new pair. The refresh token rotates: the
/// returned pair's refresh token replaces it, and the old one is spent.
///
/// # Errors
///
/// [`SignInError::SignInRequired`] when the service refuses the refresh
/// token, and the other variants for a failed request.
pub fn refresh(
    endpoints: &AuthEndpoints,
    client_id: &str,
    refresh_token: &str,
    cancel: &CancelToken,
) -> Result<TokenSet, SignInError> {
    if cancel.is_cancelled() {
        return Err(SignInError::Cancelled);
    }
    let form = form_body(&[
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("refresh_token", refresh_token),
        ("resource", RESOURCE),
    ]);
    on_auth_runtime(async {
        let client = auth_client(endpoints)?;
        let (status, body) = post_form(&client, &endpoints.token, form, cancel).await?;
        parse_refresh(status, body.as_deref(), refresh_token, now_seconds())
    })
}

/// Revoke a refresh token once. There is no retry.
///
/// # Errors
///
/// Any failure: the caller reports that remote revocation was not confirmed.
pub fn revoke(
    endpoints: &AuthEndpoints,
    client_id: &str,
    refresh_token: &str,
    cancel: &CancelToken,
) -> Result<(), SignInError> {
    if cancel.is_cancelled() {
        return Err(SignInError::Cancelled);
    }
    let form = form_body(&[
        ("token", refresh_token),
        ("token_type_hint", "refresh_token"),
        ("client_id", client_id),
    ]);
    on_auth_runtime(async {
        let client = auth_client(endpoints)?;
        let (status, _) = post_form(&client, &endpoints.revoke, form, cancel).await?;
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(SignInError::HttpStatus(status))
        }
    })
}

/// A new host identifier: `urn:uuid:` and a random UUIDv4.
#[must_use]
pub fn new_host_id() -> String {
    format!("urn:uuid:{}", uuid::Uuid::new_v4().hyphenated())
}

/// Whether `text` is a host identifier this build creates.
#[must_use]
pub fn valid_host_id(text: &str) -> bool {
    text.strip_prefix("urn:uuid:")
        .and_then(|rest| uuid::Uuid::try_parse(rest).ok().map(|id| (rest, id)))
        .is_some_and(|(rest, id)| {
            id.get_version() == Some(uuid::Version::Random) && rest == id.hyphenated().to_string()
        })
}

/// Whether `text` can be a client id or subject: 1 to 256 visible ASCII
/// characters.
#[must_use]
pub fn valid_identifier(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_IDENTIFIER_BYTES
        && text.bytes().all(|b| b.is_ascii_graphic())
}

/// The authorization address for one attempt.
fn authorize_address(
    endpoints: &AuthEndpoints,
    request: &SignInRequest,
    redirect_uri: &str,
    state: &str,
    nonce: &str,
    verifier: &str,
) -> Url {
    let challenge = base64_url(&Sha256::digest(verifier.as_bytes()));
    let mut address = endpoints.authorize.clone();
    {
        let mut query = address.query_pairs_mut();
        query.append_pair("response_type", "code");
        match &request.client_id {
            Some(client_id) => {
                query.append_pair("client_id", client_id);
            }
            None => {
                query.append_pair("client_id", DYNAMIC_CLIENT);
                query.append_pair("agent_name_hint", AGENT_NAME);
            }
        }
        query
            .append_pair("ext_agent_host_id", &request.host_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", SCOPE)
            .append_pair("resource", RESOURCE)
            .append_pair("state", state)
            .append_pair("nonce", nonce)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
    }
    address
}

/// 32 random bytes as base64url without padding.
fn random_text() -> Result<Zeroizing<String>, SignInError> {
    let mut bytes = Zeroizing::new([0_u8; 32]);
    getrandom::fill(bytes.as_mut()).map_err(|_| SignInError::Transport)?;
    Ok(Zeroizing::new(base64_url(bytes.as_ref())))
}

fn base64_url(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Encode a form body without placing secrets in a temporary URL. Every
/// byte outside the unreserved set is percent-encoded.
fn form_body(pairs: &[(&str, &str)]) -> Zeroizing<String> {
    let mut body = Zeroizing::new(String::with_capacity(4096));
    for (index, (name, value)) in pairs.iter().enumerate() {
        if index > 0 {
            body.push('&');
        }
        percent_encode_into(&mut body, name);
        body.push('=');
        percent_encode_into(&mut body, value);
    }
    body
}

fn percent_encode_into(out: &mut String, text: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
}

/// Run one sign-in exchange on a single-thread runtime made for it.
fn on_auth_runtime<T>(
    exchange: impl Future<Output = Result<T, SignInError>>,
) -> Result<T, SignInError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|_| SignInError::Transport)?
        .block_on(exchange)
}

/// No proxy, no redirect, no retry, and a 30-second deadline.
fn auth_client(endpoints: &AuthEndpoints) -> Result<Client, SignInError> {
    Client::builder()
        .no_proxy()
        .https_only(endpoints.https_only)
        .redirect(Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(AUTH_REQUEST_TIMEOUT)
        .build()
        .map_err(|_| SignInError::Transport)
}

/// Post a form and read at most 64 KiB of the answer. A longer body is
/// returned as `None`.
async fn post_form(
    client: &Client,
    url: &Url,
    body: Zeroizing<String>,
    cancel: &CancelToken,
) -> Result<(u16, Option<Vec<u8>>), SignInError> {
    let build = |remaining: Duration| {
        client
            .post(url.clone())
            .timeout(remaining)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(reqwest::header::ACCEPT, "application/json")
            .body(body.as_bytes().to_vec())
    };
    let mut response = send_request(AUTH_REQUEST_TIMEOUT, build, cancel)
        .await
        .map_err(provider_to_sign_in)?;
    let status = response.status().as_u16();
    let body = read_error_body(&mut response, cancel)
        .await
        .map_err(provider_to_sign_in)?;
    Ok((status, body))
}

/// Fetch a small JSON document, such as the signing keys.
async fn get_json(
    client: &Client,
    url: &Url,
    cancel: &CancelToken,
) -> Result<Vec<u8>, SignInError> {
    let build = |remaining: Duration| {
        client
            .get(url.clone())
            .timeout(remaining)
            .header(reqwest::header::ACCEPT, "application/json")
    };
    let mut response = send_request(AUTH_REQUEST_TIMEOUT, build, cancel)
        .await
        .map_err(provider_to_sign_in)?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(SignInError::HttpStatus(status));
    }
    read_error_body(&mut response, cancel)
        .await
        .map_err(provider_to_sign_in)?
        .ok_or(SignInError::Malformed)
}

const fn provider_to_sign_in(error: ProviderError) -> SignInError {
    match error {
        ProviderError::Cancelled => SignInError::Cancelled,
        _ => SignInError::Transport,
    }
}

/// The documented token response. Unknown fields such as
/// `earliest_refresh_at` are ignored.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    token_type: Option<String>,
    expires_in: Option<u64>,
    scope: Option<String>,
}

/// An OAuth error answer.
#[derive(Deserialize)]
struct OAuthError {
    error: Option<String>,
}

fn parse_token_response(body: Option<&[u8]>) -> Result<TokenResponse, SignInError> {
    let value = parse_strict_json(body.ok_or(SignInError::Malformed)?)
        .map_err(|_| SignInError::Malformed)?;
    serde_json::from_value(value).map_err(|_| SignInError::Malformed)
}

/// The parts every token answer shares, after their checks.
struct CheckedAccess {
    access_token: Zeroizing<String>,
    /// The granted scopes, when the answer states them.
    scopes: Option<Vec<String>>,
    expires_at: u64,
}

/// Check the parts every token answer shares.
fn checked_access(response: &mut TokenResponse, now: u64) -> Result<CheckedAccess, SignInError> {
    if !response
        .token_type
        .as_deref()
        .is_some_and(|kind| kind.eq_ignore_ascii_case("bearer"))
    {
        return Err(SignInError::Malformed);
    }
    let expires_in = response
        .expires_in
        .filter(|seconds| (1..=MAX_EXPIRES_IN).contains(seconds))
        .ok_or(SignInError::Malformed)?;
    let access = Zeroizing::new(response.access_token.take().ok_or(SignInError::Malformed)?);
    if !valid_token(&access) {
        return Err(SignInError::Malformed);
    }
    let scopes = response.scope.as_deref().map(|scope| {
        scope
            .split_ascii_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    });
    if scopes
        .as_ref()
        .is_some_and(|scopes| !scopes.iter().any(|scope| scope == PLAN_SCOPE))
    {
        return Err(SignInError::PlanUsageNotGranted);
    }
    Ok(CheckedAccess {
        access_token: access,
        scopes,
        expires_at: now.saturating_add(expires_in),
    })
}

fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= MAX_TOKEN_BYTES
        && token.bytes().all(|b| b.is_ascii_graphic())
}

/// Check the code exchange's answer and return the tokens and the ID token.
fn parse_code_exchange(
    status: u16,
    body: Option<&[u8]>,
    now: u64,
) -> Result<(TokenSet, Zeroizing<String>), SignInError> {
    if !(200..300).contains(&status) {
        return Err(SignInError::HttpStatus(status));
    }
    let mut response = parse_token_response(body)?;
    let CheckedAccess {
        access_token,
        scopes,
        expires_at,
    } = checked_access(&mut response, now)?;
    // The exchange must state the grant, so the plan scope is never assumed.
    let scopes = scopes.ok_or(SignInError::PlanUsageNotGranted)?;
    let refresh_token = Zeroizing::new(
        response
            .refresh_token
            .take()
            .ok_or(SignInError::Malformed)?,
    );
    let id_token = Zeroizing::new(response.id_token.take().ok_or(SignInError::Malformed)?);
    if !valid_token(&refresh_token) {
        return Err(SignInError::Malformed);
    }
    Ok((
        TokenSet {
            access_token,
            refresh_token,
            scopes,
            expires_at,
        },
        id_token,
    ))
}

/// Check a refresh answer. A missing refresh token keeps the old one and a
/// missing scope keeps the plan scope, which the sign-in already checked.
fn parse_refresh(
    status: u16,
    body: Option<&[u8]>,
    old_refresh_token: &str,
    now: u64,
) -> Result<TokenSet, SignInError> {
    if !(200..300).contains(&status) {
        let refused = (400..500).contains(&status)
            && body
                .and_then(|bytes| parse_strict_json(bytes).ok())
                .and_then(|value| serde_json::from_value::<OAuthError>(value).ok())
                .and_then(|error| error.error)
                .is_some_and(|code| REFUSED_REFRESH_CODES.contains(&code.as_str()));
        return Err(if refused {
            SignInError::SignInRequired
        } else {
            SignInError::HttpStatus(status)
        });
    }
    let mut response = parse_token_response(body)?;
    let CheckedAccess {
        access_token,
        scopes,
        expires_at,
    } = checked_access(&mut response, now)?;
    let refresh_token = match response.refresh_token.take() {
        Some(token) if valid_token(&token) => Zeroizing::new(token),
        Some(_) => return Err(SignInError::Malformed),
        None => Zeroizing::new(old_refresh_token.to_owned()),
    };
    Ok(TokenSet {
        access_token,
        refresh_token,
        scopes: scopes.unwrap_or_else(|| vec![PLAN_SCOPE.to_owned()]),
        expires_at,
    })
}

/// Seconds since the Unix epoch; a clock before 1970 reads as zero.
pub(crate) fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests;
