//! Tests for the ChatGPT sign-in, the ID-token check, and the token store.
//! Every server is a loopback mock; no test contacts OpenAI.

use super::*;
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, OnceLock};

use omgcode_platform::credentials::{CredentialBackend, CredentialError, CredentialId, Secret};
use rsa::pkcs1v15::SigningKey;
use rsa::signature::Signer as _;
use rsa::traits::PublicKeyParts as _;
use rsa::{BoxedUint, PrivateKey};
use serde_json::{Value, json};

use crate::control::CredentialBinding;
use crate::provider::AccessTokenSource;

/// A throwaway 2048-bit test key made for these tests only.
const TEST_P: &str = "f5c93f6b0f93bab1c2e8e10417606c971a0da285419ae7cceb2cca6cb9746ca70c62906a5b6265b9f5c629b74e480a3688370d4bf39e87f56acf0d9be185736761da1716f8226be33ad060b88e239ced77a6c7397c0cfdb11bdd74a5e3b24f5d0963659e844454c7bb3647ca0a231e4f30dc03f74828bd1d0489106ac6bc0e3b";
const TEST_Q: &str = "c6cdad45a8221bea6f1462af6a2bfcf1fadf0e3f3d1683653db503486ea82eecc0e74f3970ff3f146b72c81a3b9d881acdbc23a56b06680c37c8f29ebc111d92e0ddfac2c5ec82cc27a92aba6989c8bcabd4507babd5f2e00427d92a92bd1b093ab29c24b1c048cd8f717e6eceeff2cc85fb9fa0702454fbcd47b7cdedfb2acb";
const KID: &str = "test-key";
const ISSUED_CLIENT: &str = "oaiapp_test_client";
const SUBJECT: &str = "user-subject-1";

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("hex"))
        .collect()
}

fn test_key() -> &'static PrivateKey {
    static KEY: OnceLock<PrivateKey> = OnceLock::new();
    KEY.get_or_init(|| {
        PrivateKey::from_p_q(
            BoxedUint::from_be_slice_vartime(&hex(TEST_P)),
            BoxedUint::from_be_slice_vartime(&hex(TEST_Q)),
            BoxedUint::from_be_slice_vartime(&[1, 0, 1]),
        )
        .expect("test key")
    })
}

fn key_set(kid: &str) -> String {
    let public = test_key().to_public_key();
    json!({"keys": [{
        "kty": "RSA", "kid": kid, "alg": "RS256", "use": "sig",
        "n": base64_url(&public.n_bytes()), "e": base64_url(&public.e_bytes()),
    }]})
    .to_string()
}

fn sign(header: &Value, claims: &Value) -> String {
    let signed = format!(
        "{}.{}",
        base64_url(header.to_string().as_bytes()),
        base64_url(claims.to_string().as_bytes())
    );
    let signature =
        SigningKey::<rsa::sha2::Sha256>::new(test_key().clone()).sign(signed.as_bytes());
    let bytes: Box<[u8]> = signature.into();
    format!("{signed}.{}", base64_url(&bytes))
}

fn claims(nonce: &str) -> Value {
    json!({
        "iss": AUTH_ISSUER, "aud": [ISSUED_CLIENT], "sub": SUBJECT,
        "exp": now_seconds() + 600, "nonce": nonce, "email": "person@example.test",
    })
}

/// A random nonce for a test, like the one each sign-in attempt generates.
fn test_nonce() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn id_token(nonce: &str) -> String {
    sign(
        &json!({"alg": "RS256", "kid": KID, "typ": "JWT"}),
        &claims(nonce),
    )
}

fn token_answer(id_token: &str, scope: &str) -> String {
    json!({
        "access_token": "access-1", "refresh_token": "refresh-1", "id_token": id_token,
        "token_type": "Bearer", "expires_in": 3600, "scope": scope,
    })
    .to_string()
}

// --- A loopback mock of the sign-in service --------------------------------

type Routes = Arc<Mutex<HashMap<String, VecDeque<(u16, String)>>>>;

struct MockService {
    base: String,
    routes: Routes,
    requests: Arc<Mutex<Vec<(String, String)>>>,
}

impl MockService {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let routes: Routes = Arc::default();
        let requests: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let (served_routes, served_requests) = (Arc::clone(&routes), Arc::clone(&requests));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let Some((path, body)) = read_request(&mut stream) else {
                    continue;
                };
                served_requests
                    .lock()
                    .expect("requests")
                    .push((path.clone(), body));
                let (status, answer) = served_routes
                    .lock()
                    .expect("routes")
                    .get_mut(&path)
                    .and_then(VecDeque::pop_front)
                    .unwrap_or((500, "{}".to_owned()));
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                );
            }
        });
        Self {
            base,
            routes,
            requests,
        }
    }

    fn endpoints(&self) -> AuthEndpoints {
        AuthEndpoints::loopback(&self.base)
    }

    fn answer(&self, path: &str, status: u16, body: String) {
        self.routes
            .lock()
            .expect("routes")
            .entry(path.to_owned())
            .or_default()
            .push_back((status, body));
    }

    fn requests_to(&self, path: &str) -> Vec<String> {
        self.requests
            .lock()
            .expect("requests")
            .iter()
            .filter(|(seen, _)| seen == path)
            .map(|(_, body)| body.clone())
            .collect()
    }
}

const TOKEN_PATH: &str = "/api/accounts/oauth/token";
const KEYS_PATH: &str = "/.well-known/jwks.json";
const REVOKE_PATH: &str = "/api/accounts/oauth/revoke";

fn read_request(stream: &mut TcpStream) -> Option<(String, String)> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
    let length = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|value| value.trim().parse::<usize>().unwrap_or(0))
        })
        .unwrap_or(0);
    while bytes.len() < header_end + length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    let path = head.split(' ').nth(1)?.to_owned();
    Some((
        path,
        String::from_utf8_lossy(&bytes[header_end..]).into_owned(),
    ))
}

fn form(body: &str) -> HashMap<String, String> {
    callback::parse_query(body)
        .expect("form")
        .into_iter()
        .map(|(name, value)| (name, value.to_string()))
        .collect()
}

