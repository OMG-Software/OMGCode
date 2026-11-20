//! Settings window views, saved chat preferences, and per-turn skill selection
//! in the desktop service (ADR-0059, phase S1). Each test uses a private data
//! root; model requests go to a scripted loopback server or to a server that
//! fails the test when it is contacted.

mod common;

use std::fs;
use std::path::Path;

use omgcode_app::TokenSource;
use omgcode_app::desktop::{
    BranchState, ChatState, ConnectionSourceView, DesktopConfig, DesktopService, PreferencesSource,
};
use omgcode_core::control::{ControlStore, SkillScope, SkillSetting};
use omgcode_core::policy::Access;
use omgcode_core::skills::SkillName;
use serde_json::{Value, json};
use uuid::Uuid;

use common::{
    Fixture, MockServer, NoCallServer, Step, Workspace, json_of, read_tools, session_request,
    settings_json, text, text_of, text_only, wait_finished, wire,
};

const SKILL_BODY: &str = "DESKTOP-SKILL-BODY-MARKER follow these steps.";
const SKILL_DESCRIPTION: &str = "DESKTOP-SKILL-DESCRIPTION-MARKER";

fn new_session(service: &DesktopService, workspace: &Workspace) -> Uuid {
    service
        .session_create(wire(
            json!({"workspace_id": workspace.id, "title": "skills"}),
        ))
        .expect("create session")
        .session_id
}

/// Import a skill named `name`, optionally enabled for the whole application.
fn install_skill(fixture: &Fixture, name: &str, enabled: bool) -> SkillName {
    let source = fixture
        .data
        .parent()
        .expect("temporary root")
        .join("skill-source")
        .join(name);
    fs::create_dir_all(&source).expect("skill source");
    fs::write(
        source.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {SKILL_DESCRIPTION}\n---\n{SKILL_BODY}\n"),
    )
    .expect("SKILL.md");
    let mut control = ControlStore::open(&fixture.data).expect("control store");
    control
        .import_skill(&source, control.revision())
        .expect("import skill");
    let parsed = SkillName::new(name).expect("skill name");
    if enabled {
        control
            .set_skill_enablement(
                &parsed,
                SkillScope::Application,
                SkillSetting::Enabled,
                control.revision(),
            )
            .expect("enable skill");
    }
    parsed
}

fn send_with_skills(
    service: &DesktopService,
    session: Uuid,
    endpoint: &str,
    skills: &Value,
) -> Result<omgcode_app::desktop::ChatStatus, omgcode_app::desktop::DesktopError> {
    service.chat_send(wire(json!({
        "session_id": session,
        "message": "use the skill",
        "settings": settings_json(endpoint),
        "skills": skills,
    })))
}

fn environment_service(data: &Path, endpoint: &str) -> DesktopService {
    DesktopService::open(DesktopConfig {
        data_root: data.to_path_buf(),
        endpoint: endpoint.to_owned(),
        token_source: TokenSource::Environment,
        stream: false,
    })
    .expect("open desktop service")
}

#[test]
fn the_overview_without_a_settings_store_is_empty_but_reports_the_connection() {
    let fixture = Fixture::bare();
    let overview = environment_service(&fixture.data, "http://127.0.0.1:1234/v1")
        .settings_overview()
        .expect("overview");
    assert_eq!(overview.revision, 0);
    assert_eq!(overview.development_root, None);
    assert!(overview.workspaces.is_empty());
    assert!(overview.skills.is_empty());
    assert_eq!(overview.connection.endpoint, "http://127.0.0.1:1234/v1");
    assert_eq!(
        overview.connection.source,
        ConnectionSourceView::Environment
    );
    assert_eq!(overview.connection.profile, None);
    // Reading the overview creates an empty settings store (ADR-0060); the
    // creation itself is covered in `desktop_settings_changes.rs`.
    assert!(
        ControlStore::open_existing(&fixture.data)
            .expect("open")
            .is_some()
    );
}

