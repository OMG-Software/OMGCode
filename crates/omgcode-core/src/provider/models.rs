//! Model discovery on an OpenAI-compatible server (ADR-0066, ADR-0090).
//!
//! LM Studio, Ollama, and the hosted providers answer `GET {base}/models` with
//! `{"object":"list","data":[{"id":...}, ...]}`. The list lets an operator pick
//! a model instead of typing its name. It grants nothing: the chosen id is
//! still validated by [`super::LocalModelClient::new`] when a turn uses it, and
//! a listed id is untrusted provider text for display and selection only.

use super::{
    CONNECT_TIMEOUT, ModelEndpoint, ProviderError, bearer_header, read_whole_body, send_request,
};
use crate::storage::parse_strict_json;
use crate::turn::CancelToken;
use reqwest::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONNECTION};
use reqwest::redirect::Policy;
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

/// The most models one list may report. A longer list is refused, not cut.
/// OpenRouter reported 473 on 2026-11-24.
pub const MAX_LISTED_MODELS: usize = 2048;
/// The largest list body read. OpenRouter's list, which describes every
/// model, was 918 KB on 2026-11-24; a chat reply keeps the 1 MiB limit.
const MAX_LIST_BYTES: u64 = 4 * 1024 * 1024;
/// The longest model id accepted, the same bound the chat client applies.
const MAX_MODEL_ID_BYTES: usize = 256;
/// Total deadline for one list request. Listing reads no model, so it is
/// shorter than the thirty-second chat deadline.
const LIST_TIMEOUT: Duration = Duration::from_secs(10);

/// One model the local server reports.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ListedModel {
    /// The exact id to send as `model`. Untrusted provider text.
    pub id: String,
    /// Whether the id carries Ollama's cloud tag (`:cloud`, or a tag ending in
    /// `-cloud`). Ollama runs such a model on its hosted service, so a prompt
    /// sent to it leaves this machine. This is a naming convention, not proof:
    /// `false` does not show that a model runs locally.
    pub cloud_tag: bool,
}

/// List the models the local server at `base_url` reports, sorted by id.
///
/// `base_url` must pass the same endpoint rule as a chat connection: the
/// loopback rule (ADR-0048) or a hosted rule (ADR-0071, ADR-0090), which also
/// requires a token. `token`, when present, is sent exactly as a chat request
/// sends it. The request is one `GET {base}/models` with no proxy, no
/// redirect, no retry, a five-second connect timeout, a ten-second total
/// deadline, and a 4 MiB body limit. It runs on a single-thread runtime made for this call,
/// so the function must not be called from inside another async runtime.
///
/// # Errors
///
/// `InvalidConfiguration` for a refused endpoint and `InvalidToken` for an
/// unusable token, both before anything is sent; `Transport` for a connection,
/// read, or deadline failure; `HttpStatus` for a non-success status;
/// `ResponseTooLarge` for a body over 4 MiB; and `MalformedResponse` for a
/// body outside the list schema, more than [`MAX_LISTED_MODELS`] entries, a
/// repeated id, or an id that is empty, over 256 bytes, or holds a control or
/// bidirectional formatting character.
pub fn list_local_models(
    base_url: &str,
    token: Option<&str>,
) -> Result<Vec<ListedModel>, ProviderError> {
    list_with_timeout(base_url, token, LIST_TIMEOUT)
}