/// The longest a simulated browser waits for one answer, and for all of its
/// requests to finish after the sign-in returned.
const BROWSER_WAIT: Duration = Duration::from_secs(10);

/// Connect to a loopback listener as a browser would. On Windows a connect
/// can be handed a local port whose earlier connection to this listener is
/// still in TIME_WAIT, and is then refused with `AddrInUse` before any byte
/// is sent (ADR-0078). As the model client does, hold that port and retry,
/// skipping at most 64 ports; any other failure fails the test.
fn connect_to_listener(address: &str) -> TcpStream {
    let mut held = Vec::new();
    loop {
        match TcpStream::connect(address) {
            Ok(stream) => return stream,
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse && held.len() < 64 => {
                held.push(crate::connect::reserve_next_port().expect("a port to hold"));
            }
            Err(error) => panic!("callback connect: {error}"),
        }
    }
}

/// Send one browser request to the callback listener and return the status.
fn browse(redirect: &str, path_and_query: &str) -> String {
    let address = redirect
        .strip_prefix("http://")
        .and_then(|rest| rest.split('/').next())
        .expect("redirect address");
    let mut stream = connect_to_listener(address);
    // A listener that never answers fails the test instead of hanging it.
    stream
        .set_read_timeout(Some(BROWSER_WAIT))
        .expect("read timeout");
    write!(
        stream,
        "GET {path_and_query} HTTP/1.1\r\nHost: {address}\r\n\r\n"
    )
    .expect("send");
    // The listener reads the whole request and closes after its answer, so
    // a read error or a missing or malformed status line fails the test.
    let mut answer = String::new();
    stream
        .read_to_string(&mut answer)
        .expect("the callback answer");
    let status = answer.lines().next().unwrap_or_default();
    let code = status
        .strip_prefix("HTTP/1.1 ")
        .and_then(|rest| rest.split_once(' '))
        .map(|(code, _reason)| code);
    assert!(
        code.is_some_and(|code| code.len() == 3 && code.bytes().all(|byte| byte.is_ascii_digit())),
        "not an HTTP status line: {status:?}"
    );
    status.to_owned()
}

/// What the simulated browser saw in the authorization address.
struct Authorization {
    url: Url,
}

impl Authorization {
    fn get(&self, name: &str) -> Option<String> {
        self.url
            .query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    }
}

/// Run a sign-in whose browser sends the requests `visit` makes from the
/// authorization address. `prepare` may add mock answers first.
fn run_sign_in(
    service: &MockService,
    request: &SignInRequest,
    prepare: impl Fn(&Authorization, &MockService),
    visit: impl Fn(&Authorization) -> Vec<String> + Send + Sync + 'static,
) -> (
    Result<SignedIn, SignInError>,
    Option<Authorization>,
    Vec<String>,
) {
    let seen: Arc<Mutex<Option<Url>>> = Arc::default();
    let statuses: Arc<Mutex<Vec<String>>> = Arc::default();
    let browser: Arc<Mutex<Option<std::thread::JoinHandle<()>>>> = Arc::default();
    let sent: Arc<Mutex<usize>> = Arc::default();
    let visit = Arc::new(visit);
    let mut opener = |address: &str| {
        let url = Url::parse(address).expect("authorization address");
        let authorization = Authorization { url: url.clone() };
        prepare(&authorization, service);
        *seen.lock().expect("seen") = Some(url);
        let redirect = authorization.get("redirect_uri").expect("redirect");
        let requests = visit(&authorization);
        *sent.lock().expect("sent") = requests.len();
        let statuses = Arc::clone(&statuses);
        *browser.lock().expect("browser") = Some(std::thread::spawn(move || {
            for request in requests {
                let status = browse(&redirect, &request);
                statuses.lock().expect("statuses").push(status);
            }
        }));
        Ok(())
    };
    let result = sign_in_within(
        &service.endpoints(),
        request,
        &mut opener,
        Duration::from_secs(10),
        &CancelToken::new(),
    );
    let url = seen.lock().expect("seen").take();
    // Every request was answered before the sign-in returned, so the browser
    // thread ends within a bounded wait; its failure fails the test.
    let worker = browser.lock().expect("browser").take();
    if let Some(worker) = worker {
        let deadline = std::time::Instant::now() + BROWSER_WAIT;
        while !worker.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "the browser thread did not finish"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Err(panic) = worker.join() {
            std::panic::resume_unwind(panic);
        }
    }
    let statuses = statuses.lock().expect("statuses").clone();
    // Each request the browser sent got exactly one answer.
    assert_eq!(statuses.len(), *sent.lock().expect("sent"), "{statuses:?}");
    (result, url.map(|url| Authorization { url }), statuses)
}

fn new_request() -> SignInRequest {
    SignInRequest {
        client_id: None,
        subject: None,
        host_id: new_host_id().expect("host"),
    }
}

fn returning_request(client: &str, subject: &str) -> SignInRequest {
    SignInRequest {
        client_id: Some(client.to_owned()),
        subject: Some(subject.to_owned()),
        host_id: new_host_id().expect("host"),
    }
}

/// Answer the code exchange and the key set with a valid token for the
/// address's nonce.
fn valid_answers(authorization: &Authorization, service: &MockService) {
    let nonce = authorization.get("nonce").expect("nonce");
    service.answer(TOKEN_PATH, 200, token_answer(&id_token(&nonce), SCOPE));
    service.answer(KEYS_PATH, 200, key_set(KID));
}

fn callback_with(authorization: &Authorization, extra: &str) -> String {
    let state = authorization.get("state").expect("state");
    format!("/auth/callback?code=the-code&state={state}{extra}")
}

