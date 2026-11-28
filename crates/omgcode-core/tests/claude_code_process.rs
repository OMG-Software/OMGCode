//! A Claude Code connection end to end, with a fake `claude` (ADR-0109).
//!
//! This test binary has no libtest harness. Started with `-p` it acts as a
//! fake `claude`: it reads its prompt, starts the MCP relay named in its MCP
//! configuration, speaks MCP to OMGCode through it, and writes stream-json
//! lines shaped like those Claude Code 2.1.325 was observed to write. Started
//! with `mcp-relay` it is the relay. Otherwise it runs the tests below.
//!
//! Everything else is real: the recorded connection and, on Windows, its
//! AppContainer grants; the confined launch; the named pipe or private
//! socket; the nonce; the MCP server; and the turn runtime with its policy
//! checks and journals.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use omgcode_core::action_journal::{ActionDisposition, ActionJournal, ActionRecord, ActionTool};
use omgcode_core::control::{ClaudeCodeDeclaration, ControlStore};
use omgcode_core::policy::{Access, PermissionSet, WorkspaceId};
use omgcode_core::prompt::{self, BASE_PROMPT, BASE_PROMPT_VERSION, ModelMessage, ModelRole};
use omgcode_core::provider::ProviderError;
use omgcode_core::provider::claude_code::{
    ClaudeCodeClient, ClaudeCodeConfig, ClaudeCodeFailure, ClaudeCodeModel, RELAY_NONCE_VARIABLE,
    RELAY_SUBCOMMAND, relay,
};
use omgcode_core::session::{RequestManifest, SessionJournal};
use omgcode_core::turn::{CancelToken, TurnError, TurnLimits, TurnRuntime};
use serde_json::{Value, json};
use tempfile::TempDir;

const SECRET: &str = "workspace secret 7c1f";

