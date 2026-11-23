//! Close order and port lifetime for loopback mock HTTP servers.

use std::io::Read;
use std::net::TcpListener;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

/// Wait, within the stream's read timeout, until a client that received a
/// complete `Connection: close` reply closes its side, and only then let the
/// caller drop the stream.
///
/// The client then holds the connection's TIME_WAIT. On Windows the next
/// client can receive the same local port, and a TIME_WAIT that the server
/// holds after a large request delays that connect until it times out
/// (ADR-0078). Call this only after a reply framed by `Content-Length`, never
/// after a reply that the close delimits, and only on a stream with a read
/// timeout. A read error or timeout ends the wait; the caller still closes.
///
/// Returns whether the client's close, orderly or abortive, was observed.
/// Any other read error, including
/// the timeout or `WouldBlock` on a stream left non-blocking, returns `false`:
/// then the fixture, not the client, closes first. A shared fixture asserts
/// the result so a regression of the close order fails loudly.
pub(crate) fn await_client_close(stream: &mut impl Read) -> bool {
    let mut sink = [0_u8; 512];
    loop {
        match stream.read(&mut sink) {
            Ok(0) => return true,
            Ok(_) => {}
            // An abortive close, as a cancelled client makes, leaves no
            // TIME_WAIT on either side.
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ) =>
            {
                return true;
            }
            Err(_) => return false,
        }
    }
}

/// Keep a non-blocking `listener` bound until `stop` disconnects, closing
/// every further connection at once.
///
/// A scripted server that dropped its listener after the last reply would
/// free the port while its client may still send a request. A concurrently
/// started test's mock can then bind that port and receive the request. The
/// owner drops the sender when its test finishes or unwinds.
pub(crate) fn refuse_until_stopped(listener: &TcpListener, stop: &Receiver<()>) {
    while matches!(stop.try_recv(), Err(TryRecvError::Empty)) {
        match listener.accept() {
            Ok((stream, _)) => drop(stream),
            Err(_) => thread::sleep(Duration::from_millis(5)),
        }
    }
}