#[test]
fn a_first_sign_in_registers_a_client_and_returns_the_verified_account() {
    let service = MockService::start();
    let request = new_request();
    let (result, authorization, statuses) =
        run_sign_in(&service, &request, valid_answers, |authorization| {
            vec![callback_with(
                authorization,
                &format!("&client_id={ISSUED_CLIENT}"),
            )]
        });
    let signed_in = result.expect("sign-in");
    assert_eq!(signed_in.client_id, ISSUED_CLIENT);
    assert_eq!(signed_in.subject, SUBJECT);
    assert_eq!(signed_in.email.as_deref(), Some("person@example.test"));
    assert_eq!(signed_in.tokens.access_token.as_str(), "access-1");
    assert_eq!(signed_in.tokens.refresh_token.as_str(), "refresh-1");
    assert!(
        signed_in
            .tokens
            .scopes
            .iter()
            .any(|scope| scope == PLAN_SCOPE)
    );
    assert!(statuses.iter().all(|status| status.contains("200")));

    let authorization = authorization.expect("browser opened");
    assert_eq!(
        authorization.get("client_id").as_deref(),
        Some(DYNAMIC_CLIENT)
    );
    assert_eq!(
        authorization.get("agent_name_hint").as_deref(),
        Some(AGENT_NAME)
    );
    assert_eq!(authorization.get("response_type").as_deref(), Some("code"));
    assert_eq!(authorization.get("scope").as_deref(), Some(SCOPE));
    assert_eq!(authorization.get("resource").as_deref(), Some(RESOURCE));
    assert_eq!(
        authorization.get("code_challenge_method").as_deref(),
        Some("S256")
    );
    assert_eq!(
        authorization.get("ext_agent_host_id"),
        Some(request.host_id.clone())
    );
    assert!(authorization.get("login_hint").is_none());
    assert!(authorization.get("id_token_hint").is_none());
    let redirect = authorization.get("redirect_uri").expect("redirect");
    assert!(redirect.starts_with("http://127.0.0.1:") && redirect.ends_with("/auth/callback"));

    let exchange = form(&service.requests_to(TOKEN_PATH)[0]);
    assert_eq!(exchange["grant_type"], "authorization_code");
    assert_eq!(exchange["client_id"], ISSUED_CLIENT);
    assert_eq!(exchange["code"], "the-code");
    assert_eq!(exchange["redirect_uri"], redirect);
    assert_eq!(exchange["resource"], RESOURCE);
    let challenge = base64_url(&Sha256::digest(exchange["code_verifier"].as_bytes()));
    assert_eq!(authorization.get("code_challenge"), Some(challenge));
}

#[test]
fn a_returning_sign_in_reuses_its_client_and_needs_the_same_account() {
    let service = MockService::start();
    let request = returning_request(ISSUED_CLIENT, SUBJECT);
    let (result, authorization, _) =
        run_sign_in(&service, &request, valid_answers, |authorization| {
            vec![callback_with(authorization, "")]
        });
    assert_eq!(result.expect("sign-in").client_id, ISSUED_CLIENT);
    let authorization = authorization.expect("browser opened");
    assert_eq!(
        authorization.get("client_id").as_deref(),
        Some(ISSUED_CLIENT)
    );
    assert!(authorization.get("agent_name_hint").is_none());

    let other_account = returning_request(ISSUED_CLIENT, "someone-else");
    let (result, _, _) = run_sign_in(&service, &other_account, valid_answers, |authorization| {
        vec![callback_with(authorization, "")]
    });
    assert_eq!(
        result.expect_err("another account"),
        SignInError::IdentityMismatch
    );
    // The other account's session is not used, so it is revoked; the first,
    // successful sign-in revoked nothing.
    assert_revoked_once(&service, true, &SignInError::IdentityMismatch);
}

#[test]
fn a_callback_that_does_not_match_the_attempt_is_refused_before_any_exchange() {
    /// A request, the callback path a browser would bring back, and the refusal.
    type Case = (SignInRequest, fn(&Authorization) -> String, SignInError);
    let cases: Vec<Case> = vec![
        (
            new_request(),
            |_| format!("/auth/callback?code=c&state=forged&client_id={ISSUED_CLIENT}"),
            SignInError::StateMismatch,
        ),
        (
            new_request(),
            |_| format!("/auth/callback?code=c&client_id={ISSUED_CLIENT}"),
            SignInError::StateMismatch,
        ),
        (
            new_request(),
            |a| {
                format!(
                    "/auth/callback?error=access_denied&state={}",
                    a.get("state").expect("state")
                )
            },
            SignInError::AccessDenied,
        ),
        (
            new_request(),
            |a| callback_with(a, ""),
            SignInError::Callback("no issued client registration"),
        ),
        (
            new_request(),
            |a| callback_with(a, "&client_id=dynamic_agent_client"),
            SignInError::Callback("no issued client registration"),
        ),
        (
            returning_request(ISSUED_CLIENT, SUBJECT),
            |a| callback_with(a, "&client_id=oaiapp_other"),
            SignInError::ClientMismatch,
        ),
        (
            new_request(),
            |a| callback_with(a, &format!("&client_id={ISSUED_CLIENT}&code=second")),
            SignInError::Callback("a repeated parameter"),
        ),
        (
            new_request(),
            |a| {
                format!(
                    "/auth/callback?state={}&client_id={ISSUED_CLIENT}",
                    a.get("state").expect("state")
                )
            },
            SignInError::Callback("no authorization code"),
        ),
    ];
    for (request, make, expected) in cases {
        let service = MockService::start();
        let (result, _, statuses) =
            run_sign_in(&service, &request, valid_answers, move |a| vec![make(a)]);
        assert_eq!(result.expect_err("refused callback"), expected);
        assert!(
            service.requests_to(TOKEN_PATH).is_empty(),
            "no code exchange for {expected:?}"
        );
        assert!(statuses.iter().all(|status| !status.contains("the-code")));
    }
}

#[test]
fn another_path_is_not_found_and_the_wait_continues() {
    let service = MockService::start();
    let (result, _, statuses) = run_sign_in(&service, &new_request(), valid_answers, |a| {
        vec![
            "/favicon.ico".to_owned(),
            "/auth/callbackx?code=c".to_owned(),
            callback_with(a, &format!("&client_id={ISSUED_CLIENT}")),
        ]
    });
    result.expect("sign-in after stray requests");
    assert!(statuses[0].contains("404") && statuses[1].contains("404"));
}