/// [`list_local_models`] with a chosen deadline, so a unit test can bound a
/// stalled fixture. Production callers always use [`LIST_TIMEOUT`].
fn list_with_timeout(
    base_url: &str,
    token: Option<&str>,
    timeout: Duration,
) -> Result<Vec<ListedModel>, ProviderError> {
    let model_endpoint = ModelEndpoint::parse(base_url)?;
    model_endpoint.check_token(token)?;
    let https_only = model_endpoint.https_only();
    let endpoint = model_endpoint.with_suffix("/models");
    let authorization = token.map(bearer_header).transpose()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|_| ProviderError::Transport)?;
    // Nothing cancels a list; the token only satisfies the shared body reader.
    let never = CancelToken::new();
    let bytes = runtime.block_on(async {
        // The client is built inside the runtime that drives it, as for chat.
        let client = Client::builder()
            .no_proxy()
            .https_only(https_only)
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(timeout)
            .build()
            .map_err(|_| ProviderError::Transport)?;
        // `Connection: close` asks the server to close after its reply. A
        // server that closes at once then holds the TIME_WAIT, which cannot
        // refuse a later Windows connect. That order was not observed: when
        // the server waits, the client still closes first, and the reused-port
        // skip handles the TIME_WAIT it holds. A server-held TIME_WAIT delays
        // a reconnect only after a large request, not a list (ADR-0078).
        // Only a plain-HTTP loopback request sends it: HTTP/2 forbids the
        // header, and a hosted endpoint is HTTPS. This build negotiates
        // HTTP/1.1 only, so the cloud request would accept it today.
        let close_after_reply = !https_only;
        let build = |remaining: Duration| {
            let mut request = client
                .get(endpoint.clone())
                .timeout(remaining)
                .header(ACCEPT, "application/json");
            if close_after_reply {
                request = request.header(CONNECTION, "close");
            }
            if let Some(authorization) = &authorization {
                request = request.header(AUTHORIZATION, authorization.clone());
            }
            request
        };
        let response = send_request(timeout, build, &never).await?;
        read_whole_body(response, MAX_LIST_BYTES, &never).await
    })?;
    parse_model_list(&bytes)
}

/// Parse a model list strictly. Duplicate keys at any depth are refused, and
/// members other than `data` and each entry's `id` are ignored.
fn parse_model_list(bytes: &[u8]) -> Result<Vec<ListedModel>, ProviderError> {
    let value = parse_strict_json(bytes).map_err(|_| ProviderError::MalformedResponse)?;
    let entries = value
        .get("data")
        .and_then(Value::as_array)
        .filter(|entries| entries.len() <= MAX_LISTED_MODELS)
        .ok_or(ProviderError::MalformedResponse)?;
    let mut ids = entries
        .iter()
        .map(|entry| {
            entry
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| listable_id(id))
                .map(str::to_owned)
                .ok_or(ProviderError::MalformedResponse)
        })
        .collect::<Result<Vec<String>, ProviderError>>()?;
    ids.sort_unstable();
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(ProviderError::MalformedResponse);
    }
    Ok(ids
        .into_iter()
        .map(|id| ListedModel {
            cloud_tag: has_cloud_tag(&id),
            id,
        })
        .collect())
}

/// An id the chat client would accept that also displays as what it is: no
/// bidirectional formatting character can reorder how it reads in a list.
fn listable_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_MODEL_ID_BYTES
        && !id.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
                )
        })
}

