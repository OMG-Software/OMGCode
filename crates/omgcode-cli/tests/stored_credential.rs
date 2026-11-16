//! `chat` and `session compact` with `--provider-profile` through the real
//! binary, the real settings store, the real native credential store, and a
//! recording loopback model server (ADR-0047). On Linux these need an unlocked
//! Secret Service default collection and fail without one.
//!
//! Test tokens are obviously fake. A scene deletes only the native items it
//! stored, when it drops.

mod support;

use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use omgcode_core::control::{CredentialId, CredentialStore};
use serde_json::{Value, json};

use support::workspace_fixture::Fixture;
use support::{
    CLI_DEADLINE, assert_error_code, assert_success, cli_command, json_output, path_arg, run_cli,
    run_with_deadline,
};

const STORED: &str = "fake-cli-stored-token-81f2c0d4";
const FROM_ENV: &str = "fake-cli-environment-token-3a7be915";
const MOCK_DEADLINE: Duration = Duration::from_secs(30);
const MAX_HTTP_BYTES: usize = 1024 * 1024;

/// One request the recording server received.
struct Recorded {
    head: String,
    body: String,
}

impl Recorded {
    fn header(&self, name: &str) -> Option<String> {
        let prefix = format!("{}:", name.to_ascii_lowercase());
        self.head.lines().find_map(|line| {
            line.to_ascii_lowercase()
                .starts_with(&prefix)
                .then(|| line[prefix.len()..].trim().to_owned())
        })
    }
}

/// A loopback server that answers every request with a fixed completion and
/// records it, until it is finished.
struct Recorder {
    endpoint: String,
    stop: Arc<AtomicBool>,
    worker: JoinHandle<Vec<Recorded>>,
}

impl Recorder {
    fn start(reply: &'static str) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind recorder");
        listener.set_nonblocking(true).expect("bounded accept");
        let address = listener.local_addr().expect("recorder address");
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + MOCK_DEADLINE;
            let mut recorded = Vec::new();
            while !flag.load(Ordering::SeqCst) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if let Some(request) = serve(&mut stream, reply) {
                            recorded.push(request);
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
            recorded
        });
        Self {
            endpoint: format!("http://{address}/v1"),
            stop,
            worker,
        }
    }

    /// Stop the server after the CLI has exited, and return what it received.
    fn finish(self) -> Vec<Recorded> {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.join().expect("recorder thread")
    }
}

fn serve(stream: &mut TcpStream, reply: &str) -> Option<Recorded> {
    stream.set_nonblocking(false).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let (head, body) = loop {
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&bytes[..end]).into_owned();
            let length: usize = head
                .to_ascii_lowercase()
                .lines()
                .find_map(|line| line.strip_prefix("content-length:").map(str::to_owned))?
                .trim()
                .parse()
                .ok()?;
            if length > MAX_HTTP_BYTES {
                return None;
            }
            while bytes.len() < end + 4 + length {
                let read = stream.read(&mut buffer).ok()?;
                if read == 0 {
                    return None;
                }
                bytes.extend_from_slice(&buffer[..read]);
            }
            break (head, bytes[end + 4..end + 4 + length].to_vec());
        }
        let read = stream.read(&mut buffer).ok()?;
        if read == 0 || bytes.len() > MAX_HTTP_BYTES {
            return None;
        }
        bytes.extend_from_slice(&buffer[..read]);
    };
    let body_text = String::from_utf8_lossy(&body).into_owned();
    let payload = serde_json::to_vec(&json!({
        "id": "fixture-response",
        "object": "chat.completion",
        "created": 1,
        "model": "fixture",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": reply},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 11, "completion_tokens": 3, "total_tokens": 14}
    }))
    .ok()?;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    stream
        .write_all(response.as_bytes())
        .and_then(|()| stream.write_all(&payload))
        .ok()?;
    Some(Recorded {
        head,
        body: body_text,
    })
}

/// A registered workspace in a private data root, and the native items this
/// test stored.
struct Scene {
    fixture: Fixture,
    created: Vec<CredentialId>,
}

impl Scene {
    fn new() -> Self {
        Self {
            fixture: Fixture::new(b"note\n"),
            created: Vec::new(),
        }
    }

    fn data(&self) -> &Path {
        &self.fixture.data
    }

    fn revision(&self) -> String {
        let output = run_cli(self.data(), &["workspace", "status"]);
        assert_success(&output);
        json_output(&output)["revision"]
            .as_u64()
            .expect("revision")
            .to_string()
    }

    fn add_profile(&self, name: &str, kind: &str, auth: &str) {
        let revision = self.revision();
        let output = run_cli(
            self.data(),
            &[
                "workspace",
                "provider-profile-add",
                name,
                "--kind",
                kind,
                "--auth",
                auth,
                "--revision",
                &revision,
            ],
        );
        assert_success(&output);
    }

