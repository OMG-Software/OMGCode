//! Settings window views, saved chat preferences, and per-turn skill selection
//! in the desktop service (ADR-0059, phase S1). Each test uses a private data
//! root; model requests go to a scripted loopback server or to a server that
//! fails the test when it is contacted.

mod common;

use std::fs;
use std::path::Path;

use omgcode_app::TokenSource;
use omgcode_app::desktop::{
    BranchState, ChatState, ConnectionSourceView, DesktopConfig, DesktopService, GitStatusState,
    PreferencesSource,
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
fn the_settings_window_and_the_composer_each_save_their_own_chat_defaults() {
    let fixture = Fixture::bare();
    let endpoint = "http://127.0.0.1:1234/v1";
    let service = fixture.service(endpoint);
    assert_eq!(
        service.chat_defaults().preferences,
        PreferencesSource::Defaults
    );
    let defaults = service.chat_defaults();
    assert_eq!(defaults.settings.turn_timeout_seconds, 120);
    assert_eq!(
        defaults.tool_call_limit_max,
        omgcode_core::turn::MAX_TOOL_CALLS
    );
    assert_eq!(
        defaults.turn_timeout_seconds_max,
        omgcode_core::turn::MAX_TURN_DEADLINE.as_secs()
    );
    assert_eq!(
        (
            defaults.compact_threshold_min,
            defaults.compact_threshold_max
        ),
        (128_000, 1_000_000)
    );
    let overview = service.settings_overview().expect("overview");
    assert_eq!(json_of(&overview.chat)["source"], "defaults");
    assert_eq!(overview.chat.defaults.context_tokens, 0);
    assert_eq!(overview.chat.instructions_max_bytes, Some(16 * 1024));

    // The settings window saves the budgets and the global instructions.
    let overview = service
        .settings_save_chat_defaults(wire(json!({"defaults": {
            "context_tokens": 32_768,
            "max_output_tokens": 2048,
            "max_tool_calls": 64,
            "turn_timeout_seconds": 900,
            "instructions": "Prefer short answers."
        }})))
        .expect("settings save");
    assert_eq!(json_of(&overview.chat)["source"], "saved");
    assert_eq!(overview.chat.defaults.context_tokens, 32_768);

    // The composer saves its own fields; the settings window's stay.
    let saved = service
        .chat_preferences_save(wire(json!({"preferences": {
            "compact_threshold": 256_000,
            "auto_compact": true,
            "enter_to_send": false
        }})))
        .expect("composer save");
    assert_eq!(saved.preferences, PreferencesSource::Saved);
    assert!(!saved.enter_to_send);
    assert_eq!(json_of(&saved)["enter_to_send"], false);
    assert_eq!(saved.settings.endpoint, endpoint);
    assert_eq!(saved.settings.context_tokens, 32_768);
    assert_eq!(saved.settings.max_output_tokens, 2048);
    assert_eq!(saved.settings.max_tool_calls, 64);
    assert_eq!(saved.settings.turn_timeout_seconds, 900);
    assert_eq!(saved.settings.instructions, "Prefer short answers.");
    assert_eq!(saved.settings.compact_threshold, 256_000);
    assert!(saved.settings.auto_compact);

    // A later settings save keeps the composer's fields.
    service
        .settings_save_chat_defaults(wire(json!({"defaults": {
            "context_tokens": 65_536,
            "max_output_tokens": 4096,
            "max_tool_calls": 32,
            "turn_timeout_seconds": 300,
            "instructions": ""
        }})))
        .expect("second settings save");
    let reopened = fixture.service(endpoint).chat_defaults();
    assert_eq!(reopened.settings.context_tokens, 65_536);
    assert_eq!(reopened.settings.instructions, "");
    assert_eq!(reopened.settings.compact_threshold, 256_000);
    assert!(reopened.settings.auto_compact);
    assert!(!reopened.enter_to_send);
    assert_eq!(json_of(&reopened)["preferences"], "saved");
}

#[test]
fn composer_preferences_carry_only_their_own_fields_within_the_slider_range() {
    let fixture = Fixture::bare();
    let service = fixture.service("http://127.0.0.1:1234/v1");
    for extra in [
        json!({"endpoint": "http://example.com/v1"}),
        json!({"model": "m"}),
        json!({"context_tokens": 1}),
        json!({"instructions": "x"}),
    ] {
        let mut preferences =
            json!({"compact_threshold": 200_000, "auto_compact": false, "enter_to_send": true});
        for (key, value) in extra.as_object().expect("object") {
            preferences[key] = value.clone();
        }
        let parsed = serde_json::from_value::<omgcode_app::desktop::ChatPreferencesSaveRequest>(
            json!({ "preferences": preferences }),
        );
        assert!(parsed.is_err(), "{extra} is refused");
    }

    for threshold in [0_u64, 127_999, 1_000_001] {
        let error = service
            .chat_preferences_save(wire(json!({"preferences": {
                "compact_threshold": threshold,
                "auto_compact": false,
                "enter_to_send": true
            }})))
            .expect_err("outside the range");
        assert_eq!(error.code(), "invalid_request");
    }
    assert_eq!(
        service.chat_defaults().preferences,
        PreferencesSource::Defaults
    );
    for threshold in [128_000_u64, 1_000_000] {
        service
            .chat_preferences_save(wire(json!({"preferences": {
                "compact_threshold": threshold,
                "auto_compact": false,
                "enter_to_send": true
            }})))
            .expect("an end of the range");
    }
}

#[test]
fn settings_chat_defaults_are_checked_and_nothing_is_written_when_refused() {
    let fixture = Fixture::bare();
    let service = fixture.service("http://127.0.0.1:1234/v1");
    let valid = json!({
        "context_tokens": 8192,
        "max_output_tokens": 512,
        "max_tool_calls": 16,
        "turn_timeout_seconds": 120,
        "instructions": ""
    });
    for (field, value) in [
        ("context_tokens", json!(0)),
        ("max_output_tokens", json!(0)),
        ("max_tool_calls", json!(0)),
        (
            "max_tool_calls",
            json!(omgcode_core::turn::MAX_TOOL_CALLS + 1),
        ),
        ("turn_timeout_seconds", json!(0)),
        ("instructions", json!("i".repeat(16 * 1024 + 1))),
    ] {
        let mut defaults = valid.clone();
        defaults[field] = value;
        let error = service
            .settings_save_chat_defaults(wire(json!({ "defaults": defaults })))
            .expect_err("refused");
        assert_eq!(error.code(), "invalid_request", "{field}");
    }
    assert_eq!(
        service.chat_defaults().preferences,
        PreferencesSource::Defaults
    );
    let parsed = serde_json::from_value::<omgcode_app::desktop::ChatBudgetsSaveRequest>(
        json!({"defaults": {
            "context_tokens": 1, "max_output_tokens": 1, "max_tool_calls": 1,
            "turn_timeout_seconds": 1, "instructions": "", "compact_threshold": 1
        }}),
    );
    assert!(parsed.is_err(), "a composer field is refused");
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
    let overview = service.settings_overview().expect("overview");
    assert_eq!(json_of(&overview.chat)["source"], "unreadable");

    let saved = service
        .chat_preferences_save(wire(json!({"preferences": {
            "compact_threshold": 200_000,
            "auto_compact": false,
            "enter_to_send": true
        }})))
        .expect("save replaces the damaged file");
    assert_eq!(saved.preferences, PreferencesSource::Saved);
    // The damaged file's other fields are not recovered; the built-in ones apply.
    assert_eq!(saved.settings.context_tokens, 0);
    assert!(saved.enter_to_send);
}

#[test]
fn a_composer_save_without_enter_to_send_reads_it_as_on() {
    let fixture = Fixture::bare();
    let service = fixture.service("http://127.0.0.1:1234/v1");
    let saved = service
        .chat_preferences_save(wire(json!({"preferences": {
            "compact_threshold": 200_000,
            "auto_compact": true
        }})))
        .expect("a request without the field is accepted");
    assert!(saved.enter_to_send);
}

/// Saves chat defaults, then holds the preferences file so it cannot be read,
/// and checks that neither window's save changes it.
fn assert_saves_refused_while(hold: impl FnOnce(&Path) -> Box<dyn FnOnce()>) {
    let fixture = Fixture::bare();
    let service = fixture.service("http://127.0.0.1:1234/v1");
    service
        .settings_save_chat_defaults(wire(json!({"defaults": {
            "context_tokens": 65_536,
            "max_output_tokens": 4096,
            "max_tool_calls": 32,
            "turn_timeout_seconds": 300,
            "instructions": "Keep these."
        }})))
        .expect("first save");
    let file = fixture
        .data
        .join("desktop-v1")
        .join("chat-preferences.json");
    let before = fs::read(&file).expect("saved file");
    let release = hold(&file);

    let composer = service
        .chat_preferences_save(wire(json!({"preferences": {
            "compact_threshold": 400_000,
            "auto_compact": true,
            "enter_to_send": false
        }})))
        .expect_err("the composer's save is refused");
    assert_eq!(composer.code(), "preferences");
    let settings = service
        .settings_save_chat_defaults(wire(json!({"defaults": {
            "context_tokens": 8192,
            "max_output_tokens": 1024,
            "max_tool_calls": 8,
            "turn_timeout_seconds": 60,
            "instructions": ""
        }})))
        .expect_err("the settings window's save is refused");
    assert_eq!(settings.code(), "preferences");

    release();
    assert_eq!(fs::read(&file).expect("file after"), before, "unchanged");
    let reply = service.chat_defaults();
    assert_eq!(reply.settings.context_tokens, 65_536);
    assert_eq!(reply.settings.instructions, "Keep these.");
}

#[cfg(windows)]
#[test]
fn a_save_changes_nothing_while_another_program_holds_the_preferences_file() {
    use std::os::windows::fs::OpenOptionsExt;
    /// Lets others delete or rename the file, but not read or write it.
    const FILE_SHARE_DELETE: u32 = 0x4;
    assert_saves_refused_while(|file| {
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_DELETE)
            .open(file)
            .expect("hold the file");
        Box::new(move || drop(held))
    });
}

