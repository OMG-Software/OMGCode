//! Fixtures shared by the desktop service tests: a private data root, a
//! registered development root and workspaces, a scripted loopback model
//! server, and bounded polling helpers.
//!
//! Each test crate uses a subset of these helpers, so unused-item warnings are
//! allowed for this one shared module.
#![allow(dead_code)]

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use omgcode_app::AppError;
use omgcode_app::desktop::{ChatState, ChatStatus, DesktopConfig, DesktopService, SessionRequest};
use omgcode_core::control::{ControlStore, ProfileDeclaration};
use omgcode_core::policy::PermissionSet;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

/// A made-up token, used only to prove it never leaks.
pub const SECRET: &str = "fixture-token-7f3a91";
/// How long the scripted server waits for a request or a release.
const SERVER_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_HTTP_BYTES: usize = 1024 * 1024;
/// How long a test polls for a state before it fails.
pub const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// No token.
pub fn no_token() -> Result<Option<String>, AppError> {
    Ok(None)
}

/// The fixture token.
pub fn fixed_token() -> Result<Option<String>, AppError> {
    Ok(Some(SECRET.to_owned()))
}

/// Build a request or any wire type from JSON, as the shell would.
pub fn wire<T: DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("wire value deserializes")
}

/// Serialize a response to text, as the shell would.
pub fn text_of<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("response serializes")
}

/// Serialize a response to a JSON value.
pub fn json_of<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("response serializes")
}

/// The request object that names only a session.
pub fn session_request(id: Uuid) -> Value {
    json!({"session_id": id})
}

/// One scripted reply of the loopback model server.
pub struct Step {
    reply: Value,
    gate: Option<Receiver<()>>,
    arrived: Option<Sender<()>>,
}

impl Step {
    /// Reply as soon as the request has been read.
    pub fn reply(reply: Value) -> Self {
        Self {
            reply,
            gate: None,
            arrived: None,
        }
    }

    /// Wait for the gate to be released (or its sender dropped) before replying.
    pub fn gated(reply: Value, gate: Receiver<()>) -> Self {
        Self {
            reply,
            gate: Some(gate),
            arrived: None,
        }
    }

    /// Announce on `arrived` once the request has been read.
    pub fn announcing(mut self, arrived: Sender<()>) -> Self {
        self.arrived = Some(arrived);
        self
    }
}

/// A loopback model server that answers one scripted reply per connection.
pub struct MockServer {
    pub endpoint: String,
    worker: JoinHandle<Result<usize, String>>,
}

impl MockServer {
    pub fn start(steps: Vec<Step>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind mock server");
        listener.set_nonblocking(true).expect("bounded accept");
        let address = listener.local_addr().expect("mock address");
        let worker = thread::spawn(move || {
            let mut served = 0;
            for step in steps {
                let Ok(mut stream) = accept(&listener) else {
                    break;
                };
                read_body(&mut stream)?;
                if let Some(arrived) = &step.arrived {
                    // The test may have stopped listening; that is not an error.
                    arrived.send(()).ok();
                }
                if let Some(gate) = &step.gate {
                    // A release and a dropped sender both let the reply go.
                    gate.recv_timeout(SERVER_TIMEOUT).ok();
                }
                let bytes = serde_json::to_vec(&step.reply).map_err(|error| error.to_string())?;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                );
                // The client may have given up (a cancelled turn); that is not an error.
                stream
                    .write_all(head.as_bytes())
                    .and_then(|()| stream.write_all(&bytes))
                    .ok();
                served += 1;
            }
            Ok(served)
        });
        Self {
            endpoint: format!("http://{address}/v1"),
            worker,
        }
    }

    /// Wait for the server thread and return how many requests it answered.
    pub fn finish(self) -> usize {
        self.worker
            .join()
            .expect("mock server thread")
            .expect("mock server exchange")
    }
}