#[test]
fn a_slow_browser_step_counts_against_the_sign_in_wait() {
    let service = MockService::start();
    let started = std::time::Instant::now();
    // The browser step alone outlasts the wait, so the callback wait must
    // not start a fresh one.
    let mut slow = |_: &str| {
        std::thread::sleep(Duration::from_secs(2));
        Ok(())
    };
    let result = sign_in_within(
        &service.endpoints(),
        &new_request(),
        &mut slow,
        Duration::from_millis(1500),
        &CancelToken::new(),
    );
    assert_eq!(result.expect_err("timed out"), SignInError::TimedOut);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the deadline includes the browser step: {:?}",
        started.elapsed()
    );
}

#[test]
fn a_cancel_during_the_browser_step_ends_the_sign_in() {
    let service = MockService::start();
    let cancel = CancelToken::new();
    let canceller = cancel.clone();
    let started = std::time::Instant::now();
    let mut stalled = |_: &str| {
        canceller.cancel();
        std::thread::sleep(Duration::from_millis(200));
        Ok(())
    };
    let result = sign_in_within(
        &service.endpoints(),
        &new_request(),
        &mut stalled,
        Duration::from_secs(30),
        &cancel,
    );
    assert_eq!(result.expect_err("cancelled"), SignInError::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn the_wait_stops_when_cancelled_or_timed_out() {
    let service = MockService::start();
    let cancel = CancelToken::new();
    let canceller = cancel.clone();
    let started = std::time::Instant::now();
    let mut opener = |_: &str| {
        let canceller = canceller.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            canceller.cancel();
        });
        Ok(())
    };
    let result = sign_in_within(
        &service.endpoints(),
        &new_request(),
        &mut opener,
        Duration::from_secs(30),
        &cancel,
    );
    assert_eq!(result.expect_err("cancelled"), SignInError::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(5));

    let mut idle = |_: &str| Ok(());
    let result = sign_in_within(
        &service.endpoints(),
        &new_request(),
        &mut idle,
        Duration::from_millis(200),
        &CancelToken::new(),
    );
    assert_eq!(result.expect_err("timed out"), SignInError::TimedOut);

    let mut broken = |_: &str| Err(SignInError::Browser);
    let result = sign_in_within(
        &service.endpoints(),
        &new_request(),
        &mut broken,
        Duration::from_secs(1),
        &CancelToken::new(),
    );
    assert_eq!(result.expect_err("no browser"), SignInError::Browser);
}

#[test]
fn an_exchange_without_the_plan_scope_or_with_a_bad_id_token_stores_nothing() {
    let without_plan = |a: &Authorization, service: &MockService| {
        let nonce = a.get("nonce").expect("nonce");
        service.answer(
            TOKEN_PATH,
            200,
            token_answer(&id_token(&nonce), "openid email offline_access"),
        );
        service.answer(KEYS_PATH, 200, key_set(KID));
    };
    let wrong_nonce = |_: &Authorization, service: &MockService| {
        service.answer(
            TOKEN_PATH,
            200,
            token_answer(&id_token(&test_nonce()), SCOPE),
        );
        service.answer(KEYS_PATH, 200, key_set(KID));
    };
    let failed_exchange = |_: &Authorization, service: &MockService| {
        service.answer(
            TOKEN_PATH,
            400,
            json!({"error": "invalid_grant"}).to_string(),
        );
    };
    let keys_unavailable = |a: &Authorization, service: &MockService| {
        let nonce = a.get("nonce").expect("nonce");
        service.answer(TOKEN_PATH, 200, token_answer(&id_token(&nonce), SCOPE));
        service.answer(KEYS_PATH, 503, "{}".to_owned());
    };
    // Whether the refusal came after OpenAI issued a usable session, which
    // is then revoked. A refused exchange or plan scope returns no session.
    for (prepare, expected, revoked) in [
        (
            without_plan as fn(&Authorization, &MockService),
            SignInError::PlanUsageNotGranted,
            false,
        ),
        (wrong_nonce, SignInError::IdToken("wrong nonce"), true),
        (failed_exchange, SignInError::HttpStatus(400), false),
        (keys_unavailable, SignInError::HttpStatus(503), true),
    ] {
        let service = MockService::start();
        let (result, _, _) = run_sign_in(&service, &new_request(), prepare, |a| {
            vec![callback_with(a, &format!("&client_id={ISSUED_CLIENT}"))]
        });
        assert_eq!(result.expect_err("refused exchange"), expected);
        assert_revoked_once(&service, revoked, &expected);
    }
}

/// Check that a refused sign-in sent one revocation of the issued refresh
/// token when `revoked`, and none otherwise.
fn assert_revoked_once(service: &MockService, revoked: bool, case: &SignInError) {
    let sent = service.requests_to(REVOKE_PATH);
    if !revoked {
        assert!(sent.is_empty(), "{case:?}: {sent:?}");
        return;
    }
    assert_eq!(sent.len(), 1, "{case:?}");
    let form = form(&sent[0]);
    assert_eq!(form["token"], "refresh-1");
    assert_eq!(form["token_type_hint"], "refresh_token");
    assert_eq!(form["client_id"], ISSUED_CLIENT);
}