#[cfg(unix)]
#[test]
fn a_save_changes_nothing_when_the_preferences_file_is_a_link() {
    assert_saves_refused_while(|file| {
        // The link points at the saved bytes, which a merge could not read.
        let target = file.with_extension("target");
        fs::rename(file, &target).expect("move the file aside");
        std::os::unix::fs::symlink(&target, file).expect("link");
        let file = file.to_path_buf();
        Box::new(move || {
            fs::remove_file(&file).expect("remove link");
            fs::rename(&target, &file).expect("restore the file");
        })
    });
}

#[test]
fn enter_to_send_must_be_a_boolean() {
    let refused = serde_json::from_value::<omgcode_app::desktop::ChatPreferencesSaveRequest>(
        json!({"preferences": {
            "compact_threshold": 200_000,
            "auto_compact": false,
            "enter_to_send": "yes"
        }}),
    );
    assert!(refused.is_err(), "a non-boolean value is refused");
}

/// Both headings and the blank line a turn adds when it sends both texts.
const BOTH_HEADINGS: usize = "Global instructions:\n".len() + 2 + "Workspace instructions:\n".len();
const INSTRUCTIONS_MAX: usize = 16 * 1024;

#[test]
fn every_save_keeps_the_global_and_workspace_instructions_sendable_together() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let service = fixture.service("http://127.0.0.1:1234/v1");
    let room_of = |overview: &omgcode_app::desktop::SettingsOverview| {
        overview
            .workspaces
            .iter()
            .find(|view| view.workspace_id == workspace.id)
            .expect("the workspace")
            .instructions_max_bytes
    };
    let save_global = |text: String| {
        service.settings_save_chat_defaults(wire(json!({"defaults": {
            "context_tokens": 8192,
            "max_output_tokens": 1024,
            "max_tool_calls": 8,
            "turn_timeout_seconds": 60,
            "instructions": text
        }})))
    };
    let save_workspace = |text: String| {
        service.settings_set_workspace_instructions(wire(json!({
            "workspace_id": workspace.id,
            "instructions": text
        })))
    };

    // Nothing saved: a workspace text leaves room for its own heading.
    let overview = service.settings_overview().expect("overview");
    assert_eq!(room_of(&overview), Some(INSTRUCTIONS_MAX - 24));
    assert_eq!(overview.chat.instructions_max_bytes, Some(INSTRUCTIONS_MAX));

    // A long workspace text shrinks the global room to exactly what fits.
    let workspace_len = 10_000;
    let overview = save_workspace("w".repeat(workspace_len)).expect("workspace");
    let global_room = INSTRUCTIONS_MAX - BOTH_HEADINGS - workspace_len;
    assert_eq!(overview.chat.instructions_max_bytes, Some(global_room));
    let error = save_global("g".repeat(global_room + 1)).expect_err("too long beside it");
    assert_eq!(error.code(), "invalid_request");
    assert_eq!(
        error.message(),
        format!(
            "The global instructions can be at most {global_room} bytes, so that a turn can send \
             them with the longest workspace instructions and their headings. Shorten them, or \
             that workspace's instructions."
        )
    );
    let overview = save_global("g".repeat(global_room)).expect("exactly fits");
    assert_eq!(room_of(&overview), Some(workspace_len));

    // Now the workspace text cannot grow, but can shrink or be removed.
    let error = save_workspace("w".repeat(workspace_len + 1)).expect_err("too long beside it");
    assert_eq!(error.code(), "invalid_request");
    assert_eq!(
        error.message(),
        format!(
            "The workspace instructions can be at most {workspace_len} bytes, so that a turn can \
             send them with the global instructions and their headings. Shorten them, or the \
             global instructions."
        )
    );
    save_workspace("short".to_owned()).expect("shorter");
    save_workspace(String::new()).expect("removed");

    // Every accepted pair is sent: a turn composes them within the core bound.
    let overview = save_global("g".repeat(INSTRUCTIONS_MAX)).expect("alone, the whole bound");
    assert_eq!(room_of(&overview), Some(0));
    let error = save_workspace("w".to_owned()).expect_err("no room left");
    assert_eq!(error.code(), "invalid_request");
}

