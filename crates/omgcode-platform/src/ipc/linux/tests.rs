//! Socket tests. The client runs in this process on another thread: Linux
//! has no per-container identity to check, so the boundary under test is the
//! directory and the user check.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::*;
use crate::ipc::{IpcError, ListenerTarget, LocalListener, LocalStream, ShutdownHandle, connect};

const LONG: Duration = Duration::from_secs(30);
const PAYLOAD_BYTES: usize = 512 * 1024;
/// Far more than a socket's buffers hold, so a write nobody reads blocks.
const FILLING_BYTES: usize = 16 * 1024 * 1024;

fn private_directory() -> tempfile::TempDir {
    let directory = tempfile::Builder::new()
        .prefix("omgcode-ipc-")
        .tempdir()
        .expect("temporary directory");
    set_mode(directory.path(), 0o700);
    directory
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set mode");
}

fn target(directory: &Path, name: &str) -> ListenerTarget {
    ListenerTarget::PrivateDirectory {
        directory: directory.to_owned(),
        name: OsString::from(name),
    }
}

fn bind_in(directory: &Path) -> LocalListener {
    LocalListener::bind(target(directory, "relay.sock")).expect("bind")
}

fn refused(result: Result<LocalListener, IpcError>) -> &'static str {
    match result {
        Err(IpcError::InvalidTarget(reason)) => reason,
        Err(other) => panic!("expected a refused target, got {other:?}"),
        Ok(_) => panic!("the target was accepted"),
    }
}

/// Whether `handle` finished within `limit`.
fn finished_within<T>(handle: &JoinHandle<T>, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(5));
    }
    true
}

fn join_within<T>(handle: JoinHandle<T>, limit: Duration) -> T {
    assert!(
        finished_within(&handle, limit),
        "the thread did not finish in {limit:?}"
    );
    handle.join().expect("thread panicked")
}

/// Join `handle` within `limit`. A thread that overruns is not left holding
/// the connection: `shutdown` ends its blocked reads and writes, and the
/// peer's, and the thread is joined within a second bound before the test
/// fails.
fn join_or_shut<T>(handle: JoinHandle<T>, limit: Duration, shutdown: &ShutdownHandle) -> T {
    if finished_within(&handle, limit) {
        return handle.join().expect("thread panicked");
    }
    let shut = shutdown.shutdown();
    let ended = finished_within(&handle, LONG);
    if ended {
        let _ = handle.join();
    }
    panic!("the thread did not finish in {limit:?}; shutdown: {shut:?}; it then ended: {ended}");
}

/// The calling thread's kernel id: `/proc/thread-self` links to
/// `<pid>/task/<tid>`.
fn thread_id() -> u32 {
    fs::read_link("/proc/thread-self")
        .ok()
        .and_then(|link| link.file_name()?.to_str()?.parse().ok())
        .expect("this thread's id")
}

/// Whether thread `id` of this process is asleep in the kernel: the state
/// field of its `stat`, after the parenthesised name, is `S`. A thread that
/// already ended is not.
fn asleep(id: u32) -> bool {
    fs::read_to_string(format!("/proc/self/task/{id}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            == Some("S")
    })
}

/// Wait, bounded, for a thread that sent its id immediately before a
/// blocking call to sleep in the kernel. Nothing between the send and the
/// call sleeps, so the call is then in progress, not about to start.
fn await_blocked(pending: &Receiver<u32>) {
    let id = pending
        .recv_timeout(LONG)
        .expect("the thread never reached its blocking call");
    let deadline = Instant::now() + LONG;
    while !asleep(id) {
        assert!(Instant::now() < deadline, "the thread never blocked");
        thread::sleep(Duration::from_millis(1));
    }
}

/// Accept on another thread, which sends its id immediately before the
/// blocking `accept` call.
fn accept_in_thread(
    listener: LocalListener,
    cancel: &CancelSignal,
    wait: Duration,
) -> (JoinHandle<Result<LocalStream, IpcError>>, Receiver<u32>) {
    let cancel = cancel.clone();
    let (started, pending) = mpsc::channel();
    let handle = thread::spawn(move || {
        started
            .send(thread_id())
            .expect("report that accept is starting");
        listener.accept(Instant::now() + wait, &cancel)
    });
    (handle, pending)
}

fn client(address: &OsStr) -> JoinHandle<LocalStream> {
    let address = address.to_owned();
    thread::spawn(move || connect(&address, Instant::now() + LONG).expect("connect"))
}