fn accept(listener: &TcpListener) -> Result<TcpStream, String> {
    let deadline = Instant::now() + SERVER_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .map_err(|error| error.to_string())?;
                stream
                    .set_read_timeout(Some(SERVER_TIMEOUT))
                    .map_err(|error| error.to_string())?;
                return Ok(stream);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err("no further request".to_owned());
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn read_body(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            let length: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .ok_or("missing content length")?
                .trim()
                .parse()
                .map_err(|_| "invalid content length")?;
            if length > MAX_HTTP_BYTES {
                return Err("request too large".to_owned());
            }
            while bytes.len() < end + 4 + length {
                let read = stream
                    .read(&mut buffer)
                    .map_err(|error| error.to_string())?;
                if read == 0 {
                    return Err("truncated body".to_owned());
                }
                bytes.extend_from_slice(&buffer[..read]);
            }
            return Ok(bytes[end + 4..end + 4 + length].to_vec());
        }
        if bytes.len() > MAX_HTTP_BYTES {
            return Err("headers too large".to_owned());
        }
        let read = stream
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            return Err("connection closed before headers".to_owned());
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
}

/// A plain assistant reply.
pub fn text(content: &str) -> Value {
    json!({
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }]
    })
}

/// An assistant reply that requests one tool call.
pub fn tool_call(id: &str, name: &str, arguments: &Value) -> Value {
    json!({
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments.to_string()}
                }]
            },
            "finish_reason": "tool_calls"
        }]
    })
}

/// An endpoint nothing listens on.
pub fn unused_endpoint() -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve port");
    let port = listener.local_addr().expect("address").port();
    drop(listener);
    format!("http://127.0.0.1:{port}/v1")
}

/// A registered workspace.
#[derive(Clone, Debug)]
pub struct Workspace {
    pub id: Uuid,
    pub path: PathBuf,
}

/// Private application data, a development root, and registered workspaces.
pub struct Fixture {
    _temporary: TempDir,
    pub data: PathBuf,
    pub development: PathBuf,
}

impl Fixture {
    /// Application data only; no settings store exists.
    pub fn bare() -> Self {
        let temporary = tempfile::tempdir().expect("private fixture root");
        let data = temporary.path().join("application-data");
        let development = temporary.path().join("development");
        // The checkpoint store refuses a data root that is not private. On
        // Unix that means mode 0700, which the default create mode is not.
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&data)
                .expect("application data");
        }
        #[cfg(not(unix))]
        fs::create_dir_all(&data).expect("application data");
        fs::create_dir_all(&development).expect("development root");
        #[cfg(windows)]
        restrict_to_current_user(&data);
        Self {
            _temporary: temporary,
            data,
            development,
        }
    }

    /// Application data with a settings store that has a development root.
    pub fn new() -> Self {
        let fixture = Self::bare();
        let mut control = ControlStore::open(&fixture.data).expect("private control store");
        control
            .set_development_root(&fixture.development, control.revision())
            .expect("development root");
        fixture
    }

    /// Register `name` below the development root with `permissions`.
    pub fn workspace(&self, name: &str, permissions: PermissionSet) -> Workspace {
        let path = self.development.join(name);
        fs::create_dir_all(&path).expect("workspace directory");
        let mut control = ControlStore::open(&self.data).expect("control store");
        let id = control
            .register_workspace(&path, control.revision())
            .expect("registered workspace")
            .id();
        control
            .set_permissions(id, permissions, control.revision())
            .expect("workspace permissions");
        Workspace {
            id: *id.as_uuid(),
            path,
        }
    }

    /// Declare a toolchain profile so the command tool can be offered.
    pub fn declare_profile(&self) {
        let tools = self.data.parent().expect("temporary root").join("tools");
        fs::create_dir_all(&tools).expect("toolchain root");
        let mut control = ControlStore::open(&self.data).expect("control store");
        control
            .add_profile(
                &ProfileDeclaration {
                    name: "system".into(),
                    read_only_roots: vec![tools],
                    path_entries: Vec::new(),
                    environment: Vec::new(),
                },
                control.revision(),
            )
            .expect("toolchain profile");
    }

    pub fn service(&self, endpoint: &str) -> DesktopService {
        self.service_with(endpoint, no_token)
    }

    pub fn service_with(
        &self,
        endpoint: &str,
        source: fn() -> Result<Option<String>, AppError>,
    ) -> DesktopService {
        DesktopService::open_with_token_source(
            DesktopConfig {
                data_root: self.data.clone(),
                endpoint: endpoint.to_owned(),
            },
            source,
        )
        .expect("open desktop service")
    }
}

