mod support;

use serde_json::{Value, json};
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;
use uuid::Uuid;

use omgcode_core::control::ControlStore;
#[cfg(unix)]
use support::CLI_DEADLINE;
use support::{
    GRANT_CLI_DEADLINE, HANG_FIXTURE_ENV, assert_control_error, assert_error_code, assert_success,
    json_output, path_arg, run_cli, run_cli_for, run_with_deadline,
};

/// Park only when the timeout regression launches this test in a child process.
#[test]
fn process_timeout_fixture() {
    if std::env::var_os(HANG_FIXTURE_ENV).is_some() {
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(b"HANG_FIXTURE_READY\n")
            .expect("write child readiness marker");
        stdout.flush().expect("flush child readiness marker");
        loop {
            thread::park();
        }
    }
}
/// Verify that the deadline kills and reaps a child that does not exit.
#[test]
fn process_deadline_kills_and_reaps_a_hung_child() {
    // Given a fixture child that parks indefinitely after the test harness starts it.
    let executable = std::env::current_exe().expect("current test executable");
    let mut command = Command::new(executable);
    command
        .env_remove("OMGCODE_LM_TOKEN")
        .env(HANG_FIXTURE_ENV, "1")
        .args(["process_timeout_fixture", "--nocapture"]);

    // When the deadline expires, the helper kills and waits for the child process.
    let result =
        run_with_deadline(command, Duration::from_secs(3)).expect("run bounded hanging fixture");
    assert!(result.timed_out, "the fixture must exceed its deadline");
    assert!(
        String::from_utf8_lossy(&result.output.stdout).contains("HANG_FIXTURE_READY"),
        "the child must reach its parked state before timeout"
    );
    assert!(
        !result.output.status.success(),
        "the killed child must report a non-success exit"
    );
}
fn set_root(data_root: &Path, development_root: &Path) -> Value {
    let output = run_cli(
        data_root,
        &[
            "workspace",
            "set-root",
            path_arg(development_root),
            "--revision",
            "0",
        ],
    );
    assert_success(&output);
    json_output(&output)
}