#[test]
fn bind_refuses_a_directory_whose_mode_is_not_exactly_0700() {
    let directory = private_directory();
    for mode in [0o755, 0o750, 0o770, 0o777, 0o500, 0o600, 0o1700, 0o2700] {
        set_mode(directory.path(), mode);
        let reason = refused(LocalListener::bind(target(directory.path(), "s")));
        assert!(reason.contains("0700"), "mode {mode:o}: {reason}");
    }
    set_mode(directory.path(), 0o700);
    assert!(LocalListener::bind(target(directory.path(), "s")).is_ok());
}

#[test]
fn bind_refuses_a_symbolic_link_anywhere_in_the_directory_path() {
    let root = private_directory();
    let real = root.path().join("real");
    fs::create_dir(&real).expect("real directory");
    set_mode(&real, 0o700);
    let link = root.path().join("link");
    symlink(&real, &link).expect("link to the directory");
    refused(LocalListener::bind(target(&link, "s")));
    // A link in a parent component is refused as well.
    let nested = real.join("inner");
    fs::create_dir(&nested).expect("inner directory");
    set_mode(&nested, 0o700);
    refused(LocalListener::bind(target(&link.join("inner"), "s")));
    // The same directory by its real path is accepted.
    assert!(LocalListener::bind(target(&nested, "s")).is_ok());
}

#[test]
fn bind_refuses_unusable_directories_and_names() {
    let directory = private_directory();
    let file = directory.path().join("file");
    fs::write(&file, b"x").expect("file");
    refused(LocalListener::bind(target(&file, "s")));
    refused(LocalListener::bind(target(
        &directory.path().join("missing"),
        "s",
    )));
    refused(LocalListener::bind(target(Path::new("relative"), "s")));
    for name in ["", ".", "..", "a/b", "/abs", "nul\0byte"] {
        refused(LocalListener::bind(target(directory.path(), name)));
    }
    refused(LocalListener::bind(target(
        directory.path(),
        &"n".repeat(300),
    )));
    // A path that cannot fit in `sun_path` is refused before anything is made.
    let long_name = "n".repeat(MAX_SOCKET_PATH_BYTES);
    refused(LocalListener::bind(target(directory.path(), &long_name)));
    assert_eq!(
        fs::read_dir(directory.path()).expect("list").count(),
        1,
        "only the file made above exists"
    );
}

#[test]
fn bind_never_replaces_an_existing_entry() {
    let directory = private_directory();
    let existing = directory.path().join("relay.sock");
    fs::write(&existing, b"keep").expect("existing entry");
    let result = LocalListener::bind(target(directory.path(), "relay.sock"));
    assert!(matches!(result, Err(IpcError::Os { .. })), "{result:?}");
    assert_eq!(fs::read(&existing).expect("entry is still there"), b"keep");
}

#[test]
fn the_socket_exists_while_listening_and_is_removed_by_accept() {
    let directory = private_directory();
    let listener = bind_in(directory.path());
    let path = PathBuf::from(listener.address());
    assert_eq!(path, directory.path().join("relay.sock"));
    assert!(path.exists());
    let connecting = client(listener.address());
    let stream = listener
        .accept(Instant::now() + LONG, &CancelSignal::new())
        .expect("accept");
    assert!(!path.exists(), "a late client finds no socket");
    drop(stream);
    drop(join_within(connecting, LONG));
}

#[test]
fn dropping_an_unused_listener_removes_only_its_own_socket() {
    let directory = private_directory();
    let listener = bind_in(directory.path());
    let path = PathBuf::from(listener.address());
    drop(listener);
    assert!(!path.exists());
    // An entry that replaced the socket is left alone.
    let listener = bind_in(directory.path());
    fs::remove_file(&path).expect("remove the socket");
    fs::write(&path, b"replacement").expect("replacement entry");
    drop(listener);
    assert_eq!(fs::read(&path).expect("replacement"), b"replacement");
}

