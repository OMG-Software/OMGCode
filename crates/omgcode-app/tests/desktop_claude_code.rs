//! A Claude Code connection in the desktop shell (ADR-0111): found on the
//! search path or chosen in a file dialog, added from the `settings` window
//! with its own new sign-in folder after one confirmation, granted on
//! Windows, signed in through a recorded sign-in start, chosen for one
//! session only, offered its aliases and efforts, refused as the default
//! connection, and stale once removed. A scripted prompt stands in for the
//! native dialogs and records each sign-in instead of starting it. No
//! `claude` program runs here: the program is an empty file, and every turn
//! is refused before any process could start. On Windows an accepted add
//! changes real access lists on temporary folders and revokes them again.

mod common;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use omgcode_app::TokenSource;
use omgcode_app::desktop::{
    ClaudeCodeHost, ClaudeCodeSignIn, Confirmation, ConnectionOrigin, ConnectionStatus,
    DesktopConfig, DesktopError, DesktopService, FilePurpose, FolderPurpose, ReasoningEffort,
    SettingsOverview, SettingsPrompt,
};
use omgcode_core::control::{ClaudeCodeDeclaration, ControlStore};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

use common::{Fixture, NoCallServer, settings_json, text_only, wire};

/// The label a session records for the connection `claude`.
const LABEL: &str = "claude-code:claude";

/// Answers the native dialogs in order, records every confirmation, and
/// records each sign-in start, answering it with `sign_in_failure`.
#[derive(Default)]
struct Scripted {
    files: RefCell<VecDeque<Option<PathBuf>>>,
    answers: RefCell<VecDeque<bool>>,
    shown: RefCell<Vec<Confirmation>>,
    sign_ins: RefCell<Vec<ClaudeCodeSignIn>>,
    sign_in_failure: Option<String>,
}

impl Scripted {
    /// Only confirmations, answered in order; a dialog is unexpected.
    fn answers(answers: &[bool]) -> Self {
        Self {
            answers: RefCell::new(answers.iter().copied().collect()),
            ..Self::default()
        }
    }

    /// One file dialog, then confirmations, each in order.
    fn file(file: Option<&Path>, answers: &[bool]) -> Self {
        Self {
            files: RefCell::new(VecDeque::from([file.map(Path::to_path_buf)])),
            ..Self::answers(answers)
        }
    }

    fn shown(&self) -> Vec<Confirmation> {
        self.shown.take()
    }

    fn sign_ins(&self) -> Vec<ClaudeCodeSignIn> {
        self.sign_ins.take()
    }
}

impl SettingsPrompt for Scripted {
    fn pick_folder(&self, purpose: FolderPurpose, _start: Option<&Path>) -> Option<PathBuf> {
        panic!("an unexpected folder dialog: {purpose:?}");
    }

