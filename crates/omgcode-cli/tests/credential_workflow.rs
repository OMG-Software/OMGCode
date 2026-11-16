//! Provider credentials through the real CLI and the real native store. On
//! Linux these need an unlocked Secret Service default collection and fail
//! without one.

mod support;

use omgcode_core::control::{CredentialError, CredentialId, CredentialStore};
use serde_json::Value;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use tempfile::TempDir;

use support::{
    CLI_DEADLINE, assert_control_error, assert_error_code, assert_success, cli_command,
    json_output, run_cli, run_with_deadline,
};

const SENTINEL: &str = "sk-omgcode-cli-sentinel-91c4f0e2";
const ROTATED: &str = "sk-omgcode-cli-rotated-5be713aa";

struct Scene {
    temporary: TempDir,
    data: PathBuf,
}

/// A trusted data root with two `api_key` profiles and a subscription profile.
fn scene() -> Scene {
    let temporary = TempDir::new().expect("temporary directory");
    let data = temporary.path().join("application-data");
    #[cfg(windows)]
    support::workspace_fixture::restrict_to_current_user(&data);
    let status = run_cli(&data, &["workspace", "status"]);
    assert_success(&status);
    for (name, kind, auth) in [
        ("keys", "openai", "api_key"),
        ("router", "open_router", "api_key"),
        ("plan", "openai", "subscription"),
    ] {
        let revision = revision(&data);
        let output = run_cli(
            &data,
            &[
                "workspace",
                "provider-profile-add",
                name,
                "--kind",
                kind,
                "--auth",
                auth,
                "--revision",
                &revision.to_string(),
            ],
        );
        assert_success(&output);
    }
    Scene { temporary, data }
}

fn revision(data: &Path) -> u64 {
    let output = run_cli(data, &["workspace", "status"]);
    assert_success(&output);
    json_output(&output)["revision"].as_u64().expect("revision")
}

/// Run the CLI with `input` as its standard input, from a file outside the
/// data root so the data-root scan sees only what the CLI wrote.
fn run_with_stdin(scene: &Scene, args: &[&str], input: &[u8]) -> Output {
    let path = scene.temporary.path().join("stdin-input");
    fs::write(&path, input).expect("stdin fixture");
    let mut command = cli_command(&scene.data, args);
    command.stdin(Stdio::from(File::open(&path).expect("open stdin fixture")));
    let result = run_with_deadline(command, CLI_DEADLINE).expect("run CLI");
    assert!(!result.timed_out, "CLI timed out");
    fs::remove_file(&path).expect("remove stdin fixture");
    result.output
}

fn set(scene: &Scene, profile: &str, input: &[u8]) -> Output {
    let revision = revision(&scene.data).to_string();
    run_with_stdin(
        scene,
        &[
            "workspace",
            "provider-credential-set",
            profile,
            "--revision",
            &revision,
        ],
        input,
    )
}

fn credential_id(output: &Value) -> CredentialId {
    CredentialId::parse(
        output["provider_credential"]["credential_id"]
            .as_str()
            .expect("credential id"),
    )
    .expect("valid credential id")
}

/// The test process reads the native store directly to prove what was stored.
fn native(data: &Path) -> CredentialStore {
    CredentialStore::open(data).expect("native credential store")
}

fn assert_absent(data: &Path, id: &CredentialId) {
    assert!(
        matches!(native(data).load(id), Err(CredentialError::NotFound)),
        "credential {id} must be deleted"
    );
}

fn assert_no_secret(output: &Output) {
    for secret in [SENTINEL, ROTATED] {
        for (name, stream) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
            assert!(
                !String::from_utf8_lossy(stream).contains(secret),
                "{name} contains a secret"
            );
        }
    }
}

