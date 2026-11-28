//! A fake `claude -p` for the Claude Code connection tests (ADR-0109).
//!
//! It reads its prompt, starts the MCP relay its MCP configuration names,
//! speaks MCP to OMGCode through it, and writes stream-json lines shaped
//! like those Claude Code 2.1.325 was observed to write. The scenario is the
//! word after the last `SCENARIO:` in the prompt. Both the core and the CLI
//! tests include this file.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use omgcode_core::provider::claude_code::RELAY_NONCE_VARIABLE;
use serde_json::{Value, json};

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

/// Act as `claude -p` with `arguments`, the whole command line.
pub fn run(arguments: &[OsString]) -> ExitCode {
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
    // The last scenario named wins, so a resumed session's newest message
    // decides; a summary request repeats the history and offers no tools.
    let scenario = prompt
        .rfind("SCENARIO:")
        .and_then(|at| prompt[at + "SCENARIO:".len()..].split_whitespace().next())
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
            // Any Anthropic variable or a Claude token passed through from
            // OMGCode's own environment counts.
            let anthropic = if env::vars_os().any(|(name, _)| {
                let name = name.to_string_lossy().to_ascii_uppercase();
                name.starts_with("ANTHROPIC") || name == "CLAUDE_CODE_OAUTH_TOKEN"
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
        "tool" if client.is_none() => {
            let text = "summary: the operator listed the workspace";
            emit(&mut output, &text_message(text));
            emit(&mut output, &result(text, false));
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
        "hang" => {
            // The delta tells the test the process is running, so it can
            // cancel a live turn rather than one still starting.
            emit(
                &mut output,
                &json!({
                    "type": "stream_event", "parent_tool_use_id": null,
                    "event": { "type": "content_block_delta", "index": 0,
                               "delta": { "type": "text_delta", "text": "ready" } },
                }),
            );
            std::thread::sleep(Duration::from_secs(120));
        }
        _ => {
            emit(&mut output, &result("unknown scenario", true));
        }
    }
    drop(client);
    ExitCode::SUCCESS
}