#[test]
fn the_id_token_check_refuses_every_invalid_token() {
    let keys = key_set(KID);
    // The nonce is generated, as a real attempt's is, never a literal.
    let nonce = test_nonce();
    let header = json!({"alg": "RS256", "kid": KID});
    let check = |token: &str| {
        id_token::validate(
            token,
            keys.as_bytes(),
            AUTH_ISSUER,
            ISSUED_CLIENT,
            &nonce,
            now_seconds(),
        )
    };
    let with = |change: &dyn Fn(&mut Value)| {
        let mut claims = claims(&nonce);
        change(&mut claims);
        sign(&header, &claims)
    };
    assert_eq!(
        check(&sign(&header, &claims(&nonce)))
            .expect("valid")
            .subject,
        SUBJECT
    );
    assert_eq!(
        check(&with(&|c| c["aud"] = json!(ISSUED_CLIENT)))
            .expect("string audience")
            .subject,
        SUBJECT
    );
    let cases: Vec<(String, &str)> = vec![
        (
            with(&|c| c["iss"] = json!("https://evil.test")),
            "wrong issuer",
        ),
        (
            with(&|c| c["aud"] = json!(["someone-else"])),
            "wrong audience",
        ),
        (with(&|c| c["exp"] = json!(now_seconds() - 120)), "expired"),
        (with(&|c| c["nonce"] = json!(test_nonce())), "wrong nonce"),
        (with(&|c| c["sub"] = json!("")), "no subject"),
        (
            sign(&json!({"alg": "HS256", "kid": KID}), &claims(&nonce)),
            "not signed with RS256",
        ),
        (
            sign(&json!({"alg": "RS256", "kid": "unknown"}), &claims(&nonce)),
            "unknown signing key",
        ),
        (
            sign(&json!({"alg": "RS256"}), &claims(&nonce)),
            "no key identifier",
        ),
        ("a.b".to_owned(), "not a signed token"),
    ];
    for (token, reason) in cases {
        assert_eq!(
            check(&token).expect_err(reason),
            SignInError::IdToken(reason)
        );
    }
    // A changed payload under the original signature fails verification.
    let valid = sign(&header, &claims(&nonce));
    let parts: Vec<&str> = valid.split('.').collect();
    let mut forged_claims = claims(&nonce);
    forged_claims["sub"] = json!("attacker");
    let forged = format!(
        "{}.{}.{}",
        parts[0],
        base64_url(forged_claims.to_string().as_bytes()),
        parts[2]
    );
    assert_eq!(
        check(&forged).expect_err("forged"),
        SignInError::IdToken("bad signature")
    );
}

#[test]
fn a_refresh_rotates_the_token_and_a_refused_refresh_needs_a_new_sign_in() {
    let rotated = json!({
        "access_token": "access-2", "refresh_token": "refresh-2",
        "token_type": "Bearer", "expires_in": 3600,
    })
    .to_string();
    let tokens = parse_refresh(200, Some(rotated.as_bytes()), "refresh-1", 100).expect("refresh");
    assert_eq!(tokens.access_token.as_str(), "access-2");
    assert_eq!(tokens.refresh_token.as_str(), "refresh-2");
    assert_eq!(tokens.expires_at, 3700);

    let kept = json!({"access_token": "a", "token_type": "Bearer", "expires_in": 60}).to_string();
    let tokens = parse_refresh(200, Some(kept.as_bytes()), "refresh-1", 0).expect("refresh");
    assert_eq!(tokens.refresh_token.as_str(), "refresh-1");

    for code in REFUSED_REFRESH_CODES {
        let body = json!({"error": code}).to_string();
        assert_eq!(
            parse_refresh(400, Some(body.as_bytes()), "r", 0).expect_err("refused"),
            SignInError::SignInRequired
        );
    }
    let other = json!({"error": "invalid_client"}).to_string();
    assert_eq!(
        parse_refresh(401, Some(other.as_bytes()), "r", 0).expect_err("status"),
        SignInError::HttpStatus(401)
    );
    assert_eq!(
        parse_refresh(503, None, "r", 0).expect_err("status"),
        SignInError::HttpStatus(503)
    );
    let narrowed =
        json!({"access_token": "a", "token_type": "Bearer", "expires_in": 60, "scope": "openid"})
            .to_string();
    assert_eq!(
        parse_refresh(200, Some(narrowed.as_bytes()), "r", 0).expect_err("scope"),
        SignInError::PlanUsageNotGranted
    );
    let wrong_type =
        json!({"access_token": "a", "token_type": "mac", "expires_in": 60}).to_string();
    assert_eq!(
        parse_refresh(200, Some(wrong_type.as_bytes()), "r", 0).expect_err("type"),
        SignInError::Malformed
    );
}

#[test]
fn refresh_and_revoke_send_the_documented_forms() {
    let service = MockService::start();
    let rotated = json!({"access_token": "a2", "refresh_token": "r2", "token_type": "Bearer", "expires_in": 3600});
    service.answer(TOKEN_PATH, 200, rotated.to_string());
    let tokens = refresh(
        &service.endpoints(),
        ISSUED_CLIENT,
        "r1",
        &CancelToken::new(),
    )
    .expect("refresh");
    assert_eq!(tokens.refresh_token.as_str(), "r2");
    let sent = form(&service.requests_to(TOKEN_PATH)[0]);
    assert_eq!(sent["grant_type"], "refresh_token");
    assert_eq!(sent["client_id"], ISSUED_CLIENT);
    assert_eq!(sent["refresh_token"], "r1");
    assert_eq!(sent["resource"], RESOURCE);
    assert!(!sent.contains_key("scope"));

    service.answer(REVOKE_PATH, 200, String::new());
    revoke(
        &service.endpoints(),
        ISSUED_CLIENT,
        "r2",
        &CancelToken::new(),
    )
    .expect("revoked");
    let sent = form(&service.requests_to(REVOKE_PATH)[0]);
    assert_eq!(sent["token"], "r2");
    assert_eq!(sent["token_type_hint"], "refresh_token");
    assert_eq!(sent["client_id"], ISSUED_CLIENT);
    service.answer(REVOKE_PATH, 503, String::new());
    assert_eq!(
        revoke(
            &service.endpoints(),
            ISSUED_CLIENT,
            "r2",
            &CancelToken::new()
        )
        .expect_err("unconfirmed"),
        SignInError::HttpStatus(503)
    );
    assert_eq!(service.requests_to(REVOKE_PATH).len(), 2, "no retry");
}

// --- The two-slot token store ----------------------------------------------

#[derive(Default)]
struct MemoryStore {
    items: Mutex<HashMap<String, Vec<u8>>>,
}

impl SlotStore for MemoryStore {
    fn load(&self, id: &CredentialId) -> Result<Secret, CredentialError> {
        let items = self.items.lock().expect("items");
        let bytes = items.get(id.as_str()).ok_or(CredentialError::NotFound)?;
        Secret::new(bytes.clone())
    }