    fn start_claude_code_sign_in(&self, sign_in: &ClaudeCodeSignIn) -> Result<(), String> {
        self.sign_ins.borrow_mut().push(sign_in.clone());
        self.sign_in_failure.clone().map_or(Ok(()), Err)
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

/// A `claude` program, a sign-in folder, a relay, and a parent for new
/// sign-in folders, outside application storage, the development root, and
/// every workspace, as core requires. The program and the relay are empty
/// files: nothing here starts them.
struct Outside {
    _temporary: TempDir,
    program: PathBuf,
    sign_in: PathBuf,
    root: PathBuf,
    relay: PathBuf,
    sign_ins: PathBuf,
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
        let relay_folder = root.join("relay");
        fs::create_dir_all(&relay_folder).expect("relay folder");
        let relay = relay_folder.join(if cfg!(windows) {
            "omgcode.exe"
        } else {
            "omgcode"
        });
        fs::write(&relay, b"").expect("relay file");
        let sign_ins = root.join("sign-ins");
        Self {
            _temporary: temporary,
            program,
            sign_in,
            root,
            relay,
            sign_ins,
        }
    }

    /// The host Add uses: `search` as `PATH`, this relay, and this parent
    /// for new sign-in folders.
    fn host(&self, search: &[&Path]) -> ClaudeCodeHost {
        ClaudeCodeHost {
            relay: Some(self.relay.clone()),
            search_path: Some(std::env::join_paths(search).expect("search path")),
            home: None,
            sign_in_root: Some(self.sign_ins.clone()),
        }
    }

    fn bin(&self) -> &Path {
        self.program.parent().expect("program folder")
    }

    /// The sign-in folders Add left behind.
    fn created(&self) -> Vec<PathBuf> {
        fs::read_dir(&self.sign_ins).map_or_else(
            |_| Vec::new(),
            |entries| entries.map(|entry| entry.expect("entry").path()).collect(),
        )
    }
}

fn open(fixture: &Fixture, default: &str, relay: Option<PathBuf>) -> DesktopService {
    open_host(
        fixture,
        default,
        ClaudeCodeHost {
            relay,
            ..ClaudeCodeHost::default()
        },
    )
}

fn open_host(fixture: &Fixture, default: &str, claude_code: ClaudeCodeHost) -> DesktopService {
    DesktopService::open(DesktopConfig {
        data_root: fixture.data.clone(),
        endpoint: default.to_owned(),
        token_source: TokenSource::Active,
        stream: false,
        claude_code,
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

/// The variables a sign-in may carry: the listed operator folders and the
/// ones OMGCode sets. Any other, such as an Anthropic key, is never passed.
const SIGN_IN_VARIABLES: [&str; 16] = [
    "SystemRoot",
    "windir",
    "SystemDrive",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "TEMP",
    "TMP",
    "PATH",
    "ComSpec",
    "PATHEXT",
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_CODE_SAFE_MODE",
    "DISABLE_AUTOUPDATER",
    "DISABLE_TELEMETRY",
    "HOME",
];

/// Adding finds the program on the search path, skipping one in a workspace
/// that core refuses, creates a new empty sign-in folder, and asks one
/// confirmation. On Windows it also grants the four folders and starts
/// Claude's sign-in once, with `CLAUDE_CONFIG_DIR` set to that folder;
/// elsewhere the confirmation names the terminal command instead.
#[test]
fn a_claude_code_connection_is_found_created_granted_and_signed_in_after_one_confirmation() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let outside = Outside::new();
    let planted = workspace
        .path
        .join(outside.program.file_name().expect("name"));
    fs::write(&planted, b"").expect("planted program");
    let default = NoCallServer::start();
    let service = open_host(
        &fixture,
        &default.endpoint,
        outside.host(&[&workspace.path, outside.bin()]),
    );
    let before = revision(&fixture);

    let prompt = Scripted::answers(&[true]);
    let overview = add(&service, &fixture, "claude", &prompt).expect("added");

    let shown = prompt.shown();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].title, "Add a Claude Code connection");
    assert!(
        shown[0].message.contains("new and empty"),
        "{}",
        shown[0].message
    );
    assert!(
        shown[0].message.contains("OMGCode never reads the sign-in"),
        "{}",
        shown[0].message
    );
    let created = outside.created();
    assert_eq!(created.len(), 1);
    let folder = &created[0];
    assert_eq!(fs::read_dir(folder).expect("listed").count(), 0);
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
    assert!(setup.program.contains("bin"), "{}", setup.program);
    let name = folder.file_name().expect("folder name").to_string_lossy();
    assert!(
        setup.config_folder.contains(name.as_ref()),
        "{}",
        setup.config_folder
    );

    let sign_ins = prompt.sign_ins();
    if cfg!(windows) {
        assert!(
            shown[0].message.contains("claude auth login"),
            "{}",
            shown[0].message
        );
        assert!(shown[0].message.contains("relay"), "{}", shown[0].message);
        assert_eq!(setup.grants.len(), 4, "{:?}", setup.grants);
        assert!(setup.grants.iter().all(|grant| grant.state == "active"));
        assert_eq!(sign_ins.len(), 1);
        let sign_in = &sign_ins[0];
        assert_eq!(&sign_in.folder, folder);
        assert_eq!(sign_in.arguments, ["auth", "login"]);
        assert!(
            sign_in
                .program
                .to_string_lossy()
                .ends_with(&*outside.program.file_name().expect("name").to_string_lossy())
        );
        assert!(
            sign_in
                .environment
                .iter()
                .any(|(name, value)| name == "CLAUDE_CONFIG_DIR" && Path::new(value) == folder)
        );
        for (name, _) in &sign_in.environment {
            assert!(
                SIGN_IN_VARIABLES.iter().any(|kept| name == *kept),
                "{name:?} is not a sign-in variable"
            );
        }
        // Revoking removes the grants and the sandbox this test made.
        revoke(&service, &fixture, "claude", &Scripted::answers(&[true])).expect("revoked");
    } else {
        assert!(
            shown[0].message.contains("CLAUDE_CONFIG_DIR"),
            "{}",
            shown[0].message
        );
        assert!(setup.grants.is_empty());
        assert!(sign_ins.is_empty());
        assert_eq!(revision(&fixture), before + 1);
    }
}

/// When no program is found, a native file dialog chooses it; the
/// confirmation names that program.
#[test]
fn a_program_is_chosen_in_a_file_dialog_when_none_is_found() {
    let fixture = Fixture::new();
    let outside = Outside::new();
    let default = NoCallServer::start();
    let service = open_host(&fixture, &default.endpoint, outside.host(&[]));
    let before = revision(&fixture);

    let prompt = Scripted::file(Some(&outside.program), &[false]);
    assert_eq!(
        add(&service, &fixture, "claude", &prompt)
            .expect_err("cancelled")
            .code(),
        "cancelled"
    );
    let shown = prompt.shown();
    assert_eq!(shown.len(), 1);
    assert!(shown[0].message.contains("bin"), "{}", shown[0].message);
    assert!(prompt.files.borrow().is_empty());
    assert!(prompt.sign_ins().is_empty());
    assert!(outside.created().is_empty());
    assert_eq!(revision(&fixture), before);
}

/// A cancelled dialog, a refused program or name, and No record nothing
/// and leave no sign-in folder. A refusal comes before the confirmation, so
/// the operator is never asked to confirm a choice core would refuse.
#[test]
fn adding_a_claude_code_connection_records_nothing_when_cancelled_or_refused() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let outside = Outside::new();
    let default = NoCallServer::start();
    let before = revision(&fixture);