/// Whether the tag after the last `:` is `cloud` or ends in `-cloud`.
fn has_cloud_tag(id: &str) -> bool {
    id.rsplit_once(':')
        .is_some_and(|(_, tag)| tag == "cloud" || tag.ends_with("-cloud"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::REFUSED_ENDPOINTS;
    use crate::test_support::await_client_close;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Instant;

    /// The body LM Studio returned on the development host, ids unchanged.
    const LM_STUDIO_LIST: &str = r#"{"data":[{"id":"google/gemma-4-e4b","object":"model","owned_by":"organization_owner"},{"id":"omnicoder-9b","object":"model","owned_by":"organization_owner"},{"id":"text-embedding-nomic-embed-text-v1.5","object":"model","owned_by":"organization_owner"}],"object":"list"}"#;
    /// The body Ollama returned on the development host, ids unchanged.
    const OLLAMA_LIST: &str = r#"{"object":"list","data":[{"id":"kimi-k3:cloud","object":"model","created":1788094731,"owned_by":"library"},{"id":"gemma4:e2b","object":"model","created":1782044505,"owned_by":"library"},{"id":"lfm2.5:latest","object":"model","created":1782035596,"owned_by":"library"},{"id":"glm-5.1:cloud","object":"model","created":1778854415,"owned_by":"library"}]}"#;

    fn ids(models: &[ListedModel]) -> Vec<&str> {
        models.iter().map(|model| model.id.as_str()).collect()
    }

    /// The shape `https://ollama.com/v1/models` returned on 2026-11-20 (without
    /// a key), shortened to three entries.
    const OLLAMA_CLOUD_LIST: &str = r#"{"object":"list","data":[{"id":"minimax-m3","object":"model","created":1780272000,"owned_by":"ollama"},{"id":"gpt-oss:120b","object":"model","created":1754000000,"owned_by":"ollama"},{"id":"kimi-k3","object":"model","created":1788094731,"owned_by":"ollama"}]}"#;

    #[test]
    fn parses_the_ollama_cloud_list_shape() {
        let models = parse_model_list(OLLAMA_CLOUD_LIST.as_bytes()).expect("cloud list");
        assert_eq!(ids(&models), ["gpt-oss:120b", "kimi-k3", "minimax-m3"]);
        assert!(models.iter().all(|model| !model.cloud_tag));
    }

    /// A hosted endpoint needs a token, so a list request without one is
    /// refused before any connection, and a malformed endpoint is refused.
    #[test]
    fn a_cloud_list_without_a_token_is_refused_before_any_connection() {
        for endpoint in ["https://ollama.com/v1", "https://ollama.com/v1/"] {
            assert_eq!(
                list_local_models(endpoint, None),
                Err(ProviderError::InvalidToken),
                "{endpoint}"
            );
            assert_eq!(
                list_local_models(endpoint, Some("bad\nkey")),
                Err(ProviderError::InvalidToken),
                "{endpoint}"
            );
        }
        for endpoint in ["http://ollama.com/v1", "https://ollama.com:443/v1"] {
            assert_eq!(
                list_local_models(endpoint, Some("not-a-real-key")),
                Err(ProviderError::InvalidConfiguration),
                "{endpoint}"
            );
        }
        assert_eq!(
            list_local_models("https://provider-api.example.org/v1", None),
            Err(ProviderError::InvalidToken)
        );
    }

    #[test]
    fn parses_the_lm_studio_and_ollama_lists_sorted_by_id() {
        let lm_studio = parse_model_list(LM_STUDIO_LIST.as_bytes()).expect("LM Studio list");
        assert_eq!(
            ids(&lm_studio),
            [
                "google/gemma-4-e4b",
                "omnicoder-9b",
                "text-embedding-nomic-embed-text-v1.5"
            ]
        );
        assert!(lm_studio.iter().all(|model| !model.cloud_tag));

        let ollama = parse_model_list(OLLAMA_LIST.as_bytes()).expect("Ollama list");
        assert_eq!(
            ollama,
            [
                ("gemma4:e2b", false),
                ("glm-5.1:cloud", true),
                ("kimi-k3:cloud", true),
                ("lfm2.5:latest", false),
            ]
            .map(|(id, cloud_tag)| ListedModel {
                id: id.to_owned(),
                cloud_tag
            })
        );
    }

    #[test]
    fn the_cloud_tag_is_read_from_the_last_tag_only() {
        for (id, expected) in [
            ("gpt-oss:120b-cloud", true),
            ("kimi-k3:cloud", true),
            ("cloud", false),
            ("cloud:latest", false),
            ("qwen3:cloudy", false),
            ("my-cloud-model", false),
        ] {
            assert_eq!(has_cloud_tag(id), expected, "{id}");
        }
    }

    #[test]
    fn an_empty_list_is_valid() {
        assert_eq!(parse_model_list(br#"{"data":[]}"#), Ok(Vec::new()));
    }

    #[test]
    fn refuses_bodies_outside_the_list_schema() {
        let too_long = format!(r#"{{"data":[{{"id":"{}"}}]}}"#, "m".repeat(257));
        let longest = format!(r#"{{"data":[{{"id":"{}"}}]}}"#, "m".repeat(256));
        assert!(parse_model_list(longest.as_bytes()).is_ok());
        let entry = r#"{"id":"m"}"#;
        let many = |count: usize| {
            let entries: Vec<String> = (0..count).map(|n| format!(r#"{{"id":"m{n}"}}"#)).collect();
            format!(r#"{{"data":[{}]}}"#, entries.join(","))
        };
        assert!(parse_model_list(many(MAX_LISTED_MODELS).as_bytes()).is_ok());
        for body in [
            String::new(),
            "[]".to_owned(),
            r#"{"object":"list"}"#.to_owned(),
            r#"{"data":{}}"#.to_owned(),
            r#"{"data":[{}]}"#.to_owned(),
            r#"{"data":[{"id":7}]}"#.to_owned(),
            r#"{"data":["m"]}"#.to_owned(),
            r#"{"data":[{"id":""}]}"#.to_owned(),
            format!(r#"{{"data":[{entry},{entry}]}}"#),
            r#"{"data":[{"id":"a","id":"b"}]}"#.to_owned(),
            r#"{"data":[],"data":[]}"#.to_owned(),
            too_long,
            many(MAX_LISTED_MODELS + 1),
        ] {
            assert_eq!(
                parse_model_list(body.as_bytes()),
                Err(ProviderError::MalformedResponse),
                "{body:?}"
            );
        }
    }

    /// Control and bidirectional formatting characters are refused, written
    /// as JSON escapes so this file holds none of them.
    #[test]
    fn refuses_ids_that_would_display_as_something_else() {
        for escape in [
            "\\u0000", "\\u0007", "\\u001b", "\\u007f", "\\u061c", "\\u200e", "\\u200f", "\\u202a",
            "\\u202e", "\\u2066", "\\u2069",
        ] {
            let body = format!(r#"{{"data":[{{"id":"model{escape}name"}}]}}"#);
            assert_eq!(
                parse_model_list(body.as_bytes()),
                Err(ProviderError::MalformedResponse),
                "{escape}"
            );
        }
    }

    #[test]
    fn refuses_every_listed_endpoint_and_a_bad_token_before_sending() {
        for base_url in REFUSED_ENDPOINTS {
            assert_eq!(
                list_local_models(base_url, None),
                Err(ProviderError::InvalidConfiguration),
                "{base_url:?}"
            );
        }
        // Port 9 is never contacted: the token is refused first.
        for token in ["", "line\nbreak"] {
            assert_eq!(
                list_local_models("http://127.0.0.1:9/v1", Some(token)),
                Err(ProviderError::InvalidToken)
            );
        }
    }

    /// Serve one canned response and return the request head the client sent.
    fn serve_once(response: &'static [u8]) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("private fixture");
        listener.set_nonblocking(true).expect("bounded accept");
        let base_url = format!("http://{}/v1", listener.local_addr().expect("address"));
        let task = thread::spawn(move || {
            let end = Instant::now() + Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < end, "fixture accept deadline");
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            };
            stream.set_nonblocking(false).expect("blocking stream");
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .expect("read deadline");
            let mut head = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                assert!(
                    head.len() < 16 * 1024 && Instant::now() < end,
                    "bounded head"
                );
                let count = stream.read(&mut buffer).expect("request bytes");
                assert!(count > 0, "request head must be complete");
                head.extend_from_slice(&buffer[..count]);
            }
            // The client may already have given up; the head is what matters.
            let _ignored = stream.write_all(response);
            // A client that gave up still closes or resets first.
            assert!(
                await_client_close(&mut stream),
                "the client closes the fixture connection first"
            );
            String::from_utf8(head).expect("ASCII head")
        });
        (base_url, task)
    }

    #[test]
    fn sends_one_get_with_the_token_and_parses_the_reply() {
        let (base_url, task) = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 48\r\nConnection: close\r\n\r\n{\"object\":\"list\",\"data\":[{\"id\":\"b\"},{\"id\":\"a\"}]}",
        );
        let models = list_local_models(&base_url, Some("fixture-token")).expect("list");
        assert_eq!(ids(&models), ["a", "b"]);
        let head = task.join().expect("fixture thread");
        assert!(head.starts_with("GET /v1/models HTTP/1.1\r\n"), "{head}");
        assert!(
            head.lines()
                .any(|line| line.eq_ignore_ascii_case("authorization: Bearer fixture-token")),
            "{head}"
        );
    }

    #[test]
    fn a_missing_token_sends_no_authorization() {
        let (base_url, task) = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"data\":[]}",
        );
        assert_eq!(list_local_models(&base_url, None), Ok(Vec::new()));
        let head = task.join().expect("fixture thread").to_ascii_lowercase();
        assert!(!head.contains("authorization"), "{head}");
    }

    #[test]
    fn a_failure_status_is_reported_without_its_body() {
        let (base_url, task) = serve_once(
            b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"data\":[]}",
        );
        assert_eq!(
            list_local_models(&base_url, None),
            Err(ProviderError::HttpStatus(401))
        );
        task.join().expect("fixture thread");
    }

    #[test]
    fn a_redirect_is_not_followed() {
        let (base_url, task) = serve_once(
            b"HTTP/1.1 302 Found\r\nLocation: http://192.0.2.1:1234/v1/models\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(
            list_local_models(&base_url, None),
            Err(ProviderError::HttpStatus(302))
        );
        task.join().expect("fixture thread");
    }

    #[test]
    fn the_deadline_bounds_a_server_that_never_answers() {
        // The fixture accepts and holds the connection without answering
        // until the test releases it, or for at most three seconds, so only
        // the client's own deadline can end the request early.
        let listener = TcpListener::bind("127.0.0.1:0").expect("private fixture");
        listener.set_nonblocking(true).expect("bounded accept");
        let base_url = format!("http://{}/v1", listener.local_addr().expect("address"));
        let (release, released) = std::sync::mpsc::channel::<()>();
        let task = thread::spawn(move || {
            // Both waits are bounded, so a client that never connects ends the
            // fixture too, and the join below cannot hang.
            let end = Instant::now() + Duration::from_secs(3);
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break Some(stream),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= end || released.try_recv().is_ok() {
                            break None;
                        }
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            };
            // A timeout or a dropped sender both end the hold.
            let _ignored = released.recv_timeout(end.saturating_duration_since(Instant::now()));
            drop(stream);
        });
        let started = Instant::now();
        let result = list_with_timeout(&base_url, None, Duration::from_millis(250));
        let elapsed = started.elapsed();
        // The fixture may already be gone after its own limit.
        let _ignored = release.send(());
        task.join().expect("fixture thread");
        assert_eq!(result, Err(ProviderError::Transport));
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    }

    /// Back-to-back requests to a server that never closes first all succeed.
    ///
    /// The fixture ignores `Connection: close` and holds each connection until
    /// the client closes it, so every request leaves a client-side TIME_WAIT
    /// to the fixture. On Windows the next pre-bound connect is then usually
    /// handed that same local port and refused with `WSAEADDRINUSE` (ADR-0078);
    /// the client must skip it. Elsewhere the requests simply succeed.
    #[test]
    fn consecutive_requests_survive_client_side_time_wait() {
        const REQUESTS: usize = 6;
        let listener = TcpListener::bind("127.0.0.1:0").expect("private fixture");
        listener.set_nonblocking(true).expect("bounded accept");
        let base_url = format!("http://{}/v1", listener.local_addr().expect("address"));
        let task = thread::spawn(move || {
            let end = Instant::now() + Duration::from_secs(10);
            let body = LM_STUDIO_LIST.as_bytes();
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            for connection in 0..REQUESTS {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < end, "fixture accept deadline");
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => panic!("fixture accept: {error}"),
                    }
                };
                stream.set_nonblocking(false).expect("blocking stream");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("read deadline");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let count = stream.read(&mut buffer).expect("request bytes");
                    assert!(count > 0 && request.len() < 16 * 1024, "bounded head");
                    request.extend_from_slice(&buffer[..count]);
                }
                stream.write_all(head.as_bytes()).expect("response head");
                stream.write_all(body).expect("response body");
                // A timeout here would mean the fixture closes first, which
                // is not the case this test pins.
                assert!(
                    await_client_close(&mut stream),
                    "the client closes connection {connection} first"
                );
            }
        });
        for request in 0..REQUESTS {
            let models = list_local_models(&base_url, None)
                .unwrap_or_else(|error| panic!("request {request}: {error}"));
            assert_eq!(models.len(), 3, "request {request}");
        }
        task.join().expect("fixture thread");
    }
}
