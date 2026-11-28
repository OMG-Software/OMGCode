//! A Claude Code connection in the desktop shell (ADR-0111): added from the
//! `settings` window through native dialogs and one confirmation, chosen for
//! one session only, offered its aliases and efforts, refused as the default
//! connection, and stale once removed. A scripted prompt stands in for the
//! native dialogs. No `claude` program runs here: the program is an empty
//! file, and every turn is refused before any process could start.

mod common;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use omgcode_app::TokenSource;
use omgcode_app::desktop::{
    Confirmation, ConnectionOrigin, ConnectionStatus, DesktopConfig, DesktopError, DesktopService,
    FilePurpose, FolderPurpose, ReasoningEffort, SettingsOverview, SettingsPrompt,
};
use omgcode_core::control::{ClaudeCodeDeclaration, ControlStore};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

use common::{Fixture, NoCallServer, settings_json, text_only, wire};

/// The label a session records for the connection `claude`.
const LABEL: &str = "claude-code:claude";

/// Answers the native dialogs in order and records every confirmation.
#[derive(Default)]
struct Scripted {
    files: RefCell<VecDeque<Option<PathBuf>>>,
    folders: RefCell<VecDeque<Option<PathBuf>>>,
    answers: RefCell<VecDeque<bool>>,
    shown: RefCell<Vec<Confirmation>>,
}

impl Scripted {
    /// Only confirmations, answered in order; a dialog is unexpected.
    fn answers(answers: &[bool]) -> Self {
        Self {
            answers: RefCell::new(answers.iter().copied().collect()),
            ..Self::default()
        }
    }

    /// A file dialog, a folder dialog, and confirmations, each in order.
    fn dialogs(file: Option<&Path>, folder: Option<&Path>, answers: &[bool]) -> Self {
        Self {
            files: RefCell::new(VecDeque::from([file.map(Path::to_path_buf)])),
            folders: RefCell::new(VecDeque::from([folder.map(Path::to_path_buf)])),
            answers: RefCell::new(answers.iter().copied().collect()),
            shown: RefCell::default(),
        }
    }

    fn shown(&self) -> Vec<Confirmation> {
        self.shown.take()
    }
}

impl SettingsPrompt for Scripted {
    fn pick_folder(&self, purpose: FolderPurpose, _start: Option<&Path>) -> Option<PathBuf> {
        assert_eq!(purpose, FolderPurpose::ClaudeCodeSignIn);
        self.folders
            .borrow_mut()
            .pop_front()
            .expect("an unexpected folder dialog")
    }

    fn pick_file(&self, purpose: FilePurpose, _start: Option<&Path>) -> Option<PathBuf> {
        assert_eq!(purpose, FilePurpose::ClaudeCodeProgram);
        self.files
            .borrow_mut()
            .pop_front()
            .expect("an unexpected file dialog")
    }

    fn confirm(&self, confirmation: &Confirmation) -> bool {
        self.shown.borrow_mut().push(confirmation.clone());
        self.answers
            .borrow_mut()
            .pop_front()
            .expect("an unexpected confirmation")
    }
}

/// A `claude` program and a sign-in folder outside application storage, the
/// development root, and every workspace, as core requires. The program is
/// an empty file: nothing here starts it.
struct Outside {
    _temporary: TempDir,
    program: PathBuf,
    sign_in: PathBuf,
    root: PathBuf,
}

impl Outside {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("outside folder");
        let root = temporary.path().to_path_buf();
        let bin = root.join("bin");
        fs::create_dir_all(&bin).expect("program folder");
        let program = bin.join(if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        });
        fs::write(&program, b"").expect("program file");
        let sign_in = root.join("sign-in");
        fs::create_dir_all(&sign_in).expect("sign-in folder");
        Self {
            _temporary: temporary,
            program,
            sign_in,
            root,
        }
    }
}

fn open(fixture: &Fixture, default: &str, relay: Option<PathBuf>) -> DesktopService {
    DesktopService::open(DesktopConfig {
        data_root: fixture.data.clone(),
        endpoint: default.to_owned(),
        token_source: TokenSource::Active,
        stream: false,
        claude_code_relay: relay,
    })
    .expect("open desktop service")
}

fn revision(fixture: &Fixture) -> u64 {
    ControlStore::open(&fixture.data)
        .expect("control store")
        .revision()
}

fn profile_count(fixture: &Fixture) -> usize {
    ControlStore::open(&fixture.data)
        .expect("control store")
        .provider_profiles()
        .len()
}