#[test]
fn the_overview_shows_the_recorded_configuration_without_skill_text() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", read_tools());
    fixture.declare_profile();
    install_skill(&fixture, "demo-skill", true);
    let overview = fixture
        .service("http://127.0.0.1:1234/v1")
        .settings_overview()
        .expect("overview");

    assert!(overview.revision > 0);
    let root = overview.development_root.as_deref().expect("a root is set");
    assert!(root.ends_with("development"), "{root}");

    assert_eq!(overview.workspaces.len(), 1);
    let listed = &overview.workspaces[0];
    assert_eq!(listed.workspace_id, workspace.id);
    assert!(listed.folder_display.ends_with("project"));
    assert!(
        listed.model_tools
            && listed.read.permits()
            && listed.write == Access::Deny
            && listed.execute == Access::Deny
    );
    assert_eq!(listed.package_cache_root, None);

    assert_eq!(overview.toolchain_profiles.len(), 1);
    let profile = &overview.toolchain_profiles[0];
    assert_eq!(profile.name, "system");
    assert_eq!(profile.read_only_roots.len(), 1);
    assert!(profile.read_only_roots[0].ends_with("tools"));

    assert_eq!(overview.skills.len(), 1);
    assert_eq!(overview.skills[0].name, "demo-skill");
    assert_eq!(overview.skills[0].application, Some(true));
    assert!(overview.skills[0].workspace_overrides.is_empty());

    assert_eq!(overview.connection.source, ConnectionSourceView::Injected);

    // The imported package's description and body never reach the window.
    let serialized = text_of(&overview);
    assert!(!serialized.contains(SKILL_DESCRIPTION));
    assert!(!serialized.contains("DESKTOP-SKILL-BODY-MARKER"));
}

#[test]
fn the_overview_escapes_hidden_characters_in_recorded_paths() {
    let fixture = Fixture::new();
    // A workspace folder name with a bidirectional override is shown escaped.
    fixture.workspace("odd\u{202e}name", read_tools());
    let overview = fixture
        .service("http://127.0.0.1:1234/v1")
        .settings_overview()
        .expect("overview");
    let folder = &overview.workspaces[0].folder_display;
    assert!(!folder.contains('\u{202e}'), "{folder}");
    assert!(folder.contains("\\u{202e}"), "{folder}");
}

#[test]
fn session_skills_lists_only_skills_enabled_for_the_sessions_workspace() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let enabled = install_skill(&fixture, "enabled-skill", true);
    install_skill(&fixture, "disabled-skill", false);
    let service = fixture.service("http://127.0.0.1:1234/v1");
    let session = new_session(&service, &workspace);

    let listed = service
        .session_skills(wire(session_request(session)))
        .expect("skills");
    assert!(listed.workspace_registered);
    assert_eq!(listed.skills, [enabled.as_str()]);

    // A workspace override that disables it removes it at once.
    let mut control = ControlStore::open_existing(&fixture.data)
        .expect("open")
        .expect("exists");
    let workspace_id = control
        .registered_workspace(workspace.id)
        .expect("registered")
        .id();
    control
        .set_skill_enablement(
            &enabled,
            SkillScope::Workspace { workspace_id },
            SkillSetting::Disabled,
            control.revision(),
        )
        .expect("override");
    drop(control);
    let listed = service
        .session_skills(wire(session_request(session)))
        .expect("skills");
    assert!(listed.skills.is_empty());

    let unknown = service
        .session_skills(wire(session_request(Uuid::new_v4())))
        .expect_err("unknown session");
    assert_eq!(unknown.code(), "not_found");
}

#[test]
fn a_malformed_skill_selection_is_refused_before_a_turn_starts() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    install_skill(&fixture, "demo-skill", true);
    let server = NoCallServer::start();
    let service = fixture.service(&server.endpoint);
    let session = new_session(&service, &workspace);

    for (skills, code) in [
        (json!(["Not A Name"]), "invalid_request"),
        (json!(["a", "b", "c", "d", "e"]), "invalid_request"),
        (
            json!(["demo-skill", "demo-skill"]),
            "skill_selection_invalid",
        ),
    ] {
        let error =
            send_with_skills(&service, session, &server.endpoint, &skills).expect_err("refused");
        assert_eq!(error.code(), code, "{skills}");
    }
    assert_eq!(
        service
            .chat_status(wire(session_request(session)))
            .expect("status")
            .state,
        ChatState::Idle
    );
    server.assert_untouched();
}

#[test]
fn a_skill_that_is_not_enabled_fails_the_turn_without_a_model_request() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    install_skill(&fixture, "demo-skill", false);
    let server = NoCallServer::start();
    let service = fixture.service(&server.endpoint);
    let session = new_session(&service, &workspace);

    send_with_skills(&service, session, &server.endpoint, &json!(["demo-skill"]))
        .expect("the turn starts");
    let status = wait_finished(&service, session);
    assert_eq!(status.state, ChatState::Failed);
    server.assert_untouched();
}

