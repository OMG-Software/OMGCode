//! Real CLI tests for chat with the persisted model read/list tool preference.

mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use support::run_scene::{Scene, run_command, shell};
use support::spawn_gate::without_child_spawns;
use support::workspace_fixture::Fixture;
use support::{
    CLI_DEADLINE, HANG_FIXTURE_ENV, assert_error_code, assert_success, json_output, path_arg,
    run_cli, run_with_deadline,
};

const NOTE: &[u8] = b"workspace notes for the model";
const IN_ROOT: &str = "sentinel-in-development-root";
const BEYOND_ROOT: &str = "sentinel-beyond-development-root";
const MOCK_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_HTTP_BYTES: usize = 1024 * 1024;

/// Serve a fixed sequence of responses, one per connection, and keep each request.
struct SequenceServer {
    endpoint: String,
    requests: Receiver<Value>,
    worker: JoinHandle<Result<usize, String>>,
}

/// Work run after one numbered request arrives and before its reply is sent.
type BeforeReply = (usize, Box<dyn FnOnce() + Send>);

impl SequenceServer {
    fn start(responses: Vec<Value>) -> Self {
        Self::start_with(responses, None)
    }

    fn start_with(responses: Vec<Value>, before_reply: Option<BeforeReply>) -> Self {
        let mut before_reply = before_reply;
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind private mock server");
        listener.set_nonblocking(true).expect("bounded accept mode");
        let address = listener.local_addr().expect("mock server address");
        let (sender, requests) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut served = 0;
            for response in responses {
                // The CLI stops early after a refused call; that is not an error.
                let Ok(mut stream) = accept(&listener) else {
                    break;
                };
                let body = read_body(&mut stream)?;
                let request: Value = serde_json::from_slice(&body)
                    .map_err(|error| format!("request JSON: {error}"))?;
                sender.send(request).map_err(|error| error.to_string())?;
                if before_reply
                    .as_ref()
                    .is_some_and(|(index, _)| *index == served)
                    && let Some((_, work)) = before_reply.take()
                {
                    work();
                }
                let bytes = serde_json::to_vec(&response).map_err(|error| error.to_string())?;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                );
                stream
                    .write_all(head.as_bytes())
                    .and_then(|()| stream.write_all(&bytes))
                    .map_err(|error| error.to_string())?;
                served += 1;
            }
            Ok(served)
        });
        Self {
            endpoint: format!("http://{address}/v1"),
            requests,
            worker,
        }
    }

    /// Return every captured request after the server thread has finished.
    fn finish(self) -> Vec<Value> {
        let served = self
            .worker
            .join()
            .expect("mock server thread")
            .expect("mock server exchange");
        let requests: Vec<Value> = self.requests.try_iter().collect();
        assert_eq!(requests.len(), served);
        requests
    }
}

fn accept(listener: &TcpListener) -> Result<TcpStream, String> {
    let deadline = Instant::now() + MOCK_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .map_err(|error| error.to_string())?;
                stream
                    .set_read_timeout(Some(MOCK_TIMEOUT))
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

fn text(content: &str) -> Value {
    json!({
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }]
    })
}

fn tool_call(id: &str, name: &str, arguments: &Value) -> Value {
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

/// Change one workspace preference through the CLI at the current revision.
fn set_permission(data: &Path, workspace_id: &str, flag: &str, value: bool) {
    let status = run_cli(data, &["workspace", "status"]);
    assert_success(&status);
    let revision = json_output(&status)["revision"].to_string();
    let output = run_cli(
        data,
        &[
            "workspace",
            "permissions",
            workspace_id,
            "--revision",
            &revision,
            flag,
            if value { "true" } else { "false" },
        ],
    );
    assert_success(&output);
}

impl Fixture {
    /// Create a session for the registered workspace through the CLI.
    fn session(&self) -> PathBuf {
        let output = run_cli(
            &self.data,
            &["session", "new", "--workspace", path_arg(&self.workspace)],
        );
        assert_success(&output);
        PathBuf::from(json_output(&output)["journal"].as_str().expect("journal"))
    }

    fn set(&self, flag: &str, value: bool) {
        set_permission(&self.data, &self.workspace_id.to_string(), flag, value);
    }

    fn chat(&self, journal: &Path, endpoint: &str, message: &str) -> Output {
        run_cli(
            &self.data,
            &[
                "chat",
                "--journal",
                path_arg(journal),
                "--endpoint",
                endpoint,
                "--model",
                "fixture",
                "--context-tokens",
                "131072",
                "--max-output-tokens",
                "64",
                message,
            ],
        )
    }

    fn root(&self) -> PathBuf {
        self.workspace
            .parent()
            .expect("development root")
            .to_path_buf()
    }
}

fn user_texts(request: &Value) -> Vec<String> {
    request["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .filter(|message| message["role"] == "user")
        .map(|message| message["content"].as_str().expect("text").to_owned())
        .collect()
}

#[test]
fn a_disabled_preference_keeps_chat_text_only_and_settings_bytes_compatible() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    let status = run_cli(&fixture.data, &["workspace", "status"]);
    assert_success(&status);
    assert_eq!(
        json_output(&status)["workspaces"][0]["permissions"]["model_tools"],
        false,
        "status must report the disabled preference explicitly"
    );
    let settings = fs::read_to_string(fixture.data.join("control-v1.jsonl")).expect("settings");
    assert!(
        !settings.contains("model_tools"),
        "a disabled preference must not change persisted settings bytes"
    );

    let server = SequenceServer::start(vec![text("text only")]);
    let output = fixture.chat(&journal, &server.endpoint, "read note.txt");
    let requests = server.finish();

    assert_success(&output);
    let result = json_output(&output);
    assert_eq!(result["reply"]["content"], "text only");
    assert_eq!(result["model_tools"]["mode"], "disabled");
    assert_eq!(result["model_tools"]["reason"], "preference_disabled");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].get("tools").is_none(), "no tools were offered");
}

#[test]
fn an_enabled_preference_lets_the_model_read_the_workspace_through_the_runtime() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    let settings = fs::read_to_string(fixture.data.join("control-v1.jsonl")).expect("settings");
    assert!(settings.contains("\"model_tools\":true"));

    let server = SequenceServer::start(vec![
        tool_call(
            "call-1",
            "workspace_read_file",
            &json!({"path": "note.txt"}),
        ),
        text("The note says hello."),
    ]);
    let output = fixture.chat(&journal, &server.endpoint, "what does note.txt say?");
    let requests = server.finish();

    assert_success(&output);
    let result = json_output(&output);
    assert_eq!(result["reply"]["content"], "The note says hello.");
    assert_eq!(result["model_tools"]["mode"], "enabled");
    assert_eq!(
        result["model_tools"]["workspace_id"],
        fixture.workspace_id.to_string()
    );
    assert_eq!(result["model_tools"]["tool_calls"], 1);
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request["tool_choice"], "auto");
        let names: Vec<&str> = request["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|tool| tool["function"]["name"].as_str().expect("name"))
            .collect();
        assert_eq!(
            names,
            [
                "workspace_read_file",
                "workspace_list_directory",
                "workspace_edit_file",
                "workspace_multi_edit_file",
                "workspace_create_file",
                "workspace_rename_file",
                "workspace_delete_file"
            ]
        );
        // The compiled base prompt remains the only system message.
        let systems = request["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .filter(|message| message["role"] == "system")
            .count();
        assert_eq!(systems, 1);
    }
    let tool_text = user_texts(&requests[1])
        .into_iter()
        .find(|text| text.starts_with("Tool result [call call-1"))
        .expect("source-labelled tool result sent as user content");
    assert!(tool_text.contains("untrusted workspace data"));
    assert!(tool_text.contains(std::str::from_utf8(NOTE).expect("UTF-8 note")));

    let events = fs::read_to_string(&journal).expect("journal");
    assert!(events.contains("\"tool.requested\""));
    assert!(events.contains("\"tool.completed\""));

    // The read is listed in the model action journal as a standing allow.
    let listed = run_cli(&fixture.data, &["actions", "list"]);
    assert_success(&listed);
    let listing = json_output(&listed);
    let actions = listing["actions"].as_array().expect("actions");
    assert_eq!(actions.len(), 1, "{listing}");
    let action = &actions[0];
    assert_eq!(action["tool"], "read_file");
    assert_eq!(action["call_id"], "call-1");
    assert_eq!(action["targets"][0]["payload"], "note.txt");
    assert_eq!(action["workspace_id"], fixture.workspace_id.to_string());
    let executed = &action["disposition"]["executed"];
    assert_eq!(executed["authorization"]["type"], "standing_allow");
    assert_eq!(executed["finish"]["outcome"], "succeeded");
}

