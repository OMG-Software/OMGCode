use super::*;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::control::{ControlStore, CredentialId, PackageAuth, PackageProfileDeclaration, Secret};
use crate::policy::{Access, PermissionSet, WorkspaceId};
use crate::test_support::private_data_root;

const PROFILE: &str = "registries";
const MAX_REQUEST_HEADER_BYTES: usize = 16 * 1024;

/// One bounded local HTTP response.
struct Fixture {
    address: SocketAddr,
    task: JoinHandle<(String, usize)>,
}

impl Fixture {
    fn start(status: u16, body: Vec<u8>, headers: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        listener
            .set_nonblocking(true)
            .expect("make fixture accept bounded");
        let address = listener.local_addr().expect("fixture address");
        let headers = headers.to_owned();
        let task = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
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
            stream
                .set_nonblocking(false)
                .expect("make accepted stream blocking");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .expect("write timeout");
            let head = read_head(&mut stream);
            let response = format!(
                "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response headers");
            stream.write_all(&body).expect("write response body");
            (head, body.len())
        });
        Self { address, task }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    fn finish(self) -> String {
        join_bounded(self.task)
    }
}

fn read_head(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 1024];
    while bytes.len() < MAX_REQUEST_HEADER_BYTES {
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

fn join_bounded(task: JoinHandle<(String, usize)>) -> String {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !task.is_finished() {
        assert!(Instant::now() < deadline, "fixture finished");
        thread::sleep(Duration::from_millis(2));
    }
    let (head, _) = task.join().expect("fixture joined");
    head
}

/// A workspace with `task_network`, a package profile, and the control data.
struct Scene {
    _directory: tempfile::TempDir,
    data: PathBuf,
    workspace_id: WorkspaceId,
    cache_root: PathBuf,
}

impl Scene {
    fn new(origins: &[String]) -> Self {
        Self::build(origins, true)
    }

    #[cfg(not(target_os = "linux"))]
    fn unconfigured(origins: &[String]) -> Self {
        Self::build(origins, false)
    }

    fn build(origins: &[String], configure_cache: bool) -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let data = directory.path().join("data");
        let root = directory.path().join("dev");
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        private_data_root(&data);
        let mut store = ControlStore::open(&data).expect("store");
        store.set_development_root(&root, 0).expect("root");
        let workspace_id = store
            .register_workspace(&workspace, store.revision())
            .expect("register")
            .id();
        let permissions = PermissionSet {
            task_network: Access::Prompt,
            ..PermissionSet::default()
        };
        store
            .set_permissions(workspace_id, permissions, store.revision())
            .expect("permissions");
        store
            .add_package_profile(
                &PackageProfileDeclaration {
                    name: PROFILE.into(),
                    origins: origins.to_vec(),
                },
                store.revision(),
            )
            .expect("profile");
        let cache_root = directory.path().join("cache");
        if configure_cache && !cfg!(target_os = "linux") {
            fs::create_dir(&cache_root).expect("cache root");
            store
                .set_package_cache_root(workspace_id, &cache_root, store.revision())
                .expect("cache root");
        }
        Self {
            _directory: directory,
            data,
            workspace_id,
            cache_root,
        }
    }

    fn request(&self, url: String, name: &str, max_bytes: u64) -> FetchRequest {
        FetchRequest {
            workspace_id: self.workspace_id,
            profile: PROFILE.into(),
            url,
            name: name.into(),
            max_bytes,
            timeout: Duration::from_secs(10),
        }
    }

    /// Bind a registry credential to one declared origin through the real
    /// control journal and the real native credential store.
    fn bind(&self, origin: &str, auth: PackageAuth, secret: &str) -> CredentialId {
        let credentials = CredentialStore::open(&self.data)
            .expect("a native credential store is required (on Linux, an unlocked Secret Service)");
        let mut store = ControlStore::open(&self.data).expect("store");
        let revision = store.revision();
        let set = store
            .set_package_credential(
                &credentials,
                PROFILE,
                origin,
                auth,
                &Secret::new(secret.as_bytes().to_vec()).expect("secret"),
                revision,
            )
            .expect("bind credential");
        set.binding.id().clone()
    }

    /// Delete a bound credential's native item, as an operator could outside
    /// OMGCode, leaving the binding in place.
    fn delete_native_item(&self, id: &CredentialId) {
        CredentialStore::open(&self.data)
            .expect("native credential store")
            .delete(id)
            .expect("delete item");
    }

    fn cache_file(&self, name: &str) -> PathBuf {
        if cfg!(target_os = "linux") {
            self.data
                .join("package-cache")
                .join(self.workspace_id.to_string())
                .join(name)
        } else {
            self.cache_root.join(name)
        }
    }

    fn audit_text(&self) -> String {
        let store = ControlStore::open_existing(&self.data)
            .expect("open")
            .expect("store");
        serde_json::to_string(&store.audit(100).expect("audit")).expect("audit JSON")
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// A listener that records whether anything connected to it.
struct Silent {
    listener: TcpListener,
}

impl Silent {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind silent listener");
        listener.set_nonblocking(true).expect("nonblocking");
        Self { listener }
    }

    fn address(&self) -> SocketAddr {
        self.listener.local_addr().expect("address")
    }

    /// Whether no connection is waiting. A connection, even one that sent no
    /// bytes, would have been accepted by the kernel into the backlog.
    fn received_nothing(&self) -> bool {
        matches!(
            self.listener.accept(),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock
        )
    }
}

/// The values of every `Authorization` header in a raw request head.
fn authorization_values(head: &str) -> Vec<String> {
    head.lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("authorization")
                .then(|| value.trim().to_owned())
        })
        .collect()
}

