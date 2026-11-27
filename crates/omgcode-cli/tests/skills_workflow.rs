mod support;

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use support::{
    assert_error_code, assert_success, json_output, path_arg, register_workspace, run_cli,
};
use tempfile::TempDir;

const SKILL_MD: &str =
    "---\nname: demo-skill\ndescription: Demonstrates a skill.\n---\nFollow the steps.\n";

struct Scene {
    _temporary: TempDir,
    data: PathBuf,
    sources: PathBuf,
    workspace_id: String,
    revision: u64,
}

fn write_skill(sources: &Path, name: &str, skill_md: &str) -> PathBuf {
    let folder = sources.join(name);
    fs::create_dir_all(folder.join("references")).expect("skill folders");
    fs::write(folder.join("SKILL.md"), skill_md).expect("SKILL.md");
    fs::write(folder.join("references").join("notes.md"), "Notes.\n").expect("notes");
    folder
}

/// Create the data root owner-only: the private store refuses inherited grants.
fn private_data_root(data: &Path) {
    #[cfg(windows)]
    support::workspace_fixture::restrict_to_current_user(data);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir(data).expect("data root");
        fs::set_permissions(data, fs::Permissions::from_mode(0o700)).expect("private mode");
    }
}

/// A control store with a development root and one registered workspace.
fn scene() -> Scene {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let data = temporary.path().join("application-data");
    let development = temporary.path().join("development");
    let workspace = development.join("project");
    fs::create_dir_all(&workspace).expect("workspace");
    let sources = temporary.path().join("sources");
    fs::create_dir_all(&sources).expect("sources");
    private_data_root(&data);
    let root = run_cli(
        &data,
        &[
            "workspace",
            "set-root",
            path_arg(&development),
            "--revision",
            "0",
        ],
    );
    assert_success(&root);
    let registered = register_workspace(&data, &workspace, 1);
    Scene {
        _temporary: temporary,
        data,
        sources,
        workspace_id: registered["registration_id"]
            .as_str()
            .expect("workspace id")
            .to_owned(),
        revision: registered["snapshot"]["revision"]
            .as_u64()
            .expect("revision"),
    }
}

impl Scene {
    /// Run a skills command that must succeed and track the new revision.
    fn ok(&mut self, args: &[&str]) -> Value {
        let mut full = vec!["skills"];
        full.extend_from_slice(args);
        let output = run_cli(&self.data, &full);
        assert_success(&output);
        let value = json_output(&output);
        self.revision = value["revision"].as_u64().expect("revision in output");
        value
    }

    fn revision_arg(&self) -> String {
        self.revision.to_string()
    }

    fn add(&mut self, folder: &Path) -> Value {
        let revision = self.revision_arg();
        self.ok(&["add", path_arg(folder), "--revision", &revision])
    }

    fn fails(&self, args: &[&str], code: &str) {
        let mut full = vec!["skills"];
        full.extend_from_slice(args);
        assert_error_code(&run_cli(&self.data, &full), code);
    }
}

/// Every file below `folder` with its bytes, for before/after comparison.
fn snapshot(folder: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read directory") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push((path.clone(), fs::read(&path).expect("read file")));
            }
        }
    }
    found.sort();
    found
}

fn find<'a>(listing: &'a Value, name: &str) -> &'a Value {
    listing["skills"]
        .as_array()
        .expect("skills array")
        .iter()
        .find(|skill| skill["name"] == name)
        .expect("skill listed")
}