/// Makes `file` exist but be unreadable until the returned function runs: a
/// handle that lets others only delete or rename it on Windows, a link (which
/// the bounded read refuses) on Unix.
fn hold_unreadable(file: &Path) -> Box<dyn FnOnce()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_DELETE: u32 = 0x4;
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_DELETE)
            .open(file)
            .expect("hold the file");
        Box::new(move || drop(held))
    }
    #[cfg(unix)]
    {
        let target = file.with_extension("target");
        fs::rename(file, &target).expect("move the file aside");
        std::os::unix::fs::symlink(&target, file).expect("link");
        let file = file.to_path_buf();
        Box::new(move || {
            fs::remove_file(&file).expect("remove link");
            fs::rename(&target, &file).expect("restore the file");
        })
    }
}

#[test]
fn a_room_is_unknown_while_the_text_it_must_fit_beside_cannot_be_read() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let service = fixture.service("http://127.0.0.1:1234/v1");
    let save_global = |text: &str| {
        service.settings_save_chat_defaults(wire(json!({"defaults": {
            "context_tokens": 8192,
            "max_output_tokens": 1024,
            "max_tool_calls": 8,
            "turn_timeout_seconds": 60,
            "instructions": text
        }})))
    };
    let save_workspace = |text: &str| {
        service.settings_set_workspace_instructions(wire(json!({
            "workspace_id": workspace.id,
            "instructions": text
        })))
    };
    let workspace_room = || {
        service
            .settings_overview()
            .expect("overview")
            .workspaces
            .iter()
            .find(|view| view.workspace_id == workspace.id)
            .expect("the workspace")
            .instructions_max_bytes
    };
    let desktop = fixture.data.join("desktop-v1");
    let preferences_file = desktop.join("chat-preferences.json");
    let workspace_file = desktop
        .join("workspace-instructions")
        .join(format!("{}.json", workspace.id));

    // The global text cannot be read: the overview shows no workspace room,
    // and a workspace save of text is refused, but removing the text is not.
    save_workspace("Use tabs.").expect("workspace");
    save_global("Be brief.").expect("global");
    let saved = fs::read(&workspace_file).expect("saved");
    let release = hold_unreadable(&preferences_file);
    assert_eq!(workspace_room(), None);
    let error = save_workspace("Use spaces.").expect_err("room unknown");
    assert_eq!(error.code(), "preferences");
    assert_eq!(
        fs::read(&workspace_file).expect("kept"),
        saved,
        "nothing was saved"
    );
    save_workspace("").expect("removing needs no room");
    assert!(!workspace_file.exists(), "removed");
    release();
    assert_eq!(workspace_room(), Some(INSTRUCTIONS_MAX - BOTH_HEADINGS - 9));

    // A workspace text cannot be read: the overview shows no global room,
    // and a global save of text is refused, but removing the text is not.
    save_workspace("Use tabs.").expect("workspace");
    let saved = fs::read(&preferences_file).expect("saved");
    let release = hold_unreadable(&workspace_file);
    let overview = service.settings_overview().expect("overview");
    assert_eq!(overview.chat.instructions_max_bytes, None);
    let error = save_global("A longer global text.").expect_err("room unknown");
    assert_eq!(error.code(), "preferences");
    assert_eq!(
        fs::read(&preferences_file).expect("kept"),
        saved,
        "nothing was saved"
    );
    let overview = save_global("").expect("removing needs no room");
    assert_eq!(overview.chat.defaults.instructions, "");
    release();

    // Readable again, both rooms are known and both saves go through.
    let overview = service.settings_overview().expect("overview");
    assert_eq!(
        overview.chat.instructions_max_bytes,
        Some(INSTRUCTIONS_MAX - BOTH_HEADINGS - 9)
    );
    save_global("A longer global text.").expect("global");
    save_workspace("Use spaces.").expect("workspace");
}

