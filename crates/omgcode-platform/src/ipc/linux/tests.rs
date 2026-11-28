//! Socket tests. The client runs in this process on another thread: Linux
//! has no per-container identity to check, so the boundary under test is the
//! directory and the user check.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::*;
use crate::ipc::{IpcError, ListenerTarget, LocalListener, LocalStream, connect};

const LONG: Duration = Duration::from_secs(30);
const PAYLOAD_BYTES: usize = 512 * 1024;

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

fn join_within<T>(handle: JoinHandle<T>, limit: Duration) -> T {
    let deadline = Instant::now() + limit;
    while !handle.is_finished() {
        assert!(
            Instant::now() < deadline,
            "the thread did not finish in {limit:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
    handle.join().expect("thread panicked")
}

fn accept_in_thread(
    listener: LocalListener,
    cancel: &CancelSignal,
    wait: Duration,
) -> JoinHandle<Result<LocalStream, IpcError>> {
    let cancel = cancel.clone();
    thread::spawn(move || listener.accept(Instant::now() + wait, &cancel))
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
    let (reader, mut writer, _shutdown) = stream.split().expect("split");
    let mut line = String::new();
    BufReader::new(reader).read_line(&mut line).expect("read");
    assert_eq!(line, "hello\n");
    writer.write_all(b"world\n").expect("write");
    assert_eq!(join_within(connecting, LONG), "world\n");
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
    let (reader, mut writer, _shutdown) = stream.split().expect("split");
    // The peer answers only after the whole payload, so this read is blocked
    // for the whole write.
    let reading = thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(reader).read_line(&mut line).map(|_| line)
    });
    let writing = thread::spawn(move || writer.write_all(&vec![b'x'; PAYLOAD_BYTES]));
    join_within(writing, LONG).expect("the write finished");
    assert_eq!(join_within(reading, LONG).expect("read"), "done\n");
    join_within(connecting, LONG);
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
    let reading = thread::spawn(move || {
        let mut buffer = [0_u8; 16];
        reader.read(&mut buffer)
    });
    // The result does not depend on the read having started: shutdown also
    // ends a read that starts later.
    thread::sleep(Duration::from_millis(100));
    shutdown.shutdown().expect("shutdown");
    assert_eq!(
        join_within(reading, Duration::from_secs(10)).expect("read"),
        0
    );
    shutdown.shutdown().expect("shutdown again");
    let mut writer = writer;
    assert!(writer.write(b"x").is_err());
    let (mut peer_reader, _peer_writer, _peer_shutdown) = peer.split().expect("split peer");
    let mut buffer = [0_u8; 4];
    assert_eq!(peer_reader.read(&mut buffer).expect("peer read"), 0);
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
    let accepting = accept_in_thread(bind_in(directory.path()), &cancel, LONG);
    thread::sleep(Duration::from_millis(100));
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