#[test]
fn skills_lifecycle_adds_shows_enables_overrides_inherits_and_removes() {
    // Given a registered workspace and a valid skill folder.
    let mut scene = scene();
    let folder = write_skill(&scene.sources, "demo-skill", SKILL_MD);
    let before = snapshot(&folder);
    let workspace = scene.workspace_id.clone();

    // When it is added, it is registered disabled with its files reported.
    let added = scene.add(&folder);
    assert_eq!(added["skill"]["name"], "demo-skill");
    assert_eq!(added["skill"]["file_count"], 2);
    assert_eq!(added["files"].as_array().expect("files").len(), 2);
    let listed = scene.ok(&["list", "--workspace", &workspace]);
    let skill = find(&listed, "demo-skill");
    assert_eq!(skill["effective"], false);
    assert!(skill.get("application").is_none());

    // Then show returns the stored body even though the skill is disabled.
    let shown = scene.ok(&["show", "demo-skill", "--workspace", &workspace]);
    assert_eq!(shown["skill"]["body"], "Follow the steps.\n");
    assert_eq!(shown["skill"]["effective"], false);
    assert_eq!(shown["skill"]["description"], "Demonstrates a skill.");

    // When enabled for the application, every workspace inherits it.
    let revision = scene.revision_arg();
    let enabled = scene.ok(&[
        "enable",
        "demo-skill",
        "--application",
        "--revision",
        &revision,
    ]);
    assert_eq!(enabled["skill"]["application"], true);
    let listed = scene.ok(&["list", "--workspace", &workspace]);
    assert_eq!(find(&listed, "demo-skill")["effective"], true);

    // When one workspace disables it, the override wins.
    let revision = scene.revision_arg();
    scene.ok(&[
        "disable",
        "demo-skill",
        "--workspace",
        &workspace,
        "--revision",
        &revision,
    ]);
    let listed = scene.ok(&["list", "--workspace", &workspace]);
    let skill = find(&listed, "demo-skill");
    assert_eq!(skill["effective"], false);
    assert_eq!(skill["workspace_overrides"][0]["enabled"], false);

    // When it inherits again, the application setting applies.
    let revision = scene.revision_arg();
    scene.ok(&[
        "inherit",
        "demo-skill",
        "--workspace",
        &workspace,
        "--revision",
        &revision,
    ]);
    let listed = scene.ok(&["list", "--workspace", &workspace]);
    let skill = find(&listed, "demo-skill");
    assert_eq!(skill["effective"], true);
    assert!(skill.get("workspace_overrides").is_none());

    // When it is removed, the registry is empty and the source folder is intact.
    let revision = scene.revision_arg();
    let removed = scene.ok(&["remove", "demo-skill", "--revision", &revision]);
    assert_eq!(removed["removed"]["name"], "demo-skill");
    assert_eq!(removed["warnings"].as_array().expect("warnings").len(), 0);
    let listed = scene.ok(&["list"]);
    assert!(listed["skills"].as_array().expect("skills").is_empty());
    assert_eq!(snapshot(&folder), before);
    scene.fails(&["show", "demo-skill"], "skill_not_found");
}

#[test]
fn skills_commands_refuse_with_stable_codes() {
    // Given a registered skill.
    let mut scene = scene();
    let folder = write_skill(&scene.sources, "demo-skill", SKILL_MD);
    scene.add(&folder);
    let revision = scene.revision_arg();
    let workspace = scene.workspace_id.clone();

    // Then a stale revision is a control conflict.
    scene.fails(
        &["enable", "demo-skill", "--application", "--revision", "1"],
        "control",
    );
    // A duplicate name is refused before anything is copied.
    scene.fails(
        &["add", path_arg(&folder), "--revision", &revision],
        "skill_name_taken",
    );
    // An unknown skill is refused for every mutation.
    scene.fails(
        &[
            "enable",
            "missing-skill",
            "--application",
            "--revision",
            &revision,
        ],
        "skill_not_found",
    );
    scene.fails(
        &["remove", "missing-skill", "--revision", &revision],
        "skill_not_found",
    );
    // An invalid name is refused with the name code.
    scene.fails(&["show", "Bad_Name"], "name_invalid_character");
    // Inherit has no application form, and a scope is required.
    let usage = run_cli(
        &scene.data,
        &[
            "skills",
            "inherit",
            "demo-skill",
            "--application",
            "--revision",
            &revision,
        ],
    );
    assert!(!usage.status.success());
    let usage = run_cli(
        &scene.data,
        &["skills", "enable", "demo-skill", "--revision", &revision],
    );
    assert!(!usage.status.success());
    // An unregistered workspace is refused.
    let unknown = "00000000-0000-4000-8000-000000000000";
    scene.fails(
        &[
            "enable",
            "demo-skill",
            "--workspace",
            unknown,
            "--revision",
            &revision,
        ],
        "skill_workspace_unknown",
    );
    assert_ne!(workspace, unknown);

    // Invalid packages are refused and register nothing.
    let no_manifest = scene.sources.join("no-manifest");
    fs::create_dir_all(&no_manifest).expect("folder");
    scene.fails(
        &["add", path_arg(&no_manifest), "--revision", &revision],
        "skill_md_missing",
    );
    let mismatch = write_skill(
        &scene.sources,
        "other-name",
        "---\nname: renamed-skill\ndescription: Mismatch.\n---\nBody.\n",
    );
    scene.fails(
        &["add", path_arg(&mismatch), "--revision", &revision],
        "skill_name_directory_mismatch",
    );
    let listed = scene.ok(&["list"]);
    assert_eq!(listed["skills"].as_array().expect("skills").len(), 1);
}

