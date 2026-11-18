//! Registry credentials for the trusted package fetch, through the real CLI,
//! the real native credential store, and a loopback registry fixture. On
//! Linux these need an unlocked Secret Service default collection and fail
//! without one.
#![cfg(any(target_os = "linux", windows))]

mod support;

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use omgcode_core::control::{CredentialError, CredentialId, CredentialStore};
use serde_json::Value;
use support::{
    CLI_DEADLINE, assert_control_error, assert_error_code, assert_success, cli_command,
    json_output, path_arg, run_cli, run_with_deadline,
};
use tempfile::TempDir;

const PROFILE: &str = "registries";
const SENTINEL: &str = "reg-omgcode-cli-sentinel-3d7a90";
const ROTATED: &str = "alice:reg-omgcode-rotated-sentinel-5b21";
const BODY: &[u8] = b"fetched-package-bytes";

/// A one-connection loopback registry that records the request head.
struct Fixture {
    address: SocketAddr,
    task: JoinHandle<String>,
}

impl Fixture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        listener.set_nonblocking(true).expect("nonblocking");
        let address = listener.local_addr().expect("fixture address");
        let task = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "fixture accept timed out");
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                }
            };
            stream.set_nonblocking(false).expect("blocking stream");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("read timeout");
            let head = read_head(&mut stream);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                BODY.len()
            );
            stream.write_all(response.as_bytes()).expect("headers");
            stream.write_all(BODY).expect("body");
            head
        });
        Self { address, task }
    }

    fn origin(&self) -> String {
        format!("http://{}/fixtures/", self.address)
    }

    fn finish(self) -> String {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !self.task.is_finished() {
            assert!(Instant::now() < deadline, "fixture finished");
            thread::sleep(Duration::from_millis(2));
        }
        self.task.join().expect("fixture joined")
    }
}

fn read_head(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 1024];
    while bytes.len() < 16 * 1024 {
        let read = stream.read(&mut buffer).expect("read request");
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(bytes).expect("request head is UTF-8")
}

struct Scene {
    temporary: TempDir,
    data: PathBuf,
    workspace_id: String,
}

fn revision(data: &Path) -> u64 {
    let output = run_cli(data, &["workspace", "status"]);
    assert_success(&output);
    json_output(&output)["revision"].as_u64().expect("revision")
}