    fn store(&self, id: &CredentialId, secret: &Secret) -> Result<(), CredentialError> {
        let mut items = self.items.lock().expect("items");
        if items.contains_key(id.as_str()) {
            return Err(CredentialError::AlreadyExists);
        }
        items.insert(id.as_str().to_owned(), secret.expose().to_vec());
        Ok(())
    }

    fn delete(&self, id: &CredentialId) -> Result<(), CredentialError> {
        self.items
            .lock()
            .expect("items")
            .remove(id.as_str())
            .map(|_| ())
            .ok_or(CredentialError::NotFound)
    }
}

fn token_set(access: &str, refresh: &str, expires_at: u64) -> TokenSet {
    TokenSet {
        access_token: Zeroizing::new(access.to_owned()),
        refresh_token: Zeroizing::new(refresh.to_owned()),
        scopes: vec![PLAN_SCOPE.to_owned()],
        expires_at,
    }
}

fn binding_id() -> CredentialId {
    CredentialId::parse(&uuid::Uuid::new_v4().simple().to_string()).expect("id")
}

#[test]
fn the_companion_slot_is_a_distinct_stable_identifier() {
    let id = binding_id();
    let companion = companion_credential_id(&id);
    assert_ne!(companion, id);
    assert_eq!(companion, companion_credential_id(&id));
    assert_ne!(companion, companion_credential_id(&binding_id()));
}

#[test]
fn records_alternate_slots_and_an_unreadable_slot_is_never_skipped() {
    let store = MemoryStore::default();
    let id = binding_id();
    assert_eq!(
        load_current_record(&store, &id, ISSUED_CLIENT, SUBJECT).expect_err("empty"),
        SignInError::SignInRequired
    );
    let first =
        initial_token_record(ISSUED_CLIENT, SUBJECT, &token_set("a1", "r1", 10)).expect("record");
    store.store(&id, &first).expect("store");
    let current = load_current_record(&store, &id, ISSUED_CLIENT, SUBJECT).expect("first");
    assert_eq!((current.generation(), current.slot()), (1, &id));

    for (generation, access) in [(2, "a2"), (3, "a3"), (4, "a4")] {
        let current = load_current_record(&store, &id, ISSUED_CLIENT, SUBJECT).expect("current");
        store_next_record(
            &store,
            &id,
            &current,
            ISSUED_CLIENT,
            SUBJECT,
            &token_set(access, access, 10),
        )
        .expect("next");
        let next = load_current_record(&store, &id, ISSUED_CLIENT, SUBJECT).expect("next");
        assert_eq!(next.generation(), generation);
        assert_eq!(next.tokens.access_token.as_str(), access);
        assert_ne!(next.slot(), current.slot(), "the write used the other slot");
        assert_eq!(store.items.lock().expect("items").len(), 2);
    }
    // A record for another registration is never used, and is not skipped
    // for the other slot either.
    assert_eq!(
        load_current_record(&store, &id, "oaiapp_other", SUBJECT).expect_err("other client"),
        SignInError::TokenStore
    );
    // A corrupt newer slot never falls back to the older record, whose
    // refresh token may already have been rotated.
    let current = load_current_record(&store, &id, ISSUED_CLIENT, SUBJECT).expect("current");
    store.delete(current.slot()).expect("delete");
    store
        .store(
            current.slot(),
            &Secret::new(b"{not json".to_vec()).expect("secret"),
        )
        .expect("store");
    assert_eq!(
        load_current_record(&store, &id, ISSUED_CLIENT, SUBJECT).expect_err("corrupt newer"),
        SignInError::TokenStore
    );
}

fn tokens_over(
    store: &Arc<MemoryStore>,
    id: &CredentialId,
    root: &std::path::Path,
    service: &MockService,
) -> ChatGptTokens {
    ChatGptTokens::for_test(
        root.to_path_buf(),
        CredentialBinding::new(id.clone(), CredentialBackend::WindowsDpapi),
        ISSUED_CLIENT.to_owned(),
        SUBJECT.to_owned(),
        service.endpoints(),
        Arc::clone(store) as Arc<dyn SlotStore + Send + Sync>,
    )
}

#[test]
fn a_fresh_token_is_used_and_an_expiring_one_is_refreshed_once_under_the_lock() {
    let root = tempfile::tempdir().expect("root");
    let service = MockService::start();
    let store = Arc::new(MemoryStore::default());
    let id = binding_id();
    let fresh = token_set("fresh", "r1", now_seconds() + 3600);
    store
        .store(
            &id,
            &initial_token_record(ISSUED_CLIENT, SUBJECT, &fresh).expect("record"),
        )
        .expect("store");
    let source = tokens_over(&store, &id, root.path(), &service);
    assert_eq!(
        source
            .access_token(&CancelToken::new())
            .expect("token")
            .as_str(),
        "fresh"
    );
    assert!(
        service.requests_to(TOKEN_PATH).is_empty(),
        "no refresh for a fresh token"
    );

    // Expire it: the next two concurrent requests refresh exactly once.
    let current =
        load_current_record(store.as_ref(), &id, ISSUED_CLIENT, SUBJECT).expect("current");
    store_next_record(
        store.as_ref(),
        &id,
        &current,
        ISSUED_CLIENT,
        SUBJECT,
        &token_set("old", "r2", now_seconds() + 60),
    )
    .expect("expiring");
    let rotated = json!({"access_token": "new", "refresh_token": "r3", "token_type": "Bearer", "expires_in": 3600});
    service.answer(TOKEN_PATH, 200, rotated.to_string());
    let source = Arc::new(source);
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let source = Arc::clone(&source);
            std::thread::spawn(move || {
                source
                    .access_token(&CancelToken::new())
                    .map(|token| token.to_string())
            })
        })
        .collect();
    for worker in workers {
        assert_eq!(worker.join().expect("worker").expect("token"), "new");
    }
    let refreshes = service.requests_to(TOKEN_PATH);
    assert_eq!(refreshes.len(), 1, "one refresh for both requests");
    assert_eq!(form(&refreshes[0])["refresh_token"], "r2");
    let stored = load_current_record(store.as_ref(), &id, ISSUED_CLIENT, SUBJECT).expect("stored");
    assert_eq!(stored.generation(), 3);
    assert_eq!(stored.tokens.refresh_token.as_str(), "r3");
}