#[test]
fn workspace_commands_persist_cas_permissions_policy_and_audit_results() {
    // Given private application data adjacent to, but outside, the development root.
    let temporary = TempDir::new().expect("private temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    let first_workspace = development_root.join("first");
    let second_workspace = development_root.join("second");
    fs::create_dir_all(&first_workspace).expect("create first workspace");
    fs::create_dir_all(&second_workspace).expect("create second workspace");

    // When status initializes an empty store, the first root update advances revision 0 to 1.
    let status = run_cli(&data_root, &["workspace", "status"]);
    assert_success(&status);
    let initial = json_output(&status);
    assert_eq!(initial["revision"], 0);
    assert_eq!(initial["development_root"], Value::Null);
    let root_snapshot = set_root(&data_root, &development_root);
    assert_eq!(root_snapshot["revision"], 1);
    assert_ne!(root_snapshot["development_root"], Value::Null);

    // When two paths register, their IDs persist and an idempotent repeat
    // does not advance revision. On Windows each registration also grants
    // the workspace's container read-write on its folder (ADR-0107), one
    // more revision; Linux records no grant.
    let _cleanup = RevokeGrantsOnDrop(&data_root);
    let step: u64 = if cfg!(windows) { 2 } else { 1 };
    let register_cli = |workspace: &Path, revision: u64| {
        let output = run_cli_for(
            &data_root,
            &[
                "workspace",
                "register",
                path_arg(workspace),
                "--revision",
                &revision.to_string(),
            ],
            GRANT_CLI_DEADLINE,
        );
        assert_success(&output);
        json_output(&output)
    };
    let first = register_cli(&first_workspace, 1);
    let first_id = first["registration_id"]
        .as_str()
        .expect("first workspace ID");
    assert_eq!(first["snapshot"]["revision"], 1 + step);
    assert_eq!(first.get("grant_id").is_some(), cfg!(windows), "{first}");
    let first_repeat = register_cli(&first_workspace, 1 + step);
    assert_eq!(first_repeat["registration_id"], first_id);
    assert_eq!(first_repeat["snapshot"]["revision"], 1 + step);
    assert!(first_repeat.get("grant_id").is_none(), "{first_repeat}");

    let second = register_cli(&second_workspace, 1 + step);
    let second_id = second["registration_id"]
        .as_str()
        .expect("second workspace ID");
    assert_ne!(first_id, second_id);
    let base = 1 + 2 * step;
    assert_eq!(second["snapshot"]["revision"], base);
    let grants = second["snapshot"]["container_grants"].as_array();
    if cfg!(windows) {
        let grants = grants.expect("grants");
        assert_eq!(grants.len(), 2, "{second}");
        for grant in grants {
            assert_eq!(grant["access"], "read_write", "{grant}");
            assert_eq!(grant["state"], "active", "{grant}");
        }
    } else {
        assert!(grants.is_none(), "{second}");
    }

    // When flags are omitted, defaults allow read, prompt for write, and deny
    // execute, network, and host services, with model tools off. Status
    // reports every preference and per-tool limit explicitly.
    let second_permissions = second["snapshot"]["workspaces"]
        .as_array()
        .expect("workspace registrations")
        .iter()
        .find(|workspace| workspace["id"] == second_id)
        .expect("second workspace registration");
    let default_permissions = &second_permissions["permissions"];
    assert_eq!(
        default_permissions,
        &json!({
            "read": "allow",
            "write": "prompt",
            "execute": "deny",
            "task_network": "deny",
            "model_tools": false,
            "host_services": "deny",
            "web_fetch": "deny",
            "web_fetch_hosts": [],
            "write_tools": {
                "edit": "allow",
                "multi_edit": "allow",
                "create": "allow",
                "rename": "allow",
                "delete": "allow",
                "create_directory": "allow"
            }
        })
    );

    // When a caller supplies an obsolete revision, the CLI rejects the update.
    let permissions = run_cli(
        &data_root,
        &[
            "workspace",
            "permissions",
            first_id,
            "--revision",
            &base.to_string(),
            "--execute",
            "true",
            "--task-network",
            "true",
        ],
    );
    assert_success(&permissions);
    let permissions = json_output(&permissions);
    assert_eq!(permissions["revision"], base + 1);
    let limited = run_cli(
        &data_root,
        &[
            "workspace",
            "permissions",
            first_id,
            "--revision",
            &(base + 1).to_string(),
            "--create-directory-tool",
            "prompt",
        ],
    );
    assert_success(&limited);
    let limited = json_output(&limited);
    assert_eq!(limited["revision"], base + 2);
    let limits = limited["workspaces"]
        .as_array()
        .expect("workspace registrations")
        .iter()
        .find(|workspace| workspace["id"] == first_id)
        .expect("first workspace registration")["permissions"]["write_tools"]
        .clone();
    assert_eq!(limits["create_directory"], "prompt");
    assert_eq!(limits["create"], "allow");
    let stale = run_cli(
        &data_root,
        &[
            "workspace",
            "permissions",
            first_id,
            "--revision",
            &base.to_string(),
            "--read",
            "false",
        ],
    );
    assert_control_error(&stale);

    // When policy checks run, default read/write are eligible while execute/network stay disabled.
    let default_session = Uuid::new_v4().to_string();
    for (operation, eligible) in [
        ("read", true),
        ("write", true),
        ("execute", false),
        ("task-network", false),
    ] {
        let check = run_cli(
            &data_root,
            &[
                "workspace",
                "check",
                second_id,
                "--session",
                &default_session,
                "--operation",
                operation,
            ],
        );
        assert_success(&check);
        let decision = json_output(&check)["decision"].clone();
        assert_eq!(decision["configured_allowed"], eligible);
        assert_eq!(decision["execution_allowed"], false);
    }

    let session_id = Uuid::new_v4().to_string();
    // When policy checks run, an explicitly enabled flag remains configuration eligibility only.
    for (operation, eligible) in [
        ("read", true),
        ("write", true),
        ("execute", true),
        ("task-network", true),
    ] {
        let check = run_cli(
            &data_root,
            &[
                "workspace",
                "check",
                first_id,
                "--session",
                &session_id,
                "--operation",
                operation,
            ],
        );
        assert_success(&check);
        let decision = json_output(&check)["decision"].clone();
        assert_eq!(decision["configured_allowed"], eligible);
        assert_eq!(decision["execution_allowed"], false);
    }

    // When a valid binding targets another registered workspace, policy denies the scope mismatch.
    let cross_workspace = run_cli(
        &data_root,
        &[
            "workspace",
            "check",
            first_id,
            "--session",
            &session_id,
            "--operation",
            "read",
            "--target-workspace",
            second_id,
        ],
    );
    assert_success(&cross_workspace);
    let decision = json_output(&cross_workspace)["decision"].clone();
    assert_eq!(decision["configured_allowed"], false);
    assert_eq!(decision["reason"], "workspace_mismatch");
    assert_eq!(decision["execution_allowed"], false);

    // When the operator requests recent audit history, checks appear in the durable log.
    let audit = run_cli(&data_root, &["workspace", "audit", "--limit", "20"]);
    assert_success(&audit);
    let entries = json_output(&audit)["audit"]
        .as_array()
        .expect("audit entries")
        .clone();
    let policy_checks = entries
        .iter()
        .filter(|entry| entry["action"]["type"] == "policy_checked")
        .count();
    assert_eq!(
        policy_checks, 9,
        "expected four default, four enabled, and one cross-workspace check; entries: {entries:#?}"
    );
}

#[test]
fn configured_sessions_keep_workspace_ids_and_fail_closed_outside_the_root() {
    // Given application data and a development root that do not overlap.
    let temporary = TempDir::new().expect("private temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    let workspace = development_root.join("project");
    let outside_workspace = temporary.path().join("outside");
    fs::create_dir_all(&workspace).expect("create workspace");
    fs::create_dir(&outside_workspace).expect("create outside workspace");
    set_root(&data_root, &development_root);
    let expected_id = Value::from(register(&data_root, &workspace, 1));

    // When new sessions use the same configured workspace, they record its stable ID.
    let first = run_cli(
        &data_root,
        &["session", "new", "--workspace", path_arg(&workspace)],
    );
    let second = run_cli(
        &data_root,
        &["session", "new", "--workspace", path_arg(&workspace)],
    );
    assert_success(&first);
    assert_success(&second);
    assert_eq!(json_output(&first)["session"]["workspace_id"], expected_id);
    assert_eq!(json_output(&second)["session"]["workspace_id"], expected_id);

    // When session creation targets an unregistered path outside the root, it fails before journaling.
    let outside = run_cli(
        &data_root,
        &[
            "session",
            "new",
            "--workspace",
            path_arg(&outside_workspace),
        ],
    );
    assert_control_error(&outside);
    let journal_dir = data_root.join("sessions");
    let journal_count = fs::read_dir(&journal_dir)
        .expect("session directory exists")
        .flat_map(|year| {
            fs::read_dir(year.expect("year directory").path()).expect("month directory")
        })
        .flat_map(|month| fs::read_dir(month.expect("month entry").path()).expect("day directory"))
        .flat_map(|day| fs::read_dir(day.expect("day entry").path()).expect("journal files"))
        .count();
    assert_eq!(journal_count, 2, "outside session must not be written");
}

/// Verify Unix FIFO paths fail promptly and release the settings-store lock.
#[cfg(unix)]
#[test]
fn fifo_paths_fail_promptly_and_release_the_control_store() {
    // Given a FIFO below a valid development root and private application data.
    let temporary = TempDir::new().expect("private temporary directory");
    let development_root = temporary.path().join("development");
    fs::create_dir(&development_root).expect("create development root");
    let fifo = development_root.join("workspace-fifo");
    let mut mkfifo = Command::new("mkfifo");
    mkfifo.arg(&fifo).env_remove("OMGCODE_LM_TOKEN");
    let created = run_with_deadline(mkfifo, CLI_DEADLINE).expect("run mkfifo fixture command");
    assert!(!created.timed_out, "mkfifo fixture command must finish");
    assert_success(&created.output);

    // When the FIFO is selected as the root, the CLI must return a controlled failure.
    let root_data = temporary.path().join("root-application-data");
    let root_attempt = run_cli(
        &root_data,
        &["workspace", "set-root", path_arg(&fifo), "--revision", "0"],
    );
    assert_control_error(&root_attempt);
    let after_root_failure = run_cli(&root_data, &["workspace", "status"]);
    assert_success(&after_root_failure);
    let after_root_failure = json_output(&after_root_failure);
    assert_eq!(after_root_failure["revision"], 0);
    assert_eq!(after_root_failure["development_root"], Value::Null);

    // When the valid root is configured, registering the FIFO also fails and releases the lock.
    let registration_data = temporary.path().join("registration-application-data");
    let root_snapshot = set_root(&registration_data, &development_root);
    assert_eq!(root_snapshot["revision"], 1);
    let registration_attempt = run_cli(
        &registration_data,
        &["workspace", "register", path_arg(&fifo), "--revision", "1"],
    );
    assert_control_error(&registration_attempt);
    let after_registration_failure = run_cli(&registration_data, &["workspace", "status"]);
    assert_success(&after_registration_failure);
    let after_registration_failure = json_output(&after_registration_failure);
    assert_eq!(after_registration_failure["revision"], 1);
    assert_ne!(after_registration_failure["development_root"], Value::Null);
}
#[test]
fn corrupt_control_state_fails_closed_for_new_sessions() {
    // Given a private application-data directory with a corrupt control journal.
    let temporary = TempDir::new().expect("private temporary directory");
    let data_root = temporary.path().join("application-data");
    fs::create_dir_all(&data_root).expect("create application data");
    fs::write(
        data_root.join("control-v1.jsonl"),
        b"not a valid control journal\n",
    )
    .expect("write corrupt control state");
    let workspace = temporary.path().join("workspace");
    fs::create_dir(&workspace).expect("create workspace");

    // When a session tries to open that state, it reports control failure without fallback.
    let output = run_cli(
        &data_root,
        &["session", "new", "--workspace", path_arg(&workspace)],
    );
    assert_control_error(&output);
    assert!(!data_root.join("sessions").exists());
}

#[test]
fn legacy_session_creation_remains_available_without_a_control_store() {
    // Given a private data root with no control store, legacy text sessions still work.
    let temporary = TempDir::new().expect("private temporary directory");
    let data_root = temporary.path().join("application-data");
    let workspace = temporary.path().join("workspace");
    fs::create_dir(&workspace).expect("create workspace");

    // When a text session is created, it records the legacy per-session workspace ID.
    let output = run_cli(
        &data_root,
        &["session", "new", "--workspace", path_arg(&workspace)],
    );
    assert_success(&output);
    let output = json_output(&output);
    assert!(output["session"]["workspace_id"].as_str().is_some());
}

#[test]
fn capabilities_report_configuration_only_policy_and_trusted_runs_only() {
    // Given a private data directory that the capabilities command does not need to open.
    let temporary = TempDir::new().expect("private temporary directory");
    let data_root = temporary.path().join("application-data");

    // When capabilities are requested, policy configuration is reported separately from runners.
    let output = run_cli(&data_root, &["capabilities"]);
    assert_success(&output);
    let capabilities = json_output(&output);
    assert_eq!(capabilities["workspace_registry"], true);
    assert_eq!(capabilities["permission_policy"], true);
    assert_eq!(capabilities["permission_audit"], true);
    assert_eq!(capabilities["policy_decision"], "configuration_only");
    assert_eq!(capabilities["workspace_file_tools"], false);
    assert_eq!(capabilities["workspace_container_grants"], cfg!(windows));
    let runs = cfg!(any(target_os = "linux", windows));
    assert_eq!(capabilities["toolchain_profiles"], runs);
    assert_eq!(capabilities["model_command_tool"], "approval_required");
    assert_eq!(capabilities["model_write_tools"], "approval_required");
    assert_eq!(capabilities["package_network_profiles"], true);
    assert_eq!(capabilities["package_network_enforcement"], false);
    assert_eq!(capabilities["package_fetch"], true);
    assert_eq!(capabilities["provider_profiles"], true);
    assert_eq!(capabilities["provider_adapters"], false);
    assert_eq!(capabilities["approvals"], true);
    assert_eq!(capabilities["skills"]["registry"], true);
    assert_eq!(capabilities["skills"]["scripts"], false);
    if runs {
        assert_eq!(capabilities["process_execution"], "trusted_cli_only");
    } else {
        assert_eq!(capabilities["process_execution"], false);
    }
    let backend = if cfg!(target_os = "linux") {
        "landlock"
    } else if cfg!(windows) {
        "appcontainer"
    } else {
        "unavailable"
    };
    assert_eq!(capabilities["sandbox_backend"], backend);
    assert!(!data_root.exists());
}

/// Package-network profiles are recorded with revisions but never authorize a
/// run. Invalid origins and duplicates are refused without a revision change.
#[test]
fn package_profiles_are_configuration_only_and_revisioned() {
    let temporary = TempDir::new().expect("temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    fs::create_dir_all(&development_root).expect("development root");
    let snapshot = set_root(&data_root, &development_root);
    let revision = snapshot["revision"].as_u64().expect("revision");

    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "package-profile-add",
            "crates",
            "--registry",
            "https://registry.example.com/api/v4",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_success(&output);
    let added = json_output(&output);
    assert!(added["package_profile_id"].as_str().is_some());
    assert_eq!(added["snapshot"]["package_profiles"][0]["name"], "crates");
    let revision = added["snapshot"]["revision"]
        .as_u64()
        .expect("revision after add");

    let output = run_cli(&data_root, &["workspace", "package-profile-show", "CRATES"]);
    assert_success(&output);
    let shown = json_output(&output);
    assert_eq!(shown["package_profile"]["name"], "crates");
    assert_eq!(shown["package_profile"]["origins"][0]["scheme"], "https");
    assert_eq!(
        shown["package_profile"]["origins"][0]["host"],
        "registry.example.com"
    );
    assert_eq!(shown["package_profile"]["origins"][0]["port"], 443);
    assert_eq!(
        shown["package_profile"]["origins"][0]["path_prefix"],
        "/api/v4/"
    );

    let output = run_cli(&data_root, &["workspace", "status"]);
    assert_success(&output);
    assert_eq!(
        json_output(&output)["package_profiles"][0]["name"],
        "crates"
    );

    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "package-profile-add",
            "plain",
            "--registry",
            "http://registry.example.com/",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_control_error(&output);
    let output = run_cli(&data_root, &["workspace", "status"]);
    assert_eq!(
        json_output(&output)["revision"],
        Value::from(revision),
        "a refused profile changed the revision"
    );

    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "package-profile-add",
            "CRATES",
            "--registry",
            "https://other.example.com/",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_control_error(&output);

    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "package-profile-remove",
            "crates",
            "--revision",
            "0",
        ],
    );
    assert_control_error(&output);

    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "package-profile-remove",
            "crates",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_success(&output);
    assert!(
        json_output(&output)["snapshot"]
            .get("package_profiles")
            .is_none()
    );

    let output = run_cli(&data_root, &["workspace", "package-profile-show", "crates"]);
    assert_control_error(&output);
}

/// Provider profiles record only a connection shape and authentication mode;
/// they hold no credential and enable no adapter.
#[test]
fn provider_profiles_are_configuration_only_and_revisioned() {
    let temporary = TempDir::new().expect("temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    fs::create_dir_all(&development_root).expect("development root");
    let revision = set_root(&data_root, &development_root)["revision"]
        .as_u64()
        .expect("revision");

    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "provider-profile-add",
            "work",
            "--kind",
            "openai",
            "--auth",
            "subscription",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_success(&output);
    let added = json_output(&output);
    assert!(added["provider_profile_id"].as_str().is_some());
    assert_eq!(added["snapshot"]["provider_profiles"][0]["name"], "work");
    assert_eq!(added["snapshot"]["provider_profiles"][0]["kind"], "openai");
    assert_eq!(
        added["snapshot"]["provider_profiles"][0]["auth"],
        "subscription"
    );
    let revision = added["snapshot"]["revision"]
        .as_u64()
        .expect("revision after add");

    // ADR-0093: a subscription profile without OpenAI's endpoint is not a
    // ChatGPT connection. Sign-in is refused before the browser step (standard
    // error holds only the JSON error, never an address) and before any
    // network use; sign-out of a profile that never signed in is refused too.
    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "provider-signin",
            "work",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_control_error(&output);
    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "provider-signout",
            "work",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert!(!output.status.success(), "sign-out unexpectedly succeeded");
    serde_json::from_slice::<Value>(&output.stderr).expect("CLI error is JSON");

    let output = run_cli(&data_root, &["workspace", "provider-profile-show", "WORK"]);
    assert_success(&output);
    let shown = json_output(&output);
    assert_eq!(shown["provider_profile"]["name"], "work");
    assert_eq!(shown["provider_profile"]["kind"], "openai");
    assert_eq!(shown["provider_profile"]["auth"], "subscription");

    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "provider-profile-add",
            "invalid",
            "--kind",
            "ollama_local",
            "--auth",
            "api_key",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_control_error(&output);
    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "provider-profile-add",
            "WORK",
            "--kind",
            "anthropic",
            "--auth",
            "api_key",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_control_error(&output);

    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "provider-profile-remove",
            "work",
            "--revision",
            "0",
        ],
    );
    assert_control_error(&output);
    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "provider-profile-remove",
            "work",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_success(&output);
    assert!(
        json_output(&output)["snapshot"]
            .get("provider_profiles")
            .is_none()
    );
    let output = run_cli(&data_root, &["workspace", "provider-profile-show", "work"]);
    assert_control_error(&output);
}