/// A trusted data root with a registered `task_network` workspace and a
/// package profile with the fixture origin and a second, unbound origin.
fn scene(origin: &str) -> Scene {
    let temporary = TempDir::new().expect("temporary directory");
    let data = temporary.path().join("application-data");
    #[cfg(windows)]
    support::workspace_fixture::restrict_to_current_user(&data);
    let root = temporary.path().join("dev");
    let workspace = root.join("work");
    fs::create_dir_all(&workspace).expect("workspace");
    assert_success(&run_cli(
        &data,
        &["workspace", "set-root", path_arg(&root), "--revision", "0"],
    ));
    let registered = run_cli(
        &data,
        &[
            "workspace",
            "register",
            path_arg(&workspace),
            "--revision",
            &revision(&data).to_string(),
        ],
    );
    assert_success(&registered);
    let workspace_id = json_output(&registered)["registration_id"]
        .as_str()
        .expect("workspace id")
        .to_owned();
    assert_success(&run_cli(
        &data,
        &[
            "workspace",
            "permissions",
            &workspace_id,
            "--task-network",
            "true",
            "--revision",
            &revision(&data).to_string(),
        ],
    ));
    assert_success(&run_cli(
        &data,
        &[
            "workspace",
            "package-profile-add",
            PROFILE,
            "--registry",
            origin,
            "--registry",
            "https://cdn.example.com/files/",
            "--revision",
            &revision(&data).to_string(),
        ],
    ));
    #[cfg(windows)]
    {
        let cache_root = temporary.path().join("package-cache-root");
        fs::create_dir(&cache_root).expect("cache root");
        assert_success(&run_cli(
            &data,
            &[
                "workspace",
                "set-package-cache",
                &workspace_id,
                path_arg(&cache_root),
                "--revision",
                &revision(&data).to_string(),
            ],
        ));
    }
    Scene {
        temporary,
        data,
        workspace_id,
    }
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

fn bind(scene: &Scene, origin: &str, auth: &str, input: &[u8]) -> Output {
    let revision = revision(&scene.data).to_string();
    run_with_stdin(
        scene,
        &[
            "workspace",
            "package-credential-set",
            PROFILE,
            "--origin",
            origin,
            "--auth",
            auth,
            "--revision",
            &revision,
        ],
        input,
    )
}

fn credential_id(output: &Value) -> CredentialId {
    CredentialId::parse(
        output["package_credential"]["credential_id"]
            .as_str()
            .expect("credential id"),
    )
    .expect("valid credential id")
}

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
    for secret in [SENTINEL, ROTATED, "reg-omgcode-rotated"] {
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
                for secret in [SENTINEL, "reg-omgcode-rotated"] {
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
    assert!(files > 0, "the scan found no files");
}

fn fetch(scene: &Scene, url: &str, name: &str) -> Output {
    run_cli(
        &scene.data,
        &[
            "packages",
            "fetch",
            &scene.workspace_id,
            "--profile",
            PROFILE,
            "--url",
            url,
            "--name",
            name,
        ],
    )
}

/// Bind, show, fetch with the header, rotate, and remove, with no output or
/// file ever holding the secret.
#[test]
fn a_bound_credential_is_shown_sent_rotated_and_removed_without_disclosure() {
    let fixture = Fixture::start();
    let scene = scene(&fixture.origin());
    let data = &scene.data;
    let origin = fixture.origin();

    // The secret is read from piped input, one line ending removed.
    let output = bind(
        &scene,
        &origin,
        "bearer",
        format!("{SENTINEL}\r\n").as_bytes(),
    );
    assert_success(&output);
    assert_no_secret(&output);
    let first = json_output(&output);
    let first_id = credential_id(&first);
    assert_eq!(first["package_credential"]["auth"], "bearer");
    assert_eq!(first["package_credential"]["profile"], PROFILE);
    assert_eq!(
        first["package_credential"]["backend"],
        native(data).backend().as_str()
    );
    assert_eq!(
        first["package_credential"]["replaced_credential_id"],
        Value::Null
    );
    assert_eq!(
        native(data).load(&first_id).expect("stored").expose(),
        SENTINEL.as_bytes()
    );

    // Show lists the bound origin, auth, identifier, and backend only.
    let output = run_cli(data, &["workspace", "package-profile-show", PROFILE]);
    assert_success(&output);
    assert_no_secret(&output);
    let shown = json_output(&output);
    let credentials = shown["package_profile"]["credentials"]
        .as_array()
        .expect("credentials");
    assert_eq!(credentials.len(), 1);
    assert_eq!(credentials[0]["auth"], "bearer");
    assert_eq!(
        credentials[0]["binding"]["credential_id"],
        first_id.as_str()
    );
    assert!(
        credentials[0]["origin"]
            .as_str()
            .expect("origin")
            .starts_with("http://127.0.0.1:")
    );

    // A fetch sends exactly the bearer header and reports only the identifier.
    let url = format!("{origin}pkg.bin");
    let output = fetch(&scene, &url, "pkg.bin");
    assert_success(&output);
    assert_no_secret(&output);
    let report = json_output(&output)["fetch"].clone();
    assert_eq!(report["credential_id"], first_id.as_str());
    assert_eq!(report["bytes"], BODY.len());
    let head = fixture.finish();
    assert!(
        head.lines()
            .any(|line| line.eq_ignore_ascii_case(&format!("authorization: Bearer {SENTINEL}"))),
        "{head}"
    );
    let audit = run_cli(data, &["workspace", "audit", "--limit", "100"]);
    assert_success(&audit);
    assert_no_secret(&audit);
    assert!(
        String::from_utf8_lossy(&audit.stdout).contains(first_id.as_str()),
        "the fetch start names the credential"
    );

    // Rotation binds a new identifier (now `basic`) and deletes the old item.
    let output = bind(&scene, &origin, "basic", format!("{ROTATED}\n").as_bytes());
    assert_success(&output);
    assert_no_secret(&output);
    let second = json_output(&output);
    let second_id = credential_id(&second);
    assert_ne!(second_id, first_id);
    assert_eq!(
        second["package_credential"]["replaced_credential_id"],
        first_id.as_str()
    );
    assert_absent(data, &first_id);
    assert_eq!(
        native(data).load(&second_id).expect("rotated").expose(),
        ROTATED.as_bytes()
    );
    assert_no_secret_in_files(data);

    // Removal unbinds then deletes the native item.
    let current = revision(data).to_string();
    let output = run_cli(
        data,
        &[
            "workspace",
            "package-credential-remove",
            PROFILE,
            "--origin",
            &origin,
            "--revision",
            &current,
        ],
    );
    assert_success(&output);
    assert_no_secret(&output);
    assert_eq!(
        json_output(&output)["removed_package_credential"]["credential_id"],
        second_id.as_str()
    );
    assert_absent(data, &second_id);
    let current = revision(data).to_string();
    let output = run_cli(
        data,
        &[
            "workspace",
            "package-credential-remove",
            PROFILE,
            "--origin",
            &origin,
            "--revision",
            &current,
        ],
    );
    assert_control_error(&output);
    let output = run_cli(data, &["workspace", "package-profile-show", PROFILE]);
    assert!(
        json_output(&output)["package_profile"]
            .get("credentials")
            .is_none()
    );
    assert_no_secret_in_files(data);
}

/// Refused input records nothing and stores nothing.
#[test]
fn refused_registry_credential_input_records_nothing() {
    let scene = scene("http://127.0.0.1:9/fixtures/");
    let data = &scene.data;
    let origin = "http://127.0.0.1:9/fixtures/";
    let before = revision(data);

    // Empty or oversized input is refused as credential input.
    for input in [&b""[..], b"\n"] {
        assert_error_code(&bind(&scene, origin, "bearer", input), "credential_input");
    }
    let oversized = vec![b'k'; 16 * 1024 + 1];
    assert_error_code(
        &bind(&scene, origin, "bearer", &oversized),
        "credential_input",
    );
    // Invalid content is a control refusal, whatever the scheme.
    for (auth, input) in [
        ("bearer", format!("{SENTINEL} extra")),
        ("bearer", format!(" {SENTINEL}")),
        ("bearer", format!("{SENTINEL}\u{7}")),
        ("bearer", format!("{SENTINEL}\tx")),
        ("bearer", "tok\u{e9}n-sentinel".to_owned()),
        ("basic", SENTINEL.to_owned()),
        ("basic", format!(":{SENTINEL}")),
    ] {
        let output = bind(&scene, origin, auth, input.as_bytes());
        assert_control_error(&output);
        assert_no_secret(&output);
    }
    // A bad scheme, an undeclared origin, an unknown profile, a stale revision.
    assert_error_code(
        &bind(&scene, origin, "token", SENTINEL.as_bytes()),
        "configuration",
    );
    assert_control_error(&bind(
        &scene,
        "https://other.example.com/",
        "bearer",
        SENTINEL.as_bytes(),
    ));
    assert_control_error(&bind(
        &scene,
        "http://127.0.0.1:9/other/",
        "bearer",
        SENTINEL.as_bytes(),
    ));
    let output = run_with_stdin(
        &scene,
        &[
            "workspace",
            "package-credential-set",
            "missing",
            "--origin",
            origin,
            "--auth",
            "bearer",
            "--revision",
            &revision(data).to_string(),
        ],
        SENTINEL.as_bytes(),
    );
    assert_control_error(&output);
    let output = run_with_stdin(
        &scene,
        &[
            "workspace",
            "package-credential-set",
            PROFILE,
            "--origin",
            origin,
            "--auth",
            "bearer",
            "--revision",
            &(before - 1).to_string(),
        ],
        SENTINEL.as_bytes(),
    );
    assert_control_error(&output);
    assert_no_secret(&output);

    assert_eq!(revision(data), before, "no refused command was recorded");
    #[cfg(windows)]
    assert!(
        !data.join("credentials-v1").exists(),
        "no native credential was stored"
    );
    assert_no_secret_in_files(data);
}

/// A fetch to an origin without a binding sends no header, even when another
/// origin of the profile has one.
#[test]
fn a_fetch_to_an_unbound_origin_sends_no_authorization() {
    let fixture = Fixture::start();
    let scene = scene(&fixture.origin());
    // Bind the profile's other origin; the fixture origin stays unbound.
    let output = bind(
        &scene,
        "https://cdn.example.com/files/",
        "bearer",
        SENTINEL.as_bytes(),
    );
    assert_success(&output);
    let output = fetch(&scene, &format!("{}pkg.bin", fixture.origin()), "pkg.bin");
    assert_success(&output);
    assert!(json_output(&output)["fetch"].get("credential_id").is_none());
    let head = fixture.finish();
    assert!(
        !head.to_ascii_lowercase().contains("authorization:"),
        "{head}"
    );
    assert_no_secret(&output);
    assert_no_secret_in_files(&scene.data);
}

/// Removing a profile deletes every credential bound to its origins.
#[test]
fn removing_a_package_profile_deletes_its_credentials() {
    let scene = scene("http://127.0.0.1:9/fixtures/");
    let data = &scene.data;
    let mut ids = Vec::new();
    for origin in [
        "http://127.0.0.1:9/fixtures/",
        "https://cdn.example.com/files/",
    ] {
        let output = bind(&scene, origin, "bearer", SENTINEL.as_bytes());
        assert_success(&output);
        ids.push(credential_id(&json_output(&output)));
    }
    let current = revision(data).to_string();
    let output = run_cli(
        data,
        &[
            "workspace",
            "package-profile-remove",
            PROFILE,
            "--revision",
            &current,
        ],
    );
    assert_success(&output);
    assert_no_secret(&output);
    let removed = json_output(&output);
    assert!(removed.get("warning").is_none(), "{removed}");
    let listed = removed["removed_credential_ids"]
        .as_array()
        .expect("removed ids");
    assert_eq!(listed.len(), 2);
    for id in &ids {
        assert!(listed.iter().any(|value| value == id.as_str()));
        assert_absent(data, id);
    }
    assert_control_error(&run_cli(
        data,
        &["workspace", "package-profile-show", PROFILE],
    ));
    assert_no_secret_in_files(data);
}

/// Capabilities report that registry credentials are available with the
/// native store.
#[test]
fn capabilities_report_registry_credentials() {
    let temporary = TempDir::new().expect("temporary directory");
    let output = run_cli(&temporary.path().join("unused"), &["capabilities"]);
    assert_success(&output);
    assert_eq!(
        json_output(&output)["package_registry_credentials"],
        cfg!(any(windows, target_os = "linux"))
    );
}
