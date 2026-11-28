//! A Claude Code connection end to end, with a fake `claude` (ADR-0109).
//!
//! This test binary has no libtest harness. Started with `-p` it acts as a
//! fake `claude`: it reads its prompt, starts the MCP relay named in its MCP
//! configuration, speaks MCP to OMGCode through it, and writes stream-json
//! lines shaped like those Claude Code 2.1.325 was observed to write. Started
//! with `mcp-relay` it is the relay. Otherwise it supervises a copy of
//! itself that runs the tests below, and cleans up after that copy even when
//! a test ended it with a turn still running.
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
use std::process::{Command, ExitCode, ExitStatus};
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

fn main() -> ExitCode {
    let arguments: Vec<OsString> = env::args_os().collect();
    match arguments.get(1).and_then(|argument| argument.to_str()) {
        Some("-p") => return fake_claude::run(&arguments),
        Some(command) if command == RELAY_SUBCOMMAND => return fake_relay(&arguments),
        _ => {}
    }
    let filter = arguments
        .iter()
        .skip(1)
        .filter_map(|argument| argument.to_str())
        .find(|argument| !argument.starts_with('-'))
        .map(str::to_owned);
    if env::var_os(SUPERVISED_ENV).is_none() {
        return supervise(&arguments, filter.as_deref());
    }
    if arguments
        .get(1)
        .is_some_and(|argument| argument == ABANDON_ARGUMENT)
    {
        return abandon_one_turn();
    }
    #[cfg_attr(not(unix), allow(unused_mut, reason = "one test is Unix only"))]
    let mut tests: Vec<(&str, fn())> = vec![
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
        (
            "a_reply_the_relay_does_not_read_ends_at_the_request_timeout",
            a_reply_the_relay_does_not_read_ends_at_the_request_timeout,
        ),
        (
            "a_reply_the_relay_does_not_read_ends_on_cancellation",
            a_reply_the_relay_does_not_read_ends_on_cancellation,
        ),
        (
            "a_launch_that_starts_no_process_leaves_no_turn_record",
            a_launch_that_starts_no_process_leaves_no_turn_record,
        ),
    ];
    #[cfg(unix)]
    tests.push((
        "a_turn_whose_record_cannot_be_written_starts_no_process",
        a_turn_whose_record_cannot_be_written_starts_no_process,
    ));
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
// The supervisor: the tests run in a copy of this binary, cleaned up after.
// ---------------------------------------------------------------------------

/// Set in the copy of this binary that runs the tests under a supervisor.
const SUPERVISED_ENV: &str = "OMGCODE_SUPERVISED_TEST";

/// Makes a supervised copy abandon one turn, for the supervisor's own test.
const ABANDON_ARGUMENT: &str = "--abandon-one-turn";

/// The supervisor's own test, which runs in the supervising process.
const SUPERVISOR_TEST: &str = "a_supervisor_stops_and_cleans_up_after_an_abandoned_turn";

/// The exit code of a copy that ended with a turn still running.
const ABANDONED_EXIT: u8 = 70;

/// What the supervisor cleaned up after a copy ended.
struct Cleaned {
    /// The scenes whose fixtures outlived the copy.
    scenes: usize,
    /// The fakes and relays of those scenes whose end was observed.
    ended: usize,
    /// The grants revoked for those scenes' connections.
    #[cfg(windows)]
    revoked: usize,
    /// The container profiles deleted for those scenes' connections.
    #[cfg(windows)]
    profiles_deleted: usize,
}

/// Run the tests in a supervised copy of this binary, clean up after it,
/// and then run the supervisor's own test.
///
/// A test that abandons a turn ([`abandon`]) ends its copy, and no drop of
/// the copy runs, so its scene's fixtures and, on Linux, the relay outlive
/// it. Every temporary folder of a copy lies in one folder of this process,
/// so once the copy ended, [`clean_up_after`] can stop what it left
/// running, observe the end of each fake and relay it started, delete each
/// connection's container profile on Windows, and remove that folder.
fn supervise(arguments: &[OsString], filter: Option<&str>) -> ExitCode {
    let mut passed = match supervised_copy(0, &arguments[1..]) {
        Ok((status, cleaned)) => {
            if cleaned.scenes > 0 {
                eprintln!(
                    "the supervisor stopped and cleaned up {} abandoned scenes",
                    cleaned.scenes
                );
            }
            status.success()
        }
        Err(error) => {
            eprintln!("{error}");
            false
        }
    };
    if filter.is_none_or(|filter| SUPERVISOR_TEST.contains(filter)) {
        let outcome = supervisor_test();
        if let Err(error) = &outcome {
            eprintln!("{SUPERVISOR_TEST}: {error}");
        }
        let ok = outcome.is_ok();
        println!(
            "test {SUPERVISOR_TEST} ... {}",
            if ok { "ok" } else { "FAILED" }
        );
        println!(
            "\ntest result: {}. {} passed; {} failed",
            if ok { "ok" } else { "FAILED" },
            usize::from(ok),
            usize::from(!ok)
        );
        passed &= ok;
    }
    if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The supervisor's own test: a copy that ends while its turn's fake and
/// relay run, its writes blocked, leaves nothing once cleaned up.
fn supervisor_test() -> Result<(), String> {
    let (status, cleaned) = supervised_copy(1, &[OsString::from(ABANDON_ARGUMENT)])?;
    if status.code() != Some(i32::from(ABANDONED_EXIT)) {
        return Err(format!("the copy did not abandon its turn: {status}"));
    }
    if cleaned.scenes != 1 || cleaned.ended != 2 {
        return Err(format!(
            "{} scenes left, {} processes seen ended; expected 1 and 2",
            cleaned.scenes, cleaned.ended
        ));
    }
    #[cfg(windows)]
    if cleaned.profiles_deleted != 1 || cleaned.revoked == 0 {
        return Err(format!(
            "{} grants revoked and {} container profiles deleted",
            cleaned.revoked, cleaned.profiles_deleted
        ));
    }
    Ok(())
}

/// Run a copy of this binary with `arguments`, its temporary folders in a
/// folder of this process, and clean up after it. `run` keeps its scope's
/// name unique.
fn supervised_copy(run: u32, arguments: &[OsString]) -> Result<(ExitStatus, Cleaned), String> {
    let folder = TempDir::new().map_err(|error| format!("the supervision folder: {error}"))?;
    let scope = format!(
        "omgcode-claude-code-test-{}-{run}.scope",
        std::process::id()
    );
    let program = env::current_exe().map_err(|error| format!("the test executable: {error}"))?;
    let status = copy_command(&program, &scope)
        .args(arguments)
        .env(SUPERVISED_ENV, "1")
        .env("TMPDIR", folder.path())
        .env("TMP", folder.path())
        .env("TEMP", folder.path())
        .status()
        .map_err(|error| format!("start the supervised copy: {error}"))?;
    let cleaned = clean_up_after(folder, &scope)?;
    Ok((status, cleaned))
}

/// A copy of this binary in its own scope with a delegated cgroup, which a
/// Claude Code run's tree limits need. A missing user service manager fails
/// the tests rather than skipping them.
#[cfg(target_os = "linux")]
fn copy_command(program: &Path, scope: &str) -> Command {
    let mut command = Command::new("systemd-run");
    command
        .args(["--user", "--scope", "--quiet", "--expand-environment=no"])
        .arg(format!("--unit={scope}"))
        .args(["-p", "Delegate=yes", "--"])
        .arg(program);
    command
}

/// A copy of this binary.
#[cfg(not(target_os = "linux"))]
fn copy_command(program: &Path, _scope: &str) -> Command {
    Command::new(program)
}

/// Stop what a copy left running in `folder`, observe the end of every fake
/// and relay it started, and remove its fixtures.
///
/// On Windows the kernel ends a copy's process trees when the copy exits,
/// because each tree's job ends when its last handle closes. On Linux the
/// copy's exit ends the fake but not its relay, so the copy's scope is
/// stopped, which ends every process left in it. Either way an end counts
/// only once each fake's and relay's alive lock is seen free, and a folder
/// whose processes were not seen to end is kept and named.
fn clean_up_after(folder: TempDir, scope: &str) -> Result<Cleaned, String> {
    #[cfg(target_os = "linux")]
    stop_scope(scope)?;
    #[cfg(not(target_os = "linux"))]
    let _ = scope;
    let mut cleaned = Cleaned {
        scenes: 0,
        ended: 0,
        #[cfg(windows)]
        revoked: 0,
        #[cfg(windows)]
        profiles_deleted: 0,
    };
    let entries =
        fs::read_dir(folder.path()).map_err(|error| format!("the supervision folder: {error}"))?;
    for entry in entries {
        let base = entry
            .map_err(|error| format!("the supervision folder: {error}"))?
            .path();
        let (data, sign_in) = Scene::folders(&base);
        if !sign_in.is_dir() {
            continue;
        }
        cleaned.scenes += 1;
        match fake_claude::alive_ended(&sign_in, EXIT_PROBE) {
            Ok(ended) => cleaned.ended += ended,
            Err(error) => {
                let kept = folder.keep();
                return Err(format!(
                    "a process of the scene in {} was not seen to end: {error}; kept {}",
                    base.display(),
                    kept.display()
                ));
            }
        }
        #[cfg(windows)]
        if let Some(revocation) = revoke_left_connection(&data)? {
            cleaned.revoked += revocation.revoked;
            cleaned.profiles_deleted += usize::from(revocation.profile_deleted);
        }
        #[cfg(not(windows))]
        let _ = data;
    }
    folder
        .close()
        .map_err(|error| format!("remove the supervision folder: {error}"))?;
    Ok(cleaned)
}

/// Stop a copy's scope, which ends every process still in it, and check
/// that it is inactive. A scope whose processes all ended is already gone.
#[cfg(target_os = "linux")]
fn stop_scope(scope: &str) -> Result<(), String> {
    let state = || {
        Command::new("systemctl")
            .args(["--user", "show", "--property=ActiveState", "--value", scope])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .map_err(|error| format!("read the state of {scope}: {error}"))
    };
    if state()? != "inactive" {
        // `stop` returns once the scope's processes ended or were killed.
        let stopped = Command::new("systemctl")
            .args(["--user", "stop", scope])
            .status()
            .map_err(|error| format!("stop {scope}: {error}"))?;
        if !stopped.success() {
            return Err(format!("stop {scope}: {stopped}"));
        }
    }
    match state()?.as_str() {
        "inactive" => Ok(()),
        other => Err(format!("{scope} is {other} after its stop")),
    }
}

/// Revoke the grants of a scene's connection that outlived its copy, which
/// deletes its container profile, as the scene's drop would have.
#[cfg(windows)]
fn revoke_left_connection(
    data: &Path,
) -> Result<Option<omgcode_core::control::ClaudeCodeRevocation>, String> {
    if !data.is_dir() {
        return Ok(None);
    }
    let mut control = ControlStore::open(data)
        .map_err(|error| format!("the settings in {}: {error}", data.display()))?;
    if control.claude_code_setup("claude").is_err() {
        return Ok(None);
    }
    control
        .revoke_claude_code_grants("claude")
        .map(Some)
        .map_err(|failure| {
            format!(
                "revoke the connection in {}: {}",
                data.display(),
                failure.error
            )
        })
}

/// In a supervised copy, for the supervisor's own test: start a turn whose
/// reply write blocks and, once both the fake and its relay were seen
/// running, end the copy as [`abandon`] does, with the turn, its process
/// tree, and the scene's fixtures left behind.
fn abandon_one_turn() -> ExitCode {
    let scene = Scene::new();
    let flood = start_flood(&scene, CancelToken::new());
    let until = Instant::now() + Duration::from_secs(60);
    while Instant::now() < until && !flood.turn.is_finished() {
        if fake_claude::stalled(&scene.sign_in) && fake_claude::processes_running(&scene.sign_in) {
            end_test_process("the supervisor's own test abandons its turn");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    eprintln!(
        "the turn never stalled with both processes running; {}",
        fake_claude::diagnosis(&scene.sign_in)
    );
    ExitCode::FAILURE
}

/// End this copy of the tests with a turn still running. No drop runs, so
/// the supervisor stops and observes what is left and removes the fixtures.
fn end_test_process(message: &str) -> ! {
    eprintln!("{message}; ending this copy of the tests for its supervisor to clean up");
    std::process::exit(i32::from(ABANDONED_EXIT));
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
    /// A scene's data root and sign-in folder in its temporary folder
    /// `base`. The data root is shaped like an operator's own, so a socket
    /// kept in the runtime folder could not fit `sun_path` on Linux.
    fn folders(base: &Path) -> (PathBuf, PathBuf) {
        let home = base.join("home").join("operator");
        (
            home.join(".local").join("share").join("omgcode"),
            base.join("claude-sign-in"),
        )
    }

    fn new() -> Self {
        let temporary = TempDir::new().expect("temporary directory");
        let base = temporary.path();
        let (data, sign_in) = Self::folders(base);
        fs::create_dir_all(data.parent().expect("data parent")).expect("home folders");
        let root = base.join("development-root");
        let workspace = root.join("workspace");
        let tools = base.join("tools");
        let relay_folder = base.join("relay");
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

    /// The recorded connection's profile identifier.
    fn profile_id(&self) -> uuid::Uuid {
        ControlStore::open(&self.data)
            .expect("control store")
            .claude_code_setup("claude")
            .expect("connection")
            .0
    }

    /// The names of the turn records left in the connection's storage.
    fn turn_records(&self) -> Vec<String> {
        fs::read_dir(self.data.join("claude-code"))
            .expect("the connection's storage")
            .filter_map(|entry| {
                let name = entry.expect("entry").file_name().into_string().ok()?;
                name.ends_with(".unconfirmed").then_some(name)
            })
            .collect()
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
    // The relay's socket fits because it lives in a short private folder
    // (ADR-0109 decision 6): one in this runtime folder would not.
    #[cfg(target_os = "linux")]
    {
        let in_runtime = scene
            .config
            .runtime_folder
            .join("turns")
            .join("0".repeat(32))
            .join("relay.sock");
        assert!(
            in_runtime.as_os_str().len() >= 108,
            "the data root is too short to need the private folder: {}",
            in_runtime.display()
        );
    }
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

/// A launch that fails after its turn record was written, but before any
/// process started, clears the record and releases the turn lock: nothing
/// ran, so nothing is left for the operator to acknowledge.
fn a_launch_that_starts_no_process_leaves_no_turn_record() {
    let mut scene = Scene::new();
    scene.config.program = scene.config.program.with_file_name(if cfg!(windows) {
        "missing-claude.exe"
    } else {
        "missing-claude"
    });
    let result =
        scene
            .client()
            .complete_streaming(&messages("hello"), &mut |_| {}, &CancelToken::new());
    assert!(
        result.is_err(),
        "a missing program cannot reply: {result:?}"
    );
    assert!(
        !fake_claude::started(&scene.sign_in),
        "{}",
        fake_claude::diagnosis(&scene.sign_in)
    );
    assert_eq!(scene.turn_records(), Vec::<String>::new());
    wait_for_claude_code_turns(&scene.data, scene.profile_id(), Duration::from_secs(5))
        .expect("the turn left no record and released its lock");
}

/// A turn whose record cannot be written starts no process: without its
/// record, an OMGCode process that ended before the tree did would leave
/// nothing to fail a later wait. Here the connection's storage refuses new
/// entries, while the turn lock file already exists and opens.
#[cfg(unix)]
fn a_turn_whose_record_cannot_be_written_starts_no_process() {
    use std::os::unix::fs::PermissionsExt;
    let scene = Scene::new();
    let profile_id = scene.profile_id();
    // Makes the turn lock file, and finds nothing to acknowledge.
    assert!(
        !omgcode_core::control::acknowledge_claude_code_cleanup(&scene.data, profile_id)
            .expect("the turn lock file")
    );
    let storage = scene.data.join("claude-code");
    fs::set_permissions(&storage, fs::Permissions::from_mode(0o500)).expect("refuse new entries");
    let probe = fs::File::create_new(storage.join("probe"));
    let result = probe.is_err().then(|| {
        scene
            .client()
            .complete_streaming(&messages("hello"), &mut |_| {}, &CancelToken::new())
    });
    fs::set_permissions(&storage, fs::Permissions::from_mode(0o700)).expect("restore the mode");
    let Some(result) = result else {
        let _ = fs::remove_file(storage.join("probe"));
        panic!(
            "the storage folder still took a new entry; run this test as a user other than root"
        );
    };
    assert_eq!(claude_failure(result), ClaudeCodeFailure::Failed);
    assert!(
        !fake_claude::started(&scene.sign_in),
        "{}",
        fake_claude::diagnosis(&scene.sign_in)
    );
    assert_eq!(scene.turn_records(), Vec::<String>::new());
    wait_for_claude_code_turns(&scene.data, profile_id, Duration::from_secs(5))
        .expect("the refused turn released its lock");
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
        // Taken first, so a delay after the request counts against the turn.
        let cancelled = Instant::now();
        trigger.cancel();
        (started, cancelled)
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

/// How a flood turn ended: whether it was cancelled, and the model's last
/// failure.
struct FloodEnd {
    cancelled: bool,
    error: Option<ProviderError>,
    took: Duration,
    /// Whether the exit probe saw the fake and its relay running while the
    /// write stalled.
    seen_running: bool,
    /// On Linux, whether the turn's thread was seen asleep in its reply
    /// write to the relay; `None` where no such probe exists.
    seen_blocked: Option<bool>,
    /// When the turn was cancelled, for a flood asked to cancel it.
    cancelled_at: Option<Instant>,
}

/// A flood turn running on its own thread.
struct FloodStart {
    turn: std::thread::JoinHandle<(bool, Option<ProviderError>)>,
    /// On Linux, the identifier of the turn's thread.
    thread: Option<String>,
    started: Instant,
}

/// Start, on its own thread, a turn whose fake floods the relay with
/// requests and reads none of the replies, so OMGCode's write of a reply
/// blocks.
fn start_flood(scene: &Scene, cancel: CancelToken) -> FloodStart {
    let client = scene.client();
    let (data, workspace, workspace_id) = (
        scene.data.clone(),
        scene.workspace.clone(),
        scene.workspace_id,
    );
    let (thread_sender, thread_id) = std::sync::mpsc::channel();
    let started = Instant::now();
    let turn = std::thread::spawn(move || {
        // The reply write runs on this thread, so its system call is this
        // thread's. A closed channel only means the test stopped waiting.
        let _ = thread_sender.send(this_thread_id());
        let mut model = ClaudeCodeModel::new(&client);
        let mut journal =
            SessionJournal::create_with_workspace_id(&data, &workspace, "Claude", workspace_id)
                .expect("session");
        let outcome = TurnRuntime::new(&mut journal, &data, workspace_id, TurnLimits::default())
            .expect("runtime")
            .with_cancel_token(cancel)
            .run(&mut model, "SCENARIO:flood", "", manifest());
        assert!(outcome.is_err(), "a flooded turn cannot finish");
        (
            matches!(outcome, Err(TurnError::Cancelled)),
            model.last_error(),
        )
    });
    let thread = thread_id
        .recv_timeout(Duration::from_secs(30))
        .ok()
        .flatten();
    FloodStart {
        turn,
        thread,
        started,
    }
}

/// Run a flood turn ([`start_flood`]). The turn runs on its own thread, so
/// one that never ends fails the test after `limit`. With
/// `cancel_once_blocked`, the turn is cancelled once the fake marked the
/// stall, both processes were seen running, and, on Linux, the turn's
/// thread was seen asleep in its reply write.
fn flood_turn(
    scene: &Scene,
    cancel: CancelToken,
    limit: Duration,
    cancel_once_blocked: bool,
) -> FloodEnd {
    let stop = cancel.clone();
    let FloodStart {
        turn,
        thread: turn_thread,
        started,
    } = start_flood(scene, cancel);
    let mut seen_running = false;
    let mut seen_blocked = turn_thread.as_ref().map(|_| false);
    let mut cancelled_at = None;
    while !turn.is_finished() {
        if started.elapsed() >= limit {
            abandon(turn, &stop, &scene.sign_in, limit);
        }
        let stalled = fake_claude::stalled(&scene.sign_in);
        // While the reply write stalls, both processes run, so the exit
        // probe used after the turn must see them running here.
        if stalled && !seen_running {
            seen_running = fake_claude::processes_running(&scene.sign_in);
        }
        if let Some(thread) = &turn_thread
            && seen_blocked == Some(false)
            && asleep_in_socket_write(thread)
        {
            seen_blocked = Some(true);
        }
        if cancel_once_blocked
            && cancelled_at.is_none()
            && stalled
            && seen_running
            && seen_blocked != Some(false)
        {
            // Taken first, so a delay after the request counts against the
            // turn.
            cancelled_at = Some(Instant::now());
            stop.cancel();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let took = started.elapsed();
    let (cancelled, error) = turn.join().expect("the turn thread");
    FloodEnd {
        cancelled,
        error,
        took,
        seen_running,
        seen_blocked,
        cancelled_at,
    }
}

/// This thread's identifier, which `/proc` names its system calls by.
#[cfg(target_os = "linux")]
fn this_thread_id() -> Option<String> {
    let link = fs::read_link("/proc/thread-self").ok()?;
    link.file_name()?.to_str().map(str::to_owned)
}

/// No system call probe exists here.
#[cfg(not(target_os = "linux"))]
fn this_thread_id() -> Option<String> {
    None
}

/// Whether the thread `thread` of this process is asleep in a write to a
/// socket. `/proc` names the system call a sleeping thread is in and its
/// first argument, the descriptor, so this is OMGCode's reply write to the
/// relay when `thread` runs the turn, whose only socket write that is.
#[cfg(target_os = "linux")]
fn asleep_in_socket_write(thread: &str) -> bool {
    let Ok(text) = fs::read_to_string(format!("/proc/self/task/{thread}/syscall")) else {
        return false;
    };
    let mut fields = text.split_whitespace();
    let (Some(number), Some(first)) = (fields.next(), fields.next()) else {
        return false;
    };
    let write_family = [
        libc::SYS_write,
        libc::SYS_writev,
        libc::SYS_sendto,
        libc::SYS_sendmsg,
    ];
    number
        .parse::<libc::c_long>()
        .is_ok_and(|number| write_family.contains(&number))
        && u64::from_str_radix(first.trim_start_matches("0x"), 16).is_ok_and(|descriptor| {
            fs::read_link(format!("/proc/self/fd/{descriptor}"))
                .is_ok_and(|target| target.to_string_lossy().starts_with("socket:"))
        })
}

/// Never called where [`this_thread_id`] names no thread.
#[cfg(not(target_os = "linux"))]
fn asleep_in_socket_write(_thread: &str) -> bool {
    false
}

/// How long a turn that overran its limit has, once cancelled, to end.
const ABANDON_BOUND: Duration = Duration::from_secs(30);

/// End a turn that overran `limit` and fail the test: cancel the turn and
/// wait at most [`ABANDON_BOUND`] for its thread, so no turn is left using
/// the scene while later tests run. A thread that does not end even then
/// ends this copy of the tests ([`end_test_process`]), which ends the thread
/// with it; the supervisor ([`clean_up_after`]) then stops the process tree
/// the turn left, observes its end, and removes the scene's fixtures.
fn abandon<T>(
    turn: std::thread::JoinHandle<T>,
    stop: &CancelToken,
    sign_in: &Path,
    limit: Duration,
) -> ! {
    stop.cancel();
    let until = Instant::now() + ABANDON_BOUND;
    while !turn.is_finished() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    let diagnosis = fake_claude::diagnosis(sign_in);
    if turn.is_finished() {
        // The turn's own result does not matter once it overran.
        let _ = turn.join();
        panic!("the turn did not end in {limit:?}, only once cancelled; {diagnosis}");
    }
    end_test_process(&format!(
        "the turn did not end in {limit:?}, nor within {ABANDON_BOUND:?} of its cancellation; {diagnosis}"
    ));
}

/// How long the fake and its relay may take to be seen ended once the turn
/// returned.
const EXIT_PROBE: Duration = Duration::from_secs(10);

/// A reply write that the relay never reads ends at the request timeout:
/// the turn fails with `claude_code_failed` instead of waiting for the
/// relay. On Linux the turn's thread is seen asleep in that write first;
/// elsewhere the fake's stall marker is the only evidence of it, and the
/// launch tests prove a blocked write's release with a controlled writer.
fn a_reply_the_relay_does_not_read_ends_at_the_request_timeout() {
    let mut scene = Scene::new();
    let timeout = Duration::from_secs(15);
    scene.config.request_timeout = timeout;
    let end = flood_turn(&scene, CancelToken::new(), Duration::from_secs(120), false);
    assert!(
        fake_claude::stalled(&scene.sign_in),
        "{}",
        fake_claude::diagnosis(&scene.sign_in)
    );
    assert_ne!(
        end.seen_blocked,
        Some(false),
        "the reply write was never seen asleep"
    );
    assert!(!end.cancelled);
    assert_eq!(
        end.error,
        Some(ProviderError::ClaudeCode(ClaudeCodeFailure::Failed))
    );
    assert!(end.took >= timeout, "{:?}", end.took);
    assert!(
        end.seen_running,
        "{}",
        fake_claude::diagnosis(&scene.sign_in)
    );
    // The error proves nothing about the processes: both must be seen gone.
    fake_claude::processes_ended(&scene.sign_in, EXIT_PROBE).expect("the fake and its relay ended");
}

/// A reply write that the relay never reads ends promptly when the turn is
/// cancelled, which happens only once the write is blocked: on Linux once
/// the turn's thread is seen asleep in it, elsewhere once the fake marked
/// the stall.
fn a_reply_the_relay_does_not_read_ends_on_cancellation() {
    let scene = Scene::new();
    let end = flood_turn(&scene, CancelToken::new(), Duration::from_secs(120), true);
    let returned = Instant::now();
    let Some(cancelled_at) = end.cancelled_at else {
        panic!(
            "never cancelled; {}",
            fake_claude::diagnosis(&scene.sign_in)
        );
    };
    assert!(
        end.seen_running,
        "{}",
        fake_claude::diagnosis(&scene.sign_in)
    );
    assert_ne!(
        end.seen_blocked,
        Some(false),
        "the reply write was never seen asleep"
    );
    assert!(end.cancelled, "{:?}", end.error);
    let stopping = returned.saturating_duration_since(cancelled_at);
    assert!(stopping < Duration::from_secs(10), "{stopping:?}");
    fake_claude::processes_ended(&scene.sign_in, EXIT_PROBE).expect("the fake and its relay ended");
}

fn fake_relay(arguments: &[OsString]) -> ExitCode {
    let Some(address) = arguments.get(2) else {
        return ExitCode::FAILURE;
    };
    let nonce = env::var(RELAY_NONCE_VARIABLE).unwrap_or_default();
    if let Some(folder) = env::var_os(fake_claude::ALIVE_FOLDER_VARIABLE) {
        fake_claude::hold_alive(Path::new(&folder), fake_claude::RELAY_ALIVE_FILE);
    }
    match relay(address, &nonce, io::stdin(), io::stdout()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
