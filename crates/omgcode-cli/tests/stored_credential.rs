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

use support::mock_close::await_client_close;
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
    // A request answered but not closed by the client fails the test through
    // `finish`, so it is never dropped from what a negative check inspects.
    assert!(
        await_client_close(stream),
        "the client did not close the connection first"
    );
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

    /// Record a profile with no endpoint, as one recorded before endpoints
    /// existed.
    fn add_profile(&self, name: &str, kind: &str, auth: &str) {
        let output = self.try_add_profile(name, kind, auth, None);
        assert_success(&output);
        forget_recorded_endpoint(self.data(), name);
    }

    /// Record a profile that names its endpoint.
    fn add_profile_at(&self, name: &str, kind: &str, auth: &str, endpoint: &str) {
        let output = self.try_add_profile(name, kind, auth, Some(endpoint));
        assert_success(&output);
    }

    fn try_add_profile(
        &self,
        name: &str,
        kind: &str,
        auth: &str,
        endpoint: Option<&str>,
    ) -> Output {
        let revision = self.revision();
        let mut args = vec![
            "workspace",
            "provider-profile-add",
            name,
            "--kind",
            kind,
            "--auth",
            auth,
            "--revision",
            &revision,
        ];
        if let Some(endpoint) = endpoint {
            args.extend(["--endpoint", endpoint]);
        }
        run_cli(self.data(), &args)
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

    /// The `chat` argument list for a new session, ending at the message. A
    /// profile supplies its own endpoint, so a caller passes one or the other
    /// except in the tests of the usage error.
    fn chat_args(
        &self,
        endpoint: Option<&str>,
        profile: Option<&str>,
        message: &str,
    ) -> Vec<String> {
        let mut args: Vec<String> = [
            "chat",
            "--workspace",
            path_arg(&self.fixture.workspace),
            "--model",
            "fixture",
            "--context-tokens",
            "131072",
            "--max-output-tokens",
            "32",
        ]
        .map(str::to_owned)
        .to_vec();
        if let Some(endpoint) = endpoint {
            args.extend(["--endpoint".to_owned(), endpoint.to_owned()]);
        }
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
    let server = Recorder::start("hello");
    scene.add_profile_at("lms", "lm_studio", "api_key", &server.endpoint);
    scene.bind("lms", STORED);

    let args = scene.chat_args(None, Some("lms"), &format!("use {STORED}"));
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
    let args = scene.chat_args(Some(&server.endpoint), None, "hello");
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
    let server = Recorder::start("hello");
    scene.add_profile_at("open", "lm_studio", "none", &server.endpoint);
    let args = scene.chat_args(None, Some("open"), "hello");
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
    let server = Recorder::start("hello");
    scene.add_profile_at("unbound", "lm_studio", "api_key", &server.endpoint);
    scene.add_profile("openai-key", "openai", "api_key");
    scene.bind("openai-key", STORED);

    for (name, cause) in [
        ("missing", "unknown_profile"),
        // A hosted profile recorded without an endpoint cannot be used (ADR-0092).
        ("openai-key", "endpoint_not_recorded"),
        ("unbound", "not_bound"),
    ] {
        let args = scene.chat_args(None, Some(name), "hello");
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
    let args = scene.chat_args(None, Some(""), "hello");
    let output = run_with_env(scene.data(), &args, None);
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn session_compact_uses_the_stored_token() {
    let mut scene = Scene::new();
    let server = Recorder::start("a reply");
    scene.add_profile_at("lms", "lm_studio", "api_key", &server.endpoint);
    scene.bind("lms", STORED);

    // Two turns, with no token, give the journal something to compact.
    let first = scene.chat_args(Some(&server.endpoint), None, "first request");
    let output = run_with_env(scene.data(), &first, None);
    assert_success(&output);
    let journal = json_output(&output)["journal"]
        .as_str()
        .expect("journal path")
        .to_owned();
    let mut second = scene.chat_args(Some(&server.endpoint), None, "latest request");
    // Resume the same journal instead of creating a session.
    second.splice(1..3, ["--journal".to_owned(), journal.clone()]);
    let output = run_with_env(scene.data(), &second, None);
    assert_success(&output);

    let compact: Vec<String> = [
        "session",
        "compact",
        &journal,
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
        json!([
            "lm_studio",
            "ollama_cloud",
            "anthropic",
            "openai",
            "google",
            "meta",
            "moonshot",
            "tencent",
            "thinking_machines",
            "minimax",
            "open_router",
            "z_ai",
            "deepseek",
            "xai",
            "openai_compatible"
        ])
    );
    assert_eq!(capabilities["provider_adapters"], false);
}

/// The error message of a failed command.
fn error_message(output: &Output) -> String {
    let error: Value = serde_json::from_slice(&output.stderr).expect("error JSON");
    error["error"]["message"]
        .as_str()
        .expect("message")
        .to_owned()
}

#[test]
fn a_profile_records_and_shows_its_endpoint_and_one_without_prints_as_before() {
    let scene = Scene::new();
    let endpoint = "http://127.0.0.1:1234/v1";
    let revision = scene.revision();
    let output = scene.try_add_profile("lms", "lm_studio", "api_key", Some(endpoint));
    assert_success(&output);
    let added = json_output(&output);
    assert_eq!(
        added["snapshot"]["provider_profiles"][0]["endpoint"],
        endpoint
    );
    assert!(revision != scene.revision());
    scene.add_profile("old", "lm_studio", "none");

    let shown = json_output(&run_cli(
        scene.data(),
        &["workspace", "provider-profile-show", "lms"],
    ));
    assert_eq!(shown["provider_profile"]["endpoint"], endpoint);
    let old = json_output(&run_cli(
        scene.data(),
        &["workspace", "provider-profile-show", "old"],
    ));
    let old = old["provider_profile"].as_object().expect("profile object");
    assert!(!old.contains_key("endpoint"), "{old:?}");
    let mut keys: Vec<&str> = old.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["auth", "credential", "id", "kind", "name", "revision"]
    );

    let status = json_output(&run_cli(scene.data(), &["workspace", "status"]));
    let profiles = status["provider_profiles"].as_array().expect("profiles");
    assert_eq!(profiles[0]["endpoint"], endpoint);
    assert!(profiles[1].get("endpoint").is_none());
}

#[test]
fn an_endpoint_the_adapter_would_refuse_is_refused_when_the_profile_is_added() {
    let scene = Scene::new();
    let long = format!("http://127.0.0.1:{}1234/v1", "0".repeat(300));
    let cases: Vec<(&str, String)> = [
        ("lm_studio", "https://127.0.0.1:1234/v1"),
        ("lm_studio", "http://localhost:1234/v1"),
        ("lm_studio", "http://192.0.2.1:1234/v1"),
        ("lm_studio", "http://127.0.0.1/v1"),
        ("lm_studio", "http://127.0.0.1:0/v1"),
        ("lm_studio", "http://user@127.0.0.1:1234/v1"),
        ("lm_studio", "http://127.0.0.1:1234/v1?q=1"),
        ("lm_studio", "http://127.0.0.1:1234/v1#f"),
        ("lm_studio", "http://127.0.0.1:1234/other"),
        ("lm_studio", "http://127.0.0.1:1234/v1\u{7}"),
    ]
    .into_iter()
    .map(|(kind, endpoint)| (kind, endpoint.to_owned()))
    .chain([("lm_studio", long)])
    .collect();
    for (kind, endpoint) in &cases {
        let output = scene.try_add_profile("bad", kind, "none", Some(endpoint));
        assert_error_code(&output, "control");
        let message = error_message(&output);
        assert!(
            message.contains("local provider endpoint") || message.contains("256 bytes"),
            "{endpoint:?}: {message}"
        );
    }
    let output = scene.try_add_profile("bad", "openai", "api_key", Some("http://127.0.0.1:1/v1"));
    assert_error_code(&output, "control");
    assert!(error_message(&output).contains("only its one documented endpoint"));
    let status = json_output(&run_cli(scene.data(), &["workspace", "status"]));
    assert!(status.get("provider_profiles").is_none(), "{status}");
}

#[test]
fn chat_with_a_profile_sends_to_the_profile_endpoint_and_to_no_other() {
    let mut scene = Scene::new();
    let profile_server = Recorder::start("hello");
    let other = Recorder::start("hello");
    scene.add_profile_at("lms", "lm_studio", "api_key", &profile_server.endpoint);
    scene.bind("lms", STORED);

    let args = scene.chat_args(None, Some("lms"), "hello");
    let output = run_with_env(scene.data(), &args, Some(("OMGCODE_LM_TOKEN", FROM_ENV)));
    let requests = profile_server.finish();
    let untouched = other.finish();
    assert_success(&output);
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].header("authorization"),
        Some(format!("Bearer {STORED}"))
    );
    assert!(untouched.is_empty(), "the other endpoint saw a request");
}