/// Scan every file under the data root, including journals and credential
/// files, for the plaintext secrets.
fn assert_no_secret_in_files(root: &Path) {
    let mut pending = vec![root.to_path_buf()];
    let mut files = 0;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read data directory") {
            let entry = entry.expect("directory entry");
            let kind = entry.file_type().expect("file type");
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                files += 1;
                let bytes = fs::read(entry.path()).expect("read data file");
                for secret in [SENTINEL, ROTATED] {
                    assert!(
                        !bytes
                            .windows(secret.len())
                            .any(|window| window == secret.as_bytes()),
                        "{} contains a secret",
                        entry.path().display()
                    );
                }
            }
        }
    }
    assert!(files > 0, "the scan must find the control journal");
}

/// Set, show, rotate, and remove a credential, with the secret only on stdin
/// and never in any output or file.
#[test]
fn credentials_are_set_shown_rotated_and_removed_without_disclosure() {
    let scene = scene();
    let data = &scene.data;

    // Set reads stdin, strips exactly one CRLF, and stores the exact bytes.
    let output = set(&scene, "keys", format!("{SENTINEL}\r\n").as_bytes());
    assert_success(&output);
    assert_no_secret(&output);
    let first = json_output(&output);
    let first_id = credential_id(&first);
    let backend = first["provider_credential"]["backend"]
        .as_str()
        .expect("backend")
        .to_owned();
    assert_eq!(backend, native(data).backend().as_str());
    assert_eq!(first["provider_credential"]["profile"], "keys");
    assert_eq!(
        first["provider_credential"]["replaced_credential_id"],
        Value::Null
    );
    assert!(first.get("warning").is_none());
    assert_eq!(
        native(data).load(&first_id).expect("stored").expose(),
        SENTINEL.as_bytes()
    );

    // Show and status report the binding and backend, never the secret.
    let output = run_cli(data, &["workspace", "provider-profile-show", "KEYS"]);
    assert_success(&output);
    assert_no_secret(&output);
    let shown = json_output(&output);
    let credential = &shown["provider_profile"]["credential"];
    assert_eq!(credential["state"], "bound");
    assert_eq!(credential["id"], first_id.as_str());
    assert_eq!(credential["backend"], backend);
    let output = run_cli(data, &["workspace", "status"]);
    assert_success(&output);
    assert_no_secret(&output);
    let status = json_output(&output);
    assert_eq!(
        status["provider_profiles"][1]["credential"],
        serde_json::json!({"state": "none"})
    );

    // Rotation binds a new identifier and deletes the old native item.
    let output = set(&scene, "keys", format!("{ROTATED}\n").as_bytes());
    assert_success(&output);
    assert_no_secret(&output);
    let second = json_output(&output);
    let second_id = credential_id(&second);
    assert_ne!(second_id, first_id);
    assert_eq!(
        second["provider_credential"]["replaced_credential_id"],
        first_id.as_str()
    );
    assert_absent(data, &first_id);
    assert_eq!(
        native(data).load(&second_id).expect("rotated").expose(),
        ROTATED.as_bytes()
    );

    // Remove records the unbinding, then deletes the native item.
    let current = revision(data).to_string();
    let output = run_cli(
        data,
        &[
            "workspace",
            "provider-credential-remove",
            "keys",
            "--revision",
            &current,
        ],
    );
    assert_success(&output);
    assert_no_secret(&output);
    let removed = json_output(&output);
    assert_eq!(
        removed["removed_provider_credential"]["credential_id"],
        second_id.as_str()
    );
    assert!(removed.get("warning").is_none());
    assert_absent(data, &second_id);
    let output = run_cli(data, &["workspace", "provider-profile-show", "keys"]);
    assert_success(&output);
    assert_eq!(
        json_output(&output)["provider_profile"]["credential"]["state"],
        "none"
    );

    // A second removal is refused by the journal.
    let current = revision(data).to_string();
    let output = run_cli(
        data,
        &[
            "workspace",
            "provider-credential-remove",
            "keys",
            "--revision",
            &current,
        ],
    );
    assert_control_error(&output);

    // The audit view of the journal names identifiers, never secrets.
    let output = run_cli(data, &["workspace", "audit", "--limit", "100"]);
    assert_success(&output);
    assert_no_secret(&output);
    let audit = String::from_utf8(output.stdout).expect("audit text");
    assert!(audit.contains(first_id.as_str()));
    assert_no_secret_in_files(data);
}