/// Approvals are requested, decided, and consumed once; a denied or replayed
/// request is refused. No operation executes.
#[test]
fn approvals_are_requested_decided_and_consumed_once() {
    let temporary = TempDir::new().expect("temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    let workspace = development_root.join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let revision = set_root(&data_root, &development_root)["revision"]
        .as_u64()
        .expect("revision");
    let workspace_id = register(&data_root, &workspace, revision);
    let session = Uuid::new_v4().to_string();
    let digest = "a".repeat(64);

    let output = run_cli(
        &data_root,
        &[
            "approvals",
            "request",
            "--session",
            &session,
            "--workspace",
            &workspace_id,
            "--operation",
            "command",
            "--digest",
            &digest,
            "--summary",
            "Run the project build",
            "--ttl-secs",
            "900",
        ],
    );
    assert_success(&output);
    let approval = json_output(&output)["approval"].clone();
    let approval_id = approval["id"].as_str().expect("approval id").to_owned();
    assert_eq!(approval["state"], "pending");

    let output = run_cli(&data_root, &["approvals", "list"]);
    assert_success(&output);
    assert_eq!(json_output(&output)["approvals"][0]["id"], approval_id);

    let output = run_cli(&data_root, &["approvals", "approve", &approval_id]);
    assert_success(&output);
    assert_eq!(json_output(&output)["decision"], "approved");
    assert_eq!(json_output(&output)["approval"]["state"], "approved");

    let output = run_cli(
        &data_root,
        &[
            "approvals",
            "consume",
            &approval_id,
            "--session",
            &session,
            "--workspace",
            &workspace_id,
            "--operation",
            "command",
            "--digest",
            &digest,
        ],
    );
    assert_success(&output);
    assert_eq!(json_output(&output)["approval"]["state"], "consumed");
    let output = run_cli(
        &data_root,
        &[
            "approvals",
            "consume",
            &approval_id,
            "--session",
            &session,
            "--workspace",
            &workspace_id,
            "--operation",
            "command",
            "--digest",
            &digest,
        ],
    );
    assert_error_code(&output, "approval");

    let denied_digest = "b".repeat(64);
    let output = run_cli(
        &data_root,
        &[
            "approvals",
            "request",
            "--session",
            &session,
            "--workspace",
            &workspace_id,
            "--operation",
            "command",
            "--digest",
            &denied_digest,
            "--summary",
            "Second request",
            "--ttl-secs",
            "900",
        ],
    );
    assert_success(&output);
    let denied_id = json_output(&output)["approval"]["id"]
        .as_str()
        .expect("denied id")
        .to_owned();
    let output = run_cli(&data_root, &["approvals", "deny", &denied_id]);
    assert_success(&output);
    assert_eq!(json_output(&output)["decision"], "denied");
    let output = run_cli(
        &data_root,
        &[
            "approvals",
            "consume",
            &denied_id,
            "--session",
            &session,
            "--workspace",
            &workspace_id,
            "--operation",
            "command",
            "--digest",
            &denied_digest,
        ],
    );
    assert_error_code(&output, "approval");

    let output = run_cli(
        &data_root,
        &[
            "approvals",
            "request",
            "--session",
            &session,
            "--workspace",
            &workspace_id,
            "--operation",
            "command",
            "--digest",
            "not-a-digest",
            "--summary",
            "Invalid",
        ],
    );
    assert_error_code(&output, "approval");
    let missing = Uuid::new_v4();
    let output = run_cli(&data_root, &["approvals", "show", &missing.to_string()]);
    assert_error_code(&output, "approval");
}

/// Register `workspace` with the core primitive, which records only the
/// registration. The CLI `register` command also grants the workspace's
/// Windows container its folder (ADR-0107); tests of other commands use this
/// so their revisions match on every platform and no AppContainer state
/// outlives them. `workspace_commands_persist_cas_permissions_policy_and_audit_results`
/// covers the command itself.
fn register(data_root: &Path, workspace: &Path, revision: u64) -> String {
    let mut store = ControlStore::open_existing(data_root)
        .expect("open settings")
        .expect("settings exist");
    store
        .register_workspace(workspace, revision)
        .expect("register workspace")
        .id()
        .as_uuid()
        .to_string()
}

/// Revokes every recorded container grant when dropped, so a test that
/// registers through the CLI leaves no ACL entry or AppContainer profile
/// behind, even when it fails. It reports failures and never panics.
struct RevokeGrantsOnDrop<'a>(&'a Path);