#[test]
fn a_bound_bearer_credential_is_sent_to_its_origin_and_audited_by_identifier() {
    let body = b"bearer-bytes".to_vec();
    let fixture = Fixture::start(200, body.clone(), "");
    let scene = Scene::new(&[format!("http://{}/fixtures/", fixture.address)]);
    let origin = format!("http://{}/fixtures/", fixture.address);
    let id = scene.bind(&origin, PackageAuth::Bearer, "tok-sentinel_9f3.abc");
    let report = fetch(
        &scene.data,
        &scene.request(fixture.url("/fixtures/pkg.bin"), "pkg.bin", 1024),
    )
    .expect("fetch with a credential");
    assert_eq!(report.credential_id.as_deref(), Some(id.as_str()));
    let head = fixture.finish();
    assert_eq!(
        authorization_values(&head),
        vec!["Bearer tok-sentinel_9f3.abc".to_owned()],
        "{head}"
    );
    let audit = scene.audit_text();
    assert!(audit.contains(id.as_str()), "{audit}");
    assert!(!audit.contains("sentinel"), "{audit}");
    let receipt = serde_json::to_string(&report).expect("report JSON");
    assert!(!receipt.contains("sentinel"), "{receipt}");
    assert!(
        !fs::read(scene.data.join("control-v1.jsonl"))
            .expect("journal")
            .windows(8)
            .any(|window| window == b"sentinel")
    );
}

#[test]
fn a_bound_basic_credential_is_sent_as_base64_user_and_password() {
    let fixture = Fixture::start(200, b"basic-bytes".to_vec(), "");
    let scene = Scene::new(&[format!("http://{}/fixtures/", fixture.address)]);
    scene.bind(
        &format!("http://{}/fixtures/", fixture.address),
        PackageAuth::Basic,
        "alice:s3cret",
    );
    fetch(
        &scene.data,
        &scene.request(fixture.url("/fixtures/pkg.bin"), "pkg.bin", 1024),
    )
    .expect("fetch with a credential");
    let head = fixture.finish();
    assert_eq!(
        authorization_values(&head),
        vec!["Basic YWxpY2U6czNjcmV0".to_owned()],
        "{head}"
    );
}