fn main() -> ExitCode {
    let arguments: Vec<OsString> = env::args_os().collect();
    match arguments.get(1).and_then(|argument| argument.to_str()) {
        Some("-p") => return fake_claude(&arguments),
        Some(command) if command == RELAY_SUBCOMMAND => return fake_relay(&arguments),
        _ => {}
    }
    let filter = arguments
        .iter()
        .skip(1)
        .filter_map(|argument| argument.to_str())
        .find(|argument| !argument.starts_with('-'))
        .map(str::to_owned);
    let tests: [(&str, fn()); 7] = [
        (
            "a_text_reply_streams_and_finishes",
            a_text_reply_streams_and_finishes,
        ),
        (
            "a_tool_call_runs_through_the_turn_runtime",
            a_tool_call_runs_through_the_turn_runtime,
        ),
        (
            "an_offered_extra_tool_fails_the_lockdown",
            an_offered_extra_tool_fails_the_lockdown,
        ),
        (
            "a_streamed_unoffered_tool_block_ends_the_process",
            a_streamed_unoffered_tool_block_ends_the_process,
        ),
        ("a_failed_sign_in_is_named", a_failed_sign_in_is_named),
        (
            "a_relay_with_the_wrong_nonce_is_refused",
            a_relay_with_the_wrong_nonce_is_refused,
        ),
        (
            "cancellation_ends_the_process_promptly",
            cancellation_ends_the_process_promptly,
        ),
    ];
    let mut failed = 0;
    let mut ran = 0;
    for (name, test) in tests {
        if filter
            .as_deref()
            .is_some_and(|filter| !name.contains(filter))
        {
            continue;
        }
        ran += 1;
        let outcome = std::panic::catch_unwind(test);
        if outcome.is_ok() {
            println!("test {name} ... ok");
        } else {
            failed += 1;
            println!("test {name} ... FAILED");
        }
    }
    println!(
        "\ntest result: {}. {} passed; {failed} failed",
        if failed == 0 { "ok" } else { "FAILED" },
        ran - failed
    );
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

// ---------------------------------------------------------------------------
// The scene: a recorded connection whose program and relay are this binary.
// ---------------------------------------------------------------------------

struct Scene {
    _temporary: TempDir,
    data: PathBuf,
    workspace: PathBuf,
    workspace_id: WorkspaceId,
    sign_in: PathBuf,
    config: ClaudeCodeConfig,
}

impl Scene {
    fn new() -> Self {
        let temporary = TempDir::new().expect("temporary directory");
        let base = temporary.path();
        let data = base.join("application-data");
        let root = base.join("development-root");
        let workspace = root.join("workspace");
        let tools = base.join("tools");
        let relay_folder = base.join("relay");
        let sign_in = base.join("claude-sign-in");
        for path in [&root, &workspace, &tools, &relay_folder, &sign_in] {
            fs::create_dir(path).expect("scene folder");
        }
        fs::write(workspace.join("notes.txt"), SECRET).expect("workspace file");
        #[cfg(windows)]
        make_private_root(&data);
        let program = tools.join(if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        });
        let relay_program = relay_folder.join(if cfg!(windows) {
            "omgcode.exe"
        } else {
            "omgcode"
        });
        let me = env::current_exe().expect("test executable");
        fs::copy(&me, &program).expect("copy program");
        fs::copy(&me, &relay_program).expect("copy relay");

        let mut control = ControlStore::open(&data).expect("control store");
        control
            .set_development_root(&root, control.revision())
            .expect("root");
        let workspace_id = control
            .register_workspace(&workspace, control.revision())
            .expect("workspace")
            .id();
        control
            .set_permissions(
                workspace_id,
                PermissionSet {
                    model_tools: true,
                    read: Access::Allow,
                    ..PermissionSet::default()
                },
                control.revision(),
            )
            .expect("permissions");
        control
            .add_claude_code_profile(
                &ClaudeCodeDeclaration {
                    name: "claude".into(),
                    program: program.clone(),
                    config_folder: sign_in.clone(),
                },
                control.revision(),
            )
            .expect("profile");
        if cfg!(windows) {
            control
                .grant_claude_code("claude", &relay_folder, control.revision())
                .expect("grants");
        }
        let paths = control
            .resolve_claude_code("claude", &relay_program)
            .expect("resolved");
        let config = ClaudeCodeConfig {
            program: paths.program,
            config_folder: paths.config_folder,
            runtime_folder: paths.runtime_folder,
            relay_program: paths.relay_program,
            container: paths.container,
            model: "fake-model".into(),
            effort: None,
            max_output_tokens: 1000,
            request_timeout: Duration::from_secs(60),
        };
        Self {
            _temporary: temporary,
            data,
            workspace,
            workspace_id,
            sign_in,
            config,
        }
    }

    fn client(&self) -> ClaudeCodeClient {
        ClaudeCodeClient::new(self.config.clone()).expect("client")
    }
}

impl Drop for Scene {
    fn drop(&mut self) {
        if cfg!(windows)
            && let Ok(mut control) = ControlStore::open(&self.data)
        {
            // Revoking deletes the container profile; a failure only leaves an
            // entry on folders that are about to be removed.
            let _ = control.revoke_claude_code_grants("claude");
        }
    }
}

#[cfg(windows)]
fn make_private_root(data: &Path) {
    let whoami = Command::new("whoami")
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
    fs::create_dir(data).expect("application data");
    let granted = Command::new("icacls")
        .arg(data)
        .args(["/inheritance:r", "/grant:r", &format!("*{sid}:(OI)(CI)F")])
        .output()
        .expect("icacls");
    assert!(granted.status.success(), "icacls fixture failed");
}

fn messages(scenario: &str) -> Vec<ModelMessage> {
    vec![
        ModelMessage {
            role: ModelRole::System,
            content: "base prompt".into(),
        },
        ModelMessage {
            role: ModelRole::User,
            content: format!("SCENARIO:{scenario}"),
        },
    ]
}

fn claude_failure(result: Result<String, ProviderError>) -> ClaudeCodeFailure {
    match result {
        Err(ProviderError::ClaudeCode(failure)) => failure,
        other => panic!("expected a Claude Code failure, got {other:?}"),
    }
}