#[test]
fn the_action_listing_is_empty_before_any_model_call_and_bounds_its_limit() {
    let fixture = Fixture::new(NOTE);
    let listed = run_cli(&fixture.data, &["actions", "list"]);
    assert_success(&listed);
    assert_eq!(json_output(&listed), json!({"actions": []}));
    for limit in ["0", "1001"] {
        let refused = run_cli(&fixture.data, &["actions", "list", "--limit", limit]);
        assert_error_code(&refused, "configuration");
    }
}

#[test]
fn the_server_token_in_a_workspace_file_is_redacted_before_the_journal_and_model() {
    const TOKEN: &str = "fixture-lm-credential-654321";
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    fs::write(
        fixture.workspace.join("secret.txt"),
        format!(
            "server token {TOKEN}
"
        ),
    )
    .expect("write file with the token");
    let server = SequenceServer::start(vec![
        tool_call(
            "call-1",
            "workspace_read_file",
            &json!({"path": "secret.txt"}),
        ),
        text("Read it."),
    ]);

    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_omgcode"));
    command
        .env("OMGCODE_LM_TOKEN", TOKEN)
        .env_remove(HANG_FIXTURE_ENV)
        .arg("--data-dir")
        .arg(&fixture.data)
        .args(["chat", "--journal", path_arg(&journal)])
        .args(["--endpoint", &server.endpoint, "--model", "fixture"])
        .args(["--context-tokens", "131072", "--max-output-tokens", "64"])
        .arg("read secret.txt");
    let result = run_with_deadline(command, CLI_DEADLINE).expect("run OMGCode CLI");
    assert!(!result.timed_out, "CLI exceeded its deadline");
    let output = result.output;
    let requests = server.finish();

    assert_success(&output);
    assert_eq!(json_output(&output)["model_tools"]["tool_calls"], 1);
    assert!(!String::from_utf8_lossy(&output.stdout).contains(TOKEN));
    let tool_text = user_texts(&requests[1])
        .into_iter()
        .find(|text| text.starts_with("Tool result [call call-1"))
        .expect("tool result sent to the model");
    assert!(tool_text.contains("server token [REDACTED]"));
    for request in &requests {
        assert!(!request.to_string().contains(TOKEN));
    }
    let events = fs::read_to_string(&journal).expect("journal");
    assert!(events.contains("server token [REDACTED]"));
    assert!(!events.contains(TOKEN));
}

/// Run chat with a configured LM Studio token against `server`.
fn chat_with_token(fixture: &Fixture, journal: &Path, endpoint: &str, token: &str) -> Output {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_omgcode"));
    command
        .env("OMGCODE_LM_TOKEN", token)
        .env_remove(HANG_FIXTURE_ENV)
        .arg("--data-dir")
        .arg(&fixture.data)
        .args(["chat", "--journal", path_arg(journal)])
        .args(["--endpoint", endpoint, "--model", "fixture"])
        .args(["--context-tokens", "131072", "--max-output-tokens", "64"])
        .arg("read a file");
    let result = run_with_deadline(command, CLI_DEADLINE).expect("run OMGCode CLI");
    assert!(!result.timed_out, "CLI exceeded its deadline");
    result.output
}

#[test]
fn a_tool_call_carrying_the_server_token_is_refused_before_the_journal() {
    const TOKEN: &str = "fixture-lm-credential-987654";
    // A backslash is escaped inside the JSON-encoded arguments string, so only
    // a search of the decoded arguments finds this token.
    const ESCAPED_TOKEN: &str = r"fixture\lm-credential-246810";
    let in_arguments = tool_call(
        "call-1",
        "workspace_read_file",
        &json!({"path": format!("{TOKEN}.txt")}),
    );
    let in_id = tool_call(
        &format!("call-{TOKEN}"),
        "workspace_read_file",
        &json!({"path": "note.txt"}),
    );
    let in_encoded_arguments = tool_call(
        "call-1",
        "workspace_read_file",
        &json!({"path": format!("{ESCAPED_TOKEN}.txt")}),
    );
    for (token, reply) in [
        (TOKEN, in_arguments),
        (TOKEN, in_id),
        (ESCAPED_TOKEN, in_encoded_arguments),
    ] {
        let fixture = Fixture::new(NOTE);
        let journal = fixture.session();
        fixture.set("--model-tools", true);
        let server = SequenceServer::start(vec![reply]);
        let output = chat_with_token(&fixture, &journal, &server.endpoint, token);
        let requests = server.finish();

        assert_error_code(&output, "provider");
        assert_eq!(requests.len(), 1, "a refused call ends the turn");
        assert!(!String::from_utf8_lossy(&output.stdout).contains(token));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(token));
        let events = fs::read_to_string(&journal).expect("journal");
        assert!(!events.contains("lm-credential"));
        assert!(!events.contains("\"tool.requested\""));
        assert!(events.contains("turn.failed"));
    }
}

#[test]
fn text_chat_withholds_saved_tool_results_after_model_tools_are_disabled() {
    let note = std::str::from_utf8(NOTE).expect("UTF-8 note");
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    let server = SequenceServer::start(vec![
        tool_call(
            "call-1",
            "workspace_read_file",
            &json!({"path": "note.txt"}),
        ),
        text("Read it."),
    ]);
    let output = fixture.chat(&journal, &server.endpoint, "read note.txt");
    let requests = server.finish();
    assert_success(&output);
    assert!(requests[1].to_string().contains(note));

    fixture.set("--model-tools", false);
    let server = SequenceServer::start(vec![text("No file access now.")]);
    let output = fixture.chat(&journal, &server.endpoint, "what did it say?");
    let requests = server.finish();

    assert_success(&output);
    let result = json_output(&output);
    assert_eq!(result["model_tools"]["mode"], "disabled");
    assert_eq!(result["model_tools"]["withheld_tool_results"], 1);
    assert!(!requests[0].to_string().contains(note));
    assert!(
        user_texts(&requests[0])
            .iter()
            .any(|text| text.starts_with("Tool result withheld"))
    );
    let events = fs::read_to_string(&journal).expect("journal");
    assert!(
        events.contains(note),
        "the journal keeps the original result"
    );
}

#[test]
fn a_length_limited_tool_mode_reply_reports_its_finish_reason() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    let server = SequenceServer::start(vec![json!({
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "A partial"},
            "finish_reason": "length"
        }]
    })]);
    let output = fixture.chat(&journal, &server.endpoint, "say a lot");
    server.finish();

    assert_success(&output);
    let result = json_output(&output);
    assert_eq!(result["model_tools"]["mode"], "enabled");
    assert_eq!(result["reply"]["content"], "A partial");
    assert_eq!(result["reply"]["finish_reason"], "length");
    assert_eq!(result["model_tools"]["withheld_tool_results"], 0);
}

