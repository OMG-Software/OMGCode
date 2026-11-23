//! Retry a connect that Windows refused because the local port it chose still
//! holds a TIME_WAIT connection to the same server (ADR-0078).
//!
//! On Windows, `hyper-util` binds each client socket to port 0 before it
//! connects (`hyper-util` 0.1.21, `client/legacy/connect/http.rs`,
//! `bind_local_address`). The system then picks the local port without knowing
//! the destination. It returns the lowest free port, and a port whose earlier
//! connection to the same server is in TIME_WAIT counts as free, so `connect`
//! fails with `WSAEADDRINUSE` (10048). A connect without that bind was refused
//! the same way on the development host, so the bind is not the only cause.
//! The client usually holds that TIME_WAIT itself, from the previous request
//! it closed. Holding a bound, unconnected socket on the refused port makes
//! the next bind choose a later port.
//!
//! The retry is safe for every request: the refusal happens at `connect`, so no
//! byte of the request, and no credential header, reached any server. The
//! model provider client and the trusted package fetch both send through it.

use std::time::{Duration, Instant};

use reqwest::{RequestBuilder, Response};

/// The most ports one request skips. Each skip is a local connect refusal,
/// which costs about a millisecond, and holds one bound socket per address
/// family until the request is sent.
const MAX_SKIPPED_PORTS: usize = 64;

/// Send the request that `build` makes, skipping local ports that the
/// operating system refuses as already in use for this destination.
///
/// `build` is called once per attempt because sending consumes the request.
/// It receives the time left before `deadline`, so a caller that bounds the
/// request with a per-request timeout keeps one deadline across every
/// attempt rather than starting a new one; that timeout also covers reading
/// the body. Any other error, a refusal after [`MAX_SKIPPED_PORTS`] skips or
/// at the deadline, or a refusal that no port could be reserved for, is
/// returned as `send` reported it. The caller cancels the wait. The held
/// sockets are bound but never listen or connect, so they accept nothing, and
/// they are closed when this returns.
pub(crate) async fn send_skipping_reused_ports(
    deadline: Instant,
    mut build: impl FnMut(Duration) -> RequestBuilder,
) -> Result<Response, reqwest::Error> {
    let mut held = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match build(remaining).send().await {
            Err(error)
                if is_reused_local_port(&error)
                    && held.len() < MAX_SKIPPED_PORTS
                    && Instant::now() < deadline =>
            {
                match reserve_next_port() {
                    Some(port) => held.push(port),
                    None => return Err(error),
                }
            }
            result => return result,
        }
    }
}

/// Report whether a connect failed with `WSAEADDRINUSE`, the refusal of a
/// local port already in use for the destination.
#[cfg(windows)]
fn is_reused_local_port(error: &reqwest::Error) -> bool {
    /// Winsock's `WSAEADDRINUSE`.
    const WSAEADDRINUSE: i32 = 10048;
    if !error.is_connect() {
        return false;
    }
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = source {
        if current
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::raw_os_error)
            == Some(WSAEADDRINUSE)
        {
            return true;
        }
        source = current.source();
    }
    false
}

/// Only Windows chooses the port before the destination is known.
#[cfg(not(windows))]
const fn is_reused_local_port(_error: &reqwest::Error) -> bool {
    false
}

/// The sockets that hold one skipped port, one per address family.
struct HeldPort {
    _v4: Option<tokio::net::TcpSocket>,
    _v6: Option<tokio::net::TcpSocket>,
}

/// Bind one unconnected socket per address family to port 0, the same
/// unspecified addresses `hyper-util` binds, so each takes the port the next
/// connect would otherwise receive.
///
/// The destination's family is not known here, so both are held. Returns
/// `None` when neither family can bind; then there is no way to skip the port.
fn reserve_next_port() -> Option<HeldPort> {
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
    let v4 = tokio::net::TcpSocket::new_v4().ok().filter(|socket| {
        socket
            .bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
            .is_ok()
    });
    let v6 = tokio::net::TcpSocket::new_v6().ok().filter(|socket| {
        socket
            .bind(SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)))
            .is_ok()
    });
    if v4.is_none() && v6.is_none() {
        return None;
    }
    Some(HeldPort { _v4: v4, _v6: v6 })
}