fn manifest() -> RequestManifest {
    RequestManifest {
        base_prompt: BASE_PROMPT.to_owned(),
        prompt_version: BASE_PROMPT_VERSION.to_owned(),
        prompt_sha256: prompt::base_prompt_sha256(),
        custom_instructions: String::new(),
        model: "fake-model".into(),
        base_url: "http://127.0.0.1:1234/v1".into(),
        context_tokens: 32_000,
        max_output_tokens: 1_000,
        tool_schema_version: None,
        tools: None,
        skills: Vec::new(),
        reasoning_effort: None,
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// A no-tool run streams the main agent's text, returns the final text, and
/// confines the process: it can write its sign-in folder but cannot read the
/// workspace, and it sees no inherited credential variable.
fn a_text_reply_streams_and_finishes() {
    let scene = Scene::new();
    let mut deltas = String::new();
    let mut on_text = |piece: &str| deltas.push_str(piece);
    let reply = scene
        .client()
        .complete_streaming(
            &messages(&format!(
                "text probe={}",
                scene.workspace.join("notes.txt").display()
            )),
            &mut on_text,
            &CancelToken::new(),
        )
        .expect("reply");
    assert_eq!(deltas, "hello world");
    assert!(reply.starts_with("hello world"), "{reply}");
    // The test process itself reads the file; the confined process cannot.
    assert!(fs::read(scene.workspace.join("notes.txt")).is_ok());
    assert!(reply.contains("workspace=denied"), "{reply}");
    assert!(reply.contains("anthropic=absent"), "{reply}");
    assert!(reply.contains("config=set"), "{reply}");
    assert!(reply.contains("prompt=labelled"), "{reply}");
    assert!(
        scene.sign_in.join("fake-state.txt").is_file(),
        "the sign-in folder is writable"
    );
}

/// A tool call travels relay → MCP → turn runtime → workspace → MCP → relay,
/// and the final reply quotes the result. The runtime journals the call.
fn a_tool_call_runs_through_the_turn_runtime() {
    let scene = Scene::new();
    let client = scene.client();
    let mut model = ClaudeCodeModel::new(&client);
    let mut journal = SessionJournal::create_with_workspace_id(
        &scene.data,
        &scene.workspace,
        "Claude",
        scene.workspace_id,
    )
    .expect("session");
    let report = TurnRuntime::new(
        &mut journal,
        &scene.data,
        scene.workspace_id,
        TurnLimits::default(),
    )
    .expect("runtime")
    .run(&mut model, "SCENARIO:tool", "", manifest())
    .expect("turn");
    assert_eq!(report.tool_calls, 1);
    assert_eq!(report.rounds, 2);
    assert!(report.reply.starts_with("listed:"), "{}", report.reply);
    assert!(report.reply.contains("notes.txt"), "{}", report.reply);
    drop(journal);
    let actions = actions(&scene.data);
    assert_eq!(actions.len(), 1, "{actions:?}");
    assert_eq!(actions[0].call_id, "toolu_fake_1");
    assert_eq!(actions[0].tool, ActionTool::ListDirectory);
    assert!(
        matches!(actions[0].disposition, ActionDisposition::Executed { .. }),
        "{actions:?}"
    );
}

/// Every model action record. A missing journal means none.
fn actions(data: &Path) -> Vec<ActionRecord> {
    ActionJournal::recent(data, 1000).unwrap_or_default()
}

/// An init event that offers a tool OMGCode did not offer fails closed.
fn an_offered_extra_tool_fails_the_lockdown() {
    let scene = Scene::new();
    let failure = claude_failure(scene.client().complete_streaming(
        &messages("lockdown"),
        &mut |_| {},
        &CancelToken::new(),
    ));
    assert_eq!(failure, ClaudeCodeFailure::Lockdown);
}

/// A tool block that names an unoffered tool ends the process from its first
/// streamed event, and the turn fails.
fn a_streamed_unoffered_tool_block_ends_the_process() {
    let scene = Scene::new();
    let client = scene.client();
    let mut model = ClaudeCodeModel::new(&client);
    let mut journal = SessionJournal::create_with_workspace_id(
        &scene.data,
        &scene.workspace,
        "Claude",
        scene.workspace_id,
    )
    .expect("session");
    let error = TurnRuntime::new(
        &mut journal,
        &scene.data,
        scene.workspace_id,
        TurnLimits::default(),
    )
    .expect("runtime")
    .run(&mut model, "SCENARIO:badtool", "", manifest())
    .expect_err("the lockdown ends the turn");
    assert!(matches!(error, TurnError::Model(_)), "{error:?}");
    assert_eq!(
        model.last_error(),
        Some(ProviderError::ClaudeCode(ClaudeCodeFailure::Lockdown))
    );
    assert!(actions(&scene.data).is_empty());
}

fn a_failed_sign_in_is_named() {
    let scene = Scene::new();
    let failure = claude_failure(scene.client().complete_streaming(
        &messages("signin"),
        &mut |_| {},
        &CancelToken::new(),
    ));
    assert_eq!(failure, ClaudeCodeFailure::NotSignedIn);
}

/// A relay that presents the wrong nonce is disconnected, and the run fails
/// before any tool call is accepted.
fn a_relay_with_the_wrong_nonce_is_refused() {
    let scene = Scene::new();
    let client = scene.client();
    let mut model = ClaudeCodeModel::new(&client);
    let mut journal = SessionJournal::create_with_workspace_id(
        &scene.data,
        &scene.workspace,
        "Claude",
        scene.workspace_id,
    )
    .expect("session");
    let error = TurnRuntime::new(
        &mut journal,
        &scene.data,
        scene.workspace_id,
        TurnLimits::default(),
    )
    .expect("runtime")
    .run(&mut model, "SCENARIO:badnonce", "", manifest())
    .expect_err("the channel is refused");
    assert!(matches!(error, TurnError::Model(_)), "{error:?}");
    assert_eq!(
        model.last_error(),
        Some(ProviderError::ClaudeCode(ClaudeCodeFailure::Protocol))
    );
    assert!(actions(&scene.data).is_empty());
}

fn cancellation_ends_the_process_promptly() {
    let scene = Scene::new();
    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    let timer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        trigger.cancel();
    });
    let started = Instant::now();
    let result = scene
        .client()
        .complete_streaming(&messages("hang"), &mut |_| {}, &cancel);
    timer.join().expect("timer");
    assert!(
        matches!(result, Err(ProviderError::Cancelled)),
        "{result:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
}

// ---------------------------------------------------------------------------
// The fake `claude`.
// ---------------------------------------------------------------------------

fn option<'a>(arguments: &'a [OsString], name: &str) -> Option<&'a OsString> {
    arguments
        .iter()
        .position(|argument| argument == name)
        .and_then(|index| arguments.get(index + 1))
}