#[test]
fn an_enabled_skill_reaches_the_model_request_for_that_turn_only() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    install_skill(&fixture, "demo-skill", true);
    let server = MockServer::start(vec![Step::reply(text("done")), Step::reply(text("again"))]);
    let service = fixture.service(&server.endpoint);
    let session = new_session(&service, &workspace);

    send_with_skills(&service, session, &server.endpoint, &json!(["demo-skill"]))
        .expect("first turn");
    assert_eq!(wait_finished(&service, session).state, ChatState::Completed);
    send_with_skills(&service, session, &server.endpoint, &json!([])).expect("second turn");
    assert_eq!(wait_finished(&service, session).state, ChatState::Completed);

    let (count, recorded) = server.finish_recorded();
    assert_eq!(count, 2);
    assert!(recorded[0].body.contains("DESKTOP-SKILL-BODY-MARKER"));
    assert!(!recorded[1].body.contains("DESKTOP-SKILL-BODY-MARKER"));
}

#[test]
fn saved_chat_preferences_become_the_defaults_and_keep_the_fixed_endpoint() {
    let fixture = Fixture::bare();
    let endpoint = "http://127.0.0.1:1234/v1";
    let service = fixture.service(endpoint);
    assert_eq!(
        service.chat_defaults().preferences,
        PreferencesSource::Defaults
    );

    let saved = service
        .chat_preferences_save(wire(json!({"preferences": {
            "model": "qwen3-coder",
            "context_tokens": 32_768,
            "max_output_tokens": 2048,
            "instructions": "Prefer short answers.",
            "compact_threshold": 24_000,
            "auto_compact": true,
            "enter_to_send": false
        }})))
        .expect("save");
    assert_eq!(saved.preferences, PreferencesSource::Saved);
    assert!(!saved.enter_to_send);
    assert_eq!(json_of(&saved)["enter_to_send"], false);
    assert_eq!(saved.settings.endpoint, endpoint);
    assert_eq!(saved.settings.model, "qwen3-coder");
    assert_eq!(saved.settings.context_tokens, 32_768);
    assert_eq!(saved.settings.max_output_tokens, 2048);
    assert_eq!(saved.settings.instructions, "Prefer short answers.");
    assert_eq!(saved.settings.compact_threshold, 24_000);
    assert!(saved.settings.auto_compact);

    // A new service in the same data root starts from them.
    let reopened = fixture.service(endpoint).chat_defaults();
    assert_eq!(reopened, saved);
    assert_eq!(json_of(&reopened)["preferences"], "saved");
}

#[test]
fn chat_preferences_cannot_carry_an_endpoint_or_oversized_values() {
    let fixture = Fixture::bare();
    let service = fixture.service("http://127.0.0.1:1234/v1");
    let with_endpoint = serde_json::from_value::<omgcode_app::desktop::ChatPreferencesSaveRequest>(
        json!({"preferences": {
            "endpoint": "http://example.com/v1",
            "model": "m",
            "context_tokens": 1,
            "max_output_tokens": 1,
            "instructions": "",
            "compact_threshold": 1,
            "auto_compact": false
        }}),
    );
    assert!(with_endpoint.is_err(), "an endpoint field is refused");

    let error = service
        .chat_preferences_save(wire(json!({"preferences": {
            "model": "m".repeat(257),
            "context_tokens": 1,
            "max_output_tokens": 1,
            "instructions": "",
            "compact_threshold": 1,
            "auto_compact": false
        }})))
        .expect_err("too long");
    assert_eq!(error.code(), "invalid_request");
    assert_eq!(
        service.chat_defaults().preferences,
        PreferencesSource::Defaults
    );
}

#[test]
fn a_damaged_preferences_file_is_reported_and_replaced_by_a_save() {
    let fixture = Fixture::bare();
    let service = fixture.service("http://127.0.0.1:1234/v1");
    let directory = fixture.data.join("desktop-v1");
    fs::create_dir_all(&directory).expect("directory");
    fs::write(directory.join("chat-preferences.json"), b"{ not json").expect("damage");

    let reply = service.chat_defaults();
    assert_eq!(reply.preferences, PreferencesSource::Unreadable);
    assert_eq!(reply.settings.model, "");

    let saved = service
        .chat_preferences_save(wire(json!({"preferences": {
            "model": "m",
            "context_tokens": 8192,
            "max_output_tokens": 512,
            "instructions": "",
            "compact_threshold": 4096,
            "auto_compact": false
        }})))
        .expect("save replaces the damaged file");
    assert_eq!(saved.preferences, PreferencesSource::Saved);
    // A request without the field keeps the built-in choice.
    assert!(saved.enter_to_send);
}

