//! The Linux channel: a Unix stream socket in a private directory.
//!
//! # What protects it
//!
//! The directory must be owned by the current user with mode `0700`, and it
//! is opened with `openat2` and `RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS`,
//! so a link anywhere in the path is refused. The socket is then bound through
//! `/proc/self/fd/<directory descriptor>/<name>`, so the entry is created in
//! the directory that was checked even if its path is replaced afterwards.
//! Other users cannot reach the socket because they cannot enter the
//! directory; the accepted peer's user ID is also checked with `SO_PEERCRED`.
//!
//! Linux cannot cheaply prove that the peer is inside the sandbox: any program
//! of the same user that can enter the directory can connect, including a
//! sandboxed program that was granted the directory. The boundary is the
//! private directory plus the user check. A caller that must tell the
//! sandboxed relay from another same-user program adds a secret nonce that the
//! relay must present.
//!
//! The socket file is removed when the listener is dropped, which `accept`
//! does as it returns, so a late client finds nothing. Removal checks that the
//! entry is still the one that was bound and is best effort: a same-user
//! program that replaces it first only makes the removal skip.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{
    AtFlags, CWD, FileType, Mode, OFlags, ResolveFlags, fstat, openat2, statat, unlinkat,
};
use rustix::io::Errno;
use rustix::net::{
    AddressFamily, SocketAddrUnix, SocketFlags, SocketType, accept_with, bind,
    connect as connect_socket, listen, socket_with, sockopt,
};

use super::{IpcError, ListenerTarget};
use crate::process::CancelSignal;

/// Longest `sun_path` including its terminator on Linux.
const MAX_SOCKET_PATH_BYTES: usize = 107;
/// Longest file name in one directory entry.
const MAX_NAME_BYTES: usize = 255;
/// How often a wait rechecks the deadline and the cancel signal.
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Clients of another user turned away before `accept` gives up.
const MAX_REJECTED_CLIENTS: u32 = 8;
/// Pending connections the kernel queues. One client is expected.
const BACKLOG: i32 = 1;

fn os(operation: &'static str, error: Errno) -> IpcError {
    IpcError::os(operation, error.into())
}

/// The listening side: one socket, waiting for its one client.
pub(super) struct Listener {
    socket: OwnedFd,
    directory: OwnedFd,
    name: OsString,
    address: OsString,
    /// Device and inode of the bound socket file, for the removal check.
    identity: (u64, u64),
}

impl Listener {
    pub(super) fn bind(target: ListenerTarget) -> Result<Self, IpcError> {
        let ListenerTarget::PrivateDirectory { directory, name } = target;
        validate_name(&name)?;
        let address = directory.join(&name);
        if address.as_os_str().as_bytes().len() > MAX_SOCKET_PATH_BYTES {
            return Err(IpcError::InvalidTarget("the socket path is too long"));
        }
        let directory = open_private_directory(&directory)?;
        let through_descriptor =
            PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(&name);
        let socket_address = SocketAddrUnix::new(&through_descriptor)
            .map_err(|_| IpcError::InvalidTarget("the socket path is not usable"))?;
        let socket = socket_with(
            AddressFamily::UNIX,
            SocketType::STREAM,
            SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
            None,
        )
        .map_err(|error| os("create the socket", error))?;
        bind(&socket, &socket_address).map_err(|error| os("bind the socket", error))?;
        // From here the entry exists, so the value's `Drop` removes it on every
        // error path below.
        let identity =
            socket_identity(&directory, &name).map_err(|error| os("check the socket", error))?;
        let listener = Self {
            socket,
            directory,
            name,
            address: address.into_os_string(),
            identity,
        };
        listen(&listener.socket, BACKLOG).map_err(|error| os("listen", error))?;
        Ok(listener)
    }

    pub(super) fn address(&self) -> &OsStr {
        &self.address
    }

    pub(super) fn accept(
        self,
        deadline: Instant,
        cancel: &CancelSignal,
    ) -> Result<Stream, IpcError> {
        let mut rejected = 0_u32;
        loop {
            if cancel.is_cancelled() {
                return Err(IpcError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(IpcError::Deadline);
            }
            match accept_with(&self.socket, SocketFlags::CLOEXEC) {
                Ok(peer) => {
                    let own = rustix::process::getuid().as_raw();
                    let peer_uid =
                        sockopt::socket_peercred(&peer).map(|credentials| credentials.uid.as_raw());
                    if matches!(peer_uid, Ok(uid) if uid_allowed(uid, own)) {
                        let stream = UnixStream::from(peer);
                        stream
                            .set_nonblocking(false)
                            .map_err(|error| IpcError::os("configure the stream", error))?;
                        return Ok(Stream { stream });
                    }
                    // Dropping `peer` closes that client's connection.
                    rejected += 1;
                    if rejected > MAX_REJECTED_CLIENTS {
                        return Err(IpcError::PeerRejected);
                    }
                }
                Err(Errno::AGAIN | Errno::INTR | Errno::CONNABORTED) => {
                    self.wait_readable(deadline)?;
                }
                Err(error) => return Err(os("accept", error)),
            }
        }
    }

    fn wait_readable(&self, deadline: Instant) -> Result<(), IpcError> {
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(POLL_INTERVAL);
        let timeout = Timespec {
            tv_sec: 0,
            tv_nsec: i64::from(wait.subsec_nanos()),
        };
        let mut descriptors = [PollFd::new(&self.socket, PollFlags::IN)];
        match poll(&mut descriptors, Some(&timeout)) {
            Ok(_) | Err(Errno::INTR) => Ok(()),
            Err(error) => Err(os("wait for a client", error)),
        }
    }
}

impl Drop for Listener {
    /// Remove the socket file if it is still the one this listener bound. A
    /// failure only leaves a stale file that the next `bind` refuses, and
    /// `drop` must not panic or report.
    fn drop(&mut self) {
        if socket_identity(&self.directory, &self.name)
            .is_ok_and(|current| current == self.identity)
        {
            // A failed removal leaves the file; see above.
            let _ = unlinkat(&self.directory, self.name.as_os_str(), AtFlags::empty());
        }
    }
}

/// Whether a peer with user ID `peer` may connect to a listener of `own`.
fn uid_allowed(peer: u32, own: u32) -> bool {
    peer == own
}

fn socket_identity(directory: &OwnedFd, name: &OsStr) -> Result<(u64, u64), Errno> {
    let stat = statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)?;
    Ok((stat.st_dev, stat.st_ino))
}

