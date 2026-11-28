//! A Claude Code connection through the trusted CLI, with a fake `claude`
//! (ADR-0109).
//!
//! This test binary has no libtest harness. Started with `-p` it is the fake
//! `claude` of `omgcode-core/tests/fake_claude`; otherwise it runs the test
//! below. The `omgcode` binary under test is copied into a temporary folder
//! and runs from there, both as the CLI and as the MCP relay, so the relay
//! folder the connection's container is granted is that temporary folder.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use omgcode_core::control::ControlStore;
use omgcode_core::policy::{Access, PermissionSet};
use serde_json::Value;
use tempfile::TempDir;

#[path = "../../omgcode-core/tests/fake_claude/mod.rs"]
mod fake_claude;

/// The longest any one CLI command here may run. Grants create and delete
/// an AppContainer profile on Windows.
const COMMAND_DEADLINE: Duration = Duration::from_secs(120);

/// The count of Linux scopes started, which makes each scope name unique.
static SCOPES: AtomicUsize = AtomicUsize::new(0);

fn main() -> ExitCode {
    let arguments: Vec<OsString> = env::args_os().collect();
    if arguments.get(1).and_then(|argument| argument.to_str()) == Some("-p") {
        return fake_claude::run(&arguments);
    }
    let outcome = std::panic::catch_unwind(a_claude_code_connection_runs_through_the_cli);
    if outcome.is_ok() {
        println!("test a_claude_code_connection_runs_through_the_cli ... ok");
        println!("\ntest result: ok. 1 passed; 0 failed");
        ExitCode::SUCCESS
    } else {
        println!("test a_claude_code_connection_runs_through_the_cli ... FAILED");
        println!("\ntest result: FAILED. 0 passed; 1 failed");
        ExitCode::FAILURE
    }
}

struct Scene {
    _temporary: TempDir,
    data: PathBuf,
    workspace: PathBuf,
    omgcode: PathBuf,
    program: PathBuf,
    sign_in: PathBuf,
}

impl Scene {
    fn new() -> Self {
        let temporary = TempDir::new().expect("temporary directory");
        let base = temporary.path();
        let data = base.join("application-data");
        let root = base.join("development-root");
        let workspace = root.join("workspace");
        let tools = base.join("tools");
        let bin = base.join("bin");
        let sign_in = base.join("claude-sign-in");
        for path in [&root, &workspace, &tools, &bin, &sign_in] {
            fs::create_dir(path).expect("scene folder");
        }
        fs::write(workspace.join("notes.txt"), "notes").expect("workspace file");
        #[cfg(windows)]
        make_private_root(&data);
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        let program = tools.join(format!("claude{suffix}"));
        let omgcode = bin.join(format!("omgcode{suffix}"));
        fs::copy(env::current_exe().expect("test executable"), &program).expect("copy claude");
        fs::copy(env!("CARGO_BIN_EXE_omgcode"), &omgcode).expect("copy omgcode");
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
        Self {
            _temporary: temporary,
            data,
            workspace,
            omgcode,
            program,
            sign_in,
        }
    }

    /// Run the copied CLI with `arguments`. Its environment carries Claude
    /// credential variables that the `claude` process must never see. On
    /// Linux, tree limits need a delegated cgroup, so the CLI starts in a
    /// scope of its own, as an operator starts it.
    fn run(&self, arguments: &[&str]) -> Output {
        let scope = cfg!(target_os = "linux").then(|| {
            let count = SCOPES.fetch_add(1, Ordering::SeqCst);
            format!("omgcode-claude-code-cli-{}-{count}", std::process::id())
        });
        let mut command = if let Some(scope) = &scope {
            let mut command = Command::new("systemd-run");
            command
                .args([
                    "--user",
                    "--scope",
                    "--quiet",
                    "--expand-environment=no",
                    "-p",
                    "Delegate=yes",
                ])
                .arg(format!("--unit={scope}"))
                .arg("--")
                .arg(&self.omgcode);
            command
        } else {
            Command::new(&self.omgcode)
        };
        command
            .arg("--data-dir")
            .arg(&self.data)
            .args(arguments)
            .env_remove("OMGCODE_LM_TOKEN")
            .env("ANTHROPIC_API_KEY", "not-a-real-key")
            .env("ANTHROPIC_AUTH_TOKEN", "not-a-real-token")
            .env("CLAUDE_CODE_OAUTH_TOKEN", "not-a-real-token")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        finish(command.spawn().expect("start omgcode"), scope.as_deref())
    }

    fn revision(&self) -> String {
        let status = self.run(&["workspace", "status"]);
        succeeded(&status)["revision"].to_string()
    }

    fn chat(&self, session: &[&str], message: &str, extra: &[&str]) -> Output {
        let mut arguments = vec!["chat"];
        arguments.extend_from_slice(session);
        arguments.extend_from_slice(&[
            "--provider-profile",
            "claude",
            "--model",
            "fake-model",
            "--context-tokens",
            "32000",
        ]);
        arguments.extend_from_slice(extra);
        arguments.push(message);
        self.run(&arguments)
    }
}

impl Drop for Scene {
    fn drop(&mut self) {
        // A failed test can leave grants; revoking deletes the container
        // profile too. A failure here only leaves entries on folders that are
        // about to be removed.
        if cfg!(windows)
            && let Ok(Some(mut control)) = ControlStore::open_existing(&self.data)
        {
            let _ = control.revoke_claude_code_grants("claude");
        }
    }
}