#[test]
fn model_tool_calls_never_read_outside_the_workspace_through_the_cli() {
    let attempts = [
        json!({"path": "../outside.txt"}),
        json!({"path": "../../beyond.txt"}),
        json!({"path": "outside-hardlink"}),
    ];
    for arguments in attempts {
        let fixture = Fixture::new(NOTE);
        fs::write(fixture.root().join("outside.txt"), IN_ROOT).expect("in-root sentinel");
        let beyond = fixture
            .root()
            .parent()
            .expect("temporary")
            .join("beyond.txt");
        fs::write(&beyond, BEYOND_ROOT).expect("beyond-root sentinel");
        fs::hard_link(&beyond, fixture.workspace.join("outside-hardlink")).expect("hard link");
        let journal = fixture.session();
        fixture.set("--model-tools", true);

        // Serve only the escape attempt. A refused call ends the turn; any
        // follow-up request would fail to connect, which also discloses nothing.
        let server =
            SequenceServer::start(vec![tool_call("escape", "workspace_read_file", &arguments)]);
        let output = fixture.chat(&journal, &server.endpoint, "read outside");
        let requests = server.finish();

        let transcript = format!(
            "{}{}{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            fs::read_to_string(&journal).expect("journal"),
            serde_json::to_string(&requests).expect("requests"),
        );
        assert!(
            !transcript.contains(IN_ROOT) && !transcript.contains(BEYOND_ROOT),
            "{arguments}: an outside sentinel escaped"
        );
    }
}

#[test]
fn revoking_read_denies_model_reads_even_with_tools_enabled() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    fixture.set("--read", false);

    let server = SequenceServer::start(vec![tool_call(
        "call-1",
        "workspace_read_file",
        &json!({"path": "note.txt"}),
    )]);
    let output = fixture.chat(&journal, &server.endpoint, "read note.txt");
    let requests = server.finish();

    assert_error_code(&output, "permission");
    assert_eq!(
        requests.len(),
        1,
        "a denial stops before a follow-up request"
    );
    let events = fs::read_to_string(&journal).expect("journal");
    assert!(events.contains("permission_denied"));
    assert!(!events.contains(std::str::from_utf8(NOTE).expect("UTF-8 note")));
}

/// Start chat under the delegated cgroup a confined run needs.
///
/// A chat turn can perform an approved model command in the same process, and
/// on Linux the trusted runner refuses a process outside a delegated cgroup.
/// `run_command` places the CLI in a `Delegate=yes` scope there and is a plain
/// CLI launcher on Windows.
fn scoped_chat(scene: &Scene, journal: &Path, endpoint: &str, message: &str) -> Output {
    let journal = path_arg(journal);
    let arguments = [
        "chat",
        "--journal",
        journal,
        "--endpoint",
        endpoint,
        "--model",
        "fixture",
        "--context-tokens",
        "131072",
        "--max-output-tokens",
        "64",
        message,
    ];
    let result = run_with_deadline(
        run_command(&scene.data, &arguments),
        Duration::from_secs(60),
    )
    .expect("run OMGCode chat");
    assert!(
        !result.timed_out,
        "chat exceeded its deadline: stdout={} stderr={}",
        String::from_utf8_lossy(&result.output.stdout),
        String::from_utf8_lossy(&result.output.stderr)
    );
    result.output
}

/// The full operator flow: the model asks for one command, the operator
/// approves the exact request between turns, and the approved turn runs a real
/// confined program through the trusted runner on the host.
#[test]
fn an_approved_model_command_runs_successfully_through_the_trusted_runner() {
    let scene = Scene::build(true, true);
    for permission in ["--read", "--write", "--model-tools"] {
        scene.set_permission(permission, "true");
    }
    assert_success(&scene.add_profile("system", &[]));

    let session = scene.cli(&["session", "new", "--workspace", path_arg(&scene.workspace)]);
    assert_success(&session);
    let journal = PathBuf::from(json_output(&session)["journal"].as_str().expect("journal"));

    // The platform shell is the program; the tool itself has no shell mode and
    // passes every argument exactly as the model provided it.
    let command = shell("echo approved");
    let request = json!({
        "profile": "system",
        "program": command[0],
        "arguments": &command[1..],
    });

    let server = SequenceServer::start(vec![
        tool_call("call-1", "workspace_run_command", &request),
        text("Waiting for operator approval."),
    ]);
    let first = scoped_chat(&scene, &journal, &server.endpoint, "run the command");
    let requests = server.finish();

    assert_success(&first);
    let first = json_output(&first);
    assert_eq!(first["reply"]["content"], "Waiting for operator approval.");
    assert_eq!(first["model_tools"]["tool_calls"], 1);
    assert_eq!(requests.len(), 2);
    let offered = requests[0]["tools"].to_string();
    assert!(
        offered.contains("workspace_run_command"),
        "the command tool must be offered: {offered}"
    );

    let listed = scene.cli(&["approvals", "list"]);
    assert_success(&listed);
    let approvals = json_output(&listed)["approvals"]
        .as_array()
        .expect("approvals")
        .to_vec();
    assert_eq!(approvals.len(), 1, "one pending request: {approvals:?}");
    assert_eq!(approvals[0]["operation"], "command");
    assert_eq!(approvals[0]["state"], "pending");
    let approval = approvals[0]["id"].as_str().expect("approval id").to_owned();
    assert_success(&scene.cli(&["approvals", "approve", &approval]));

    let server = SequenceServer::start(vec![
        tool_call("call-2", "workspace_run_command", &request),
        text("The command finished."),
    ]);
    let second = scoped_chat(&scene, &journal, &server.endpoint, "run the command");
    let requests = server.finish();

    assert_success(&second);
    let second = json_output(&second);
    assert_eq!(second["reply"]["content"], "The command finished.");
    assert_eq!(second["model_tools"]["tool_calls"], 1);
    assert_eq!(requests.len(), 2);
    let result = user_texts(&requests[1])
        .into_iter()
        .find(|text| text.starts_with("Tool result [call call-2"))
        .expect("executed command result sent to the model");
    assert!(
        result.contains("\"status\":\"executed\""),
        "the model must receive the executed result: {result}"
    );
    let expected_stdout = if cfg!(windows) {
        "\"stdout\":\"approved\\r\\n\""
    } else {
        "\"stdout\":\"approved\\n\""
    };
    assert!(
        result.contains(expected_stdout),
        "the confined program output must reach the model: {result}"
    );

    let events = fs::read_to_string(&journal).expect("session journal");
    assert!(events.contains("tool.completed"));
    assert!(
        events.contains("\\\"status\\\":\\\"executed\\\""),
        "the session journal must record the executed result: {events}"
    );

    let audit = scene.audit();
    assert_eq!(audit["pending"], json!([]));
    let finished: Vec<&Value> = audit["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .filter(|entry| entry["action"]["type"] == "run_finished")
        .collect();
    assert_eq!(finished.len(), 1, "exactly one run: {audit}");
    assert_eq!(
        finished[0]["action"]["payload"]["finish"]["result"],
        json!({"kind": "exited", "code": 0}),
        "the confined program must exit successfully"
    );

    let listed = scene.cli(&["approvals", "list"]);
    assert_success(&listed);
    let listed = json_output(&listed);
    let approvals = listed["approvals"].as_array().expect("approvals");
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0]["state"], "consumed");

    scene.close();
}

#[test]
fn invalid_settings_stop_chat_before_any_model_request() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    let settings = fixture.data.join("control-v1.jsonl");
    let mut bytes = fs::read(&settings).expect("settings");
    bytes.extend_from_slice(b"{\"incomplete\":");
    fs::write(&settings, bytes).expect("corrupt settings tail");

    // Nothing listens here; a request would fail with a provider error instead.
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve port");
    let endpoint = format!("http://{}/v1", listener.local_addr().expect("address"));
    drop(listener);
    let output = fixture.chat(&journal, &endpoint, "read note.txt");

    support::assert_control_error(&output);
    let events = fs::read_to_string(&journal).expect("journal");
    assert!(!events.contains("turn.started"));
}