#[test]
fn workspace_instructions_are_saved_for_a_registered_workspace_only() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let service = fixture.service("http://127.0.0.1:1234/v1");
    let shown = |overview: &omgcode_app::desktop::SettingsOverview| {
        overview
            .workspaces
            .iter()
            .find(|view| view.workspace_id == workspace.id)
            .expect("the workspace")
            .instructions
            .clone()
    };
    assert_eq!(
        shown(&service.settings_overview().expect("overview")),
        Some(String::new())
    );

    let overview = service
        .settings_set_workspace_instructions(wire(json!({
            "workspace_id": workspace.id,
            "instructions": "Run cargo test before answering."
        })))
        .expect("save");
    assert_eq!(
        shown(&overview),
        Some("Run cargo test before answering.".to_owned())
    );

    let error = service
        .settings_set_workspace_instructions(wire(json!({
            "workspace_id": Uuid::new_v4(),
            "instructions": "x"
        })))
        .expect_err("unknown workspace");
    assert_eq!(error.code(), "unknown_workspace");
    let error = service
        .settings_set_workspace_instructions(wire(json!({
            "workspace_id": workspace.id,
            "instructions": "i".repeat(16 * 1024 + 1)
        })))
        .expect_err("too long");
    assert_eq!(error.code(), "invalid_request");

    let overview = service
        .settings_set_workspace_instructions(wire(json!({
            "workspace_id": workspace.id,
            "instructions": ""
        })))
        .expect("remove");
    assert_eq!(shown(&overview), Some(String::new()));

    // A damaged file is shown as unreadable.
    let file = fixture
        .data
        .join("desktop-v1")
        .join("workspace-instructions")
        .join(format!("{}.json", workspace.id));
    fs::create_dir_all(file.parent().expect("parent")).expect("directory");
    fs::write(&file, b"{ not json").expect("damage");
    assert_eq!(shown(&service.settings_overview().expect("overview")), None);
}