#[test]
fn a_same_user_client_exchanges_lines_both_ways() {
    let directory = private_directory();
    let listener = bind_in(directory.path());
    let address = listener.address().to_owned();
    let connecting = thread::spawn(move || {
        let stream = connect(&address, Instant::now() + LONG).expect("connect");
        let (reader, mut writer, _shutdown) = stream.split().expect("split");
        writer.write_all(b"hello\n").expect("write");
        let mut line = String::new();
        BufReader::new(reader).read_line(&mut line).expect("read");
        line
    });
    let stream = listener
        .accept(Instant::now() + LONG, &CancelSignal::new())
        .expect("accept");
    let (reader, mut writer, shutdown) = stream.split().expect("split");
    // A read that never completes fails the test, after shutdown released
    // both ends.
    let reading = thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(reader).read_line(&mut line).map(|_| line)
    });
    let line = join_or_shut(reading, LONG, &shutdown).expect("read");
    assert_eq!(line, "hello\n");
    writer.write_all(b"world\n").expect("write");
    assert_eq!(join_or_shut(connecting, LONG, &shutdown), "world\n");
}

#[test]
fn a_blocked_read_does_not_block_a_write_on_the_same_stream() {
    let directory = private_directory();
    let listener = bind_in(directory.path());
    let address = listener.address().to_owned();
    let connecting = thread::spawn(move || {
        let stream = connect(&address, Instant::now() + LONG).expect("connect");
        let (mut reader, mut writer, _shutdown) = stream.split().expect("split");
        let mut payload = vec![0_u8; PAYLOAD_BYTES];
        reader.read_exact(&mut payload).expect("payload");
        assert!(payload.iter().all(|byte| *byte == b'x'));
        writer.write_all(b"done\n").expect("answer");
    });
    let stream = listener
        .accept(Instant::now() + LONG, &CancelSignal::new())
        .expect("accept");
    let (reader, mut writer, shutdown) = stream.split().expect("split");
    // The peer answers only after the whole payload, so this read is blocked
    // for the whole write.
    let (started, pending) = mpsc::channel();
    let reading = thread::spawn(move || {
        let mut line = String::new();
        started
            .send(thread_id())
            .expect("report that the read is starting");
        BufReader::new(reader).read_line(&mut line).map(|_| line)
    });
    // The write starts only once the read is blocked in the kernel, so a
    // stream that serialised reads and writes would hold the write here.
    await_blocked(&pending);
    let writing = thread::spawn(move || writer.write_all(&vec![b'x'; PAYLOAD_BYTES]));
    join_or_shut(writing, LONG, &shutdown).expect("the write finished");
    assert_eq!(
        join_or_shut(reading, LONG, &shutdown).expect("read"),
        "done\n"
    );
    join_or_shut(connecting, LONG, &shutdown);
}

#[test]
fn shutdown_ends_a_blocked_read_and_the_peer_sees_the_close() {
    let directory = private_directory();
    let listener = bind_in(directory.path());
    let connecting = client(listener.address());
    let stream = listener
        .accept(Instant::now() + LONG, &CancelSignal::new())
        .expect("accept");
    let peer = join_within(connecting, LONG);
    let (mut reader, writer, shutdown) = stream.split().expect("split");
    let (started, pending) = mpsc::channel();
    let reading = thread::spawn(move || {
        let mut buffer = [0_u8; 16];
        started
            .send(thread_id())
            .expect("report that the read is starting");
        reader.read(&mut buffer)
    });
    // Shutdown runs only once the read is blocked in the kernel, so it is
    // what ends that read; a call after shutdown is checked below.
    await_blocked(&pending);
    shutdown.shutdown().expect("shutdown");
    assert_eq!(
        join_within(reading, Duration::from_secs(10)).expect("read"),
        0
    );
    shutdown.shutdown().expect("shutdown again");
    let mut writer = writer;
    assert!(writer.write(b"x").is_err());
    let (mut peer_reader, _peer_writer, _peer_shutdown) = peer.split().expect("split peer");
    // The close must reach the peer; a read that never ends fails the test.
    let peer_reading = thread::spawn(move || {
        let mut buffer = [0_u8; 4];
        peer_reader.read(&mut buffer)
    });
    assert_eq!(join_within(peer_reading, LONG).expect("peer read"), 0);
}

/// A write the peer does not read fails once the peer's socket is closed,
/// as an ended process's is, without a shutdown on this side.
#[test]
fn a_blocked_write_fails_once_the_peer_is_gone() {
    let directory = private_directory();
    let listener = bind_in(directory.path());
    let connecting = client(listener.address());
    let stream = listener
        .accept(Instant::now() + LONG, &CancelSignal::new())
        .expect("accept");
    let peer = join_within(connecting, LONG);
    let (_reader, mut writer, shutdown) = stream.split().expect("split");
    let (started, pending) = mpsc::channel();
    let payload = vec![b'x'; FILLING_BYTES];
    let writing = thread::spawn(move || {
        started
            .send(thread_id())
            .expect("report that the write is starting");
        writer.write_all(&payload)
    });
    // The peer reads nothing, so the write sleeps once the socket is full.
    await_blocked(&pending);
    drop(peer);
    let error = join_or_shut(writing, Duration::from_secs(10), &shutdown)
        .expect_err("the write fails once the peer is gone");
    assert!(
        matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
        ),
        "{error:?}"
    );
}