#[test]
fn manual_compaction_replaces_only_older_context() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();

    let server = SequenceServer::start(vec![text("first reply")]);
    let output = fixture.chat(&journal, &server.endpoint, "first request");
    server.finish();
    assert_success(&output);

    let server = SequenceServer::start(vec![text("latest reply")]);
    let output = fixture.chat(&journal, &server.endpoint, "latest request");
    server.finish();
    assert_success(&output);

    let server = SequenceServer::start(vec![text("Earlier work is summarized.")]);
    let output = run_cli(
        &fixture.data,
        &[
            "session",
            "compact",
            path_arg(&journal),
            "--endpoint",
            &server.endpoint,
            "--model",
            "fixture",
            "--context-tokens",
            "131072",
            "--summary-tokens",
            "256",
        ],
    );
    server.finish();
    assert_success(&output);
    let compaction = json_output(&output);
    assert_eq!(compaction["compaction"]["compacted_messages"], 2);
    assert_eq!(compaction["retained_messages"], 2);
    assert_eq!(compaction["summarizer_calls"], 1);

    let server = SequenceServer::start(vec![text("after compaction")]);
    let output = fixture.chat(&journal, &server.endpoint, "third request");
    let requests = server.finish();
    assert_success(&output);
    let result = json_output(&output);
    assert_eq!(result["context"]["compaction"]["compacted_messages"], 2);
    assert_eq!(result["context"]["estimate_scope"], "pre_turn_conservative");
    let texts = user_texts(&requests[0]);
    let combined = texts.join("\n");
    assert!(
        combined.contains("Compacted earlier context")
            && combined.contains("Earlier work is summarized."),
        "{combined}"
    );
    assert!(combined.contains("latest request"), "{combined}");
    assert!(!combined.contains("first request"), "{combined}");
}

#[test]
fn auto_compaction_runs_before_the_model_turn() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    let large_request = "a".repeat(15_000);
    let large_reply = "b".repeat(15_000);
    for (message, reply) in [
        (large_request.as_str(), large_reply.as_str()),
        ("latest request", "latest reply"),
    ] {
        let server = SequenceServer::start(vec![text(reply)]);
        assert_success(&fixture.chat(&journal, &server.endpoint, message));
        server.finish();
    }

    let server = SequenceServer::start(vec![
        text("Auto summary of earlier work."),
        text("auto reply"),
    ]);
    let output = run_cli(
        &fixture.data,
        &[
            "chat",
            "--journal",
            path_arg(&journal),
            "--endpoint",
            &server.endpoint,
            "--model",
            "fixture",
            "--context-tokens",
            "131072",
            "--max-output-tokens",
            "64",
            "--auto-compact",
            "--compact-threshold",
            "20000",
            "third request",
        ],
    );
    let requests = server.finish();
    assert_success(&output);
    let result = json_output(&output);
    assert_eq!(result["reply"]["content"], "auto reply");
    assert_eq!(result["context"]["compaction"]["summarizer_calls"], 1);
    assert_eq!(result["context"]["auto_compaction"], true);
    assert_eq!(requests.len(), 2, "one summary and one chat request");
    let chat = user_texts(&requests[1]).join("\n");
    assert!(chat.contains("Auto summary of earlier work."), "{chat}");
    assert!(chat.contains("latest request"), "{chat}");
    assert!(
        !chat.contains(&large_request),
        "the compacted request is not resent"
    );
}

fn compact_cli(fixture: &Fixture, journal: &Path, endpoint: &str) -> Output {
    run_cli(
        &fixture.data,
        &[
            "session",
            "compact",
            path_arg(journal),
            "--endpoint",
            endpoint,
            "--model",
            "fixture",
            "--context-tokens",
            "131072",
            "--summary-tokens",
            "256",
        ],
    )
}

#[test]
fn tool_mode_turns_use_the_compacted_context_and_withhold_a_tool_summary_without_read() {
    let note = std::str::from_utf8(NOTE).expect("UTF-8 note");
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    let server = SequenceServer::start(vec![
        tool_call(
            "call-1",
            "workspace_read_file",
            &json!({"path": "note.txt"}),
        ),
        text("Read it."),
    ]);
    assert_success(&fixture.chat(&journal, &server.endpoint, "read note.txt"));
    server.finish();
    let server = SequenceServer::start(vec![text("latest reply")]);
    assert_success(&fixture.chat(&journal, &server.endpoint, "latest request"));
    server.finish();

    // Live model_tools and read let the saved tool result reach the summarizer.
    let server = SequenceServer::start(vec![text("The note was read earlier.")]);
    let output = compact_cli(&fixture, &journal, &server.endpoint);
    let requests = server.finish();
    assert_success(&output);
    assert!(requests[0].to_string().contains(note));
    let compaction = json_output(&output);
    assert_eq!(compaction["compaction"]["includes_tool_results"], true);
    assert_eq!(compaction["compaction"]["compacted_messages"], 3);

    // A tool-enabled turn sends the summary, not the compacted messages.
    let server = SequenceServer::start(vec![text("third reply")]);
    let output = fixture.chat(&journal, &server.endpoint, "third request");
    let requests = server.finish();
    assert_success(&output);
    assert_eq!(json_output(&output)["model_tools"]["mode"], "enabled");
    let sent = user_texts(&requests[0]).join("\n");
    assert!(sent.contains("The note was read earlier."), "{sent}");
    assert!(sent.contains("latest request"), "{sent}");
    assert!(!sent.contains("read note.txt"), "{sent}");
    assert!(!sent.contains(note), "{sent}");

    // Without live read, the summary built from the tool result is withheld.
    fixture.set("--read", false);
    let server = SequenceServer::start(vec![text("fourth reply")]);
    let output = fixture.chat(&journal, &server.endpoint, "fourth request");
    let requests = server.finish();
    assert_success(&output);
    let result = json_output(&output);
    assert_eq!(result["model_tools"]["withheld_tool_results"], 1);
    let sent = user_texts(&requests[0]).join("\n");
    assert!(
        sent.contains("Compacted earlier context withheld"),
        "{sent}"
    );
    assert!(!sent.contains("The note was read earlier."), "{sent}");

    // Without live model_tools, text-only chat withholds it as well.
    fixture.set("--read", true);
    fixture.set("--model-tools", false);
    let server = SequenceServer::start(vec![text("fifth reply")]);
    let output = fixture.chat(&journal, &server.endpoint, "fifth request");
    let requests = server.finish();
    assert_success(&output);
    let result = json_output(&output);
    assert_eq!(result["model_tools"]["mode"], "disabled");
    assert_eq!(result["model_tools"]["withheld_tool_results"], 1);
    let sent = user_texts(&requests[0]).join("\n");
    assert!(
        sent.contains("Compacted earlier context withheld"),
        "{sent}"
    );
    assert!(!sent.contains("The note was read earlier."), "{sent}");
}

#[test]
fn compaction_without_model_tools_withholds_saved_tool_results_from_the_summarizer() {
    summarizer_withholds_tool_results_after_revoking("--model-tools");
}

#[test]
fn compaction_without_read_withholds_saved_tool_results_from_the_summarizer() {
    summarizer_withholds_tool_results_after_revoking("--read");
}

/// Record a read, revoke one preference, and compact: the summarizer must see
/// only the withheld placeholder, and the summary must replay afterwards.
fn summarizer_withholds_tool_results_after_revoking(flag: &str) {
    let note = std::str::from_utf8(NOTE).expect("UTF-8 note");
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    let server = SequenceServer::start(vec![
        tool_call(
            "call-1",
            "workspace_read_file",
            &json!({"path": "note.txt"}),
        ),
        text("Read it."),
    ]);
    assert_success(&fixture.chat(&journal, &server.endpoint, "read note.txt"));
    server.finish();
    let server = SequenceServer::start(vec![text("latest reply")]);
    assert_success(&fixture.chat(&journal, &server.endpoint, "latest request"));
    server.finish();

    fixture.set(flag, false);
    let server = SequenceServer::start(vec![text("A file was read earlier.")]);
    let output = compact_cli(&fixture, &journal, &server.endpoint);
    let requests = server.finish();
    assert_success(&output);
    assert!(!requests[0].to_string().contains(note));
    assert!(requests[0].to_string().contains("Tool result withheld"));
    assert_eq!(
        json_output(&output)["compaction"]["includes_tool_results"],
        false
    );

    // A summary without tool results replays without that authority.
    let server = SequenceServer::start(vec![text("text reply")]);
    let output = fixture.chat(&journal, &server.endpoint, "third request");
    let requests = server.finish();
    assert_success(&output);
    assert_eq!(
        json_output(&output)["model_tools"]["withheld_tool_results"],
        0
    );
    let sent = user_texts(&requests[0]).join("\n");
    assert!(sent.contains("A file was read earlier."), "{sent}");
}