/// The request's labelled preference message, if any.
fn preference_message(recorded: &common::Recorded) -> Option<String> {
    let body: Value = serde_json::from_str(&recorded.body).expect("request JSON");
    body["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .filter_map(|message| message["content"].as_str())
        .find(|content| content.starts_with("Saved user preferences:\n"))
        .map(str::to_owned)
}

#[test]
fn a_turn_sends_the_global_and_the_workspace_instructions_together() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let server = MockServer::start(vec![Step::reply(text("one")), Step::reply(text("two"))]);
    let service = fixture.service(&server.endpoint);
    let session = new_session(&service, &workspace);
    let mut settings = settings_json(&server.endpoint);
    settings["instructions"] = json!("Be brief.");
    let send = |settings: &Value| {
        service
            .chat_send(wire(json!({
                "session_id": session,
                "message": "hi",
                "settings": settings,
                "skills": [],
            })))
            .expect("send");
        assert_eq!(wait_finished(&service, session).state, ChatState::Completed);
    };

    // The global text alone keeps its earlier shape.
    send(&settings);
    service
        .settings_set_workspace_instructions(wire(json!({
            "workspace_id": workspace.id,
            "instructions": "Use tabs."
        })))
        .expect("save");
    send(&settings);

    let (_, requests) = server.finish_recorded();
    assert_eq!(
        preference_message(&requests[0]).as_deref(),
        Some("Saved user preferences:\nBe brief.")
    );
    assert_eq!(
        preference_message(&requests[1]).as_deref(),
        Some(
            "Saved user preferences:\nGlobal instructions:\nBe brief.\n\nWorkspace instructions:\nUse tabs."
        )
    );
}