/// Wait for `child` within [`COMMAND_DEADLINE`], reading both streams. A
/// command that overruns is stopped with its whole tree before the test
/// fails: on Linux by killing its scope, the cgroup that holds every
/// descendant; on Windows, ending `omgcode` closes its job object, whose
/// kill-on-close limit ends the confined tree.
fn finish(mut child: std::process::Child, scope: Option<&str>) -> Output {
    let mut stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");
    let out = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let err = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if started.elapsed() > COMMAND_DEADLINE {
            let scope_killed = scope.map(|scope| {
                Command::new("systemctl")
                    .args(["--user", "kill", "--signal=SIGKILL"])
                    .arg(format!("{scope}.scope"))
                    .status()
            });
            let killed = child.kill();
            let reaped = child.wait();
            panic!(
                "omgcode exceeded {COMMAND_DEADLINE:?}; scope kill: {scope_killed:?}, \
                 kill: {killed:?}, exit: {reaped:?}"
            );
        }
        thread::sleep(Duration::from_millis(20));
    };
    Output {
        status,
        stdout: out.join().expect("stdout reader"),
        stderr: err.join().expect("stderr reader"),
    }
}

fn succeeded(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "omgcode failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}

fn failed(output: &Output) -> Value {
    assert!(
        !output.status.success(),
        "omgcode unexpectedly succeeded: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error");
    error["error"].clone()
}

fn text(path: &Path) -> &str {
    path.to_str().expect("temporary paths are Unicode")
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

/// Record, grant, chat with tools, resume with streaming, compact, refuse
/// the paths that are not offered, revoke, and remove.
fn a_claude_code_connection_runs_through_the_cli() {
    let scene = Scene::new();
    let revision = scene.revision();
    let added = scene.run(&[
        "workspace",
        "claude-code-add",
        "--name",
        "claude",
        "--program",
        text(&scene.program),
        "--config-folder",
        text(&scene.sign_in),
        "--revision",
        &revision,
    ]);
    succeeded(&added);

    let workspace = ["--workspace", text(&scene.workspace)];
    if cfg!(windows) {
        // No grant yet: the turn is refused before any session or process.
        let error = failed(&scene.chat(&workspace, "SCENARIO:tool", &[]));
        assert_eq!(error["code"], "credential", "{error}");
        let message = error["message"].as_str().unwrap_or_default();
        assert!(message.contains("claude_code_unusable"), "{message}");
        assert!(message.contains("claude-code-grant"), "{message}");
        let revision = scene.revision();
        let granted = succeeded(&scene.run(&[
            "workspace",
            "claude-code-grant",
            "claude",
            "--revision",
            &revision,
        ]));
        assert_eq!(
            granted["claude_code_grants"].as_array().map(Vec::len),
            Some(4),
            "{granted}"
        );
    }

    let first = succeeded(&scene.chat(&workspace, "SCENARIO:tool", &[]));
    let reply = first["reply"]["content"].as_str().unwrap_or_default();
    assert!(reply.starts_with("listed:"), "{first}");
    assert!(reply.contains("notes.txt"), "{first}");
    assert_eq!(first["model_tools"]["tool_calls"], 1, "{first}");
    let journal = first["journal"].as_str().expect("journal").to_owned();

    let probe = format!(
        "SCENARIO:text probe={}",
        text(&scene.workspace.join("notes.txt"))
    );
    let resumed = scene.chat(&["--journal", &journal], &probe, &["--stream"]);
    let second = succeeded(&resumed);
    let reply = second["reply"]["content"].as_str().unwrap_or_default();
    assert!(reply.contains("workspace=denied"), "{second}");
    assert!(reply.contains("anthropic=absent"), "{second}");
    let streamed = String::from_utf8_lossy(&resumed.stderr);
    assert!(streamed.contains("hello world"), "{streamed}");

    let compacted = succeeded(&scene.run(&[
        "session",
        "compact",
        &journal,
        "--provider-profile",
        "claude",
        "--model",
        "fake-model",
        "--context-tokens",
        "32000",
    ]));
    assert_eq!(compacted["summarizer_calls"], 1, "{compacted}");

    // The model list and every other connection route refuse it by name.
    let error = failed(&scene.run(&["models", "--provider-profile", "claude"]));
    let message = error["message"].as_str().unwrap_or_default();
    assert!(message.contains("claude_code_cli_only"), "{message}");

    if cfg!(windows) {
        let revision = scene.revision();
        let error = failed(&scene.run(&[
            "workspace",
            "provider-profile-remove",
            "claude",
            "--revision",
            &revision,
        ]));
        assert_eq!(error["code"], "control", "{error}");
        let revoked = succeeded(&scene.run(&["workspace", "claude-code-revoke", "claude"]));
        assert_eq!(revoked["revocation"]["revoked"], 4, "{revoked}");
        assert_eq!(revoked["revocation"]["profile_deleted"], true, "{revoked}");
    }
    let revision = scene.revision();
    succeeded(&scene.run(&[
        "workspace",
        "provider-profile-remove",
        "claude",
        "--revision",
        &revision,
    ]));
}