/// Record the connection `claude` the way the trusted CLI does.
fn record(fixture: &Fixture, outside: &Outside) {
    record_program(fixture, &outside.program, &outside.sign_in);
}

/// Record the connection `claude` with `program` and `sign_in`.
fn record_program(fixture: &Fixture, program: &Path, sign_in: &Path) {
    let mut control = ControlStore::open(&fixture.data).expect("control store");
    control
        .add_claude_code_profile(
            &ClaudeCodeDeclaration {
                name: "claude".to_owned(),
                program: program.to_path_buf(),
                config_folder: sign_in.to_path_buf(),
            },
            control.revision(),
        )
        .expect("Claude Code connection");
}

fn add(
    service: &DesktopService,
    fixture: &Fixture,
    name: &str,
    prompt: &dyn SettingsPrompt,
) -> Result<SettingsOverview, DesktopError> {
    service.settings_add_claude_code(
        wire(json!({ "revision": revision(fixture), "name": name })),
        prompt,
    )
}

fn grant(
    service: &DesktopService,
    fixture: &Fixture,
    prompt: &dyn SettingsPrompt,
) -> Result<SettingsOverview, DesktopError> {
    service.settings_grant_claude_code(
        wire(json!({ "revision": revision(fixture), "name": "claude" })),
        prompt,
    )
}

fn revoke(
    service: &DesktopService,
    fixture: &Fixture,
    name: &str,
    prompt: &dyn SettingsPrompt,
) -> Result<SettingsOverview, DesktopError> {
    service.settings_revoke_claude_code(
        wire(json!({ "revision": revision(fixture), "name": name })),
        prompt,
    )
}

fn session(service: &DesktopService, workspace: Uuid) -> Uuid {
    service
        .session_create(wire(json!({"workspace_id": workspace, "title": "t"})))
        .expect("session")
        .session_id
}

fn status(service: &DesktopService, session: Uuid) -> ConnectionStatus {
    service
        .connection_status(wire(json!({ "session_id": session })))
        .expect("status")
}

fn choose(
    service: &DesktopService,
    session: Uuid,
    profile: Option<&str>,
    prompt: &dyn SettingsPrompt,
) -> Result<ConnectionStatus, DesktopError> {
    service.connection_select(
        wire(json!({ "session_id": session, "profile": profile })),
        prompt,
    )
}

fn effort(
    service: &DesktopService,
    session: Uuid,
    effort: &str,
) -> Result<ConnectionStatus, DesktopError> {
    service.session_effort_select(wire(
        json!({ "session_id": session, "reasoning_effort": effort }),
    ))
}

fn send(service: &DesktopService, session: Uuid, endpoint: &str) -> DesktopError {
    service
        .chat_send(wire(json!({
            "session_id": session,
            "message": "hello",
            "settings": settings_json(endpoint),
            "skills": [],
        })))
        .expect_err("the turn is refused")
}

/// Adding records the program and the folder the scripted dialogs chose,
/// after one confirmation that names both, and grants nothing.
#[test]
fn a_claude_code_connection_is_added_through_native_dialogs_and_one_confirmation() {
    let fixture = Fixture::new();
    let outside = Outside::new();
    let default = NoCallServer::start();
    let service = open(&fixture, &default.endpoint, None);
    let before = revision(&fixture);

    let prompt = Scripted::dialogs(Some(&outside.program), Some(&outside.sign_in), &[true]);
    let overview = add(&service, &fixture, "claude", &prompt).expect("added");

    let shown = prompt.shown();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].title, "Add a Claude Code connection");
    assert!(shown[0].message.contains("claude"), "{}", shown[0].message);
    assert!(shown[0].message.contains("sign-in"), "{}", shown[0].message);
    assert!(
        shown[0].message.contains("OMGCode never reads the sign-in"),
        "{}",
        shown[0].message
    );
    assert_eq!(revision(&fixture), before + 1);
    let profile = overview
        .provider_profiles
        .iter()
        .find(|profile| profile.name == "claude")
        .expect("listed");
    assert_eq!(profile.kind, "claude_code");
    assert_eq!(profile.endpoint, None);
    assert!(!profile.credential_bound);
    // It is never the default connection, so the window's choice list keeps
    // it out; a session chooses it instead.
    assert!(!profile.selectable);
    let setup = profile.claude_code.as_ref().expect("its setup is shown");
    assert!(setup.program.contains("claude"), "{}", setup.program);
    assert!(
        setup.config_folder.contains("sign-in"),
        "{}",
        setup.config_folder
    );
    assert!(setup.grants.is_empty());
}