#[test]
fn an_endpoint_beside_a_profile_is_a_usage_error_and_nothing_is_sent() {
    let mut scene = Scene::new();
    let profile_server = Recorder::start("hello");
    let other = Recorder::start("hello");
    scene.add_profile_at("lms", "lm_studio", "api_key", &profile_server.endpoint);
    scene.bind("lms", STORED);

    // `chat`, with the endpoint of the profile itself and with another one.
    for endpoint in [&profile_server.endpoint, &other.endpoint] {
        let args = scene.chat_args(Some(endpoint), Some("lms"), "hello");
        let output = run_with_env(scene.data(), &args, None);
        assert_eq!(output.status.code(), Some(2), "{}", output_text(&output));
        assert!(output_text(&output).contains("--endpoint"));
    }
    // `session compact`, which needs no journal to reach argument parsing.
    let compact: Vec<String> = [
        "session",
        "compact",
        "journal.jsonl",
        "--endpoint",
        &other.endpoint,
        "--model",
        "fixture",
        "--context-tokens",
        "131072",
        "--provider-profile",
        "lms",
    ]
    .map(str::to_owned)
    .to_vec();
    let output = run_with_env(scene.data(), &compact, None);
    assert_eq!(output.status.code(), Some(2), "{}", output_text(&output));
    assert!(profile_server.finish().is_empty());
    assert!(other.finish().is_empty());
}