/// A credential belongs to one origin: another origin of the same profile
/// gets no `Authorization` header and no `credential_id` in its audit start.
#[test]
fn an_unbound_origin_of_the_same_profile_sends_no_authorization() {
    let fixture = Fixture::start(200, b"public-bytes".to_vec(), "");
    let scene = Scene::new(&[
        format!("http://{}/private/", fixture.address),
        format!("http://{}/public/", fixture.address),
    ]);
    scene.bind(
        &format!("http://{}/private/", fixture.address),
        PackageAuth::Bearer,
        "tok-sentinel",
    );
    let report = fetch(
        &scene.data,
        &scene.request(fixture.url("/public/pkg.bin"), "pkg.bin", 1024),
    )
    .expect("fetch from the unbound origin");
    assert!(report.credential_id.is_none());
    let head = fixture.finish();
    assert!(authorization_values(&head).is_empty(), "{head}");
    let audit = scene.audit_text();
    let start = audit
        .split("package_fetch_started")
        .nth(1)
        .and_then(|rest| rest.split("package_fetch_finished").next())
        .expect("fetch start record");
    assert!(!start.contains("credential_id"), "{start}");
    assert!(!audit.contains("sentinel"), "{audit}");
}

/// When origins nest, the most specific one decides, so a credential bound to
/// a broad origin is not sent to a path a narrower origin declares.
#[test]
fn the_most_specific_matching_origin_decides_the_credential() {
    let fixture = Fixture::start(200, b"nested-bytes".to_vec(), "");
    let scene = Scene::new(&[
        format!("http://{}/", fixture.address),
        format!("http://{}/public/", fixture.address),
    ]);
    scene.bind(
        &format!("http://{}/", fixture.address),
        PackageAuth::Bearer,
        "tok-sentinel",
    );
    fetch(
        &scene.data,
        &scene.request(fixture.url("/public/pkg.bin"), "pkg.bin", 1024),
    )
    .expect("fetch under the narrower origin");
    let head = fixture.finish();
    assert!(authorization_values(&head).is_empty(), "{head}");
}

/// A binding that cannot be read refuses the fetch. Nothing is sent, nothing
/// falls back to an unauthenticated request, and no start record exists.
#[test]
fn a_bound_credential_that_cannot_be_read_sends_nothing() {
    let silent = Silent::new();
    let scene = Scene::new(&[format!("http://{}/fixtures/", silent.address())]);
    let id = scene.bind(
        &format!("http://{}/fixtures/", silent.address()),
        PackageAuth::Bearer,
        "tok-sentinel",
    );
    scene.delete_native_item(&id);
    let before = scene.audit_text();
    let error = fetch(
        &scene.data,
        &scene.request(
            format!("http://{}/fixtures/pkg.bin", silent.address()),
            "pkg.bin",
            1024,
        ),
    )
    .expect_err("an unreadable credential refuses the fetch");
    assert!(
        matches!(
            error,
            FetchError::Credential(PackageCredentialUseError::Store {
                error: CredentialError::NotFound,
                ..
            })
        ),
        "{error}"
    );
    assert!(error.to_string().contains(id.as_str()), "{error}");
    assert!(!error.to_string().contains("sentinel"), "{error}");
    assert!(silent.received_nothing(), "a request reached the registry");
    assert_eq!(scene.audit_text(), before, "no start record is written");
    assert!(!scene.cache_file("pkg.bin").exists());
}

/// Redirects stay refused for a credentialed request, so the credential
/// cannot follow one to another origin.
#[test]
fn a_redirect_from_a_credentialed_origin_is_not_followed() {
    let target = Silent::new();
    let fixture = Fixture::start(
        302,
        Vec::new(),
        &format!("Location: http://{}/steal\r\n", target.address()),
    );
    let scene = Scene::new(&[format!("http://{}/fixtures/", fixture.address)]);
    scene.bind(
        &format!("http://{}/fixtures/", fixture.address),
        PackageAuth::Bearer,
        "tok-sentinel",
    );
    let error = fetch(
        &scene.data,
        &scene.request(fixture.url("/fixtures/pkg.bin"), "pkg.bin", 1024),
    )
    .expect_err("redirect");
    assert!(matches!(error, FetchError::Status(302)), "{error}");
    let head = fixture.finish();
    assert_eq!(authorization_values(&head).len(), 1, "{head}");
    assert!(
        target.received_nothing(),
        "the redirect target was contacted"
    );
    assert!(!scene.audit_text().contains("sentinel"));
}