#[test]
fn a_signed_out_session_gives_no_token_and_writes_no_record() {
    let root = tempfile::tempdir().expect("root");
    let service = MockService::start();
    let store = Arc::new(MemoryStore::default());
    let id = binding_id();
    // The slot survives, as one the sign-out could not delete would, and its
    // token is about to expire, so an unmarked session would refresh it.
    store
        .store(
            &id,
            &initial_token_record(
                ISSUED_CLIENT,
                SUBJECT,
                &token_set("old", "r1", now_seconds() + 60),
            )
            .expect("record"),
        )
        .expect("store");
    let source = tokens_over(&store, &id, root.path(), &service);
    {
        let lock = SessionLock::acquire(root.path(), &id, &CancelToken::new()).expect("lock");
        assert!(!lock.is_signed_out().expect("unmarked"));
        lock.mark_signed_out().expect("mark");
        assert!(lock.is_signed_out().expect("marked"));
        // A second sign-out keeps the marker an earlier one left.
        lock.mark_signed_out().expect("mark again");
        assert!(lock.is_signed_out().expect("still marked"));
    }
    assert_eq!(
        source
            .current_access_token(&CancelToken::new())
            .expect_err("signed out"),
        SignInError::SignInRequired
    );
    assert!(service.requests_to(TOKEN_PATH).is_empty(), "no refresh");
    let stored = load_current_record(store.as_ref(), &id, ISSUED_CLIENT, SUBJECT).expect("kept");
    assert_eq!(stored.generation(), 1, "no new record");
    // A marker names one binding: another session is unaffected.
    let other = binding_id();
    let lock = SessionLock::acquire(root.path(), &other, &CancelToken::new()).expect("lock");
    assert!(!lock.is_signed_out().expect("other"));
    // Any entry at the marker's path counts, so nothing placed there can make
    // a session usable again, and marking it leaves that entry alone.
    std::fs::create_dir(
        root.path()
            .join("subscription-locks")
            .join(format!("{}.signed-out", other.as_str())),
    )
    .expect("directory");
    assert!(lock.is_signed_out().expect("a directory counts"));
    lock.mark_signed_out().expect("already marked");
}

#[test]
fn a_refused_refresh_or_a_missing_record_requires_a_new_sign_in() {
    let root = tempfile::tempdir().expect("root");
    let service = MockService::start();
    let store = Arc::new(MemoryStore::default());
    let id = binding_id();
    let source = tokens_over(&store, &id, root.path(), &service);
    assert_eq!(
        source
            .access_token(&CancelToken::new())
            .expect_err("signed out"),
        ProviderError::SignInRequired
    );
    store
        .store(
            &id,
            &initial_token_record(ISSUED_CLIENT, SUBJECT, &token_set("a", "r", 0)).expect("record"),
        )
        .expect("store");
    service.answer(
        TOKEN_PATH,
        400,
        json!({"error": "refresh_token_reused"}).to_string(),
    );
    assert_eq!(
        source
            .access_token(&CancelToken::new())
            .expect_err("refused"),
        ProviderError::SignInRequired
    );
    service.answer(TOKEN_PATH, 500, "{}".to_owned());
    assert_eq!(
        source
            .access_token(&CancelToken::new())
            .expect_err("unavailable"),
        ProviderError::AccessTokenUnavailable
    );
    let cancel = CancelToken::new();
    cancel.cancel();
    assert_eq!(
        source.access_token(&cancel).expect_err("cancelled"),
        ProviderError::Cancelled
    );
}

/// A callback wait running on its own thread, with an idle connection that
/// the listener has accepted. The connection never sends a byte.
struct IdleWait {
    result: std::sync::mpsc::Receiver<Result<callback::Callback, SignInError>>,
    _idle: TcpStream,
}

impl IdleWait {
    /// Start a wait of `wait`, open an idle connection, and return once the
    /// listener reports that it accepted it. The wait checks the cancel
    /// token and the deadline before each accept, so the accept happened
    /// before either could end the wait.
    fn start(wait: Duration, cancel: &CancelToken) -> Self {
        let mut listener = callback::CallbackListener::bind().expect("listener");
        let (accepted_sender, accepted) = std::sync::mpsc::channel();
        listener.notify_accepted(accepted_sender);
        let address = listener
            .redirect_uri()
            .strip_prefix("http://")
            .and_then(|rest| rest.split('/').next())
            .expect("address")
            .to_owned();
        let (result_sender, result) = std::sync::mpsc::channel();
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let _ = result_sender.send(listener.wait("state", None, wait, &cancel));
        });
        let idle = connect_to_listener(&address);
        accepted
            .recv_timeout(BROWSER_WAIT)
            .expect("the listener did not accept the idle connection");
        Self {
            result,
            _idle: idle,
        }
    }

    /// The wait's result, which must arrive within `within`, independently
    /// of the wait's own timing.
    fn finish(&self, within: Duration) -> Result<callback::Callback, SignInError> {
        self.result
            .recv_timeout(within)
            .expect("the wait did not end in time")
    }
}

#[test]
fn an_idle_callback_connection_does_not_delay_a_cancel_or_the_deadline() {
    // An unfixed reader blocks on the idle connection for the full five-
    // second connection limit, so each bound below is well under it.

    // Cancel: set only after the listener accepted the idle connection.
    let cancel = CancelToken::new();
    let wait = IdleWait::start(Duration::from_secs(30), &cancel);
    cancel.cancel();
    assert!(matches!(
        wait.finish(Duration::from_secs(2)),
        Err(SignInError::Cancelled)
    ));

    // Deadline: the connection was accepted before the one-second deadline,
    // and the wait still ends near it.
    let started = std::time::Instant::now();
    let wait = IdleWait::start(Duration::from_secs(1), &CancelToken::new());
    assert!(matches!(
        wait.finish(Duration::from_secs(3).saturating_sub(started.elapsed())),
        Err(SignInError::TimedOut)
    ));
}

