//! Tests for the ChatGPT settings commands.
//!
//! No test reaches OpenAI: every service here replaces the browser opener
//! with one that refuses, so a sign-in ends before any network request. One
//! sign-out test records a made-up session through the core and is cancelled
//! before the session lock, which a cancelled caller never takes, so it
//! never reaches the revocation.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use omgcode_core::control::{ControlStore, CredentialStore, ProviderProfileDeclaration};
use omgcode_core::provider::OPENAI_ENDPOINT;
use omgcode_core::provider::chatgpt::{PLAN_SCOPE, SignInError, SignedIn, TokenSet, new_host_id};
use omgcode_core::turn::CancelToken;
use zeroize::Zeroizing;

use super::super::settings_changes::{Confirmation, FolderPurpose, SettingsPrompt};
use super::super::{DesktopConfig, DesktopService};
use super::{ChatGptRequest, ChatGptSignOutResult};
use crate::TokenSource;

/// Answers confirmations in order and records each title.
struct Answers {
    answers: RefCell<VecDeque<bool>>,
    titles: RefCell<Vec<&'static str>>,
}

impl Answers {
    fn with(answers: &[bool]) -> Self {
        Self {
            answers: RefCell::new(answers.iter().copied().collect()),
            titles: RefCell::default(),
        }
    }

    fn titles(&self) -> Vec<&'static str> {
        self.titles.take()
    }
}

impl SettingsPrompt for Answers {
    fn pick_folder(&self, _purpose: FolderPurpose, _start: Option<&Path>) -> Option<PathBuf> {
        panic!("no folder dialog is expected");
    }

    fn confirm(&self, confirmation: &Confirmation) -> bool {
        self.titles.borrow_mut().push(confirmation.title);
        self.answers
            .borrow_mut()
            .pop_front()
            .expect("an unexpected confirmation")
    }
}

/// Refuses to open anything, after checking that the address is OpenAI's
/// sign-in page and names the loopback callback.
fn refusing_opener(address: &str, _cancel: &CancelToken) -> Result<(), SignInError> {
    assert!(
        address.starts_with("https://auth.openai.com/api/accounts/authorize?"),
        "{address}"
    );
    assert!(
        address.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A"),
        "{address}"
    );
    Err(SignInError::Browser)
}

/// A browser opener that must not be reached.
fn unreachable_opener(_address: &str, _cancel: &CancelToken) -> Result<(), SignInError> {
    panic!("the browser must not be opened");
}

struct Fixture {
    _directory: tempfile::TempDir,
    data: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let data = directory.path().join("data");
        private_data_root(&data);
        let mut store = ControlStore::open(&data).expect("control store");
        for (name, kind, auth) in [
            ("plan", "openai", "subscription"),
            ("keys", "openai", "api_key"),
        ] {
            let revision = store.revision();
            store
                .add_provider_profile(
                    &ProviderProfileDeclaration {
                        name: name.into(),
                        kind: kind.into(),
                        auth: auth.into(),
                        endpoint: Some(OPENAI_ENDPOINT.into()),
                    },
                    revision,
                )
                .expect("profile");
        }
        Self {
            _directory: directory,
            data,
        }
    }

    fn service(&self, opener: super::BrowserOpener) -> DesktopService {
        let mut service = DesktopService::open(DesktopConfig {
            data_root: self.data.clone(),
            // Never contacted: no turn is sent.
            endpoint: "http://127.0.0.1:9/v1".into(),
            token_source: TokenSource::Active,
            stream: false,
            claude_code_relay: None,
        })
        .expect("desktop service");
        Arc::get_mut(&mut service.inner)
            .expect("a service no one else holds")
            .open_browser = opener;
        service
    }

    fn store(&self) -> ControlStore {
        ControlStore::open(&self.data).expect("control store")
    }

    fn request(&self, name: &str) -> ChatGptRequest {
        ChatGptRequest {
            revision: self.store().revision(),
            name: name.into(),
        }
    }
}