#[test]
fn a_turn_is_refused_when_its_workspace_instructions_cannot_be_used() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let server = NoCallServer::start();
    let service = fixture.service(&server.endpoint);
    let session = new_session(&service, &workspace);
    let half = "h".repeat(8 * 1024);
    service
        .settings_set_workspace_instructions(wire(json!({
            "workspace_id": workspace.id,
            "instructions": half
        })))
        .expect("save");
    let mut settings = settings_json(&server.endpoint);
    settings["instructions"] = json!(half);
    let error = service
        .chat_send(wire(json!({
            "session_id": session,
            "message": "hi",
            "settings": settings,
            "skills": [],
        })))
        .expect_err("together they are too long");
    assert_eq!(error.code(), "invalid_request");

    let file = fixture
        .data
        .join("desktop-v1")
        .join("workspace-instructions")
        .join(format!("{}.json", workspace.id));
    fs::write(&file, b"{ not json").expect("damage");
    let error = service
        .chat_send(wire(json!({
            "session_id": session,
            "message": "hi",
            "settings": settings_json(&server.endpoint),
            "skills": [],
        })))
        .expect_err("unreadable");
    assert_eq!(error.code(), "instructions_unreadable");
    server.assert_untouched();
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

/// The Git status tab (ADR-0085): a folder without `.git` is a normal state, a
/// `.git` folder that is not a repository is a controlled `unavailable` value
/// that still carries the branch, and an unknown session is refused.
#[test]
fn workspace_git_status_reports_states_without_failing() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let service = fixture.service("http://127.0.0.1:1234/v1");
    let session = new_session(&service, &workspace);

    let status = service
        .workspace_git_status(wire(session_request(session)))
        .expect("status");
    assert_eq!(status.state, GitStatusState::NotRepository);
    assert!(status.paths.is_empty());
    assert!(!status.truncated);
    assert_eq!(status.branch.state, BranchState::None);

    fs::create_dir(workspace.path.join(".git")).expect("git dir");
    fs::write(
        workspace.path.join(".git").join("HEAD"),
        "ref: refs/heads/feature
",
    )
    .expect("head");
    let status = service
        .workspace_git_status(wire(session_request(session)))
        .expect("status");
    assert_eq!(status.state, GitStatusState::Unavailable);
    assert!(status.note.is_some());
    assert_eq!(status.branch.state, BranchState::Branch);
    assert_eq!(
        json_of(&status)["state"],
        Value::String("unavailable".to_owned())
    );

    let unknown = service
        .workspace_git_status(wire(session_request(Uuid::new_v4())))
        .expect_err("unknown session");
    assert_eq!(unknown.code(), "not_found");
}

/// A `.git` entry without `HEAD` is named by what it is: a folder that has no
/// `HEAD` is not reported as a pointer file, and a pointer file is.
#[test]
fn a_git_entry_without_head_is_named_by_what_it_is() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace("project", text_only());
    let service = fixture.service("http://127.0.0.1:1234/v1");
    let session = new_session(&service, &workspace);
    let note = || {
        let status = service
            .workspace_git_status(wire(session_request(session)))
            .expect("status");
        assert_eq!(status.state, GitStatusState::Unavailable);
        status.note.expect("note")
    };

    let git = workspace.path.join(".git");
    fs::create_dir(&git).expect("git dir");
    assert_eq!(note(), "the .git folder has no HEAD");

    fs::remove_dir(&git).expect("remove dir");
    fs::write(&git, "gitdir: ../elsewhere\n").expect("git file");
    assert_eq!(
        note(),
        "the .git entry is a file, so linked worktrees and submodules are not shown"
    );
}

/// The `settings` window's scope (ADR-0071) comes from the session's recorded
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
