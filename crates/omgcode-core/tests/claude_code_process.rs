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
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use omgcode_core::action_journal::{ActionDisposition, ActionJournal, ActionRecord, ActionTool};
use omgcode_core::control::{
    CLAUDE_CODE_TURN_STOP_WAIT, ClaudeCodeDeclaration, ControlStore, wait_for_claude_code_turns,
};
use omgcode_core::policy::{Access, PermissionSet, WorkspaceId};
use omgcode_core::prompt::{self, BASE_PROMPT, BASE_PROMPT_VERSION, ModelMessage, ModelRole};
use omgcode_core::provider::claude_code::{
    ClaudeCodeClient, ClaudeCodeConfig, ClaudeCodeFailure, ClaudeCodeModel, RELAY_NONCE_VARIABLE,
    RELAY_SUBCOMMAND, relay,
};
use omgcode_core::provider::{ModelReply, ProviderError};
use omgcode_core::session::{RequestManifest, SessionJournal};
use omgcode_core::turn::{CancelToken, TurnError, TurnLimits, TurnRuntime};
use tempfile::TempDir;

mod fake_claude;

const SECRET: &str = "workspace secret 7c1f";

/// Set in the copy of this binary that runs in a delegated cgroup on Linux.
#[cfg(target_os = "linux")]
const DELEGATED_ENV: &str = "OMGCODE_DELEGATED_TEST";

fn main() -> ExitCode {
    let arguments: Vec<OsString> = env::args_os().collect();
    match arguments.get(1).and_then(|argument| argument.to_str()) {
        Some("-p") => return fake_claude::run(&arguments),
        Some(command) if command == RELAY_SUBCOMMAND => return fake_relay(&arguments),
        _ => {}
    }
    // A Claude Code run has tree limits, which need a delegated cgroup on
    // Linux: unless this process already runs in one, the tests re-run under
    // `systemd-run --user --scope -p Delegate=yes`. A missing user service
    // manager fails them rather than skipping them.
    #[cfg(target_os = "linux")]
    if env::var_os(DELEGATED_ENV).is_none() {
        let status = Command::new("systemd-run")
            .args([
                "--user",
                "--scope",
                "--quiet",
                "--expand-environment=no",
                "-p",
                "Delegate=yes",
                "--",
            ])
            .arg(env::current_exe().expect("test executable"))
            .args(arguments.iter().skip(1))
            .env(DELEGATED_ENV, "1")
            .status()
            .expect("start systemd-run; these tests need a user service manager");
        return if status.success() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let filter = arguments
        .iter()
        .skip(1)
        .filter_map(|argument| argument.to_str())
        .find(|argument| !argument.starts_with('-'))
        .map(str::to_owned);
    let tests: [(&str, fn()); 8] = [
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
        (
            "a_withdrawn_connection_ends_its_running_turn",
            a_withdrawn_connection_ends_its_running_turn,
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
            watch: paths.watch,
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

    /// A client for the next turn. The fake's marks of an earlier run are
    /// removed, so a failure's diagnosis describes the next one.
    fn client(&self) -> ClaudeCodeClient {
        fake_claude::forget_runs(&self.sign_in);
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

fn claude_failure(result: Result<ModelReply, ProviderError>) -> ClaudeCodeFailure {
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
        .unwrap_or_else(|error| {
            panic!("reply: {error}; {}", fake_claude::diagnosis(&scene.sign_in))
        })
        .content;
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
    .unwrap_or_else(|error| panic!("turn: {error}; {}", fake_claude::diagnosis(&scene.sign_in)));
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

/// Every model action record. The journal reads a missing file as empty, so
/// any error here is a real failure.
fn actions(data: &Path) -> Vec<ActionRecord> {
    ActionJournal::recent(data, 1000).expect("read the action journal")
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

/// A cancel sent once the process has streamed text ends the turn and its
/// process promptly.
fn cancellation_ends_the_process_promptly() {
    let scene = Scene::new();
    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    let (ready, running) = std::sync::mpsc::channel::<()>();
    // The trigger cancels once the fake reports it is running, or after a
    // bound so a fake that never starts still ends the test.
    let timer = std::thread::spawn(move || {
        let started = running.recv_timeout(Duration::from_secs(60)).is_ok();
        trigger.cancel();
        (started, Instant::now())
    });
    let result = scene.client().complete_streaming(
        &messages("hang"),
        &mut |_| {
            let _ = ready.send(());
        },
        &cancel,
    );
    let returned = Instant::now();
    let (started, cancelled) = timer.join().expect("timer");
    assert!(started, "the fake process never streamed its ready text");
    assert!(
        matches!(result, Err(ProviderError::Cancelled)),
        "{result:?}"
    );
    let stopping = returned.saturating_duration_since(cancelled);
    assert!(stopping < Duration::from_secs(10), "{stopping:?}");
}

/// Revoking the connection's grants (Windows) or removing it (Linux) while
/// its process runs ends the turn with `claude_code_withdrawn`, and the
/// revocation's wait returns once the process has ended.
fn a_withdrawn_connection_ends_its_running_turn() {
    let scene = Scene::new();
    let (ready, running) = std::sync::mpsc::channel::<()>();
    let data = scene.data.clone();
    let revoker = std::thread::spawn(move || {
        let started = running.recv_timeout(Duration::from_secs(60)).is_ok();
        let mut control = ControlStore::open(&data).expect("settings");
        let (profile_id, _) = control.claude_code_setup("claude").expect("connection");
        if cfg!(windows) {
            control
                .revoke_claude_code_grants("claude")
                .expect("revoked");
        } else {
            let removed = control
                .remove_provider_profile("claude", control.revision())
                .expect("removed");
            assert_eq!(removed.profile_id, profile_id);
        }
        // The turn's watcher reads the change once the settings are released.
        drop(control);
        let waiting = Instant::now();
        let ended = wait_for_claude_code_turns(&data, profile_id, CLAUDE_CODE_TURN_STOP_WAIT);
        (started, ended, waiting.elapsed())
    });
    let result = scene.client().complete_streaming(
        &messages("hang"),
        &mut |_| {
            let _ = ready.send(());
        },
        &CancelToken::new(),
    );
    let (started, ended, waited) = revoker.join().expect("revoker");
    assert!(started, "the fake process never streamed its ready text");
    assert_eq!(claude_failure(result), ClaudeCodeFailure::Withdrawn);
    ended.expect("the wait saw every process of the connection end");
    assert!(waited < Duration::from_secs(20), "{waited:?}");
    // A turn that starts after the change starts no process.
    let failure = claude_failure(scene.client().complete_streaming(
        &messages("text"),
        &mut |_| {},
        &CancelToken::new(),
    ));
    assert_eq!(failure, ClaudeCodeFailure::Withdrawn);
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