/// Each cancelled dialog, a refused folder, and No record nothing. A
/// refusal comes before the confirmation, so the operator is never asked to
/// confirm a choice core would refuse.
#[test]
fn adding_a_claude_code_connection_records_nothing_when_cancelled_or_refused() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let outside = Outside::new();
    let default = NoCallServer::start();
    let service = open(&fixture, &default.endpoint, None);
    let before = revision(&fixture);

    let no_program = Scripted::dialogs(None, Some(&outside.sign_in), &[]);
    assert_eq!(
        add(&service, &fixture, "claude", &no_program)
            .expect_err("cancelled")
            .code(),
        "cancelled"
    );
    let no_folder = Scripted::dialogs(Some(&outside.program), None, &[]);
    assert_eq!(
        add(&service, &fixture, "claude", &no_folder)
            .expect_err("cancelled")
            .code(),
        "cancelled"
    );
    // A sign-in folder inside a workspace could be read by a model-run
    // program, so core refuses it before any confirmation.
    let inside = Scripted::dialogs(Some(&outside.program), Some(&workspace.path), &[]);
    let refused = add(&service, &fixture, "claude", &inside).expect_err("refused");
    assert_eq!(refused.code(), "control");
    assert!(inside.shown().is_empty());
    let declined = Scripted::dialogs(Some(&outside.program), Some(&outside.sign_in), &[false]);
    assert_eq!(
        add(&service, &fixture, "claude", &declined)
            .expect_err("cancelled")
            .code(),
        "cancelled"
    );
    assert_eq!(declined.shown().len(), 1);
    // An invalid name is core's refusal too, also before the confirmation.
    let bad_name = Scripted::dialogs(Some(&outside.program), Some(&outside.sign_in), &[]);
    assert_eq!(
        add(&service, &fixture, "no spaces", &bad_name)
            .expect_err("refused")
            .code(),
        "control"
    );

    assert_eq!(revision(&fixture), before);
    assert_eq!(profile_count(&fixture), 0);
}

/// The whole session path: the choice lists the connection by its label, is
/// confirmed with a message that names the program and the sign-in folder,
/// offers Claude Code's aliases and efforts, and leaves another session on
/// the default. A turn without a relay program is refused before any process.
#[test]
fn a_session_chooses_a_claude_code_connection_and_gets_its_aliases_and_efforts() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let outside = Outside::new();
    record(&fixture, &outside);
    let default = NoCallServer::start();
    let service = open(&fixture, &default.endpoint, None);
    let first = session(&service, workspace.id);
    let second = session(&service, workspace.id);

    let before = status(&service, first);
    let offered = before
        .choices
        .iter()
        .find(|choice| choice.name == "claude")
        .expect("a session may choose it");
    assert_eq!(offered.kind, "claude_code");
    assert_eq!(offered.endpoint, LABEL);

    let prompt = Scripted::answers(&[true]);
    let chosen = choose(&service, first, Some("claude"), &prompt).expect("chosen");
    let shown = prompt.shown();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].title, "Choose the model connection");
    for part in [
        "Claude Code connection claude",
        "Program: ",
        "Sign-in folder: ",
        "Anthropic",
    ] {
        assert!(
            shown[0].message.contains(part),
            "{part}: {}",
            shown[0].message
        );
    }
    assert_eq!(chosen.origin, ConnectionOrigin::Session);
    assert_eq!(chosen.connection.endpoint, LABEL);
    assert_eq!(chosen.connection.kind, Some("claude_code"));
    assert!(!chosen.stale);
    assert_eq!(
        chosen.reasoning_efforts,
        [
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
            ReasoningEffort::Max,
        ]
    );
    // Another session keeps following the default.
    assert_eq!(status(&service, second).origin, ConnectionOrigin::Default);

    // Claude Code takes `max` and refuses an effort it has no flag for.
    let maximum = effort(&service, first, "max").expect("max");
    assert_eq!(maximum.reasoning_effort, Some(ReasoningEffort::Max));
    assert_eq!(
        effort(&service, first, "minimal")
            .expect_err("refused")
            .code(),
        "invalid_request"
    );

    // The model list is Claude Code's aliases; no server is asked.
    let models = service
        .chat_models(wire(json!({ "session_id": first })))
        .expect("models");
    assert_eq!(models.endpoint, LABEL);
    assert_eq!(models.kind, Some("claude_code"));
    let ids: Vec<&str> = models
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["sonnet", "opus", "haiku", "fable", "sonnet[1m]", "opus[1m]"]
    );
    assert!(models.models.iter().all(|model| !model.cloud_tag));

    // Choosing it again is no change and asks nothing.
    let again = Scripted::answers(&[]);
    assert_eq!(
        choose(&service, first, Some("claude"), &again)
            .expect_err("unchanged")
            .code(),
        "control"
    );

    // Without an `omgcode` relay beside the application the turn is refused
    // before the program is started.
    let refused = send(&service, first, LABEL);
    assert_eq!(refused.code(), "credential");
    assert!(
        refused.message().starts_with("claude_code_unusable"),
        "{}",
        refused.message()
    );
}