/// Create `data` with an owner-only ACL, as the private store requires.
fn private_data_root(data: &Path) {
    std::fs::create_dir(data).expect("data root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(data, std::fs::Permissions::from_mode(0o700))
            .expect("private mode");
    }
    #[cfg(windows)]
    {
        use std::process::Command;
        let output = Command::new("whoami")
            .args(["/user", "/fo", "csv", "/nh"])
            .output()
            .expect("whoami");
        assert!(output.status.success(), "whoami failed");
        let text = String::from_utf8(output.stdout).expect("UTF-8");
        let sid = text
            .trim()
            .rsplit(',')
            .next()
            .expect("SID column")
            .trim_matches('"')
            .to_owned();
        let status = Command::new("icacls")
            .arg(data)
            .args(["/inheritance:r", "/grant:r", &format!("*{sid}:(OI)(CI)F")])
            .output()
            .expect("icacls");
        assert!(status.status.success(), "icacls failed");
    }
}

#[test]
fn declining_the_confirmation_records_nothing_and_opens_nothing() {
    let fixture = Fixture::new();
    let service = fixture.service(unreachable_opener);
    let before = fixture.store().revision();
    let answers = Answers::with(&[false]);
    let error = service
        .settings_sign_in_chatgpt(fixture.request("fresh"), &answers)
        .expect_err("declined");
    assert_eq!(error.code(), "cancelled");
    assert_eq!(answers.titles(), ["Sign in with ChatGPT"]);
    let store = fixture.store();
    assert_eq!(store.revision(), before);
    assert!(store.provider_profile("fresh").is_none());
}

#[test]
fn only_a_chatgpt_profile_can_sign_in() {
    let fixture = Fixture::new();
    let service = fixture.service(unreachable_opener);
    // Refused before the confirmation, so no answer is queued.
    let answers = Answers::with(&[]);
    let error = service
        .settings_sign_in_chatgpt(fixture.request("keys"), &answers)
        .expect_err("an API key profile");
    assert_eq!(error.code(), "control", "{}", error.message());
    assert!(answers.titles().is_empty());
}

#[test]
fn a_name_that_cannot_be_recorded_is_refused_before_asking() {
    let fixture = Fixture::new();
    let service = fixture.service(unreachable_opener);
    let before = fixture.store().revision();
    for name in ["", "-plan", "two words", &"x".repeat(65)] {
        let answers = Answers::with(&[]);
        let error = service
            .settings_sign_in_chatgpt(fixture.request(name), &answers)
            .expect_err("an invalid name");
        assert_eq!(error.code(), "control", "{name:?}: {}", error.message());
        assert!(answers.titles().is_empty(), "{name:?} reached the dialog");
    }
    assert_eq!(fixture.store().revision(), before);
}

#[test]
fn a_stale_revision_is_refused_first() {
    let fixture = Fixture::new();
    let service = fixture.service(unreachable_opener);
    let mut request = fixture.request("plan");
    request.revision -= 1;
    let error = service
        .settings_sign_in_chatgpt(request, &Answers::with(&[]))
        .expect_err("stale");
    assert_eq!(error.code(), "stale_revision");
}

#[test]
fn a_new_name_records_the_profile_and_a_failed_browser_step_stores_no_token() {
    let fixture = Fixture::new();
    let service = fixture.service(refusing_opener);
    let answers = Answers::with(&[true]);
    let error = service
        .settings_sign_in_chatgpt(fixture.request("fresh"), &answers)
        .expect_err("the browser cannot open");
    assert!(
        error.message().contains("browser_unavailable"),
        "{}: {}",
        error.code(),
        error.message()
    );
    assert_eq!(answers.titles(), ["Sign in with ChatGPT"]);
    let store = fixture.store();
    let profile = store.provider_profile("fresh").expect("recorded profile");
    assert_eq!(profile.profile().kind().as_str(), "openai");
    assert_eq!(profile.profile().auth().as_str(), "subscription");
    assert_eq!(
        profile
            .profile()
            .endpoint()
            .map(|endpoint| endpoint.as_str()),
        Some(OPENAI_ENDPOINT)
    );
    assert!(profile.credential().is_none());
    assert!(profile.subscription().is_none());
    // The wait's cancel token is gone, so a later cancel is a no-op.
    assert!(
        service
            .inner
            .chatgpt_sign_in
            .lock()
            .expect("slot")
            .is_none()
    );
}