fn emit(output: &mut impl Write, value: &Value) {
    let mut line = value.to_string();
    line.push('\n');
    output.write_all(line.as_bytes()).expect("stdout");
    output.flush().expect("flush");
}

struct McpClient {
    child: std::process::Child,
    input: std::process::ChildStdin,
    output: BufReader<std::process::ChildStdout>,
}

impl McpClient {
    fn start(server: &Value, nonce_override: Option<&str>) -> Self {
        let command = server["command"].as_str().expect("command");
        let arguments: Vec<&str> = server["args"]
            .as_array()
            .expect("args")
            .iter()
            .map(|argument| argument.as_str().expect("arg"))
            .collect();
        let nonce = nonce_override.map(str::to_owned).unwrap_or_else(|| {
            server["env"][RELAY_NONCE_VARIABLE]
                .as_str()
                .expect("nonce")
                .to_owned()
        });
        let mut builder = Command::new(command);
        builder
            .args(arguments)
            .env_clear()
            .env(RELAY_NONCE_VARIABLE, nonce)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for name in ["SystemRoot", "windir", "LOCALAPPDATA"] {
            if let Some(value) = env::var_os(name) {
                builder.env(name, value);
            }
        }
        let mut child = builder.spawn().expect("relay");
        let input = child.stdin.take().expect("relay stdin");
        let output = BufReader::new(child.stdout.take().expect("relay stdout"));
        Self {
            child,
            input,
            output,
        }
    }