#[test]
fn the_preflight_estimate_counts_a_withheld_tool_result_as_withheld() {
    let note = "n".repeat(7_000);
    let fixture = Fixture::new(note.as_bytes());
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    let server = SequenceServer::start(vec![
        tool_call(
            "call-1",
            "workspace_read_file",
            &json!({"path": "note.txt"}),
        ),
        text("Read it."),
    ]);
    assert_success(&fixture.chat(&journal, &server.endpoint, "read note.txt"));
    let first = server.finish();
    assert!(
        first[1].to_string().contains(&note),
        "the read result was saved"
    );

    // Without `read`, chat replaces the saved 7 KB result with a short
    // notice. Fixed overhead (base prompt, tool schemas, message framing)
    // is about 10,400 tokens, so only the withheld form fits 15,000.
    fixture.set("--read", false);
    let server = SequenceServer::start(vec![text("text reply")]);
    let output = run_cli(
        &fixture.data,
        &[
            "chat",
            "--journal",
            path_arg(&journal),
            "--endpoint",
            &server.endpoint,
            "--model",
            "fixture",
            "--context-tokens",
            "131072",
            "--max-output-tokens",
            "64",
            "--compact-threshold",
            "15000",
            "next request",
        ],
    );
    let requests = server.finish();
    assert_success(&output);
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].to_string().contains(&note));
    let result = json_output(&output);
    assert_eq!(result["model_tools"]["withheld_tool_results"], 1);
    assert!(result["context"]["compaction"].is_null(), "{result}");
}

#[test]
fn the_post_compaction_estimate_rereads_replay_authority() {
    let note = "n".repeat(7_000);
    let summary = "s".repeat(6_000);
    let fixture = Fixture::new(note.as_bytes());
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    let server = SequenceServer::start(vec![
        tool_call(
            "call-1",
            "workspace_read_file",
            &json!({"path": "note.txt"}),
        ),
        text("Read it."),
    ]);
    assert_success(&fixture.chat(&journal, &server.endpoint, "read note.txt"));
    let first = server.finish();
    assert!(
        first[1].to_string().contains(&note),
        "the read result was saved"
    );
    let server = SequenceServer::start(vec![text("latest reply")]);
    assert_success(&fixture.chat(&journal, &server.endpoint, "latest request"));
    server.finish();

    // The saved note triggers auto-compaction at 15,000. `read` is revoked
    // while the summarizer request is in flight, so the compaction keeps the
    // tool-derived summary but the turn must send the short withheld notice.
    // Fixed overhead is about 10,200 tokens: only the withheld form fits.
    let data = fixture.data.clone();
    let workspace_id = fixture.workspace_id.to_string();
    let server = SequenceServer::start_with(
        vec![text(&summary), text("auto reply")],
        Some((
            0,
            Box::new(move || set_permission(&data, &workspace_id, "--read", false)),
        )),
    );
    let output = run_cli(
        &fixture.data,
        &[
            "chat",
            "--journal",
            path_arg(&journal),
            "--endpoint",
            &server.endpoint,
            "--model",
            "fixture",
            "--context-tokens",
            "131072",
            "--max-output-tokens",
            "64",
            "--auto-compact",
            "--compact-threshold",
            "15000",
            "third request",
        ],
    );
    let requests = server.finish();
    assert_success(&output);
    assert_eq!(requests.len(), 2, "one summary and one chat request");
    assert!(
        requests[0].to_string().contains(&note),
        "the summarizer saw the note"
    );
    let result = json_output(&output);
    assert_eq!(result["reply"]["content"], "auto reply");
    assert_eq!(result["context"]["compaction"]["summarizer_calls"], 1);
    let sent = requests[1].to_string();
    assert!(!sent.contains(&summary), "the summary is withheld");
    assert!(!sent.contains(&note), "the note is withheld");
}

#[test]
fn an_oversized_request_without_auto_compaction_is_refused_before_the_provider() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    let large_request = "a".repeat(15_000);
    let large_reply = "b".repeat(15_000);
    for (message, reply) in [
        (large_request.as_str(), large_reply.as_str()),
        ("latest request", "latest reply"),
    ] {
        let server = SequenceServer::start(vec![text(reply)]);
        assert_success(&fixture.chat(&journal, &server.endpoint, message));
        server.finish();
    }

    let server = SequenceServer::start(Vec::new());
    let output = run_cli(
        &fixture.data,
        &[
            "chat",
            "--journal",
            path_arg(&journal),
            "--endpoint",
            &server.endpoint,
            "--model",
            "fixture",
            "--context-tokens",
            "131072",
            "--max-output-tokens",
            "64",
            "--compact-threshold",
            "20000",
            "third request",
        ],
    );
    server.finish();
    assert_error_code(&output, "context");
    let events = fs::read_to_string(&journal).expect("journal");
    assert!(!events.contains("context.compacted"), "{events}");
}

/// Record `count` short exchanges directly. The journal lock is held only
/// inside the spawn gate, so no parallel test's child can inherit it.
fn seed_short_exchanges(journal: &Path, count: usize) {
    without_child_spawns(|| {
        let mut session =
            omgcode_core::session::SessionJournal::open(journal).expect("open journal");
        for index in 0..count {
            session
                .record_message(
                    omgcode_core::session::MessageRole::User,
                    &format!("request {index}"),
                )
                .expect("user message");
            session
                .record_message(
                    omgcode_core::session::MessageRole::Assistant,
                    &format!("reply {index}"),
                )
                .expect("assistant message");
        }
    });
}

#[test]
fn the_history_message_limit_triggers_compaction_before_prompt_assembly_fails() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    // 64 short exchanges fill the 128-message history limit far below the
    // token trigger; the pending request would be message 129.
    seed_short_exchanges(&journal, 64);

    let server = SequenceServer::start(Vec::new());
    let output = fixture.chat(&journal, &server.endpoint, "next request");
    server.finish();
    assert_error_code(&output, "context");

    let server = SequenceServer::start(vec![text("Sixty-four exchanges."), text("next reply")]);
    let output = run_cli(
        &fixture.data,
        &[
            "chat",
            "--journal",
            path_arg(&journal),
            "--endpoint",
            &server.endpoint,
            "--model",
            "fixture",
            "--context-tokens",
            "131072",
            "--max-output-tokens",
            "64",
            "--auto-compact",
            "next request",
        ],
    );
    let requests = server.finish();
    assert_success(&output);
    let result = json_output(&output);
    assert_eq!(result["reply"]["content"], "next reply");
    assert_eq!(requests.len(), 2, "one summary and one chat request");
    assert_eq!(result["context"]["compaction"]["compacted_messages"], 126);
}

#[test]
fn more_older_messages_than_one_summary_request_accepts_compact_in_several_chunks() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    // 66 short exchanges leave 130 older messages: more than one summary
    // request's 128-message history, although their bytes fit one chunk.
    seed_short_exchanges(&journal, 66);

    let server = SequenceServer::start(vec![
        text("First chunk."),
        text("Second chunk."),
        text("Sixty-six exchanges."),
        text("next reply"),
    ]);
    let output = run_cli(
        &fixture.data,
        &[
            "chat",
            "--journal",
            path_arg(&journal),
            "--endpoint",
            &server.endpoint,
            "--model",
            "fixture",
            "--context-tokens",
            "131072",
            "--max-output-tokens",
            "64",
            "--auto-compact",
            "next request",
        ],
    );
    let requests = server.finish();
    assert_success(&output);
    let result = json_output(&output);
    assert_eq!(result["reply"]["content"], "next reply");
    assert_eq!(result["context"]["compaction"]["compacted_messages"], 130);
    assert_eq!(result["context"]["compaction"]["summarizer_calls"], 3);
    assert_eq!(requests.len(), 4, "two chunks, one combination, one chat");
    for request in requests.iter().take(3) {
        let messages = request["messages"].as_array().expect("messages");
        assert!(messages.len() <= 128 + 2, "{}", messages.len());
    }
}