#[test]
fn an_existing_profile_with_a_failed_browser_step_is_left_signed_out() {
    let fixture = Fixture::new();
    let service = fixture.service(refusing_opener);
    service
        .settings_sign_in_chatgpt(fixture.request("plan"), &Answers::with(&[true]))
        .expect_err("the browser cannot open");
    let store = fixture.store();
    let profile = store.provider_profile("plan").expect("profile");
    assert!(profile.credential().is_none());
}

/// Accepts the confirmation after the operator presses Cancel while it is
/// open.
struct CancelDuringConfirmation<'a> {
    service: &'a DesktopService,
}

impl SettingsPrompt for CancelDuringConfirmation<'_> {
    fn pick_folder(&self, _purpose: FolderPurpose, _start: Option<&Path>) -> Option<PathBuf> {
        panic!("no folder dialog is expected");
    }

    fn confirm(&self, _confirmation: &Confirmation) -> bool {
        // Without a registered token the change would run on towards OpenAI,
        // so the test stops here, before any network request.
        assert!(
            self.service
                .inner
                .chatgpt_sign_in
                .lock()
                .expect("slot")
                .is_some(),
            "no cancel token is registered"
        );
        self.service.settings_cancel_chatgpt_sign_in();
        true
    }
}

#[test]
fn a_cancel_during_the_confirmation_is_kept_and_nothing_is_recorded() {
    let fixture = Fixture::new();
    // The opener would fail the test's expectations if it were reached.
    let service = fixture.service(unreachable_opener);
    let before = fixture.store().revision();
    let error = service
        .settings_sign_in_chatgpt(
            fixture.request("fresh"),
            &CancelDuringConfirmation { service: &service },
        )
        .expect_err("cancelled");
    assert_eq!(error.code(), "cancelled", "{}", error.message());
    let store = fixture.store();
    assert_eq!(store.revision(), before);
    assert!(store.provider_profile("fresh").is_none());
    assert!(
        service
            .inner
            .chatgpt_sign_in
            .lock()
            .expect("slot")
            .is_none()
    );
}

/// Set once [`stalled_opener`] has started stalling.
static OPENER_STALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Stalls like a hung URL handler until the sign-in's cancel token is set,
/// for at most ten seconds, then reports the hand-off as made.
fn stalled_opener(_address: &str, cancel: &CancelToken) -> Result<(), SignInError> {
    OPENER_STALLED.store(true, std::sync::atomic::Ordering::SeqCst);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !cancel.is_cancelled() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Ok(())
}

#[test]
fn a_cancel_during_a_stalled_browser_step_ends_the_sign_in_and_frees_the_slot() {
    let fixture = Fixture::new();
    let service = fixture.service(stalled_opener);
    let started = std::time::Instant::now();
    let error = std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !OPENER_STALLED.load(std::sync::atomic::Ordering::SeqCst) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the browser step did not start"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            service.settings_cancel_chatgpt_sign_in();
        });
        service
            .settings_sign_in_chatgpt(fixture.request("fresh"), &Answers::with(&[true]))
            .expect_err("cancelled")
    });
    assert_eq!(error.code(), "cancelled", "{}", error.message());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "the cancel must end the stalled browser step"
    );
    assert!(
        service
            .inner
            .chatgpt_sign_in
            .lock()
            .expect("slot")
            .is_none(),
        "the sign-in slot is released"
    );
    let store = fixture.store();
    let profile = store.provider_profile("fresh").expect("recorded profile");
    assert!(profile.credential().is_none(), "no token is recorded");
}

#[test]
fn cancel_without_a_sign_in_does_nothing() {
    let fixture = Fixture::new();
    let service = fixture.service(unreachable_opener);
    service.settings_cancel_chatgpt_sign_in();
    assert!(
        service
            .inner
            .chatgpt_sign_in
            .lock()
            .expect("slot")
            .is_none()
    );
}