/// A connection removed and added again under the same name is another
/// profile, with its own program: a session that confirmed the first one is
/// stale until it confirms the replacement, and choosing it again is allowed.
#[test]
fn a_connection_added_again_under_the_same_name_needs_the_sessions_confirmation() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let outside = Outside::new();
    record(&fixture, &outside);
    let default = NoCallServer::start();
    let service = open(&fixture, &default.endpoint, None);
    let first = session(&service, workspace.id);
    choose(&service, first, Some("claude"), &Scripted::answers(&[true])).expect("chosen");

    service
        .settings_remove_connection(
            wire(json!({ "revision": revision(&fixture), "name": "claude" })),
            &Scripted::answers(&[]),
        )
        .expect("removed");
    let other = outside.root.join("other");
    fs::create_dir_all(&other).expect("another program folder");
    let replacement = other.join(if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    });
    fs::write(&replacement, b"").expect("another program");
    record_program(&fixture, &replacement, &outside.sign_in);

    // The label is the same, but the session confirmed another profile.
    let after = status(&service, first);
    assert_eq!(after.connection.endpoint, LABEL);
    assert!(after.stale);
    assert_eq!(
        send(&service, first, LABEL).code(),
        "session_connection_stale"
    );
    assert_eq!(
        service
            .chat_models(wire(json!({ "session_id": first })))
            .expect_err("stale")
            .code(),
        "session_connection_stale"
    );

    // Choosing it again is a change, confirmed with the replacement's program.
    let prompt = Scripted::answers(&[true]);
    let chosen = choose(&service, first, Some("claude"), &prompt).expect("chosen again");
    let shown = prompt.shown();
    assert_eq!(shown.len(), 1);
    assert!(shown[0].message.contains("other"), "{}", shown[0].message);
    assert!(!chosen.stale);
    assert_eq!(chosen.connection.endpoint, LABEL);
}

/// Declining the choice records nothing for the session.
#[test]
fn a_declined_claude_code_choice_leaves_the_session_on_the_default() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let outside = Outside::new();
    record(&fixture, &outside);
    let default = NoCallServer::start();
    let service = open(&fixture, &default.endpoint, None);
    let first = session(&service, workspace.id);

    let prompt = Scripted::answers(&[false]);
    assert_eq!(
        choose(&service, first, Some("claude"), &prompt)
            .expect_err("cancelled")
            .code(),
        "cancelled"
    );
    let after = status(&service, first);
    assert_eq!(after.origin, ConnectionOrigin::Default);
    assert_eq!(after.connection.endpoint, default.endpoint);
}

/// The global selection still refuses the kind, before any confirmation.
#[test]
fn the_default_connection_cannot_be_a_claude_code_connection() {
    let fixture = Fixture::new();
    let outside = Outside::new();
    record(&fixture, &outside);
    let default = NoCallServer::start();
    let service = open(&fixture, &default.endpoint, None);
    let before = revision(&fixture);

    let prompt = Scripted::answers(&[]);
    let refused = service
        .settings_select_connection(
            wire(json!({ "revision": before, "profile": "claude" })),
            &prompt,
        )
        .expect_err("refused");
    assert_eq!(refused.code(), "control");
    assert_eq!(revision(&fixture), before);
}

/// A session whose Claude Code connection was removed is stale: its next
/// turn and its model list are refused, and nothing reaches the default.
#[test]
fn removing_a_claude_code_connection_makes_the_sessions_choice_stale() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let outside = Outside::new();
    record(&fixture, &outside);
    let default = NoCallServer::start();
    let service = open(&fixture, &default.endpoint, None);
    let first = session(&service, workspace.id);
    choose(&service, first, Some("claude"), &Scripted::answers(&[true])).expect("chosen");

    // Removing a connection that is not the default asks nothing.
    service
        .settings_remove_connection(
            wire(json!({ "revision": revision(&fixture), "name": "claude" })),
            &Scripted::answers(&[]),
        )
        .expect("removed");

    assert!(status(&service, first).stale);
    assert_eq!(
        send(&service, first, LABEL).code(),
        "session_connection_stale"
    );
    assert_eq!(
        service
            .chat_models(wire(json!({ "session_id": first })))
            .expect_err("stale")
            .code(),
        "session_connection_stale"
    );
}