/// A rejected credential leaves no secret in the error, the audit, or the
/// receipt, whatever the registry answers.
#[test]
fn a_rejected_credential_is_not_echoed_anywhere() {
    let fixture = Fixture::start(401, b"denied".to_vec(), "");
    let scene = Scene::new(&[format!("http://{}/fixtures/", fixture.address)]);
    let id = scene.bind(
        &format!("http://{}/fixtures/", fixture.address),
        PackageAuth::Basic,
        "alice:pw-sentinel",
    );
    let error = fetch(
        &scene.data,
        &scene.request(fixture.url("/fixtures/pkg.bin"), "pkg.bin", 1024),
    )
    .expect_err("unauthorized");
    assert!(matches!(error, FetchError::Status(401)), "{error}");
    for text in [format!("{error}"), format!("{error:?}"), scene.audit_text()] {
        assert!(!text.contains("sentinel"), "{text}");
        assert!(!text.contains("YWxpY2U6"), "{text}");
    }
    assert!(scene.audit_text().contains(id.as_str()));
    let head = fixture.finish();
    assert_eq!(authorization_values(&head).len(), 1, "{head}");
}

#[test]
fn a_declared_origin_is_fetched_and_audited() {
    let body = b"package-bytes".to_vec();
    let fixture = Fixture::start(200, body.clone(), "");
    let scene = Scene::new(&[format!("http://{}/fixtures/", fixture.address)]);
    let url = fixture.url("/fixtures/pkg.bin");
    let report = fetch(&scene.data, &scene.request(url.clone(), "pkg.bin", 1024))
        .expect("fetch allowed origin");
    assert_eq!(report.bytes, body.len() as u64);
    assert_eq!(report.sha256, hex_digest(&body));
    assert_eq!(report.status, 200);
    assert_eq!(report.profile_name, PROFILE);
    assert_eq!(report.url_sha256, hex_digest(url.as_bytes()));
    assert_eq!(
        fs::read(scene.cache_file("pkg.bin")).expect("cache file"),
        body
    );
    let head = fixture.finish();
    assert!(head.starts_with("GET /fixtures/pkg.bin "), "{head}");

    let audit = scene.audit_text();
    assert!(audit.contains("package_fetch_started"), "{audit}");
    assert!(audit.contains("package_fetch_finished"), "{audit}");
    assert!(
        !audit.contains(&url),
        "the request URL was recorded: {audit}"
    );
    assert!(audit.contains(&report.url_sha256), "{audit}");
    assert!(audit.contains("\"kind\":\"fetched\""), "{audit}");
}

#[test]
fn a_configured_cache_root_receives_the_fetch() {
    let body = b"external-cache".to_vec();
    let fixture = Fixture::start(200, body.clone(), "");
    let scene = Scene::new(&[format!("http://{}/fixtures/", fixture.address)]);
    let cache_root = scene._directory.path().join("external-cache");
    fs::create_dir(&cache_root).expect("cache root");
    let mut store = ControlStore::open(&scene.data).expect("store");
    store
        .set_package_cache_root(scene.workspace_id, &cache_root, store.revision())
        .expect("cache root");
    drop(store);
    let url = fixture.url("/fixtures/pkg.bin");
    let report = fetch(&scene.data, &scene.request(url, "pkg.bin", 1024)).expect("fetch");
    assert_eq!(report.bytes, body.len() as u64);
    let cached = cache_root.join("pkg.bin");
    assert_eq!(fs::read(cached).expect("external cache file"), body);
    assert!(!scene.cache_file("pkg.bin").exists());
    let head = fixture.finish();
    assert!(head.starts_with("GET /fixtures/pkg.bin "), "{head}");
}

#[test]
fn an_undeclared_origin_or_path_is_refused_before_a_request() {
    // A closed loopback port stands in for an origin that was not declared.
    let scene = Scene::new(&["http://127.0.0.1:9/fixtures/".to_owned()]);
    let before = scene.audit_text();
    let error = fetch(
        &scene.data,
        &scene.request(
            "http://127.0.0.1:9/other/pkg.bin".to_owned(),
            "pkg.bin",
            1024,
        ),
    )
    .expect_err("undeclared path");
    assert!(matches!(error, FetchError::Refused(_)), "{error}");
    assert_eq!(scene.audit_text(), before);
    assert!(!scene.cache_file("pkg.bin").exists());

    let error = fetch(
        &scene.data,
        &scene.request(
            "http://127.0.0.1:10/fixtures/pkg.bin".to_owned(),
            "pkg.bin",
            1024,
        ),
    )
    .expect_err("undeclared origin");
    assert!(matches!(error, FetchError::Refused(_)), "{error}");
    assert_eq!(scene.audit_text(), before);
}