/// Permissions that offer the read and list tools.
pub fn read_tools() -> PermissionSet {
    PermissionSet {
        read: true,
        write: false,
        model_tools: true,
        ..PermissionSet::default()
    }
}

/// Permissions that offer every model tool.
pub fn all_tools() -> PermissionSet {
    PermissionSet {
        read: true,
        write: true,
        execute: true,
        model_tools: true,
        ..PermissionSet::default()
    }
}

/// Registered but with no model tools: chats are text-only.
pub fn text_only() -> PermissionSet {
    PermissionSet {
        read: false,
        write: false,
        ..PermissionSet::default()
    }
}

/// Settings the fixture model accepts.
pub fn settings_json(endpoint: &str) -> Value {
    json!({
        "endpoint": endpoint,
        "model": "fixture",
        "context_tokens": 131_072,
        "max_output_tokens": 64,
        "instructions": "",
        "compact_threshold": 200_000,
        "auto_compact": false
    })
}

/// Poll `chat_status` until `done` holds, or fail at the deadline.
pub fn wait_for(
    service: &DesktopService,
    session_id: Uuid,
    done: impl Fn(&ChatStatus) -> bool,
) -> ChatStatus {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let status = service
            .chat_status(wire::<SessionRequest>(session_request(session_id)))
            .expect("chat status");
        if done(&status) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting; last state {:?}",
            status.state
        );
        thread::sleep(Duration::from_millis(10));
    }
}

/// Wait until the turn is no longer running.
pub fn wait_finished(service: &DesktopService, session_id: Uuid) -> ChatStatus {
    wait_for(service, session_id, |status| {
        matches!(status.state, ChatState::Completed | ChatState::Failed)
    })
}

/// Wait for a signal from the mock server.
pub fn wait_arrived(arrived: &Receiver<()>) {
    arrived
        .recv_timeout(POLL_DEADLINE)
        .expect("the model request arrived");
}

/// A fresh gate: release by sending or dropping the sender.
pub fn gate() -> (Sender<()>, Receiver<()>) {
    mpsc::channel()
}

/// Give `data` the private ACL the Windows control store requires of its root.
#[cfg(windows)]
fn restrict_to_current_user(data: &std::path::Path) {
    let whoami = std::process::Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .expect("whoami");
    assert!(whoami.status.success(), "whoami fixture failed");
    let identity = String::from_utf8_lossy(&whoami.stdout).into_owned();
    let sid = identity
        .trim()
        .rsplit(',')
        .next()
        .expect("SID column")
        .trim_matches('"')
        .to_owned();
    let granted = std::process::Command::new("icacls")
        .arg(data)
        .args(["/inheritance:r", "/grant:r", &format!("*{sid}:(OI)(CI)F")])
        .output()
        .expect("icacls");
    assert!(granted.status.success(), "icacls fixture failed");
}

/// Assert that no serialized response names the data root, a journal file, or
/// the server token.
pub fn assert_no_leaks(fixture: &Fixture, responses: &[String]) {
    let data = fixture.data.to_string_lossy().into_owned();
    // The JSON form of the same path, with its backslashes escaped.
    let escaped = serde_json::to_string(&data).expect("escape path");
    let escaped = escaped.trim_matches('"').to_owned();
    for response in responses {
        assert!(!response.contains(&data), "data root in {response}");
        assert!(
            !response.contains(&escaped),
            "escaped data root in {response}"
        );
        assert!(
            !response.contains(".jsonl"),
            "journal file name in {response}"
        );
        assert!(!response.contains(SECRET), "token in {response}");
    }
}