#[test]
fn a_held_session_lock_makes_a_cancelled_waiter_stop() {
    let root = tempfile::tempdir().expect("root");
    let id = binding_id();
    let held = SessionLock::acquire(root.path(), &id, &CancelToken::new()).expect("lock");
    let cancel = CancelToken::new();
    let canceller = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        canceller.cancel();
    });
    let started = std::time::Instant::now();
    assert_eq!(
        SessionLock::acquire(root.path(), &id, &cancel).expect_err("busy"),
        SignInError::Cancelled
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(held);
    // A caller that is already cancelled never takes even a free lock.
    let cancelled = CancelToken::new();
    cancelled.cancel();
    assert_eq!(
        SessionLock::acquire(root.path(), &id, &cancelled).expect_err("cancelled"),
        SignInError::Cancelled
    );
    SessionLock::acquire(root.path(), &id, &CancelToken::new()).expect("free again");
}

#[test]
fn host_and_identifier_checks_accept_only_the_documented_shapes() {
    assert!(valid_host_id(&new_host_id().expect("host")));
    assert!(!valid_host_id("urn:uuid:not-a-uuid"));
    assert!(!valid_host_id(&format!("urn:uuid:{}", uuid::Uuid::nil())));
    assert!(!valid_host_id(&new_host_id().expect("host").to_uppercase()));
    assert!(valid_identifier(ISSUED_CLIENT));
    assert!(!valid_identifier(""));
    assert!(!valid_identifier("has space"));
    assert!(!valid_identifier(&"x".repeat(257)));
}

#[test]
fn errors_carry_fixed_text_and_stable_codes() {
    for error in [
        SignInError::HttpStatus(400),
        SignInError::IdToken("wrong nonce"),
        SignInError::Callback("a bad escape"),
    ] {
        let text = error.to_string();
        assert!(!text.contains("access-") && !text.contains("refresh-"));
        assert!(!error.code().is_empty());
    }
    assert_eq!(
        ProviderError::from(SignInError::SignInRequired),
        ProviderError::SignInRequired
    );
    // A refresh that lost the plan scope needs a new sign-in, not a retry.
    assert_eq!(
        ProviderError::from(SignInError::PlanUsageNotGranted),
        ProviderError::SignInRequired
    );
    assert_eq!(
        ProviderError::from(SignInError::Transport),
        ProviderError::AccessTokenUnavailable
    );
    // A local random-source failure is not reported as the service being
    // unreachable.
    assert_eq!(SignInError::Random.code(), "random_unavailable");
    assert!(!SignInError::Random.to_string().contains("reached"));
}

#[test]
fn a_token_response_is_read_strictly_without_a_value_tree() {
    let Ok(parsed) = parse_token_response(Some(
        br#"{"access_token":"a","refresh_token":"r","id_token":"i","token_type":"Bearer","expires_in":60,"scope":"s","earliest_refresh_at":{"x":[1,{"y":null}]}}"#,
    ))
    else {
        panic!("a valid response");
    };
    assert_eq!(
        parsed.access_token.as_deref().map(String::as_str),
        Some("a")
    );
    assert_eq!(
        parsed.refresh_token.as_deref().map(String::as_str),
        Some("r")
    );
    assert_eq!(parsed.id_token.as_deref().map(String::as_str), Some("i"));
    assert_eq!(parsed.token_type.as_deref(), Some("Bearer"));
    assert_eq!(parsed.expires_in, Some(60));
    assert_eq!(parsed.scope.as_deref(), Some("s"));
    let Ok(empty) = parse_token_response(Some(br#"{"access_token":null}"#)) else {
        panic!("null is absent");
    };
    assert!(empty.access_token.is_none());
    for body in [
        &br#"{"access_token":"a","access_token":"b"}"#[..],
        br#"{"refresh_token":"r","x":1,"refresh_token":"s"}"#,
        br#"{"extra":{"k":1,"k":2}}"#,
        br#"{"access_token":"a"} {}"#,
        br#"{"access_token":"a","expires_in":"soon"}"#,
        br#"{"access_token":7}"#,
        br#"["access_token"]"#,
        br#"{"access_token":"a""#,
    ] {
        assert!(
            matches!(
                parse_token_response(Some(body)),
                Err(SignInError::Malformed)
            ),
            "{}",
            String::from_utf8_lossy(body)
        );
    }
    assert!(matches!(
        parse_token_response(None),
        Err(SignInError::Malformed)
    ));
}

/// A key set with a certificate chain of `chain_bytes` beside the key.
fn key_set_with_chain(chain_bytes: usize) -> String {
    let mut set: Value = serde_json::from_str(&key_set(KID)).expect("key set");
    set["keys"][0]["x5c"] = json!(["A".repeat(chain_bytes)]);
    set.to_string()
}

#[test]
fn a_key_set_above_the_error_body_bound_is_read_and_one_above_a_mebibyte_is_refused() {
    // Larger than the 64 KiB bound for failure bodies, as a key set with
    // certificate chains can be.
    let large = |a: &Authorization, service: &MockService| {
        let nonce = a.get("nonce").expect("nonce");
        service.answer(TOKEN_PATH, 200, token_answer(&id_token(&nonce), SCOPE));
        service.answer(KEYS_PATH, 200, key_set_with_chain(200 * 1024));
    };
    let service = MockService::start();
    let (result, _, _) = run_sign_in(&service, &new_request(), large, |a| {
        vec![callback_with(a, &format!("&client_id={ISSUED_CLIENT}"))]
    });
    assert_eq!(result.expect("a large key set").subject, SUBJECT);

    let too_large = |a: &Authorization, service: &MockService| {
        let nonce = a.get("nonce").expect("nonce");
        service.answer(TOKEN_PATH, 200, token_answer(&id_token(&nonce), SCOPE));
        service.answer(KEYS_PATH, 200, key_set_with_chain(1024 * 1024 + 1));
    };
    let service = MockService::start();
    let (result, _, _) = run_sign_in(&service, &new_request(), too_large, |a| {
        vec![callback_with(a, &format!("&client_id={ISSUED_CLIENT}"))]
    });
    let error = result.expect_err("an oversized key set");
    assert_eq!(error, SignInError::Malformed);
    assert_revoked_once(&service, true, &error);
}