#[test]
fn cancel_sets_the_running_sign_ins_token() {
    let fixture = Fixture::new();
    let service = fixture.service(unreachable_opener);
    let cancel = omgcode_core::turn::CancelToken::new();
    *service.inner.chatgpt_sign_in.lock().expect("slot") = Some(cancel.clone());
    service.settings_cancel_chatgpt_sign_in();
    assert!(cancel.is_cancelled());
}

#[test]
fn signing_out_a_profile_that_is_not_signed_in_is_refused_before_asking() {
    let fixture = Fixture::new();
    let service = fixture.service(unreachable_opener);
    for name in ["plan", "keys", "missing"] {
        let answers = Answers::with(&[]);
        let error: Result<ChatGptSignOutResult, _> =
            service.settings_sign_out_chatgpt(fixture.request(name), &answers);
        let error = error.expect_err("not signed in");
        assert_eq!(error.code(), "credential", "{name}: {}", error.message());
        assert!(answers.titles().is_empty());
    }
}

#[test]
fn a_cancel_during_the_sign_out_confirmation_keeps_the_session() {
    let fixture = Fixture::new();
    {
        let mut store = fixture.store();
        let credentials = CredentialStore::open(&fixture.data).expect("credential store");
        let host = new_host_id().expect("host");
        let request = store
            .subscription_sign_in_request("plan", &host)
            .expect("request");
        let revision = store.revision();
        let _recorded = store
            .record_provider_subscription(
                &credentials,
                "plan",
                &request,
                &SignedIn {
                    client_id: "oaiapp_test".to_owned(),
                    subject: "subject-1".to_owned(),
                    email: None,
                    tokens: TokenSet {
                        access_token: Zeroizing::new("access-token".to_owned()),
                        refresh_token: Zeroizing::new("refresh-token".to_owned()),
                        scopes: vec![PLAN_SCOPE.to_owned()],
                        expires_at: 4_000_000_000,
                    },
                },
                revision,
            )
            .expect("recorded session");
    }
    let service = fixture.service(unreachable_opener);
    let before = fixture.store().revision();
    let error: Result<ChatGptSignOutResult, _> = service.settings_sign_out_chatgpt(
        fixture.request("plan"),
        &CancelDuringConfirmation { service: &service },
    );
    let error = error.expect_err("cancelled");
    assert_eq!(error.code(), "cancelled", "{}", error.message());
    let store = fixture.store();
    assert_eq!(store.revision(), before);
    assert!(
        store
            .provider_profile("plan")
            .and_then(|status| status.credential())
            .is_some()
    );
    assert!(
        service
            .inner
            .chatgpt_sign_in
            .lock()
            .expect("slot")
            .is_none()
    );
    // Remove the made-up session's tokens without contacting OpenAI.
    drop(store);
    let mut store = fixture.store();
    let revision = store.revision();
    let credentials = CredentialStore::open(&fixture.data).expect("credential store");
    let removal = store
        .sign_out_provider_subscription(&credentials, "plan", revision)
        .expect("local sign-out");
    assert!(removal.orphans.is_empty());
}

#[test]
fn a_poisoned_sign_in_slot_is_internal_not_busy() {
    let fixture = Fixture::new();
    let service = fixture.service(unreachable_opener);
    let inner = Arc::clone(&service.inner);
    let _ = std::thread::spawn(move || {
        let _held = inner.chatgpt_sign_in.lock().expect("slot");
        panic!("poison the slot");
    })
    .join();
    assert!(service.inner.chatgpt_sign_in.is_poisoned());
    let sign_in = service
        .settings_sign_in_chatgpt(fixture.request("fresh"), &Answers::with(&[]))
        .expect_err("poisoned");
    assert_eq!(sign_in.code(), "internal", "{}", sign_in.message());
    let sign_out: Result<ChatGptSignOutResult, _> =
        service.settings_sign_out_chatgpt(fixture.request("plan"), &Answers::with(&[]));
    let sign_out = sign_out.expect_err("poisoned");
    assert_eq!(sign_out.code(), "internal", "{}", sign_out.message());
}