/// Revoking with nothing granted confirms first and succeeds; an unknown
/// connection is refused before the confirmation.
#[test]
fn revoking_a_claude_code_connection_confirms_first() {
    let fixture = Fixture::new();
    let outside = Outside::new();
    record(&fixture, &outside);
    let default = NoCallServer::start();
    let service = open(&fixture, &default.endpoint, None);

    let unknown = Scripted::answers(&[]);
    assert_eq!(
        revoke(&service, &fixture, "missing", &unknown)
            .expect_err("refused")
            .code(),
        "control"
    );
    let declined = Scripted::answers(&[false]);
    assert_eq!(
        revoke(&service, &fixture, "claude", &declined)
            .expect_err("cancelled")
            .code(),
        "cancelled"
    );
    let prompt = Scripted::answers(&[true]);
    let overview = revoke(&service, &fixture, "claude", &prompt).expect("revoked");
    let shown = prompt.shown();
    assert_eq!(shown.len(), 1);
    assert!(
        shown[0].message.contains("30 seconds"),
        "{}",
        shown[0].message
    );
    let profile = overview
        .provider_profiles
        .iter()
        .find(|profile| profile.name == "claude")
        .expect("still recorded");
    assert!(
        profile
            .claude_code
            .as_ref()
            .expect("setup")
            .grants
            .is_empty()
    );
}

/// Outside Windows no container grant exists, so granting is refused.
#[cfg(not(windows))]
#[test]
fn granting_a_claude_code_connection_is_windows_only() {
    let fixture = Fixture::new();
    let outside = Outside::new();
    record(&fixture, &outside);
    let default = NoCallServer::start();
    let service = open(
        &fixture,
        &default.endpoint,
        Some(outside.root.join("omgcode")),
    );
    let refused = grant(&service, &fixture, &Scripted::answers(&[])).expect_err("refused");
    assert_eq!(refused.code(), "unsupported_platform");
}

/// On Windows a grant needs the relay beside the application, and its one
/// confirmation names the four folders; No grants nothing. A grant that the
/// operator confirms changes access lists, which the CLI's acceptance test
/// covers.
#[cfg(windows)]
#[test]
fn granting_a_claude_code_connection_needs_a_relay_and_names_every_folder() {
    let fixture = Fixture::new();
    let outside = Outside::new();
    record(&fixture, &outside);
    let default = NoCallServer::start();

    let without = open(&fixture, &default.endpoint, None);
    let refused = grant(&without, &fixture, &Scripted::answers(&[])).expect_err("refused");
    assert_eq!(refused.code(), "invalid_request");
    drop(without);

    let relay_folder = outside.root.join("omgcode");
    fs::create_dir_all(&relay_folder).expect("relay folder");
    let relay = relay_folder.join("omgcode.exe");
    fs::write(&relay, b"").expect("relay file");
    let service = open(&fixture, &default.endpoint, Some(relay.clone()));
    let before = revision(&fixture);
    let prompt = Scripted::answers(&[false]);
    assert_eq!(
        grant(&service, &fixture, &prompt)
            .expect_err("cancelled")
            .code(),
        "cancelled"
    );
    let shown = prompt.shown();
    assert_eq!(shown.len(), 1);
    assert_eq!(
        shown[0].title,
        "Grant folders to the Claude Code connection"
    );
    for part in ["claude.exe", "sign-in", "claude-code", "omgcode"] {
        assert!(
            shown[0].message.contains(part),
            "{part}: {}",
            shown[0].message
        );
    }
    assert_eq!(revision(&fixture), before);
    assert!(
        ControlStore::open(&fixture.data)
            .expect("control store")
            .claude_code_grants()
            .is_empty()
    );

    // A turn through a connection whose container holds no grant is refused
    // before the program is started.
    let workspace = fixture.workspace("project", text_only());
    let first = session(&service, workspace.id);
    choose(&service, first, Some("claude"), &Scripted::answers(&[true])).expect("chosen");
    let refused = send(&service, first, LABEL);
    assert_eq!(refused.code(), "credential");
    assert!(
        refused.message().contains("lacks an active grant"),
        "{}",
        refused.message()
    );
}