/// One path component that is not `.` or `..`.
fn validate_name(name: &OsStr) -> Result<(), IpcError> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes.len() > MAX_NAME_BYTES
        || bytes == b"."
        || bytes == b".."
        || bytes.iter().any(|byte| matches!(byte, b'/' | 0))
    {
        return Err(IpcError::InvalidTarget(
            "the socket name must be one plain file name",
        ));
    }
    Ok(())
}

/// Open `directory` without following any link and require that it is a
/// directory of the current user with mode `0700` exactly.
fn open_private_directory(directory: &Path) -> Result<OwnedFd, IpcError> {
    if !directory.is_absolute() {
        return Err(IpcError::InvalidTarget("the directory must be absolute"));
    }
    let descriptor = match openat2(
        CWD,
        directory,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
    ) {
        Ok(descriptor) => descriptor,
        Err(Errno::LOOP | Errno::XDEV) => {
            return Err(IpcError::InvalidTarget(
                "the directory path contains a link",
            ));
        }
        Err(Errno::NOENT | Errno::NOTDIR) => {
            return Err(IpcError::InvalidTarget("the directory does not exist"));
        }
        Err(Errno::NOSYS) => {
            return Err(IpcError::InvalidTarget("the kernel lacks openat2"));
        }
        Err(error) => return Err(os("open the directory", error)),
    };
    let stat = fstat(&descriptor).map_err(|error| os("check the directory", error))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return Err(IpcError::InvalidTarget("the path is not a directory"));
    }
    if stat.st_uid != rustix::process::getuid().as_raw() {
        return Err(IpcError::InvalidTarget(
            "the directory is not owned by the current user",
        ));
    }
    if stat.st_mode & 0o7777 != 0o700 {
        return Err(IpcError::InvalidTarget("the directory mode must be 0700"));
    }
    Ok(descriptor)
}

/// A connected socket that has not been split.
pub(super) struct Stream {
    stream: UnixStream,
}

impl Stream {
    pub(super) fn split(self) -> Result<(Reader, Writer, Shutdown), IpcError> {
        let reader = self
            .stream
            .try_clone()
            .map_err(|error| IpcError::os("duplicate the stream", error))?;
        let handle = self
            .stream
            .try_clone()
            .map_err(|error| IpcError::os("duplicate the stream", error))?;
        Ok((
            Reader { stream: reader },
            Writer {
                stream: self.stream,
            },
            Shutdown {
                stream: Arc::new(handle),
            },
        ))
    }
}

/// The read half.
pub(super) struct Reader {
    stream: UnixStream,
}

impl Reader {
    pub(super) fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buffer)
    }
}

/// The write half.
pub(super) struct Writer {
    stream: UnixStream,
}

impl Writer {
    pub(super) fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stream.write(buffer)
    }

    pub(super) fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// Shuts the socket down in both directions from any thread. The clones share
/// one duplicated descriptor.
#[derive(Clone)]
pub(super) struct Shutdown {
    stream: Arc<UnixStream>,
}

impl Shutdown {
    pub(super) fn shutdown(&self) -> io::Result<()> {
        match self.stream.shutdown(std::net::Shutdown::Both) {
            // The peer closed first; the channel is already down.
            Err(error) if error.kind() == io::ErrorKind::NotConnected => Ok(()),
            other => other,
        }
    }
}

/// Connect to the socket at `address`, retrying a full queue until `deadline`.
pub(super) fn connect(address: &OsStr, deadline: Instant) -> Result<Stream, IpcError> {
    let socket_address = SocketAddrUnix::new(Path::new(address))
        .map_err(|_| IpcError::InvalidTarget("the socket path is not usable"))?;
    loop {
        if Instant::now() >= deadline {
            return Err(IpcError::Deadline);
        }
        let socket = socket_with(
            AddressFamily::UNIX,
            SocketType::STREAM,
            SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
            None,
        )
        .map_err(|error| os("create the socket", error))?;
        match connect_socket(&socket, &socket_address) {
            Ok(()) => {
                let stream = UnixStream::from(socket);
                stream
                    .set_nonblocking(false)
                    .map_err(|error| IpcError::os("configure the stream", error))?;
                return Ok(Stream { stream });
            }
            Err(Errno::AGAIN | Errno::INTR) => {
                // The accept queue is full or the call was interrupted.
                thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(POLL_INTERVAL),
                );
            }
            Err(error) => return Err(os("connect", error)),
        }
    }
}

#[cfg(test)]
mod tests;