impl Drop for RevokeGrantsOnDrop<'_> {
    fn drop(&mut self) {
        let status = run_cli(self.0, &["workspace", "status"]);
        if !status.status.success() {
            return;
        }
        for grant in json_output(&status)["container_grants"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let Some(id) = grant["id"].as_str() else {
                continue;
            };
            let revoked = run_cli_for(
                self.0,
                &["workspace", "container-revoke", id],
                GRANT_CLI_DEADLINE,
            );
            if !revoked.status.success() {
                eprintln!(
                    "grant revocation failed during cleanup: {id}: {}",
                    String::from_utf8_lossy(&revoked.stderr)
                );
            }
        }
    }
}

#[cfg(windows)]
#[test]
fn container_grants_are_recorded_listed_and_revoked() {
    // Given a registered workspace and a toolchain directory outside the root.
    let temporary = TempDir::new().expect("private temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    let workspace = development_root.join("app");
    let toolchain = temporary.path().join("toolchain");
    fs::create_dir_all(&workspace).expect("create workspace");
    fs::create_dir_all(&toolchain).expect("create toolchain");
    set_root(&data_root, &development_root);
    let _cleanup = RevokeGrantsOnDrop(&data_root);
    let id = register(&data_root, &workspace, 1);

    // When the workspace and the toolchain are granted to its container.
    let write = run_cli(
        &data_root,
        &[
            "workspace",
            "container-grant",
            &id,
            path_arg(&workspace),
            "--access",
            "read-write",
            "--revision",
            "2",
        ],
    );
    assert_success(&write);
    let write_id = json_output(&write)["grant_id"]
        .as_str()
        .expect("grant ID")
        .to_owned();
    let refused = run_cli(
        &data_root,
        &[
            "workspace",
            "container-grant",
            &id,
            path_arg(&toolchain),
            "--access",
            "read-write",
            "--revision",
            "3",
        ],
    );
    assert_control_error(&refused);
    let read = run_cli(
        &data_root,
        &[
            "workspace",
            "container-grant",
            &id,
            path_arg(&toolchain),
            "--access",
            "read-execute",
            "--revision",
            "3",
        ],
    );
    assert_success(&read);
    let read_id = json_output(&read)["grant_id"]
        .as_str()
        .expect("grant ID")
        .to_owned();

    // Then status lists both as active, with the workspace container.
    let status = run_cli(&data_root, &["workspace", "status"]);
    assert_success(&status);
    let status = json_output(&status);
    assert_eq!(status["revision"], 4);
    let grants = status["container_grants"].as_array().expect("grants");
    assert_eq!(grants.len(), 2);
    for grant in grants {
        assert_eq!(grant["state"], "active");
        assert_eq!(grant["workspace_id"], id.as_str());
        let container = grant["container"].as_str().expect("container");
        assert_eq!(
            container,
            format!("OMGCode.Workspace.{}", id.replace('-', ""))
        );
        assert!(grant["sid"].as_str().expect("SID").starts_with("S-1-15-2-"));
    }

    // And revocation needs no revision; the last one deletes the profile.
    let first = run_cli(&data_root, &["workspace", "container-revoke", &write_id]);
    assert_success(&first);
    assert_eq!(json_output(&first)["revocation"]["profile_deleted"], false);
    let last = run_cli(&data_root, &["workspace", "container-revoke", &read_id]);
    assert_success(&last);
    let last = json_output(&last);
    assert_eq!(last["revocation"]["profile_deleted"], true);
    assert_eq!(last["snapshot"]["revision"], 4);
    assert!(last["snapshot"].get("container_grants").is_none());
    let again = run_cli(&data_root, &["workspace", "container-revoke", &read_id]);
    assert_control_error(&again);
}

#[cfg(not(windows))]
#[test]
fn container_grants_are_refused_without_appcontainer_support() {
    let temporary = TempDir::new().expect("private temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    let workspace = development_root.join("app");
    fs::create_dir_all(&workspace).expect("create workspace");
    set_root(&data_root, &development_root);
    let id = register(&data_root, &workspace, 1);
    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "container-grant",
            &id,
            path_arg(&workspace),
            "--access",
            "read-write",
            "--revision",
            "2",
        ],
    );
    assert_control_error(&output);
    let status = run_cli(&data_root, &["workspace", "status"]);
    assert_success(&status);
    let status = json_output(&status);
    assert_eq!(status["revision"], 2);
    assert!(status.get("container_grants").is_none());
}