#[test]
fn redirects_are_not_followed() {
    let fixture = Fixture::start(302, Vec::new(), "Location: /elsewhere\r\n");
    let scene = Scene::new(&[format!("http://{}/fixtures/", fixture.address)]);
    let url = fixture.url("/fixtures/pkg.bin");
    let error = fetch(&scene.data, &scene.request(url, "pkg.bin", 1024)).expect_err("redirect");
    assert!(matches!(error, FetchError::Status(302)), "{error}");
    assert!(!scene.cache_file("pkg.bin").exists());
    let audit = scene.audit_text();
    assert!(audit.contains("\"kind\":\"failed\""), "{audit}");
    assert!(audit.contains("status"), "{audit}");
}

#[test]
fn the_size_limit_is_enforced_and_leaves_no_cache_file() {
    let fixture = Fixture::start(200, b"too-large".to_vec(), "");
    let scene = Scene::new(&[format!("http://{}/fixtures/", fixture.address)]);
    let url = fixture.url("/fixtures/pkg.bin");
    let error = fetch(&scene.data, &scene.request(url, "pkg.bin", 4)).expect_err("oversize");
    assert!(matches!(error, FetchError::TooLarge(4)), "{error}");
    assert!(!scene.cache_file("pkg.bin").exists());
    let listing: Vec<_> = fs::read_dir(scene.cache_file("pkg.bin").parent().expect("cache"))
        .expect("cache listing")
        .collect();
    assert!(listing.is_empty(), "a partial cache file remained");
}

#[test]
fn an_existing_cache_file_is_not_overwritten() {
    let body = b"first".to_vec();
    let fixture = Fixture::start(200, body.clone(), "");
    let scene = Scene::new(&[format!("http://{}/fixtures/", fixture.address)]);
    let url = fixture.url("/fixtures/pkg.bin");
    fetch(&scene.data, &scene.request(url.clone(), "pkg.bin", 1024)).expect("first fetch");
    let before = scene.audit_text();
    let error =
        fetch(&scene.data, &scene.request(url, "pkg.bin", 1024)).expect_err("existing cache file");
    assert!(
        matches!(&error, FetchError::AlreadyCached(name) if name == "pkg.bin"),
        "{error}"
    );
    assert_eq!(fs::read(scene.cache_file("pkg.bin")).expect("cache"), body);
    assert_eq!(scene.audit_text(), before, "the refused fetch was recorded");
}

#[test]
fn task_network_is_required_and_bad_names_are_refused() {
    let scene = Scene::new(&["http://127.0.0.1:9/fixtures/".to_owned()]);
    let mut store = ControlStore::open(&scene.data).expect("store");
    store
        .set_permissions(
            scene.workspace_id,
            PermissionSet::default(),
            store.revision(),
        )
        .expect("disable task_network");
    drop(store);
    let before = scene.audit_text();
    let error = fetch(
        &scene.data,
        &scene.request(
            "http://127.0.0.1:9/fixtures/pkg.bin".to_owned(),
            "pkg.bin",
            1024,
        ),
    )
    .expect_err("task_network disabled");
    assert!(matches!(error, FetchError::Refused(_)), "{error}");
    assert_eq!(scene.audit_text(), before);

    let mut store = ControlStore::open(&scene.data).expect("store");
    store
        .set_permissions(
            scene.workspace_id,
            PermissionSet {
                task_network: Access::Prompt,
                ..PermissionSet::default()
            },
            store.revision(),
        )
        .expect("enable task_network");
    drop(store);
    for name in ["../pkg", "CON", ".hidden", "pkg/child"] {
        let error = fetch(
            &scene.data,
            &scene.request("http://127.0.0.1:9/fixtures/pkg.bin".to_owned(), name, 1024),
        )
        .expect_err("bad cache name");
        assert!(matches!(error, FetchError::Refused(_)), "{name}: {error}");
    }
}

