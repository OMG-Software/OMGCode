//! The dispatcher rechecks the skill selection under the lock that admits
//! the operation, so a withdrawal after the turn's last check still stops it.

use std::fs;
use std::path::PathBuf;

use serde_json::json;

use super::*;
use crate::control::{SkillScope, SkillSetting};
use crate::skills::SkillName;
use crate::test_support::private_data_root;

struct Scene {
    _directory: tempfile::TempDir,
    data: PathBuf,
    workspace_id: WorkspaceId,
    name: SkillName,
}

fn scene() -> Scene {
    let directory = tempfile::tempdir().expect("temporary directory");
    let data = directory.path().join("data");
    private_data_root(&data);
    let root = directory.path().join("projects");
    let work = root.join("work");
    fs::create_dir_all(&work).expect("workspace");
    let source = directory.path().join("sources").join("demo-skill");
    fs::create_dir_all(&source).expect("source");
    fs::write(
        source.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: Demonstrates a skill.\n---\nFollow the steps.\n",
    )
    .expect("SKILL.md");
    let mut control = ControlStore::open(&data).expect("store");
    control
        .set_development_root(&root, control.revision())
        .expect("root");
    let workspace_id = control
        .register_workspace(&work, control.revision())
        .expect("workspace")
        .id();
    control
        .set_permissions(
            workspace_id,
            PermissionSet {
                model_tools: true,
                read: true,
                ..PermissionSet::default()
            },
            control.revision(),
        )
        .expect("permissions");
    control
        .import_skill(&source, control.revision())
        .expect("import");
    let name = SkillName::new("demo-skill").expect("name");
    set(&mut control, &name, SkillSetting::Enabled);
    Scene {
        _directory: directory,
        data,
        workspace_id,
        name,
    }
}

fn set(control: &mut ControlStore, name: &SkillName, setting: SkillSetting) {
    control
        .set_skill_enablement(name, SkillScope::Application, setting, control.revision())
        .expect("enablement");
}

fn selection(scene: &Scene) -> SkillPrompt {
    let control = ControlStore::open_existing(&scene.data)
        .expect("open")
        .expect("store");
    let enabled = control
        .load_enabled_skill(scene.workspace_id, &scene.name)
        .expect("load");
    SkillPrompt {
        skill_id: enabled.record.skill_id(),
        name: scene.name.clone(),
        skill_md_sha256: enabled.loaded.skill_md_sha256,
        body: enabled.loaded.body,
        enablement_changes: enabled.enablement_changes,
    }
}

fn dispatch(scene: &Scene, skills: &[SkillPrompt]) -> ToolExecution {
    let call = ToolCall::parse(&json!({
        "id": "list-1",
        "type": "function",
        "function": {"name": "workspace_list_directory", "arguments": "{\"path\":\"\"}"}
    }))
    .expect("tool call");
    WorkspaceToolDispatcher {
        data_directory: &scene.data,
        session_id: Uuid::new_v4(),
        workspace_id: scene.workspace_id,
        cancel: CancelToken::default(),
        skills,
    }
    .dispatch(&call, 64 * 1024)
}

#[test]
fn given_a_current_selection_when_dispatching_then_the_operation_runs() {
    let scene = scene();
    let skills = [selection(&scene)];

    let execution = dispatch(&scene, &skills);

    assert!(execution.terminal.is_none(), "{:?}", execution.code);
    assert_ne!(execution.code, Some("skill_withdrawn"));
}

#[test]
fn given_a_skill_disabled_and_enabled_again_when_dispatching_then_nothing_runs() {
    let scene = scene();
    let skills = [selection(&scene)];
    {
        let mut control = ControlStore::open_existing(&scene.data)
            .expect("open")
            .expect("store");
        set(&mut control, &scene.name, SkillSetting::Disabled);
        set(&mut control, &scene.name, SkillSetting::Enabled);
    }

    let execution = dispatch(&scene, &skills);

    assert_eq!(execution.code, Some("skill_withdrawn"));
    assert!(matches!(
        execution.terminal,
        Some(TerminalToolError::SkillWithdrawn)
    ));
    assert!(matches!(execution.outcome, ToolOutcome::Denied));
}