#[test]
fn listing_an_unregistered_workspace_is_refused_while_a_registered_one_falls_back() {
    // Given a skill enabled for the application.
    let mut scene = scene();
    let folder = write_skill(&scene.sources, "demo-skill", SKILL_MD);
    scene.add(&folder);
    let revision = scene.revision_arg();
    scene.ok(&[
        "enable",
        "demo-skill",
        "--application",
        "--revision",
        &revision,
    ]);
    let unknown = "00000000-0000-4000-8000-000000000000";
    let workspace = scene.workspace_id.clone();

    // Then an unregistered workspace is refused, as the chat loader refuses it.
    scene.fails(&["list", "--workspace", unknown], "skill_workspace_unknown");
    scene.fails(
        &["show", "demo-skill", "--workspace", unknown],
        "skill_workspace_unknown",
    );
    // And a registered workspace without an override follows the application.
    let listed = scene.ok(&["list", "--workspace", &workspace]);
    assert_eq!(find(&listed, "demo-skill")["effective"], Value::Bool(true));
}

#[test]
fn skill_text_with_markup_and_control_characters_round_trips_as_json_data() {
    // Given a skill whose description and body hold markup and an escape byte.
    let mut scene = scene();
    let body = "<script>alert(1)</script>\n\u{1b}[31mred\u{1b}[0m\n```\n---\n```\n";
    let skill_md = format!(
        "---\nname: odd-skill\ndescription: |\n  <b>bold</b> \"quoted\" & more\n  second line\n---\n{body}"
    );
    let folder = write_skill(&scene.sources, "odd-skill", &skill_md);
    scene.add(&folder);

    // When it is shown, stdout carries the text as escaped JSON, never raw.
    let output = run_cli(&scene.data, &["skills", "show", "odd-skill"]);
    assert_success(&output);
    assert!(
        !output.stdout.contains(&0x1b),
        "raw escape byte reached stdout"
    );
    let shown = json_output(&output);
    assert_eq!(shown["skill"]["body"], body);
    assert_eq!(
        shown["skill"]["description"],
        "<b>bold</b> \"quoted\" & more\nsecond line\n"
    );
    let listed =
        String::from_utf8(run_cli(&scene.data, &["skills", "list"]).stdout).expect("UTF-8 listing");
    assert!(!listed.contains('\u{1b}'));
}

#[test]
fn capabilities_report_skill_import_and_registry_without_scripts() {
    // Given a data directory that capabilities does not open.
    let temporary = tempfile::tempdir().expect("temporary directory");

    // When capabilities are requested.
    let output = run_cli(&temporary.path().join("data"), &["capabilities"]);
    assert_success(&output);

    // Then skills are importable and loadable from the CLI, and never run.
    let skills = &json_output(&output)["skills"];
    assert_eq!(skills["import"], true);
    assert_eq!(skills["registry"], true);
    assert_eq!(skills["chat_loading"], serde_json::json!(["cli"]));
    assert_eq!(skills["scripts"], false);
}