/// Approving a `file_write` request needs `--writers-stopped`, which is refused
/// for every other operation; `show` and `list` report the acknowledgement.
#[test]
fn file_write_approvals_require_the_writers_stopped_acknowledgement() {
    let temporary = TempDir::new().expect("temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    let workspace = development_root.join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let revision = set_root(&data_root, &development_root)["revision"]
        .as_u64()
        .expect("revision");
    let workspace_id = register(&data_root, &workspace, revision);
    let session = Uuid::new_v4().to_string();
    let request = |operation: &str, digest: &str| {
        let output = run_cli(
            &data_root,
            &[
                "approvals",
                "request",
                "--session",
                &session,
                "--workspace",
                &workspace_id,
                "--operation",
                operation,
                "--digest",
                digest,
                "--summary",
                "edit notes.txt",
            ],
        );
        assert_success(&output);
        json_output(&output)["approval"]["id"]
            .as_str()
            .expect("approval id")
            .to_owned()
    };

    let edit = request("file_write", &"c".repeat(64));
    assert_error_code(
        &run_cli(&data_root, &["approvals", "approve", &edit]),
        "approval",
    );
    let output = run_cli(&data_root, &["approvals", "show", &edit]);
    assert_success(&output);
    assert_eq!(json_output(&output)["approval"]["state"], "pending");
    assert_eq!(json_output(&output)["approval"]["writers_stopped"], false);

    let output = run_cli(
        &data_root,
        &["approvals", "approve", &edit, "--writers-stopped"],
    );
    assert_success(&output);
    assert_eq!(json_output(&output)["decision"], "approved");
    assert_eq!(json_output(&output)["approval"]["writers_stopped"], true);
    let output = run_cli(&data_root, &["approvals", "list"]);
    assert_success(&output);
    assert_eq!(
        json_output(&output)["approvals"][0]["writers_stopped"],
        true
    );

    let command = request("command", &"d".repeat(64));
    assert_error_code(
        &run_cli(
            &data_root,
            &["approvals", "approve", &command, "--writers-stopped"],
        ),
        "approval",
    );
    let output = run_cli(&data_root, &["approvals", "show", &command]);
    assert_success(&output);
    assert_eq!(json_output(&output)["approval"]["state"], "pending");
    assert_success(&run_cli(&data_root, &["approvals", "approve", &command]));
    let output = run_cli(&data_root, &["approvals", "show", &command]);
    assert_success(&output);
    assert_eq!(json_output(&output)["approval"]["writers_stopped"], false);
}

/// The trusted CLI chooses the connection a desktop shell without a connection
/// argument uses, among local profiles with an endpoint (ADR-0064).
#[test]
fn provider_profile_select_records_and_clears_the_active_connection() {
    let temporary = TempDir::new().expect("temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    fs::create_dir_all(&development_root).expect("development root");
    let mut revision = set_root(&data_root, &development_root)["revision"]
        .as_u64()
        .expect("revision");
    for (name, kind, auth, endpoint) in [
        (
            "ollama",
            "ollama_local",
            "none",
            Some("http://127.0.0.1:11434/v1"),
        ),
        ("cloud", "openai", "api_key", None),
    ] {
        let revision_text = revision.to_string();
        let mut args = vec![
            "workspace",
            "provider-profile-add",
            name,
            "--kind",
            kind,
            "--auth",
            auth,
            "--revision",
            &revision_text,
        ];
        if let Some(endpoint) = endpoint {
            args.extend(["--endpoint", endpoint]);
        }
        let output = run_cli(&data_root, &args);
        assert_success(&output);
        revision = json_output(&output)["snapshot"]["revision"]
            .as_u64()
            .expect("revision after add");
    }

    // A profile without a local adapter and endpoint cannot be chosen.
    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "provider-profile-select",
            "cloud",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_control_error(&output);
    // A name and --default together are a usage error.
    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "provider-profile-select",
            "ollama",
            "--default",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert!(!output.status.success());

    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "provider-profile-select",
            "OLLAMA",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_success(&output);
    let selected = json_output(&output);
    let ollama_id = selected["snapshot"]["provider_profiles"]
        .as_array()
        .expect("profiles")
        .iter()
        .find(|status| status["name"] == "ollama")
        .expect("ollama profile")["id"]
        .clone();
    assert_eq!(selected["snapshot"]["active_provider"], ollama_id);
    revision = selected["snapshot"]["revision"].as_u64().expect("revision");

    let output = run_cli(
        &data_root,
        &[
            "workspace",
            "provider-profile-select",
            "--default",
            "--revision",
            &revision.to_string(),
        ],
    );
    assert_success(&output);
    assert!(
        json_output(&output)["snapshot"]
            .get("active_provider")
            .is_none()
    );
}

/// Unregistering removes the registration and its settings, deletes the saved
/// desktop instructions file, leaves the folder alone, refuses stale or
/// unknown requests, and lets the same folder register again as a new
/// workspace (ADR-0081).
#[test]
fn unregister_removes_a_workspace_and_its_instructions_and_leaves_the_folder() {
    // Given two registered workspaces, one with saved desktop instructions.
    let temporary = TempDir::new().expect("private temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    let first = development_root.join("first");
    let second = development_root.join("second");
    fs::create_dir_all(&first).expect("create first workspace");
    fs::create_dir_all(&second).expect("create second workspace");
    fs::write(first.join("keep.txt"), b"kept").expect("workspace file");
    set_root(&data_root, &development_root);
    let first_id = register(&data_root, &first, 1);
    let second_id = register(&data_root, &second, 2);
    let instructions = data_root.join("desktop-v1").join("workspace-instructions");
    fs::create_dir_all(&instructions).expect("instructions folder");
    let file = |id: &str| instructions.join(format!("{id}.json"));
    for id in [&first_id, &second_id] {
        fs::write(file(id), br#"{"version":1,"instructions":"Use tabs."}"#)
            .expect("saved instructions");
    }

    // When the operator unregisters with a stale revision, an unknown id, then correctly.
    let stale = run_cli(
        &data_root,
        &["workspace", "unregister", &first_id, "--revision", "2"],
    );
    assert_control_error(&stale);
    let unknown = run_cli(
        &data_root,
        &[
            "workspace",
            "unregister",
            &Uuid::new_v4().to_string(),
            "--revision",
            "3",
        ],
    );
    assert_control_error(&unknown);
    assert!(file(&first_id).exists(), "a refusal keeps the file");
    let removed = run_cli(
        &data_root,
        &["workspace", "unregister", &first_id, "--revision", "3"],
    );
    assert_success(&removed);
    let removed = json_output(&removed);

    // Then only that workspace and its file are gone, and the folder is untouched.
    assert_eq!(removed["unregistered_workspace_id"], first_id.as_str());
    assert_eq!(removed["snapshot"]["revision"], 4);
    let listed = removed["snapshot"]["workspaces"]
        .as_array()
        .expect("workspaces");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], second_id.as_str());
    assert!(!file(&first_id).exists());
    assert!(file(&second_id).exists());
    assert_eq!(fs::read(first.join("keep.txt")).expect("kept"), b"kept");

    // And the same folder registers again under a new identifier.
    let again = register(&data_root, &first, 4);
    assert_ne!(again, first_id);
    let unknown_now = run_cli(
        &data_root,
        &["workspace", "unregister", &first_id, "--revision", "5"],
    );
    assert_control_error(&unknown_now);
}

/// The `web_fetch` preference and its host allowlist are revisioned,
/// validated, listed, reported by status, and dropped by unregistering.
#[test]
fn web_fetch_preference_and_hosts_are_revisioned_and_validated() {
    // Given two registered workspaces.
    let temporary = TempDir::new().expect("private temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    let first = development_root.join("first");
    let second = development_root.join("second");
    fs::create_dir_all(&first).expect("create first workspace");
    fs::create_dir_all(&second).expect("create second workspace");
    set_root(&data_root, &development_root);
    let first_id = register(&data_root, &first, 1);
    let second_id = register(&data_root, &second, 2);

    // When the preference is set, only the three state words are accepted.
    for word in ["true", "false", "ask", "Allow", ""] {
        let refused = run_cli(
            &data_root,
            &[
                "workspace",
                "permissions",
                &first_id,
                "--revision",
                "3",
                "--web-fetch",
                word,
            ],
        );
        assert!(!refused.status.success(), "{word:?} must be refused");
    }
    let set = run_cli(
        &data_root,
        &[
            "workspace",
            "permissions",
            &first_id,
            "--revision",
            "3",
            "--web-fetch",
            "allow",
        ],
    );
    assert_success(&set);
    let set = json_output(&set);
    assert_eq!(set["revision"], 4);
    let status_of = |snapshot: &Value, id: &str| -> Value {
        snapshot["workspaces"]
            .as_array()
            .expect("workspace registrations")
            .iter()
            .find(|workspace| workspace["id"] == id)
            .expect("workspace registration")["permissions"]
            .clone()
    };
    assert_eq!(status_of(&set, &first_id)["web_fetch"], "allow");
    assert_eq!(status_of(&set, &second_id)["web_fetch"], "deny");

    // And hosts are added, normalised, listed, and refused when invalid.
    let added = run_cli(
        &data_root,
        &[
            "workspace",
            "web-host-add",
            &first_id,
            "Docs.Example.com.",
            "--revision",
            "4",
        ],
    );
    assert_success(&added);
    let added = json_output(&added);
    assert_eq!(added["pattern"], "docs.example.com");
    assert_eq!(added["snapshot"]["revision"], 5);
    let wildcard = run_cli(
        &data_root,
        &[
            "workspace",
            "web-host-add",
            &first_id,
            "*.rust-lang.org",
            "--revision",
            "5",
        ],
    );
    assert_success(&wildcard);
    for (pattern, revision) in [
        ("localhost", "6"),
        ("127.0.0.1", "6"),
        ("*.com", "6"),
        ("docs.example.com", "6"),
        ("docs.example.com", "5"),
    ] {
        let refused = run_cli(
            &data_root,
            &[
                "workspace",
                "web-host-add",
                &first_id,
                pattern,
                "--revision",
                revision,
            ],
        );
        assert_control_error(&refused);
    }
    let unknown = run_cli(
        &data_root,
        &[
            "workspace",
            "web-host-add",
            &Uuid::new_v4().to_string(),
            "example.com",
            "--revision",
            "6",
        ],
    );
    assert_control_error(&unknown);
    let listed = run_cli(&data_root, &["workspace", "web-host-list", &first_id]);
    assert_success(&listed);
    let listed = json_output(&listed);
    assert_eq!(listed["web_fetch"], "allow");
    assert_eq!(
        listed["web_fetch_hosts"],
        json!(["docs.example.com", "*.rust-lang.org"])
    );
    assert_eq!(listed["revision"], 6);
    let status = run_cli(&data_root, &["workspace", "status"]);
    assert_success(&status);
    let status = json_output(&status);
    assert_eq!(
        status_of(&status, &first_id)["web_fetch_hosts"],
        json!(["docs.example.com", "*.rust-lang.org"])
    );
    assert_eq!(status_of(&status, &second_id)["web_fetch_hosts"], json!([]));

    // When another preference changes, the stored state is kept.
    let model_tools = run_cli(
        &data_root,
        &[
            "workspace",
            "permissions",
            &first_id,
            "--revision",
            "6",
            "--model-tools",
            "true",
        ],
    );
    assert_success(&model_tools);
    assert_eq!(
        status_of(&json_output(&model_tools), &first_id)["web_fetch"],
        "allow"
    );

    // And removal is exact: an absent pattern is refused, a listed one leaves.
    let absent = run_cli(
        &data_root,
        &[
            "workspace",
            "web-host-remove",
            &first_id,
            "*.example.com",
            "--revision",
            "7",
        ],
    );
    assert_control_error(&absent);
    let removed = run_cli(
        &data_root,
        &[
            "workspace",
            "web-host-remove",
            &first_id,
            "DOCS.example.com",
            "--revision",
            "7",
        ],
    );
    assert_success(&removed);
    let removed = json_output(&removed);
    assert_eq!(removed["removed_pattern"], "docs.example.com");
    let listed = json_output(&run_cli(
        &data_root,
        &["workspace", "web-host-list", &first_id],
    ));
    assert_eq!(listed["web_fetch_hosts"], json!(["*.rust-lang.org"]));

    // Then unregistering drops the allowlist with the registration.
    let gone = run_cli(
        &data_root,
        &["workspace", "unregister", &first_id, "--revision", "8"],
    );
    assert_success(&gone);
    let missing = run_cli(&data_root, &["workspace", "web-host-list", &first_id]);
    assert_control_error(&missing);
}

/// Unregistering revokes every container grant of the workspace first, then
/// removes the registration; the revocation needs no extra revision
/// (ADR-0107, revising ADR-0081's refusal).
#[cfg(windows)]
#[test]
fn unregister_revokes_the_workspace_grants_first() {
    // Given a workspace with a read-write container grant.
    let temporary = TempDir::new().expect("private temporary directory");
    let data_root = temporary.path().join("application-data");
    let development_root = temporary.path().join("development");
    let workspace = development_root.join("app");
    fs::create_dir_all(&workspace).expect("create workspace");
    set_root(&data_root, &development_root);
    let _cleanup = RevokeGrantsOnDrop(&data_root);
    let id = register(&data_root, &workspace, 1);
    let grant = run_cli(
        &data_root,
        &[
            "workspace",
            "container-grant",
            &id,
            path_arg(&workspace),
            "--access",
            "read-write",
            "--revision",
            "2",
        ],
    );
    assert_success(&grant);
    let grant_id = json_output(&grant)["grant_id"]
        .as_str()
        .expect("grant ID")
        .to_owned();

    // When unregistering with a stale revision, nothing is revoked.
    let stale = run_cli(
        &data_root,
        &["workspace", "unregister", &id, "--revision", "2"],
    );
    assert_control_error(&stale);
    let status = json_output(&run_cli(&data_root, &["workspace", "status"]));
    assert_eq!(status["revision"], 3);
    assert_eq!(
        status["container_grants"].as_array().expect("grants").len(),
        1
    );

    // Then the current revision revokes the grant, deleting the container's
    // profile, and removes the workspace.
    let removed = run_cli_for(
        &data_root,
        &["workspace", "unregister", &id, "--revision", "3"],
        GRANT_CLI_DEADLINE,
    );
    assert_success(&removed);
    let removed = json_output(&removed);
    assert_eq!(
        removed["revoked"],
        json!([{"grant_id": grant_id, "profile_deleted": true}]),
        "{removed}"
    );
    assert_eq!(removed["snapshot"]["revision"], 4);
    assert_eq!(removed["snapshot"]["workspaces"], json!([]));
    assert!(removed["snapshot"].get("container_grants").is_none());
}