/// Empty or oversized input, a stale revision, and non-API-key profiles are
/// refused, and nothing is recorded.
#[test]
fn refused_credential_input_records_nothing() {
    let scene = scene();
    let data = &scene.data;
    let before = revision(data);

    for input in [&b""[..], b"\n", b"\r\n"] {
        let output = set(&scene, "keys", input);
        assert_error_code(&output, "credential_input");
    }
    let oversized = vec![b'k'; 16 * 1024 + 1];
    let output = set(&scene, "keys", &oversized);
    assert_error_code(&output, "credential_input");

    let output = set(&scene, "plan", SENTINEL.as_bytes());
    assert_control_error(&output);
    assert_no_secret(&output);
    let error: Value = serde_json::from_slice(&output.stderr).expect("error JSON");
    assert!(
        error["error"]["message"]
            .as_str()
            .expect("message")
            .contains("subscription"),
        "{error}"
    );
    let output = set(&scene, "missing", SENTINEL.as_bytes());
    assert_control_error(&output);

    let stale = (before - 1).to_string();
    let output = run_with_stdin(
        &scene,
        &[
            "workspace",
            "provider-credential-set",
            "keys",
            "--revision",
            &stale,
        ],
        SENTINEL.as_bytes(),
    );
    assert_control_error(&output);
    assert_no_secret(&output);

    assert_eq!(revision(data), before, "no refused command was recorded");
    let output = run_cli(data, &["workspace", "provider-profile-show", "keys"]);
    assert_eq!(
        json_output(&output)["provider_profile"]["credential"]["state"],
        "none"
    );
    #[cfg(windows)]
    assert!(
        !data.join("credentials-v1").exists(),
        "no native credential was stored"
    );
    assert_no_secret_in_files(data);
}

/// Removing a profile ends its binding in the same event and then deletes
/// the native item.
#[test]
fn removing_a_profile_deletes_its_credential() {
    let scene = scene();
    let data = &scene.data;
    let output = set(&scene, "router", SENTINEL.as_bytes());
    assert_success(&output);
    let id = credential_id(&json_output(&output));

    let current = revision(data).to_string();
    let output = run_cli(
        data,
        &[
            "workspace",
            "provider-profile-remove",
            "router",
            "--revision",
            &current,
        ],
    );
    assert_success(&output);
    assert_no_secret(&output);
    let removed = json_output(&output);
    assert_eq!(removed["removed_credential_id"], id.as_str());
    assert!(removed.get("warning").is_none());
    assert_absent(data, &id);
    let output = run_cli(data, &["workspace", "provider-profile-show", "router"]);
    assert_control_error(&output);
    assert_no_secret_in_files(data);
}

/// Capabilities report the native backend and that no adapter uses it.
#[test]
fn capabilities_report_the_credential_store() {
    let temporary = TempDir::new().expect("temporary directory");
    let output = run_cli(&temporary.path().join("unused"), &["capabilities"]);
    assert_success(&output);
    let capabilities = json_output(&output);
    let expected = if cfg!(windows) {
        "windows_dpapi"
    } else if cfg!(target_os = "linux") {
        "linux_secret_service"
    } else {
        "unavailable"
    };
    assert_eq!(capabilities["credential_store"]["backend"], expected);
    if cfg!(any(windows, target_os = "linux")) {
        assert_eq!(capabilities["credential_store"]["state"], "available");
    }
    assert_eq!(
        capabilities["provider_credentials"],
        cfg!(any(windows, target_os = "linux"))
    );
    assert_eq!(capabilities["provider_adapters"], false);
}