    /// Store `secret` from standard input and bind it to `name`.
    fn bind(&mut self, name: &str, secret: &str) {
        let input = self.fixture._temporary.path().join("stdin-input");
        fs::write(&input, secret).expect("stdin fixture");
        let revision = self.revision();
        let mut command = cli_command(
            self.data(),
            &[
                "workspace",
                "provider-credential-set",
                name,
                "--revision",
                &revision,
            ],
        );
        command.stdin(Stdio::from(File::open(&input).expect("open stdin fixture")));
        let result = run_with_deadline(command, CLI_DEADLINE).expect("run CLI");
        assert!(!result.timed_out, "CLI timed out");
        fs::remove_file(&input).expect("remove stdin fixture");
        assert_success(&result.output);
        let id = json_output(&result.output)["provider_credential"]["credential_id"]
            .as_str()
            .expect("credential id")
            .to_owned();
        self.created
            .push(CredentialId::parse(&id).expect("valid id"));
    }

    /// The `chat` argument list for a new session, ending at the message.
    fn chat_args(&self, endpoint: &str, profile: Option<&str>, message: &str) -> Vec<String> {
        let mut args: Vec<String> = [
            "chat",
            "--workspace",
            path_arg(&self.fixture.workspace),
            "--endpoint",
            endpoint,
            "--model",
            "fixture",
            "--context-tokens",
            "131072",
            "--max-output-tokens",
            "32",
        ]
        .map(str::to_owned)
        .to_vec();
        if let Some(profile) = profile {
            args.extend(["--provider-profile".to_owned(), profile.to_owned()]);
        }
        args.push(message.to_owned());
        args
    }
}

impl Drop for Scene {
    fn drop(&mut self) {
        // Best effort, and only for items this scene stored.
        if let Ok(credentials) = CredentialStore::open(&self.fixture.data) {
            for id in &self.created {
                credentials.delete(id).ok();
            }
        }
    }
}

/// Run the CLI with an extra environment variable.
fn run_with_env(data: &Path, args: &[String], env: Option<(&str, &str)>) -> Output {
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut command: Command = cli_command(data, &borrowed);
    if let Some((name, value)) = env {
        command.env(name, value);
    }
    let result = run_with_deadline(command, CLI_DEADLINE).expect("run CLI");
    assert!(!result.timed_out, "CLI exceeded its deadline");
    result.output
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_no_token(text: &str, tokens: &[&str]) {
    for token in tokens {
        assert!(!text.contains(token), "a token leaked into: {text}");
    }
}

/// Every file under the data root and workspace, for token text.
fn assert_no_token_in_files(root: &Path, tokens: &[&str]) {
    let mut pending: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read directory") {
            let entry = entry.expect("entry");
            let kind = entry.file_type().expect("file type");
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                let bytes = fs::read(entry.path()).expect("read file");
                let text = String::from_utf8_lossy(&bytes);
                for token in tokens {
                    assert!(
                        !text.contains(token),
                        "{} contains a token",
                        entry.path().display()
                    );
                }
            }
        }
    }
}

#[test]
fn the_stored_token_is_the_bearer_and_the_environment_token_is_never_read() {
    let mut scene = Scene::new();
    scene.add_profile("lms", "lm_studio", "api_key");
    scene.bind("lms", STORED);
    let server = Recorder::start("hello");

    let args = scene.chat_args(&server.endpoint, Some("lms"), &format!("use {STORED}"));
    let output = run_with_env(scene.data(), &args, Some(("OMGCODE_LM_TOKEN", FROM_ENV)));
    let requests = server.finish();
    assert_success(&output);

    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].header("authorization"),
        Some(format!("Bearer {STORED}"))
    );
    for request in &requests {
        assert_no_token(&request.head, &[FROM_ENV]);
        assert_no_token(&request.body, &[STORED, FROM_ENV]);
    }
    assert!(requests[0].body.contains("[REDACTED]"));
    assert_no_token(&output_text(&output), &[STORED, FROM_ENV]);
    assert_no_token_in_files(scene.data(), &[STORED, FROM_ENV]);
}

#[test]
fn without_the_flag_the_environment_token_is_still_the_bearer() {
    let scene = Scene::new();
    let server = Recorder::start("hello");
    let args = scene.chat_args(&server.endpoint, None, "hello");
    let output = run_with_env(scene.data(), &args, Some(("OMGCODE_LM_TOKEN", FROM_ENV)));
    let requests = server.finish();
    assert_success(&output);
    assert_eq!(
        requests[0].header("authorization"),
        Some(format!("Bearer {FROM_ENV}"))
    );
}