#[test]
fn enter_to_send_must_be_a_boolean() {
    let refused = serde_json::from_value::<omgcode_app::desktop::ChatPreferencesSaveRequest>(
        json!({"preferences": {
            "model": "m",
            "context_tokens": 1,
            "max_output_tokens": 1,
            "instructions": "",
            "compact_threshold": 1,
            "auto_compact": false,
            "enter_to_send": "yes"
        }}),
    );
    assert!(refused.is_err(), "a non-boolean value is refused");
}

/// The status bar's branch (ADR-0064) is read from the workspace's `HEAD` for
/// display only; a folder without `.git` has none, and an unknown session is
/// refused.
#[test]
fn session_context_reads_the_workspace_branch_for_display() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let service = fixture.service("http://127.0.0.1:1234/v1");
    let session = new_session(&service, &workspace);

    let context = service
        .session_context(wire(session_request(session)))
        .expect("context");
    assert!(context.workspace_registered);
    assert_eq!(context.branch.state, BranchState::None);
    assert_eq!(context.branch.name, None);

    fs::create_dir(workspace.path.join(".git")).expect("git dir");
    fs::write(
        workspace.path.join(".git").join("HEAD"),
        "ref: refs/heads/feature/\u{202e}status\n",
    )
    .expect("head");
    let context = service
        .session_context(wire(session_request(session)))
        .expect("context");
    assert_eq!(context.branch.state, BranchState::Branch);
    // The name is display-safe text: a bidirectional control is escaped.
    let name = context.branch.name.clone().expect("name");
    assert!(!name.contains('\u{202e}'), "{name}");
    assert!(name.starts_with("feature/"), "{name}");
    assert_eq!(
        json_of(&context)["branch"]["state"],
        Value::String("branch".to_owned())
    );

    fs::write(workspace.path.join(".git").join("HEAD"), "garbage").expect("head");
    let context = service
        .session_context(wire(session_request(session)))
        .expect("context");
    assert_eq!(context.branch.state, BranchState::Unavailable);
    assert!(context.branch.note.is_some());

    let unknown = service
        .session_context(wire(session_request(Uuid::new_v4())))
        .expect_err("unknown session");
    assert_eq!(unknown.code(), "not_found");
}

/// The `settings` window's scope (ADR-0069) comes from the session's recorded
/// workspace, is carried by every overview, and is cleared on request.
#[test]
fn the_settings_scope_follows_the_session_and_is_cleared_on_request() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let service = fixture.service("http://127.0.0.1:1234/v1");
    let session = new_session(&service, &workspace);

    // Given no scope, the overview carries none.
    assert_eq!(service.settings_overview().expect("overview").scope, None);

    // When the session's workspace is scoped, the overview names it.
    service
        .settings_scope_workspace(wire(session_request(session)))
        .expect("scope");
    let overview = service.settings_overview().expect("overview");
    assert_eq!(
        overview.scope.map(|scope| scope.workspace_id),
        Some(workspace.id)
    );
    assert_eq!(
        json_of(&overview)["scope"],
        json!({"workspace_id": workspace.id})
    );

    // When it is cleared, the overview carries none again.
    service.settings_scope_clear().expect("clear");
    assert_eq!(service.settings_overview().expect("overview").scope, None);
}

#[test]
fn an_unknown_session_leaves_the_settings_scope_unchanged() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let service = fixture.service("http://127.0.0.1:1234/v1");
    let session = new_session(&service, &workspace);
    service
        .settings_scope_workspace(wire(session_request(session)))
        .expect("scope");

    let error = service
        .settings_scope_workspace(wire(session_request(Uuid::new_v4())))
        .expect_err("an unknown session is refused");

    assert_eq!(error.code(), "not_found");
    assert_eq!(
        service
            .settings_overview()
            .expect("overview")
            .scope
            .map(|scope| scope.workspace_id),
        Some(workspace.id)
    );
}
