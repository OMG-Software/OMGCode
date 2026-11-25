//! The HTTPS client behind [`HttpsFetcher`](super::HttpsFetcher).
//!
//! One fetch builds one client inside one current-thread runtime and drops
//! both on return, so no pooled connection outlives the fetch (the same shape
//! as the model provider and package clients). Every wait, from the name
//! lookup through the connect and the body, runs under [`guarded`], which
//! enforces one deadline for the whole fetch across redirect hops and watches
//! the cancel token.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::header::{
    ACCEPT, ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_TYPE, HeaderMap, HeaderValue, LOCATION,
    USER_AGENT,
};
use reqwest::{Client, Response, StatusCode, redirect::Policy};

use super::address::{PinningResolver, ResolveError, ResolveState};
use super::html_text::html_to_text;
use super::{
    FetchError, FetchOutcome, FetchUrl, FetchedPage, MAX_BODY_BYTES, MAX_REDIRECTS, MAX_TEXT_BYTES,
    UrlRefusal, UrlRules, sanitize_text,
};
use crate::turn::CancelToken;

/// The `Accept` header value. Fixed, with no per-request variation.
const ACCEPT_VALUE: &str = "text/html, text/plain, text/markdown, application/json, \
application/xml;q=0.9, */*;q=0.1";
/// How often a waiting fetch checks the cancel token.
const CANCEL_POLL: Duration = Duration::from_millis(25);
/// The longest `Content-Type` essence kept.
const MAX_CONTENT_TYPE_BYTES: usize = 128;

/// The limits and URL rules one fetcher uses.
///
/// Production code can only make [`ClientPolicy::production`]. The loopback
/// constructor used by the tests does not exist outside `cfg(test)`, so no
/// production path can allow plain HTTP or a loopback host.
#[derive(Clone, Copy, Debug)]
pub(super) struct ClientPolicy {
    pub(super) rules: UrlRules,
    /// The whole-fetch deadline, across every redirect hop.
    pub(super) deadline: Duration,
    pub(super) connect_timeout: Duration,
}

impl ClientPolicy {
    pub(super) const fn production() -> Self {
        Self {
            rules: UrlRules::Public,
            deadline: super::FETCH_DEADLINE,
            connect_timeout: super::CONNECT_TIMEOUT,
        }
    }

    /// Plain HTTP and loopback hosts, with a short deadline. Tests only.
    #[cfg(test)]
    pub(super) const fn loopback(deadline: Duration) -> Self {
        Self {
            rules: UrlRules::LoopbackHttp,
            deadline,
            connect_timeout: deadline,
        }
    }
}

/// The content classes that are read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContentKind {
    Html,
    Text,
}

/// Await `future` under the fetch deadline, watching `cancel`.
///
/// The future is polled in short slices and is never restarted. It is dropped
/// when the token is set or the deadline passes, which closes any connection
/// it held.
async fn guarded<F: Future>(
    future: F,
    deadline: Instant,
    cancel: &CancelToken,
) -> Result<F::Output, FetchError> {
    let mut future = std::pin::pin!(future);
    loop {
        if cancel.is_cancelled() {
            return Err(FetchError::Cancelled);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(FetchError::Timeout);
        }
        if let Ok(output) = tokio::time::timeout(CANCEL_POLL.min(remaining), future.as_mut()).await
        {
            return Ok(output);
        }
    }
}