#[test]
fn auto_compaction_sizes_summary_chunks_from_the_summary_reserve_not_the_chat_reserve() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    // A 16 KiB context with an 8 KiB chat reserve leaves about 2 KB of chunk
    // room after the fixed summarizer prompt; the 2048-token summary reserve
    // leaves about 8 KB. One 3 KB exchange fits only the second allowance.
    without_child_spawns(|| {
        let mut session =
            omgcode_core::session::SessionJournal::open(&journal).expect("open journal");
        for (role, content) in [
            (omgcode_core::session::MessageRole::User, "c".repeat(3_000)),
            (
                omgcode_core::session::MessageRole::Assistant,
                "d".repeat(3_000),
            ),
            (
                omgcode_core::session::MessageRole::User,
                "latest request".to_owned(),
            ),
            (
                omgcode_core::session::MessageRole::Assistant,
                "latest reply".to_owned(),
            ),
        ] {
            session.record_message(role, &content).expect("message");
        }
    });

    let server = SequenceServer::start(vec![text("Earlier large exchange."), text("next reply")]);
    let output = run_cli(
        &fixture.data,
        &[
            "chat",
            "--journal",
            path_arg(&journal),
            "--endpoint",
            &server.endpoint,
            "--model",
            "fixture",
            "--context-tokens",
            "16384",
            "--max-output-tokens",
            "8192",
            "--auto-compact",
            "next request",
        ],
    );
    let requests = server.finish();
    assert_success(&output);
    let result = json_output(&output);
    assert_eq!(result["reply"]["content"], "next reply");
    assert_eq!(result["context"]["compaction"]["summarizer_calls"], 1);
    assert_eq!(result["context"]["compaction"]["compacted_messages"], 2);
    assert_eq!(requests.len(), 2, "one summary and one chat request");
    assert_eq!(requests[0]["max_tokens"], 2048, "summary reserve");
    assert_eq!(requests[1]["max_tokens"], 8192, "chat reserve");
}

/// The operator flow for a model edit: chat records a pending approval and
/// writes nothing, `session inspect` shows the exact request, approval needs
/// the writers-stopped acknowledgement, and the identical retry applies once
/// through a checkpoint.
#[test]
fn an_approved_model_edit_applies_through_the_operator_flow() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    let edit = json!({"path": "note.txt", "old_text": "notes", "new_text": "facts"});

    let server = SequenceServer::start(vec![
        tool_call("call-1", "workspace_edit_file", &edit),
        text("Waiting for operator approval."),
    ]);
    let first = fixture.chat(&journal, &server.endpoint, "edit the note");
    let requests = server.finish();
    assert_success(&first);
    assert_eq!(requests.len(), 2);
    let offered = requests[0]["tools"].to_string();
    assert!(
        offered.contains("workspace_edit_file"),
        "the edit tool must be offered: {offered}"
    );
    let result = user_texts(&requests[1])
        .into_iter()
        .find(|text| text.starts_with("Tool result [call call-1"))
        .expect("approval result sent to the model");
    assert!(
        result.contains("\"status\":\"approval_required\""),
        "{result}"
    );
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("note"),
        NOTE
    );

    // The operator reviews the exact recorded request before deciding.
    let inspected = run_cli(&fixture.data, &["session", "inspect", path_arg(&journal)]);
    assert_success(&inspected);
    let inspected = json_output(&inspected);
    let recorded = inspected["tool_requests"]
        .as_array()
        .expect("tool requests")
        .iter()
        .find(|request| request["tool"] == "workspace_edit_file")
        .expect("the recorded edit request");
    assert_eq!(recorded["call_id"], "call-1");
    let arguments: Value =
        serde_json::from_str(recorded["arguments"].as_str().expect("arguments")).expect("JSON");
    assert_eq!(arguments, edit);

    let listed = run_cli(&fixture.data, &["approvals", "list"]);
    assert_success(&listed);
    let approvals = json_output(&listed)["approvals"]
        .as_array()
        .expect("approvals")
        .to_vec();
    assert_eq!(approvals.len(), 1, "one pending request: {approvals:?}");
    assert_eq!(approvals[0]["operation"], "file_write");
    assert_eq!(approvals[0]["state"], "pending");
    assert_eq!(approvals[0]["writers_stopped"], false);
    let summary = approvals[0]["summary"].as_str().expect("summary");
    assert!(
        summary.starts_with("edit note.txt: 29 -> 29 bytes, sha256 "),
        "{summary}"
    );
    assert!(!summary.contains("workspace notes"), "{summary}");
    let approval = approvals[0]["id"].as_str().expect("approval id").to_owned();

    assert_error_code(
        &run_cli(&fixture.data, &["approvals", "approve", &approval]),
        "approval",
    );
    let approved = run_cli(
        &fixture.data,
        &["approvals", "approve", &approval, "--writers-stopped"],
    );
    assert_success(&approved);
    let approved = json_output(&approved);
    assert_eq!(approved["approval"]["state"], "approved");
    assert_eq!(approved["approval"]["writers_stopped"], true);
    let shown = run_cli(&fixture.data, &["approvals", "show", &approval]);
    assert_success(&shown);
    assert_eq!(json_output(&shown)["approval"]["writers_stopped"], true);
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("note"),
        NOTE
    );

    let server = SequenceServer::start(vec![
        tool_call("call-2", "workspace_edit_file", &edit),
        text("The note is edited."),
    ]);
    let second = fixture.chat(&journal, &server.endpoint, "edit the note");
    let requests = server.finish();
    assert_success(&second);
    assert_eq!(requests.len(), 2);
    let result = user_texts(&requests[1])
        .into_iter()
        .find(|text| text.starts_with("Tool result [call call-2"))
        .expect("applied result sent to the model");
    assert!(result.contains("\"status\":\"applied\""), "{result}");
    assert!(
        !result.contains("notes") && !result.contains("facts"),
        "no file text: {result}"
    );
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("note"),
        b"workspace facts for the model"
    );

    let listed = run_cli(&fixture.data, &["approvals", "list"]);
    assert_success(&listed);
    assert_eq!(json_output(&listed)["approvals"][0]["state"], "consumed");
    let checkpoints = run_cli(&fixture.data, &["file", "checkpoints"]);
    assert_success(&checkpoints);
    let operations = json_output(&checkpoints)["operations"].clone();
    assert_eq!(operations.as_array().expect("operations").len(), 1);
    assert_eq!(
        operations[0]["session_id"],
        inspected["session"]["session_id"]
    );
}

/// Without live `write`, chat does not offer the edit tool, and a call made
/// anyway is denied before any approval or file change.
#[test]
fn chat_does_not_offer_or_run_the_edit_tool_without_write() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    fixture.set("--write", false);
    let edit = json!({"path": "note.txt", "old_text": "notes", "new_text": "facts"});

    let server = SequenceServer::start(vec![tool_call("call-1", "workspace_edit_file", &edit)]);
    let output = fixture.chat(&journal, &server.endpoint, "edit the note");
    let requests = server.finish();

    assert_error_code(&output, "permission");
    assert_eq!(requests.len(), 1);
    assert!(
        !requests[0]["tools"]
            .to_string()
            .contains("workspace_edit_file")
    );
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("note"),
        NOTE
    );
    let listed = run_cli(&fixture.data, &["approvals", "list"]);
    assert_success(&listed);
    assert_eq!(json_output(&listed)["approvals"], json!([]));
    let events = fs::read_to_string(&journal).expect("journal");
    assert!(events.contains("edit_disabled"));
}