    let nothing_found = open_host(&fixture, &default.endpoint, outside.host(&[]));
    let no_program = Scripted::file(None, &[]);
    assert_eq!(
        add(&nothing_found, &fixture, "claude", &no_program)
            .expect_err("cancelled")
            .code(),
        "cancelled"
    );
    // A program inside a workspace could be replaced by a model-run
    // program, so core refuses it before any confirmation.
    let planted = workspace
        .path
        .join(outside.program.file_name().expect("name"));
    fs::write(&planted, b"").expect("planted program");
    let inside = Scripted::file(Some(&planted), &[]);
    let refused = add(&nothing_found, &fixture, "claude", &inside).expect_err("refused");
    assert_eq!(refused.code(), "control");
    assert!(inside.shown().is_empty());
    drop(nothing_found);

    let service = open_host(&fixture, &default.endpoint, outside.host(&[outside.bin()]));
    let declined = Scripted::answers(&[false]);
    assert_eq!(
        add(&service, &fixture, "claude", &declined)
            .expect_err("cancelled")
            .code(),
        "cancelled"
    );
    assert_eq!(declined.shown().len(), 1);
    assert!(declined.sign_ins().is_empty());
    // An invalid name is core's refusal too, before the confirmation and
    // without a file dialog, although a program was found.
    let bad_name = Scripted::answers(&[]);
    assert_eq!(
        add(&service, &fixture, "no spaces", &bad_name)
            .expect_err("refused")
            .code(),
        "control"
    );
    assert!(outside.created().is_empty());
    assert_eq!(revision(&fixture), before);
    assert_eq!(profile_count(&fixture), 0);
}

/// On Windows a connection is unusable without a grant of the relay folder,
/// so Add is refused before anything is created when no relay sits beside
/// the application.
#[cfg(windows)]
#[test]
fn adding_on_windows_needs_the_relay() {
    let fixture = Fixture::new();
    let outside = Outside::new();
    let default = NoCallServer::start();
    let service = open_host(
        &fixture,
        &default.endpoint,
        ClaudeCodeHost {
            relay: None,
            ..outside.host(&[outside.bin()])
        },
    );
    let refused = add(&service, &fixture, "claude", &Scripted::answers(&[])).expect_err("refused");
    assert_eq!(refused.code(), "invalid_request");
    assert!(!outside.sign_ins.exists());
    assert_eq!(profile_count(&fixture), 0);
}

/// On Windows a sign-in that does not start leaves the connection added and
/// granted, and the error names the terminal command that signs in instead.
#[cfg(windows)]
#[test]
fn a_sign_in_that_does_not_start_names_the_terminal_command() {
    let fixture = Fixture::new();
    let outside = Outside::new();
    let default = NoCallServer::start();
    let service = open_host(&fixture, &default.endpoint, outside.host(&[outside.bin()]));
    let prompt = Scripted {
        sign_in_failure: Some("on purpose".to_owned()),
        ..Scripted::answers(&[true])
    };
    let error = add(&service, &fixture, "claude", &prompt).expect_err("not started");
    assert_eq!(error.code(), "control");
    for part in ["on purpose", "CLAUDE_CONFIG_DIR", "auth login"] {
        assert!(
            error.message().contains(part),
            "{part}: {}",
            error.message()
        );
    }
    assert_eq!(prompt.sign_ins().len(), 1);
    assert_eq!(outside.created().len(), 1);
    assert_eq!(profile_count(&fixture), 1);
    revoke(&service, &fixture, "claude", &Scripted::answers(&[true])).expect("revoked");
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