/// Fetch `start` and follow same-host redirects. See the module contract in
/// [`super`] for the outcomes.
pub(super) async fn run(
    policy: ClientPolicy,
    start: &FetchUrl,
    cancel: &CancelToken,
) -> Result<FetchOutcome, FetchError> {
    let deadline = Instant::now()
        .checked_add(policy.deadline)
        .ok_or(FetchError::Internal)?;
    let state = Arc::new(ResolveState::default());
    let client = build_client(&policy, &state)?;
    let user_agent =
        HeaderValue::from_str(&format!("OMGCode/{} web_fetch", env!("CARGO_PKG_VERSION")))
            .map_err(|_| FetchError::Internal)?;

    let mut current = start.clone();
    let mut followed = 0_usize;
    loop {
        let response = guarded(
            send(&client, &current, &user_agent, deadline),
            deadline,
            cancel,
        )
        .await?
        .map_err(|error| classify_send_error(&error, &state))?;
        let status = response.status();

        if matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
            let Some(location) = response.headers().get(LOCATION) else {
                return Ok(FetchOutcome::HttpStatus {
                    url: current,
                    status: status.as_u16(),
                });
            };
            let target = redirect_target(policy.rules, &current, location);
            // The redirect body is never read; dropping the response closes
            // the connection.
            drop(response);
            let target = match target {
                Ok(target) => target,
                Err(reason) => {
                    return Ok(FetchOutcome::RedirectRefused {
                        url: current,
                        status: status.as_u16(),
                        reason,
                    });
                }
            };
            if !current.same_host(&target) {
                return Ok(FetchOutcome::CrossHostRedirect {
                    url: current,
                    status: status.as_u16(),
                    target,
                });
            }
            followed += 1;
            if followed > MAX_REDIRECTS {
                return Err(FetchError::TooManyRedirects);
            }
            current = target;
            continue;
        }

        if !status.is_success() {
            return Ok(FetchOutcome::HttpStatus {
                url: current,
                status: status.as_u16(),
            });
        }
        return read_page(start, current, status, response, deadline, cancel).await;
    }
}

fn build_client(policy: &ClientPolicy, state: &Arc<ResolveState>) -> Result<Client, FetchError> {
    Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .retry(reqwest::retry::never())
        .referer(false)
        .https_only(policy.rules == UrlRules::Public)
        .tls_version_min(reqwest::tls::Version::TLS_1_2)
        .connect_timeout(policy.connect_timeout.min(policy.deadline))
        .pool_max_idle_per_host(0)
        .dns_resolver(PinningResolver::new(Arc::clone(state)))
        .build()
        .map_err(|_| FetchError::Internal)
}

/// Send one GET with the fixed headers.
///
/// The send goes through `send_skipping_reused_ports` (ADR-0078). It applies:
/// this client opens a new connection to the same server for each redirect
/// hop and each fetch, and on Windows a local port that still holds a
/// TIME_WAIT connection to that server is refused at `connect`, before any
/// byte is sent. The retry is therefore safe for a GET and leaks nothing, it
/// has no effect on other platforms, and each attempt takes the time left, so
/// the single deadline holds. The cancel token and the deadline wrap this
/// whole call in [`guarded`].
async fn send(
    client: &Client,
    url: &FetchUrl,
    user_agent: &HeaderValue,
    deadline: Instant,
) -> Result<Response, reqwest::Error> {
    crate::connect::send_skipping_reused_ports(deadline, |remaining| {
        client
            .get(url.url().clone())
            .timeout(remaining)
            .header(USER_AGENT, user_agent.clone())
            .header(ACCEPT, HeaderValue::from_static(ACCEPT_VALUE))
            .header(ACCEPT_ENCODING, HeaderValue::from_static("identity"))
    })
    .await
}

/// Resolve a `Location` against the current URL and apply the URL policy.
fn redirect_target(
    rules: UrlRules,
    current: &FetchUrl,
    location: &HeaderValue,
) -> Result<FetchUrl, UrlRefusal> {
    let location = location
        .to_str()
        .map_err(|_| UrlRefusal::InvalidCharacter)?;
    if location.len() > super::MAX_URL_BYTES {
        return Err(UrlRefusal::TooLong);
    }
    let joined = current
        .url()
        .join(location)
        .map_err(|_| UrlRefusal::InvalidUrl)?;
    FetchUrl::parse_with(rules, joined.as_str())
}

/// Turn a failed send into a typed error.
///
/// A resolver refusal is the first source: it is recorded in `state` and
/// also sits in the error chain. Any other connect failure is a TLS failure
/// when no I/O error is in the chain, which is how `native-tls` handshake
/// errors reach the caller; that split is a heuristic over the error text
/// that is never shown.
fn classify_send_error(error: &reqwest::Error, state: &ResolveState) -> FetchError {
    let resolver = state.failure().or_else(|| chain_resolve_error(error));
    match resolver {
        Some(ResolveError::Blocked) => return FetchError::BlockedAddress,
        Some(ResolveError::Lookup) => return FetchError::DnsFailed,
        None => {}
    }
    if error.is_timeout() {
        return FetchError::Timeout;
    }
    if error.is_connect() {
        return if chain_has_io_error(error) {
            FetchError::ConnectFailed
        } else {
            FetchError::TlsFailed
        };
    }
    FetchError::Transport
}