/// The operator flow for a model multi-edit: one approval covers two
/// replacements, which apply together through one checkpoint.
#[test]
fn an_approved_model_multi_edit_applies_through_the_operator_flow() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    let edit = json!({
        "path": "note.txt",
        "edits": [
            {"old_text": "model", "new_text": "agent"},
            {"old_text": "notes", "new_text": "facts"},
        ],
    });

    let server = SequenceServer::start(vec![
        tool_call("call-1", "workspace_multi_edit_file", &edit),
        text("Waiting for operator approval."),
    ]);
    let first = fixture.chat(&journal, &server.endpoint, "edit the note twice");
    let requests = server.finish();
    assert_success(&first);
    let names: Vec<&str> = requests[0]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["function"]["name"].as_str().expect("name"))
        .collect();
    assert!(names.contains(&"workspace_multi_edit_file"), "{names:?}");
    let result = user_texts(&requests[1])
        .into_iter()
        .find(|text| text.starts_with("Tool result [call call-1"))
        .expect("approval result sent to the model");
    assert!(
        result.contains("\"status\":\"approval_required\""),
        "{result}"
    );
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("note"),
        NOTE
    );

    let inspected = run_cli(&fixture.data, &["session", "inspect", path_arg(&journal)]);
    assert_success(&inspected);
    let inspected = json_output(&inspected);
    let recorded = inspected["tool_requests"]
        .as_array()
        .expect("tool requests")
        .iter()
        .find(|request| request["tool"] == "workspace_multi_edit_file")
        .expect("the recorded multi-edit request");
    let arguments: Value =
        serde_json::from_str(recorded["arguments"].as_str().expect("arguments")).expect("JSON");
    assert_eq!(arguments, edit);

    let listed = run_cli(&fixture.data, &["approvals", "list"]);
    assert_success(&listed);
    let approvals = json_output(&listed)["approvals"]
        .as_array()
        .expect("approvals")
        .to_vec();
    assert_eq!(approvals.len(), 1, "one pending request: {approvals:?}");
    assert_eq!(approvals[0]["operation"], "file_write");
    let summary = approvals[0]["summary"].as_str().expect("summary");
    assert!(
        summary.starts_with("multi-edit note.txt (2 edits): 29 -> 29 bytes, sha256 "),
        "{summary}"
    );
    assert!(!summary.contains("workspace"), "{summary}");
    let approval = approvals[0]["id"].as_str().expect("approval id").to_owned();

    assert_error_code(
        &run_cli(&fixture.data, &["approvals", "approve", &approval]),
        "approval",
    );
    assert_success(&run_cli(
        &fixture.data,
        &["approvals", "approve", &approval, "--writers-stopped"],
    ));

    let server = SequenceServer::start(vec![
        tool_call("call-2", "workspace_multi_edit_file", &edit),
        text("The note is edited."),
    ]);
    let second = fixture.chat(&journal, &server.endpoint, "edit the note twice");
    let requests = server.finish();
    assert_success(&second);
    let result = user_texts(&requests[1])
        .into_iter()
        .find(|text| text.starts_with("Tool result [call call-2"))
        .expect("applied result sent to the model");
    assert!(result.contains("\"status\":\"applied\""), "{result}");
    assert!(result.contains("\"edits\":2"), "{result}");
    assert!(
        !result.contains("notes") && !result.contains("facts"),
        "no file text: {result}"
    );
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("note"),
        b"workspace facts for the agent"
    );
    let listed = run_cli(&fixture.data, &["approvals", "list"]);
    assert_success(&listed);
    assert_eq!(json_output(&listed)["approvals"][0]["state"], "consumed");
    let checkpoints = run_cli(&fixture.data, &["file", "checkpoints"]);
    assert_success(&checkpoints);
    assert_eq!(
        json_output(&checkpoints)["operations"]
            .as_array()
            .expect("operations")
            .len(),
        1
    );
}

/// The operator flow for a model file creation: approval, then one exclusive
/// create with no checkpoint, and a repeat is refused because the file exists.
#[test]
fn an_approved_model_create_makes_the_file_through_the_operator_flow() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    let create = json!({"path": "created.txt", "content": "fresh\n"});

    let server = SequenceServer::start(vec![
        tool_call("call-1", "workspace_create_file", &create),
        text("Waiting for operator approval."),
    ]);
    let first = fixture.chat(&journal, &server.endpoint, "add a file");
    let requests = server.finish();
    assert_success(&first);
    assert!(
        requests[0]["tools"]
            .to_string()
            .contains("workspace_create_file")
    );
    assert!(!fixture.workspace.join("created.txt").exists());

    let listed = run_cli(&fixture.data, &["approvals", "list"]);
    assert_success(&listed);
    let approvals = json_output(&listed)["approvals"]
        .as_array()
        .expect("approvals")
        .to_vec();
    assert_eq!(approvals.len(), 1, "one pending request: {approvals:?}");
    assert_eq!(approvals[0]["operation"], "file_write");
    let summary = approvals[0]["summary"].as_str().expect("summary");
    assert!(
        summary.starts_with("create created.txt: 6 bytes, sha256 "),
        "{summary}"
    );
    assert!(!summary.contains("fresh"), "{summary}");
    let approval = approvals[0]["id"].as_str().expect("approval id").to_owned();
    assert_error_code(
        &run_cli(&fixture.data, &["approvals", "approve", &approval]),
        "approval",
    );
    assert_success(&run_cli(
        &fixture.data,
        &["approvals", "approve", &approval, "--writers-stopped"],
    ));

    let server = SequenceServer::start(vec![
        tool_call("call-2", "workspace_create_file", &create),
        text("The file is created."),
        tool_call("call-3", "workspace_create_file", &create),
        text("It already exists."),
    ]);
    let second = fixture.chat(&journal, &server.endpoint, "add a file");
    assert_success(&second);
    assert_eq!(
        fs::read(fixture.workspace.join("created.txt")).expect("created"),
        b"fresh\n"
    );
    let third = fixture.chat(&journal, &server.endpoint, "add it again");
    assert_success(&third);
    let requests = server.finish();
    let result = user_texts(&requests[1])
        .into_iter()
        .find(|text| text.starts_with("Tool result [call call-2"))
        .expect("created result sent to the model");
    assert!(result.contains("\"status\":\"created\""), "{result}");
    assert!(!result.contains("fresh"), "no file text: {result}");
    assert!(
        user_texts(&requests[3]).iter().any(|text| {
            text.starts_with("Tool error [call call-3") && text.contains("file_exists")
        }),
        "a second creation is refused"
    );
    let listed = run_cli(&fixture.data, &["approvals", "list"]);
    assert_success(&listed);
    let approvals = json_output(&listed)["approvals"].clone();
    assert_eq!(approvals.as_array().expect("approvals").len(), 1);
    assert_eq!(approvals[0]["state"], "consumed");
    let checkpoints = run_cli(&fixture.data, &["file", "checkpoints"]);
    assert_success(&checkpoints);
    assert_eq!(
        json_output(&checkpoints)["operations"]
            .as_array()
            .expect("operations")
            .len(),
        0,
        "creation is not checkpointed"
    );
}