#[test]
fn a_keyless_profile_sends_no_authorization_even_when_the_environment_is_set() {
    let scene = Scene::new();
    scene.add_profile("open", "lm_studio", "none");
    let server = Recorder::start("hello");
    let args = scene.chat_args(&server.endpoint, Some("open"), "hello");
    let output = run_with_env(scene.data(), &args, Some(("OMGCODE_LM_TOKEN", FROM_ENV)));
    let requests = server.finish();
    assert_success(&output);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].header("authorization"), None);
    assert_no_token(&requests[0].head, &[FROM_ENV]);
    assert_no_token(&requests[0].body, &[FROM_ENV]);
    // No credential was ever stored in this data root, so a keyless profile
    // needed no native store to succeed.
    #[cfg(windows)]
    assert!(
        !scene.data().join("credentials-v1").exists(),
        "no native credential store was created"
    );
}

#[test]
fn an_unusable_profile_fails_with_a_credential_error_before_any_request_or_session() {
    let mut scene = Scene::new();
    scene.add_profile("lms", "lm_studio", "api_key");
    scene.add_profile("unbound", "lm_studio", "api_key");
    scene.add_profile("openai-key", "openai", "api_key");
    scene.bind("openai-key", STORED);
    let server = Recorder::start("hello");

    for (name, cause) in [
        ("missing", "unknown_profile"),
        ("openai-key", "wrong_provider"),
        ("unbound", "not_bound"),
    ] {
        let args = scene.chat_args(&server.endpoint, Some(name), "hello");
        let output = run_with_env(scene.data(), &args, Some(("OMGCODE_LM_TOKEN", FROM_ENV)));
        assert_error_code(&output, "credential");
        let error: Value = serde_json::from_slice(&output.stderr).expect("error JSON");
        let message = error["error"]["message"].as_str().expect("message");
        assert!(message.starts_with(cause), "{name}: {message}");
        // The failure precedes the session, so there is no locator to resume.
        assert!(error["error"].get("session").is_none(), "{error}");
        assert_no_token(&output_text(&output), &[STORED, FROM_ENV]);
    }
    let requests = server.finish();
    assert!(requests.is_empty(), "no model request was made");
}

#[test]
fn an_empty_profile_name_is_a_usage_error() {
    let scene = Scene::new();
    let args = scene.chat_args("http://127.0.0.1:9/v1", Some(""), "hello");
    let output = run_with_env(scene.data(), &args, None);
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn session_compact_uses_the_stored_token() {
    let mut scene = Scene::new();
    scene.add_profile("lms", "lm_studio", "api_key");
    scene.bind("lms", STORED);
    let server = Recorder::start("a reply");

    // Two turns, with no token, give the journal something to compact.
    let first = scene.chat_args(&server.endpoint, None, "first request");
    let output = run_with_env(scene.data(), &first, None);
    assert_success(&output);
    let journal = json_output(&output)["journal"]
        .as_str()
        .expect("journal path")
        .to_owned();
    let mut second = scene.chat_args(&server.endpoint, None, "latest request");
    // Resume the same journal instead of creating a session.
    second.splice(1..3, ["--journal".to_owned(), journal.clone()]);
    let output = run_with_env(scene.data(), &second, None);
    assert_success(&output);

    let compact: Vec<String> = [
        "session",
        "compact",
        &journal,
        "--endpoint",
        &server.endpoint,
        "--model",
        "fixture",
        "--context-tokens",
        "131072",
        "--summary-tokens",
        "256",
        "--provider-profile",
        "lms",
    ]
    .map(str::to_owned)
    .to_vec();
    let output = run_with_env(scene.data(), &compact, Some(("OMGCODE_LM_TOKEN", FROM_ENV)));
    let requests = server.finish();
    assert_success(&output);
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].header("authorization"), None);
    assert_eq!(requests[1].header("authorization"), None);
    assert_eq!(
        requests[2].header("authorization"),
        Some(format!("Bearer {STORED}"))
    );
    assert_no_token(&output_text(&output), &[STORED, FROM_ENV]);
    assert_no_token_in_files(scene.data(), &[STORED, FROM_ENV]);

    // An unusable profile stops compaction before any request or change.
    let server = Recorder::start("a reply");
    let mut refused = compact.clone();
    refused.truncate(refused.len() - 1);
    refused.push("missing".to_owned());
    refused[4] = server.endpoint.clone();
    let before = fs::read(&journal).expect("journal bytes");
    let output = run_with_env(scene.data(), &refused, None);
    assert_error_code(&output, "credential");
    assert!(server.finish().is_empty());
    assert_eq!(fs::read(&journal).expect("journal bytes"), before);
}

#[test]
fn capabilities_report_the_stored_credential_adapters_and_no_provider_adapters() {
    let scene = Scene::new();
    let output = run_cli(scene.data(), &["capabilities"]);
    assert_success(&output);
    let capabilities = json_output(&output);
    assert_eq!(
        capabilities["stored_credential_adapters"],
        json!(["lm_studio"])
    );
    assert_eq!(capabilities["provider_adapters"], false);
}