pub(super) fn chain_resolve_error(error: &reqwest::Error) -> Option<ResolveError> {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = source {
        if let Some(found) = current.downcast_ref::<ResolveError>() {
            return Some(*found);
        }
        source = current.source();
    }
    None
}

fn chain_has_io_error(error: &reqwest::Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = source {
        if current.downcast_ref::<std::io::Error>().is_some() {
            return true;
        }
        source = current.source();
    }
    false
}

/// Classify a `Content-Type` header into the lower-case media type and how
/// to read it. A missing, unreadable or unsupported type is `None`.
fn content_kind(headers: &HeaderMap) -> Option<(String, ContentKind)> {
    let value = headers.get(CONTENT_TYPE)?.to_str().ok()?;
    let essence = value.split(';').next()?.trim().to_ascii_lowercase();
    if essence.is_empty() || essence.len() > MAX_CONTENT_TYPE_BYTES {
        return None;
    }
    let kind = match essence.as_str() {
        "text/html" | "application/xhtml+xml" => ContentKind::Html,
        "text/plain" | "text/markdown" | "text/x-markdown" | "text/csv" | "text/xml"
        | "application/xml" | "application/json" | "application/yaml" | "application/x-yaml"
        | "text/yaml" => ContentKind::Text,
        other => {
            let structured = other.strip_prefix("application/").is_some_and(|subtype| {
                ["+xml", "+json"]
                    .iter()
                    .any(|suffix| subtype.len() > suffix.len() && subtype.ends_with(suffix))
            });
            if structured {
                ContentKind::Text
            } else {
                return None;
            }
        }
    };
    Some((essence, kind))
}

/// Refuse any `Content-Encoding` other than identity. The request asks for
/// identity and no decompression feature is compiled in, so an encoded body
/// would be raw compressed bytes.
fn encoding_is_identity(headers: &HeaderMap) -> bool {
    headers.get_all(CONTENT_ENCODING).iter().all(|value| {
        value
            .to_str()
            .is_ok_and(|text| text.trim().eq_ignore_ascii_case("identity"))
    })
}

async fn read_page(
    requested: &FetchUrl,
    final_url: FetchUrl,
    status: StatusCode,
    mut response: Response,
    deadline: Instant,
    cancel: &CancelToken,
) -> Result<FetchOutcome, FetchError> {
    let (content_type, kind) =
        content_kind(response.headers()).ok_or(FetchError::UnsupportedContentType)?;
    if !encoding_is_identity(response.headers()) {
        return Err(FetchError::UnsupportedEncoding);
    }

    let mut body: Vec<u8> = Vec::new();
    let mut body_truncated = false;
    loop {
        let chunk = guarded(response.chunk(), deadline, cancel)
            .await?
            .map_err(|error| {
                if error.is_timeout() {
                    FetchError::Timeout
                } else {
                    FetchError::BodyReadFailed
                }
            })?;
        let Some(chunk) = chunk else { break };
        let room = MAX_BODY_BYTES.saturating_sub(body.len());
        if chunk.len() > room {
            body.extend_from_slice(chunk.get(..room).unwrap_or_default());
            body_truncated = true;
            break;
        }
        body.extend_from_slice(&chunk);
    }
    // Dropping the response closes the connection, including after a cut.
    drop(response);

    // The text only goes to a model for reading and never to an
    // authorization decision, so replacing invalid UTF-8 with U+FFFD loses
    // nothing that matters; the declared charset is not honoured.
    let decoded = String::from_utf8_lossy(&body);
    let (converted, converter_truncated) = match kind {
        ContentKind::Html => {
            let page = html_to_text(&decoded, final_url.url(), MAX_TEXT_BYTES);
            (page.text, page.truncated)
        }
        ContentKind::Text => (decoded.into_owned(), false),
    };
    let (text, cut) = sanitize_text(&converted, MAX_TEXT_BYTES);
    Ok(FetchOutcome::Page(FetchedPage {
        requested_url: requested.clone(),
        final_url,
        status: status.as_u16(),
        content_type,
        text,
        body_truncated,
        text_truncated: converter_truncated || cut,
    }))
}