#[cfg(not(target_os = "linux"))]
#[test]
fn a_fetch_without_a_cache_root_is_refused_off_linux() {
    let scene = Scene::unconfigured(&["http://127.0.0.1:9/fixtures/".to_owned()]);
    let before = scene.audit_text();
    let error = fetch(
        &scene.data,
        &scene.request(
            "http://127.0.0.1:9/fixtures/pkg.bin".to_owned(),
            "pkg.bin",
            1024,
        ),
    )
    .expect_err("no cache root is available");
    assert!(matches!(error, FetchError::Refused(_)), "{error}");
    assert_eq!(scene.audit_text(), before);
    assert!(!scene.cache_file("pkg.bin").exists());
}

#[test]
fn concurrent_publication_never_replaces_the_winner() {
    use std::sync::{Arc, Barrier};

    let directory = tempfile::tempdir().expect("temporary directory");
    let first = directory.path().join("first.partial");
    let second = directory.path().join("second.partial");
    fs::write(&first, b"first").expect("first partial");
    fs::write(&second, b"second").expect("second partial");
    let final_path = directory.path().join("pkg.bin");
    let barrier = Arc::new(Barrier::new(2));
    let results: Vec<Result<(), FetchError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = [&first, &second]
            .into_iter()
            .map(|temporary| {
                let barrier = Arc::clone(&barrier);
                let final_path = final_path.clone();
                scope.spawn(move || {
                    barrier.wait();
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .build()
                        .expect("publication runtime");
                    runtime.block_on(publish_cache_file(temporary, &final_path, "pkg.bin"))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("publisher"))
            .collect()
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(FetchError::AlreadyCached(_))))
            .count(),
        1
    );
    let content = fs::read(&final_path).expect("published file");
    assert!(
        content == b"first" || content == b"second",
        "the final file is one complete payload"
    );
}

/// A server that sends one body byte at a time, each inside a read timeout.
struct TrickleFixture {
    address: SocketAddr,
    /// How long the client kept the connection open after its request
    /// arrived, or `None` when every body byte was written.
    task: JoinHandle<Option<Duration>>,
}

impl TrickleFixture {
    fn start(interval: Duration, bytes: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        listener
            .set_nonblocking(true)
            .expect("make fixture accept bounded");
        let address = listener.local_addr().expect("fixture address");
        let task = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
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
            stream
                .set_nonblocking(false)
                .expect("make accepted stream blocking");
            let head = read_head(&mut stream);
            let requested = Instant::now();
            assert!(head.starts_with("GET "), "{head}");
            let response =
                format!("HTTP/1.1 200 OK\r\nContent-Length: {bytes}\r\nConnection: close\r\n\r\n");
            if stream.write_all(response.as_bytes()).is_err() {
                return Some(requested.elapsed());
            }
            for _ in 0..bytes {
                if stream.write_all(b"x").is_err() {
                    return Some(requested.elapsed());
                }
                let _ = stream.flush();
                thread::sleep(interval);
            }
            None
        });
        Self { address, task }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    /// Waits for the fixture and returns how long the connection lasted.
    fn finish(self) -> Option<Duration> {
        let deadline = Instant::now() + Duration::from_secs(8);
        while !self.task.is_finished() {
            assert!(Instant::now() < deadline, "fixture finished");
            thread::sleep(Duration::from_millis(2));
        }
        self.task.join().expect("fixture joined")
    }
}

#[test]
fn a_trickled_body_cannot_outlive_the_fetch_deadline() {
    let fixture = TrickleFixture::start(Duration::from_millis(200), 32);
    let scene = Scene::new(&[format!("http://{}/fixtures/", fixture.address)]);
    let mut request = scene.request(fixture.url("/fixtures/slow.bin"), "slow.bin", 1024);
    request.timeout = Duration::from_secs(1);
    let error = fetch(&scene.data, &request).expect_err("the overall deadline must stop the fetch");
    assert!(
        matches!(&error, FetchError::Body(source) if source.kind() == io::ErrorKind::TimedOut),
        "the deadline, not the body, must end the fetch: {error}"
    );
    assert!(!scene.cache_file("slow.bin").exists());
    // The deadline bounds the request, not the synced control records written
    // before and after it, so the server times the connection. The 6.4-second
    // body must be abandoned within the deadline plus the writes that notice.
    let connected = fixture
        .finish()
        .expect("the client must close the connection before the body ends");
    assert!(
        connected < Duration::from_secs(2),
        "the fetch ran past its deadline: {connected:?}"
    );
}
