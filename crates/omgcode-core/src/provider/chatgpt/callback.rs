//! The loopback listener that receives the sign-in callback (ADR-0092).
//!
//! It binds only `127.0.0.1` on a port the system chooses and serves one
//! fixed page. A request to another path is answered 404 and the wait goes
//! on; the first request to the callback path ends the attempt, valid or not.
//! No page reflects a parameter.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use omgcode_platform::credentials::Zeroizing;

use super::{CALLBACK_PATH, DYNAMIC_CLIENT, SignInError, valid_identifier};
use crate::turn::CancelToken;

/// How often the wait checks for a connection and for cancellation.
const POLL: Duration = Duration::from_millis(50);
/// The time one connection has to send its request line and headers.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
/// The most one connection may send before its request is complete.
const MAX_REQUEST_BYTES: usize = 8 * 1024;
/// The most query parameters a callback may carry.
const MAX_PARAMETERS: usize = 16;

const PAGE_DONE: &str = "Sign-in to OMGCode finished. You can close this tab.";
const PAGE_FAILED: &str = "Sign-in to OMGCode did not finish. Return to OMGCode for details.";
const PAGE_NOT_FOUND: &str = "Not found.";

/// The checked callback: the code to exchange and the client it was issued
/// to.
pub(super) struct Callback {
    pub(super) code: Zeroizing<String>,
    pub(super) client_id: String,
}

/// A listener bound for one sign-in attempt.
pub(super) struct CallbackListener {
    listener: TcpListener,
    port: u16,
}

impl CallbackListener {
    /// Bind `127.0.0.1` on a system-chosen port.
    pub(super) fn bind() -> Result<Self, SignInError> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .map_err(|_| SignInError::Listener)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| SignInError::Listener)?;
        let port = listener
            .local_addr()
            .map_err(|_| SignInError::Listener)?
            .port();
        Ok(Self { listener, port })
    }

    /// The redirect address OpenAI must send the browser to.
    pub(super) fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}{CALLBACK_PATH}", self.port)
    }

    /// Wait for the callback, checking `cancel` every 50 ms.
    pub(super) fn wait(
        &self,
        state: &str,
        returning_client: Option<&str>,
        wait: Duration,
        cancel: &CancelToken,
    ) -> Result<Callback, SignInError> {
        let deadline = Instant::now() + wait;
        loop {
            if cancel.is_cancelled() {
                return Err(SignInError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(SignInError::TimedOut);
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(result) = serve(stream, state, returning_client) {
                        return result;
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => std::thread::sleep(POLL),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => return Err(SignInError::Listener),
            }
        }
    }
}

/// Serve one connection. `None` means it was not the callback and the wait
/// continues.
fn serve(
    mut stream: TcpStream,
    state: &str,
    returning_client: Option<&str>,
) -> Option<Result<Callback, SignInError>> {
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(CONNECTION_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(CONNECTION_TIMEOUT)).is_err()
    {
        return None;
    }
    let target = read_request_target(&mut stream)?;
    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    if path != CALLBACK_PATH {
        respond(&mut stream, "404 Not Found", PAGE_NOT_FOUND);
        return None;
    }
    let result = check_callback(query, state, returning_client);
    let page = if result.is_ok() {
        PAGE_DONE
    } else {
        PAGE_FAILED
    };
    respond(&mut stream, "200 OK", page);
    Some(result)
}

/// Read until the end of the headers, within 8 KiB and five seconds, and
/// return the target of a `GET` request. Anything else is dropped.
fn read_request_target(stream: &mut TcpStream) -> Option<Zeroizing<String>> {
    let deadline = Instant::now() + CONNECTION_TIMEOUT;
    let mut bytes = Zeroizing::new(Vec::with_capacity(1024));
    let mut chunk = [0_u8; 1024];
    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        if Instant::now() >= deadline || bytes.len() >= MAX_REQUEST_BYTES {
            return None;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(read) => bytes.extend_from_slice(chunk.get(..read)?),
        }
    }
    let line_end = bytes.windows(2).position(|window| window == b"\r\n")?;
    let line = std::str::from_utf8(bytes.get(..line_end)?).ok()?;
    let mut parts = line.split(' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    if method != "GET" || !version.starts_with("HTTP/1.") || parts.next().is_some() {
        return None;
    }
    target
        .starts_with('/')
        .then(|| Zeroizing::new(target.to_owned()))
}

/// A fixed page with no caching and no referrer.
fn respond(stream: &mut TcpStream, status: &str, page: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Check the callback query against this attempt.
fn check_callback(
    query: &str,
    state: &str,
    returning_client: Option<&str>,
) -> Result<Callback, SignInError> {
    let parameters = parse_query(query)?;
    let get = |name: &str| {
        parameters
            .iter()
            .find(|(key, _)| key.as_str() == name)
            .map(|(_, value)| value.as_str())
    };
    if get("state") != Some(state) {
        return Err(SignInError::StateMismatch);
    }
    if let Some(error) = get("error") {
        return Err(if error == "access_denied" {
            SignInError::AccessDenied
        } else {
            SignInError::Callback("the service returned an error")
        });
    }
    let code = get("code")
        .filter(|code| !code.is_empty())
        .ok_or(SignInError::Callback("no authorization code"))?;
    let client_id = match (returning_client, get("client_id")) {
        (Some(recorded), None) => recorded.to_owned(),
        (Some(recorded), Some(issued)) if issued == recorded => recorded.to_owned(),
        (Some(_), Some(_)) => return Err(SignInError::ClientMismatch),
        (None, Some(issued)) if issued != DYNAMIC_CLIENT && valid_identifier(issued) => {
            issued.to_owned()
        }
        (None, _) => return Err(SignInError::Callback("no issued client registration")),
    };
    Ok(Callback {
        code: Zeroizing::new(code.to_owned()),
        client_id,
    })
}

/// Split and decode a query. A parameter named twice, a bad escape, or more
/// than 16 parameters refuses the callback.
pub(super) fn parse_query(query: &str) -> Result<Vec<(String, Zeroizing<String>)>, SignInError> {
    let mut parameters: Vec<(String, Zeroizing<String>)> = Vec::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        if parameters.len() >= MAX_PARAMETERS {
            return Err(SignInError::Callback("too many parameters"));
        }
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let name = percent_decode(name).ok_or(SignInError::Callback("a bad escape"))?;
        let value = percent_decode(value).ok_or(SignInError::Callback("a bad escape"))?;
        if parameters
            .iter()
            .any(|(known, _)| known.as_str() == name.as_str())
        {
            return Err(SignInError::Callback("a repeated parameter"));
        }
        parameters.push((name.to_string(), value));
    }
    Ok(parameters)
}

/// Decode `+` and `%XX` into UTF-8 text.
fn percent_decode(text: &str) -> Option<Zeroizing<String>> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(text.len()));
    let mut input = text.bytes();
    while let Some(byte) = input.next() {
        match byte {
            b'+' => bytes.push(b' '),
            b'%' => {
                let high = char::from(input.next()?).to_digit(16)?;
                let low = char::from(input.next()?).to_digit(16)?;
                bytes.push(u8::try_from(high * 16 + low).ok()?);
            }
            other => bytes.push(other),
        }
    }
    String::from_utf8(bytes.to_vec()).ok().map(Zeroizing::new)
}