#[test]
fn a_profile_without_an_endpoint_fails_chat_and_compact_before_any_request() {
    let mut scene = Scene::new();
    scene.add_profile("old-none", "lm_studio", "none");
    scene.add_profile("old-key", "lm_studio", "api_key");
    scene.bind("old-key", STORED);
    let server = Recorder::start("hello");
    let journal = scene.fixture.data.join("unused.jsonl");
    for name in ["old-none", "old-key"] {
        let args = scene.chat_args(None, Some(name), "hello");
        let output = run_with_env(scene.data(), &args, Some(("OMGCODE_LM_TOKEN", FROM_ENV)));
        assert_error_code(&output, "credential");
        assert!(!output.status.success());
        let message = error_message(&output);
        assert!(message.starts_with("endpoint_not_recorded:"), "{message}");
        assert!(
            message.contains("add it again with an endpoint"),
            "{message}"
        );
        assert_no_token(&output_text(&output), &[STORED, FROM_ENV]);

        let compact: Vec<String> = [
            "session",
            "compact",
            path_arg(&journal),
            "--model",
            "fixture",
            "--context-tokens",
            "131072",
            "--provider-profile",
            name,
        ]
        .map(str::to_owned)
        .to_vec();
        let output = run_with_env(scene.data(), &compact, None);
        assert_error_code(&output, "credential");
        assert!(error_message(&output).starts_with("endpoint_not_recorded:"));
    }
    assert!(server.finish().is_empty());
}

#[test]
fn capabilities_report_that_provider_profiles_can_record_an_endpoint() {
    let scene = Scene::new();
    let output = run_cli(scene.data(), &["capabilities"]);
    assert_success(&output);
    let capabilities = json_output(&output);
    assert_eq!(capabilities["provider_profile_endpoints"], true);
    assert_eq!(capabilities["provider_profiles"], true);
}

/// Remove the endpoint core now records for a hosted `api_key` profile added
/// without one, so the settings journal holds the bytes 0.58.0 or earlier
/// wrote for it (ADR-0092). Replay still accepts such a profile, and the
/// refusals for one are what these tests check.
fn forget_recorded_endpoint(data: &Path, name: &str) {
    use std::fmt::Write as _;

    const FIELD: &str = ",\"endpoint\":\"";
    let path = data.join("control-v1.jsonl");
    let text = fs::read_to_string(&path).expect("settings journal");
    let marker = format!("\"name\":\"{name}\"");
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        match line.find(FIELD).filter(|_| line.contains(&marker)) {
            Some(start) => {
                let value = start + FIELD.len();
                let end = value + line[value..].find('"').expect("endpoint end") + 1;
                writeln!(out, "{}{}", &line[..start], &line[end..]).expect("line");
            }
            None => writeln!(out, "{line}").expect("line"),
        }
    }
    fs::write(&path, out).expect("settings journal");
}