/// The operator flow for a model rename: approval, then one no-replace rename
/// with no checkpoint. A repeat is refused because the source is gone.
#[test]
fn an_approved_model_rename_moves_the_file_through_the_operator_flow() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    let rename = json!({"from": "note.txt", "to": "moved.txt"});

    let server = SequenceServer::start(vec![
        tool_call("call-1", "workspace_rename_file", &rename),
        text("Waiting for operator approval."),
    ]);
    let first = fixture.chat(&journal, &server.endpoint, "rename the note");
    let requests = server.finish();
    assert_success(&first);
    let names: Vec<&str> = requests[0]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["function"]["name"].as_str().expect("name"))
        .collect();
    assert!(names.contains(&"workspace_rename_file"), "{names:?}");
    assert!(names.contains(&"workspace_delete_file"), "{names:?}");
    assert!(fixture.workspace.join("note.txt").exists());

    let listed = run_cli(&fixture.data, &["approvals", "list"]);
    assert_success(&listed);
    let approvals = json_output(&listed)["approvals"]
        .as_array()
        .expect("approvals")
        .to_vec();
    assert_eq!(approvals.len(), 1, "one pending request: {approvals:?}");
    assert_eq!(approvals[0]["operation"], "file_write");
    let summary = approvals[0]["summary"].as_str().expect("summary");
    assert!(
        summary.starts_with(&format!(
            "rename note.txt -> moved.txt: {} bytes, sha256 ",
            NOTE.len()
        )),
        "{summary}"
    );
    let approval = approvals[0]["id"].as_str().expect("approval id").to_owned();
    assert_error_code(
        &run_cli(&fixture.data, &["approvals", "approve", &approval]),
        "approval",
    );
    assert_success(&run_cli(
        &fixture.data,
        &["approvals", "approve", &approval, "--writers-stopped"],
    ));

    let server = SequenceServer::start(vec![
        tool_call("call-2", "workspace_rename_file", &rename),
        text("The note is renamed."),
        tool_call("call-3", "workspace_rename_file", &rename),
        text("It is gone."),
    ]);
    let second = fixture.chat(&journal, &server.endpoint, "rename the note");
    assert_success(&second);
    assert!(!fixture.workspace.join("note.txt").exists());
    assert_eq!(
        fs::read(fixture.workspace.join("moved.txt")).expect("moved"),
        NOTE
    );
    let third = fixture.chat(&journal, &server.endpoint, "rename it again");
    assert_success(&third);
    let requests = server.finish();
    let result = user_texts(&requests[1])
        .into_iter()
        .find(|text| text.starts_with("Tool result [call call-2"))
        .expect("renamed result sent to the model");
    assert!(result.contains("\"status\":\"renamed\""), "{result}");
    assert!(result.contains("\"from\":\"note.txt\""), "{result}");
    assert!(result.contains("\"to\":\"moved.txt\""), "{result}");
    assert!(!result.contains("notes for"), "no file text: {result}");
    assert!(
        user_texts(&requests[3]).iter().any(|text| {
            text.starts_with("Tool error [call call-3") && text.contains("not_found")
        }),
        "a second rename is refused"
    );
    let listed = run_cli(&fixture.data, &["approvals", "list"]);
    assert_success(&listed);
    let approvals = json_output(&listed)["approvals"].clone();
    assert_eq!(approvals.as_array().expect("approvals").len(), 1);
    assert_eq!(approvals[0]["state"], "consumed");
    let checkpoints = run_cli(&fixture.data, &["file", "checkpoints"]);
    assert_success(&checkpoints);
    assert_eq!(
        json_output(&checkpoints)["operations"]
            .as_array()
            .expect("operations")
            .len(),
        0,
        "rename is not checkpointed"
    );
    // The file-operation audit names both the source and the destination.
    let audit = run_cli(&fixture.data, &["file", "audit"]);
    assert_success(&audit);
    let audit = json_output(&audit);
    assert!(
        audit["entries"].as_array().expect("entries").iter().any(
            |entry| entry["action"]["payload"]["operation"] == "rename_file"
                && entry["action"]["payload"]["destination"].is_object()
        ),
        "{audit}"
    );
}

/// The operator flow for a model deletion: approval, then one checkpointed
/// removal, and the trusted `file restore` recreates the exact bytes.
#[test]
fn an_approved_model_delete_removes_the_file_and_the_operator_restores_it() {
    let fixture = Fixture::new(NOTE);
    let journal = fixture.session();
    fixture.set("--model-tools", true);
    let delete = json!({"path": "note.txt"});

    let server = SequenceServer::start(vec![
        tool_call("call-1", "workspace_delete_file", &delete),
        text("Waiting for operator approval."),
    ]);
    let first = fixture.chat(&journal, &server.endpoint, "delete the note");
    server.finish();
    assert_success(&first);
    assert!(fixture.workspace.join("note.txt").exists());

    let listed = run_cli(&fixture.data, &["approvals", "list"]);
    assert_success(&listed);
    let approvals = json_output(&listed)["approvals"]
        .as_array()
        .expect("approvals")
        .to_vec();
    assert_eq!(approvals.len(), 1, "one pending request: {approvals:?}");
    let summary = approvals[0]["summary"].as_str().expect("summary");
    assert!(
        summary.starts_with(&format!("delete note.txt: {} bytes, sha256 ", NOTE.len())),
        "{summary}"
    );
    let approval = approvals[0]["id"].as_str().expect("approval id").to_owned();
    assert_error_code(
        &run_cli(&fixture.data, &["approvals", "approve", &approval]),
        "approval",
    );
    assert_success(&run_cli(
        &fixture.data,
        &["approvals", "approve", &approval, "--writers-stopped"],
    ));

    let server = SequenceServer::start(vec![
        tool_call("call-2", "workspace_delete_file", &delete),
        text("The note is deleted."),
        tool_call("call-3", "workspace_delete_file", &delete),
        text("It is gone."),
    ]);
    let second = fixture.chat(&journal, &server.endpoint, "delete the note");
    assert_success(&second);
    assert!(!fixture.workspace.join("note.txt").exists());
    let third = fixture.chat(&journal, &server.endpoint, "delete it again");
    assert_success(&third);
    let requests = server.finish();
    let result = user_texts(&requests[1])
        .into_iter()
        .find(|text| text.starts_with("Tool result [call call-2"))
        .expect("deleted result sent to the model");
    assert!(result.contains("\"status\":\"deleted\""), "{result}");
    assert!(!result.contains("notes for"), "no file text: {result}");
    assert!(
        user_texts(&requests[3]).iter().any(|text| {
            text.starts_with("Tool error [call call-3") && text.contains("not_found")
        }),
        "a second deletion is refused"
    );

    let checkpoints = run_cli(&fixture.data, &["file", "checkpoints"]);
    assert_success(&checkpoints);
    let checkpoints = json_output(&checkpoints);
    let operations = checkpoints["operations"].as_array().expect("operations");
    assert_eq!(operations.len(), 1, "one checkpoint: {checkpoints}");
    assert_eq!(operations[0]["kind"], "remove");
    assert_eq!(operations[0]["outcome"], "succeeded");
    let checkpoint = operations[0]["operation_id"]
        .as_str()
        .expect("operation ID")
        .to_owned();
    assert!(
        result.contains(&checkpoint),
        "the result names the checkpoint: {result}"
    );

    // The trusted restore with `--removed` recreates the file and needs no
    // current-file precondition; a replacement restore refuses a removal
    // checkpoint.
    let workspace_id = fixture.workspace_id.to_string();
    let session_id = fixture.session_id.to_string();
    let restore = |extra: &[&str]| {
        let mut arguments = vec![
            "file",
            "restore",
            workspace_id.as_str(),
            "note.txt",
            "--session",
            session_id.as_str(),
            "--checkpoint",
            checkpoint.as_str(),
            "--writers-stopped",
        ];
        arguments.extend_from_slice(extra);
        run_cli(&fixture.data, &arguments)
    };
    let wrong_kind = restore(&[
        "--expected-sha256",
        &"0".repeat(64),
        "--expected-bytes",
        "1",
    ]);
    assert_error_code(&wrong_kind, "checkpoint_unavailable");
    assert!(!fixture.workspace.join("note.txt").exists());
    let restored = restore(&["--removed"]);
    assert_success(&restored);
    let restored = json_output(&restored);
    assert_eq!(restored["restored_from"], checkpoint);
    assert_eq!(restored["bytes_written"], NOTE.len());
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("restored"),
        NOTE
    );
    // The name is taken now: a second restore never replaces it.
    assert_error_code(&restore(&["--removed"]), "checkpoint_conflict");
    assert_eq!(
        fs::read(fixture.workspace.join("note.txt")).expect("kept"),
        NOTE
    );
    let after = run_cli(&fixture.data, &["file", "checkpoints"]);
    assert_success(&after);
    assert_eq!(
        json_output(&after)["operations"]
            .as_array()
            .expect("operations")
            .len(),
        1,
        "a restore of a removal records no new checkpoint"
    );
}
