//! A fake `claude -p` for the Claude Code connection tests (ADR-0109).
//!
//! It reads its prompt, starts the MCP relay its MCP configuration names,
//! speaks MCP to OMGCode through it, and writes stream-json lines shaped
//! like those Claude Code 2.1.325 was observed to write. The scenario is the
//! word after the last `SCENARIO:` in the prompt. Both the core and the CLI
//! tests include this file.
//!
//! It also leaves a start marker and any panic message in its sign-in
//! folder, which [`diagnosis`] reads, so a failed turn in CI says whether
//! the process ran at all.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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

    /// Send padded `tools/list` requests and read none of the replies.
    ///
    /// The replies soon fill the relay's pipes and the channel, so OMGCode's
    /// write of the next one blocks. Only then does OMGCode stop reading
    /// requests, and the padded requests fill the other direction until
    /// this fake's writes block. Once none was accepted for
    /// [`STALL_QUIET`], the stall is marked in `folder`; the fake then waits
    /// to be ended.
    fn flood(&mut self, folder: &Path) {
        let sent = AtomicU64::new(0);
        let input = &mut self.input;
        let pad = "x".repeat(FLOOD_PAD_BYTES);
        std::thread::scope(|scope| {
            let sent = &sent;
            scope.spawn(move || {
                for id in FLOOD_IDS {
                    let mut line = json!({
                        "jsonrpc": "2.0", "id": id, "method": "tools/list",
                        "params": { "_meta": { "pad": pad.as_str() } },
                    })
                    .to_string();
                    line.push('\n');
                    if input
                        .write_all(line.as_bytes())
                        .and_then(|()| input.flush())
                        .is_err()
                    {
                        return;
                    }
                    sent.store(id, Ordering::SeqCst);
                }
            });
            let mut last = (0, Instant::now());
            loop {
                std::thread::sleep(Duration::from_millis(50));
                let now = sent.load(Ordering::SeqCst);
                if now + 1 == FLOOD_IDS.end {
                    let _ = fs::write(folder.join(FLOODED_FILE), b"no write blocked");
                    break;
                }
                if now != last.0 {
                    last = (now, Instant::now());
                } else if last.1.elapsed() >= STALL_QUIET {
                    let _ = fs::write(folder.join(STALLED_FILE), now.to_string());
                    break;
                }
            }
            std::thread::sleep(Duration::from_secs(120));
        });
    }
}

/// The ids a flood uses: below the server's 4096, after the handshake's two.
const FLOOD_IDS: std::ops::Range<u64> = 2..4000;
/// Padding in each flood request, ignored by `tools/list`.
const FLOOD_PAD_BYTES: usize = 16 * 1024;
/// How long the relay must accept no request before the stall is marked.
const STALL_QUIET: Duration = Duration::from_secs(1);
/// Written to the sign-in folder once a flood's writes stopped.
const STALLED_FILE: &str = "fake-stalled.txt";
/// Written to the sign-in folder when every flood request was accepted.
const FLOODED_FILE: &str = "fake-flooded.txt";

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Written to the sign-in folder when the fake starts.
const STARTED_FILE: &str = "fake-started.txt";
/// Written to the sign-in folder when the fake panics.
const PANIC_FILE: &str = "fake-panic.txt";

/// Mark the start in the sign-in folder and record a later panic there.
fn record_start() {
    let Some(folder) = env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from) else {
        return;
    };
    let _ = fs::write(folder.join(STARTED_FILE), b"started");
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = fs::write(folder.join(PANIC_FILE), info.to_string());
        previous(info);
    }));
}

/// Remove what an earlier run left in `sign_in`, so [`diagnosis`] describes
/// the next run only.
pub fn forget_runs(sign_in: &Path) {
    for name in [STARTED_FILE, PANIC_FILE, STALLED_FILE, FLOODED_FILE] {
        match fs::remove_file(sign_in.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove {name}: {error}"),
        }
    }
}

/// What the fake left in `sign_in`: whether it started, its panic, and how
/// a flood ended.
pub fn diagnosis(sign_in: &Path) -> String {
    let started = sign_in.join(STARTED_FILE).is_file();
    let run = match (started, fs::read_to_string(sign_in.join(PANIC_FILE))) {
        (false, _) => "the fake claude never started".to_owned(),
        (true, Ok(panic)) => format!("the fake claude panicked: {panic}"),
        (true, Err(_)) => "the fake claude started and did not panic".to_owned(),
    };
    if sign_in.join(FLOODED_FILE).is_file() {
        format!("{run}; its flood was accepted whole, so no write blocked")
    } else {
        run
    }
}

/// Whether a flood's writes stopped because the relay stopped reading.
#[allow(dead_code, reason = "only the core process tests flood the relay")]
pub fn stalled(sign_in: &Path) -> bool {
    sign_in.join(STALLED_FILE).is_file()
}

/// Act as `claude -p` with `arguments`, the whole command line.
pub fn run(arguments: &[OsString]) -> ExitCode {
    record_start();
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
        "flood" => {
            let (Some(client), Some(folder)) = (client.as_mut(), env::var_os("CLAUDE_CONFIG_DIR"))
            else {
                return ExitCode::FAILURE;
            };
            client.flood(Path::new(&folder));
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