    fn send(&mut self, value: &Value) {
        let mut line = value.to_string();
        line.push('\n');
        self.input.write_all(line.as_bytes()).expect("mcp write");
        self.input.flush().expect("mcp flush");
    }

    fn receive(&mut self) -> Option<Value> {
        let mut line = String::new();
        match self.output.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => serde_json::from_str(&line).ok(),
        }
    }

    fn request(&mut self, id: u64, method: &str, params: &Value) -> Option<Value> {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        self.receive()
    }

    /// Initialize and list the tools, as Claude Code does.
    fn handshake(&mut self) -> Option<Vec<String>> {
        self.request(
            0,
            "initialize",
            &json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "fake-claude", "version": "0" },
            }),
        )?;
        self.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        let listed = self.request(1, "tools/list", &json!({}))?;
        Some(
            listed["result"]["tools"]
                .as_array()?
                .iter()
                .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
                .collect(),
        )
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn fake_claude(arguments: &[OsString]) -> ExitCode {
    let mut prompt = String::new();
    io::stdin().read_to_string(&mut prompt).expect("stdin");
    let mut output = io::stdout().lock();
    let config_path = option(arguments, "--mcp-config").expect("mcp config");
    let config: Value =
        serde_json::from_str(&fs::read_to_string(config_path).expect("read mcp config"))
            .expect("mcp config json");
    let model = option(arguments, "--model")
        .and_then(|model| model.to_str())
        .unwrap_or("unknown")
        .to_owned();
    let scenario = prompt
        .split("SCENARIO:")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or("")
        .to_owned();
    let server = config["mcpServers"].get("omgcode").cloned();
    let mut client = server.as_ref().map(|server| {
        let nonce = (scenario == "badnonce").then_some("0".repeat(64));
        McpClient::start(server, nonce.as_deref())
    });
    let mut tools = Vec::new();
    if let Some(client) = client.as_mut() {
        match client.handshake() {
            Some(listed) => tools = listed,
            // A refused relay: the real CLI would report the server as
            // failed; this fake simply waits to be ended.
            None => {
                std::thread::sleep(Duration::from_secs(30));
                return ExitCode::FAILURE;
            }
        }
    }
    let mut offered: Vec<String> = tools
        .iter()
        .map(|tool| format!("mcp__omgcode__{tool}"))
        .collect();
    if scenario == "lockdown" {
        offered.push("Bash".into());
    }
    let servers = if server.is_some() {
        json!([{ "name": "omgcode", "status": "connected", "source": "dynamic" }])
    } else {
        json!([])
    };
    emit(
        &mut output,
        &json!({
            "type": "system", "subtype": "init", "claude_code_version": "0.0.0-fake",
            "model": model, "apiKeySource": "none", "tools": offered, "mcp_servers": servers,
            "plugins": [], "skills": [], "slash_commands": [], "session_id": "fake",
        }),
    );
    let text_message = |text: &str| {
        json!({
            "type": "assistant", "parent_tool_use_id": null,
            "message": { "id": "msg_fake", "content": [{ "type": "text", "text": text }] },
        })
    };
    let result = |text: &str, is_error: bool| {
        json!({
            "type": "result", "subtype": "success", "is_error": is_error, "result": text,
            "terminal_reason": if is_error { "api_error" } else { "completed" },
            "stop_reason": "end_turn",
        })
    };
    match scenario.as_str() {
        "text" => {
            for piece in ["hello ", "world"] {
                emit(
                    &mut output,
                    &json!({
                        "type": "stream_event", "parent_tool_use_id": null,
                        "event": { "type": "content_block_delta", "index": 0,
                                   "delta": { "type": "text_delta", "text": piece } },
                    }),
                );
            }
            let probe = prompt
                .split("probe=")
                .nth(1)
                .and_then(|rest| rest.lines().next())
                .unwrap_or("");
            // Only an access refusal counts: a wrong or empty path must not
            // pass for confinement.
            let workspace = match fs::read(probe) {
                _ if probe.is_empty() => "no-probe",
                Ok(_) => "readable",
                Err(error) if error.kind() == io::ErrorKind::PermissionDenied => "denied",
                Err(_) => "unreachable",
            };
            let anthropic = if env::vars_os().any(|(name, _)| {
                name.to_string_lossy()
                    .to_ascii_uppercase()
                    .starts_with("ANTHROPIC")
            }) {
                "present"
            } else {
                "absent"
            };
            let config_folder = env::var_os("CLAUDE_CONFIG_DIR");
            let config_state = match &config_folder {
                Some(folder)
                    if fs::write(Path::new(folder).join("fake-state.txt"), b"x").is_ok() =>
                {
                    "set"
                }
                Some(_) => "unwritable",
                None => "missing",
            };
            let labelled = if prompt.contains(" user>>>\nSCENARIO:") {
                "labelled"
            } else {
                "unlabelled"
            };
            let text = format!(
                "hello world workspace={workspace} anthropic={anthropic} config={config_state} prompt={labelled}"
            );
            emit(&mut output, &text_message(&text));
            emit(&mut output, &result(&text, false));
        }
        "tool" => {
            let Some(client) = client.as_mut() else {
                return ExitCode::FAILURE;
            };
            let name = "mcp__omgcode__workspace_list_directory";
            emit(
                &mut output,
                &json!({
                    "type": "stream_event", "parent_tool_use_id": null,
                    "event": { "type": "content_block_start", "index": 0,
                               "content_block": { "type": "tool_use", "id": "toolu_fake_1",
                                                  "name": name, "input": {} } },
                }),
            );
            emit(
                &mut output,
                &json!({
                    "type": "assistant", "parent_tool_use_id": null,
                    "message": { "id": "msg_fake", "content": [
                        { "type": "tool_use", "id": "toolu_fake_1", "name": name,
                          "input": { "path": "" } }
                    ] },
                }),
            );
            let answer = client.request(
                2,
                "tools/call",
                &json!({
                    "name": "workspace_list_directory",
                    "arguments": { "path": "" },
                    "_meta": { "claudecode/toolUseId": "toolu_fake_1", "progressToken": 2 },
                }),
            );
            let text = answer
                .as_ref()
                .and_then(|answer| answer["result"]["content"][0]["text"].as_str())
                .unwrap_or("no answer")
                .to_owned();
            let reply = format!("listed: {text}");
            emit(&mut output, &text_message(&reply));
            emit(&mut output, &result(&reply, false));
        }
        "badtool" | "badnonce" => {
            emit(
                &mut output,
                &json!({
                    "type": "stream_event", "parent_tool_use_id": null,
                    "event": { "type": "content_block_start", "index": 0,
                               "content_block": { "type": "tool_use", "id": "toolu_bad",
                                                  "name": "Bash", "input": {} } },
                }),
            );
            std::thread::sleep(Duration::from_secs(30));
        }
        "signin" => {
            emit(
                &mut output,
                &json!({
                    "type": "assistant", "parent_tool_use_id": null, "error": "authentication_failed",
                    "message": { "id": "msg_fake", "content": [{ "type": "text", "text": "Not logged in" }] },
                }),
            );
            emit(&mut output, &result("Not logged in", true));
        }
        "hang" => std::thread::sleep(Duration::from_secs(120)),
        _ => {
            emit(&mut output, &result("unknown scenario", true));
        }
    }
    drop(client);
    ExitCode::SUCCESS
}

fn fake_relay(arguments: &[OsString]) -> ExitCode {
    let Some(address) = arguments.get(2) else {
        return ExitCode::FAILURE;
    };
    let nonce = env::var(RELAY_NONCE_VARIABLE).unwrap_or_default();
    match relay(address, &nonce, io::stdin(), io::stdout()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