#[test]
fn accept_stops_at_the_deadline_and_removes_the_socket() {
    let directory = private_directory();
    let listener = bind_in(directory.path());
    let path = PathBuf::from(listener.address());
    let started = Instant::now();
    let accepted = listener.accept(started + Duration::from_millis(300), &CancelSignal::new());
    assert!(matches!(accepted, Err(IpcError::Deadline)), "{accepted:?}");
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(!path.exists());
}

#[test]
fn accept_stops_when_cancelled() {
    let directory = private_directory();
    let cancel = CancelSignal::new();
    let (accepting, pending) = accept_in_thread(bind_in(directory.path()), &cancel, LONG);
    await_blocked(&pending);
    let started = Instant::now();
    cancel.cancel();
    let accepted = join_within(accepting, Duration::from_secs(10));
    assert!(matches!(accepted, Err(IpcError::Cancelled)), "{accepted:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
    // A signal raised before the call stops it without waiting.
    let accepted = bind_in(directory.path()).accept(Instant::now() + LONG, &cancel);
    assert!(matches!(accepted, Err(IpcError::Cancelled)), "{accepted:?}");
}

#[test]
fn only_the_current_users_id_is_allowed() {
    let own = rustix::process::getuid().as_raw();
    assert!(uid_allowed(own, own));
    assert!(!uid_allowed(own.wrapping_add(1), own));
    assert!(!uid_allowed(0, 1000));
}

#[test]
fn the_peer_credentials_of_an_accepted_client_are_this_users() {
    let directory = private_directory();
    let listener = bind_in(directory.path());
    let connecting = client(listener.address());
    let stream = listener
        .accept(Instant::now() + LONG, &CancelSignal::new())
        .expect("accept");
    let credentials = sockopt::socket_peercred(&stream.0.stream).expect("peer credentials");
    assert_eq!(credentials.uid, rustix::process::getuid());
    drop(join_within(connecting, LONG));
}

/// A full accept queue makes a nonblocking Unix-socket `connect` fail with
/// `EAGAIN`, not `EINPROGRESS`: `connect` retries it until its deadline, and
/// a retrying client connects once the queue has room.
#[test]
fn a_full_accept_queue_is_retried_until_there_is_room() {
    let directory = private_directory();
    let listener = bind_in(directory.path());
    let address = listener.address().to_owned();
    let socket_address = SocketAddrUnix::new(Path::new(&address)).expect("address");
    // Queue clients nobody accepts until the kernel turns one away.
    let mut queued = Vec::new();
    let refusal = loop {
        let socket = socket_with(
            AddressFamily::UNIX,
            SocketType::STREAM,
            SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
            None,
        )
        .expect("socket");
        match connect_socket(&socket, &socket_address) {
            Ok(()) => queued.push(socket),
            Err(error) => break error,
        }
        assert!(queued.len() < 64, "the queue never filled");
    };
    assert_eq!(refusal, Errno::AGAIN, "a full queue is reported as EAGAIN");

    let started = Instant::now();
    let error =
        connect(&address, started + Duration::from_millis(300)).expect_err("the queue stays full");
    assert!(matches!(error, IpcError::Deadline), "{error:?}");
    assert!(started.elapsed() >= Duration::from_millis(300));

    let (report, pending) = mpsc::channel();
    let connecting = thread::spawn(move || {
        report.send(thread_id()).expect("report the connect");
        connect(&address, Instant::now() + LONG)
    });
    // The client sleeps between its attempts while the queue is full.
    await_blocked(&pending);
    let first = accept_with(&listener.0.socket, SocketFlags::CLOEXEC).expect("accept one");
    let stream = join_within(connecting, LONG).expect("connects once there is room");
    drop((stream, first, queued));
}

#[test]
fn connecting_to_a_missing_socket_fails_at_once() {
    let directory = private_directory();
    let started = Instant::now();
    let error = connect(
        directory.path().join("absent.sock").as_os_str(),
        Instant::now() + LONG,
    )
    .expect_err("no listener");
    assert!(matches!(error, IpcError::Os { .. }), "{error:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
}
