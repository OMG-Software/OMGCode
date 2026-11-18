use super::*;
use std::fs;
use std::path::PathBuf;

use crate::control::{ControlError, ControlStore, FILE_NAME};

const DIGEST_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DIGEST_B: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Fixture {
    _directory: tempfile::TempDir,
    data: PathBuf,
    root: PathBuf,
    store: ControlStore,
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().expect("temporary directory");
    let data = directory.path().join("data");
    let root = directory.path().join("projects");
    fs::create_dir(&root).expect("root");
    let store = ControlStore::open(&data).expect("store");
    Fixture {
        _directory: directory,
        data,
        root,
        store,
    }
}

impl Fixture {
    fn with_workspaces(count: usize) -> (Self, Vec<WorkspaceId>) {
        let mut fixture = fixture();
        fixture
            .store
            .set_development_root(&fixture.root, 0)
            .expect("development root");
        let mut ids = Vec::new();
        for index in 0..count {
            let path = fixture.root.join(format!("work-{index}"));
            fs::create_dir(&path).expect("workspace directory");
            let revision = fixture.store.revision();
            ids.push(
                fixture
                    .store
                    .register_workspace(&path, revision)
                    .expect("workspace")
                    .id(),
            );
        }
        (fixture, ids)
    }

    fn journal(&self) -> String {
        // The journal file is locked while the store is open, so re-encode
        // the events the store holds; they equal the bytes on disk.
        self.store
            .entries
            .iter()
            .map(|entry| serde_json::to_string(entry).expect("event JSON") + "\n")
            .collect()
    }

    fn revision(&self) -> u64 {
        self.store.revision()
    }

    fn add(&mut self, name: &str) -> SkillRecord {
        let revision = self.revision();
        self.store
            .add_skill(&registration(name), revision)
            .expect("skill")
    }
}

fn skill_name(text: &str) -> SkillName {
    SkillName::new(text).expect("valid skill name")
}

fn registration(name: &str) -> SkillRegistration {
    SkillRegistration {
        skill_id: Uuid::new_v4(),
        name: skill_name(name),
        description: "Reviews a change.\nReports findings.".into(),
        license: Some("MIT".into()),
        compatibility: None,
        metadata: BTreeMap::from([("author".to_owned(), "example".to_owned())]),
        allowed_tools: vec!["Read".into(), "Grep".into()],
        package_sha256: DIGEST_A.into(),
        skill_md_sha256: DIGEST_B.into(),
        skill_md_bytes: 2048,
        file_count: 3,
        total_bytes: 4096,
        source: "C:\\skills\\example".into(),
    }
}

fn refusal(result: Result<SkillRecord, ControlError>) -> SkillRefusal {
    match result {
        Err(ControlError::Skill(refusal)) => refusal,
        other => panic!("expected a skill refusal, got {other:?}"),
    }
}

fn copy_journal(text: &str) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("copy");
    let path = directory.path().join("data");
    drop(ControlStore::open(&path).expect("directory"));
    fs::write(path.join(FILE_NAME), text).expect("fixture");
    (directory, path)
}

fn replay_refused(text: &str) {
    let (_directory, path) = copy_journal(text);
    let result = ControlStore::open(&path);
    assert!(
        matches!(result, Err(ControlError::Invalid(_))),
        "{:?}",
        result.err()
    );
}

/// A skill is recorded disabled, listed, and removed with a revision each time.
#[test]
fn given_a_registration_when_added_then_the_skill_is_recorded_disabled() {
    let (mut fixture, ids) = Fixture::with_workspaces(1);
    let before = fixture.revision();
    let record = fixture.add("reviewer");
    assert_eq!(fixture.revision(), before + 1);
    assert_eq!(record.name().as_str(), "reviewer");
    assert_eq!(record.description(), "Reviews a change.\nReports findings.");
    assert_eq!(record.license(), Some("MIT"));
    assert_eq!(record.allowed_tools(), ["Read", "Grep"]);
    assert_eq!(record.skill_md_bytes(), 2048);
    assert_eq!(fixture.store.skills().len(), 1);
    assert_eq!(fixture.store.skill(&skill_name("reviewer")), Some(&record));
    assert!(
        fixture
            .store
            .effective_skill(ids[0], &skill_name("reviewer"))
            .is_none(),
        "a new skill is disabled"
    );
    let encoded = serde_json::to_value(fixture.store.snapshot()).expect("snapshot");
    assert_eq!(encoded["skills"][0]["name"], "reviewer");
}

/// The recorded line has the documented shape.
#[test]
fn given_each_action_when_journaled_then_the_json_shape_is_stable() {
    let (mut fixture, ids) = Fixture::with_workspaces(1);
    fixture.add("reviewer");
    let revision = fixture.revision();
    fixture
        .store
        .set_skill_enablement(
            &skill_name("reviewer"),
            SkillScope::Workspace {
                workspace_id: ids[0],
            },
            SkillSetting::Enabled,
            revision,
        )
        .expect("enable");
    let revision = fixture.revision();
    fixture
        .store
        .remove_skill(&skill_name("reviewer"), revision)
        .expect("remove");
    let text = fixture.journal();
    let added: serde_json::Value = serde_json::from_str(
        text.lines()
            .find(|line| line.contains("skill_added"))
            .expect("added"),
    )
    .expect("json");
    assert_eq!(added["action"]["type"], "skill_added");
    let payload = &added["action"]["payload"];
    assert_eq!(payload["name"], "reviewer");
    assert_eq!(payload["package_sha256"], DIGEST_A);
    assert_eq!(payload["file_count"], 3);
    assert!(payload.get("compatibility").is_none());
    let enabled: serde_json::Value = serde_json::from_str(
        text.lines()
            .find(|line| line.contains("skill_enablement_set"))
            .expect("enablement"),
    )
    .expect("json");
    assert_eq!(enabled["action"]["payload"]["scope"]["kind"], "workspace");
    assert_eq!(enabled["action"]["payload"]["setting"], "enabled");
    assert!(text.contains(r#""type":"skill_removed""#));
}

/// Every kind survives a reopen.
#[test]
fn given_every_action_kind_when_the_journal_is_reopened_then_state_is_replayed() {
    let (mut fixture, ids) = Fixture::with_workspaces(2);
    fixture.add("reviewer");
    fixture.add("planner");
    let name = skill_name("reviewer");
    for (scope, setting) in [
        (SkillScope::Application, SkillSetting::Enabled),
        (
            SkillScope::Workspace {
                workspace_id: ids[0],
            },
            SkillSetting::Disabled,
        ),
        (
            SkillScope::Workspace {
                workspace_id: ids[1],
            },
            SkillSetting::Enabled,
        ),
        (
            SkillScope::Workspace {
                workspace_id: ids[1],
            },
            SkillSetting::Inherit,
        ),
    ] {
        let revision = fixture.revision();
        fixture
            .store
            .set_skill_enablement(&name, scope, setting, revision)
            .expect("setting");
    }
    let revision = fixture.revision();
    fixture
        .store
        .remove_skill(&skill_name("planner"), revision)
        .expect("remove");
    let expected_revision = fixture.revision();
    let Fixture { data, store, .. } = fixture;
    drop(store);

    let store = ControlStore::open(&data).expect("replay");
    assert_eq!(store.revision(), expected_revision);
    assert_eq!(store.skills().len(), 1);
    assert!(store.skill(&skill_name("planner")).is_none());
    assert!(store.effective_skill(ids[0], &name).is_none());
    assert!(store.effective_skill(ids[1], &name).is_some());
}

/// A stale revision is refused and records nothing.
#[test]
fn given_a_stale_revision_when_any_skill_call_is_made_then_it_conflicts() {
    let (mut fixture, _) = Fixture::with_workspaces(0);
    fixture.add("reviewer");
    let stale = fixture.revision() - 1;
    let name = skill_name("reviewer");
    assert!(matches!(
        fixture.store.add_skill(&registration("other"), stale),
        Err(ControlError::Conflict { .. })
    ));
    assert!(matches!(
        fixture.store.remove_skill(&name, stale),
        Err(ControlError::Conflict { .. })
    ));
    assert!(matches!(
        fixture.store.set_skill_enablement(
            &name,
            SkillScope::Application,
            SkillSetting::Enabled,
            stale
        ),
        Err(ControlError::Conflict { .. })
    ));
    assert_eq!(fixture.store.skills().len(), 1);
}

/// Name, identifier, and unknown-skill refusals carry stable codes.
#[test]
fn given_conflicting_requests_when_made_then_stable_codes_are_returned() {
    let (mut fixture, _) = Fixture::with_workspaces(0);
    let first = fixture.add("reviewer");
    let revision = fixture.revision();
    assert_eq!(
        refusal(fixture.store.add_skill(&registration("reviewer"), revision)),
        SkillRefusal::NameTaken
    );
    let mut same_id = registration("other");
    same_id.skill_id = first.skill_id();
    assert_eq!(
        refusal(fixture.store.add_skill(&same_id, revision)),
        SkillRefusal::IdTaken
    );
    let missing = skill_name("missing");
    assert_eq!(
        refusal(fixture.store.remove_skill(&missing, revision)),
        SkillRefusal::NotFound
    );
    assert_eq!(
        refusal(fixture.store.set_skill_enablement(
            &missing,
            SkillScope::Application,
            SkillSetting::Enabled,
            revision
        )),
        SkillRefusal::NotFound
    );
    assert_eq!(SkillRefusal::NameTaken.code(), "skill_name_taken");
    assert_eq!(SkillRefusal::NotFound.code(), "skill_not_found");
    assert_eq!(SkillRefusal::TooManySkills.code(), "too_many_skills");
    assert_eq!(fixture.revision(), revision);
}

/// `inherit` needs a workspace, and a workspace must be registered.
#[test]
fn given_a_bad_scope_when_set_then_it_is_refused_without_a_record() {
    let (mut fixture, _) = Fixture::with_workspaces(0);
    fixture.add("reviewer");
    let revision = fixture.revision();
    let name = skill_name("reviewer");
    assert_eq!(
        refusal(fixture.store.set_skill_enablement(
            &name,
            SkillScope::Application,
            SkillSetting::Inherit,
            revision
        )),
        SkillRefusal::InheritNeedsWorkspace
    );
    let unknown = SkillScope::Workspace {
        workspace_id: WorkspaceId::fresh(),
    };
    assert_eq!(
        refusal(fixture.store.set_skill_enablement(
            &name,
            unknown,
            SkillSetting::Enabled,
            revision
        )),
        SkillRefusal::UnknownWorkspace
    );
    assert_eq!(fixture.revision(), revision);
}

/// A workspace override wins, inherit restores the application value, and an
/// unset skill is disabled.
#[test]
fn given_overrides_when_resolved_then_the_workspace_wins_and_inherit_restores() {
    let (mut fixture, ids) = Fixture::with_workspaces(2);
    fixture.add("reviewer");
    let name = skill_name("reviewer");
    assert!(fixture.store.effective_skill(ids[0], &name).is_none());

    let set = |fixture: &mut Fixture, scope, setting| {
        let revision = fixture.revision();
        fixture
            .store
            .set_skill_enablement(&name, scope, setting, revision)
            .expect("setting");
    };
    set(&mut fixture, SkillScope::Application, SkillSetting::Enabled);
    assert!(fixture.store.effective_skill(ids[0], &name).is_some());
    assert!(fixture.store.effective_skill(ids[1], &name).is_some());

    let first = SkillScope::Workspace {
        workspace_id: ids[0],
    };
    set(&mut fixture, first, SkillSetting::Disabled);
    assert!(fixture.store.effective_skill(ids[0], &name).is_none());
    assert!(fixture.store.effective_skill(ids[1], &name).is_some());

    set(&mut fixture, first, SkillSetting::Inherit);
    assert!(fixture.store.effective_skill(ids[0], &name).is_some());

    set(
        &mut fixture,
        SkillScope::Application,
        SkillSetting::Disabled,
    );
    set(&mut fixture, first, SkillSetting::Enabled);
    assert!(fixture.store.effective_skill(ids[0], &name).is_some());
    assert!(fixture.store.effective_skill(ids[1], &name).is_none());

    let statuses = fixture.store.skill_statuses(Some(ids[0]));
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].application, Some(false));
    assert_eq!(
        statuses[0].workspace_overrides,
        [SkillWorkspaceOverride {
            workspace_id: ids[0],
            enabled: true
        }]
    );
    assert_eq!(statuses[0].effective, Some(true));
    assert_eq!(
        fixture.store.skill_statuses(None)[0].effective,
        None,
        "no effective value without a workspace"
    );
}

/// Removing a skill drops its settings, so a re-added name starts disabled.
#[test]
fn given_a_removed_skill_when_the_name_is_added_again_then_settings_are_gone() {
    let (mut fixture, ids) = Fixture::with_workspaces(1);
    fixture.add("reviewer");
    let name = skill_name("reviewer");
    for scope in [
        SkillScope::Application,
        SkillScope::Workspace {
            workspace_id: ids[0],
        },
    ] {
        let revision = fixture.revision();
        fixture
            .store
            .set_skill_enablement(&name, scope, SkillSetting::Enabled, revision)
            .expect("enable");
    }
    let revision = fixture.revision();
    fixture.store.remove_skill(&name, revision).expect("remove");
    assert!(fixture.store.snapshot().skill_enablement.is_empty());
    fixture.add("reviewer");
    assert!(fixture.store.effective_skill(ids[0], &name).is_none());
    let encoded = serde_json::to_value(fixture.store.snapshot()).expect("snapshot");
    assert!(encoded.get("skill_enablement").is_none());
}

/// The registry holds at most `MAX_REGISTERED_SKILLS`.
#[test]
fn given_a_full_registry_when_one_more_is_added_then_too_many_skills() {
    let (mut fixture, _) = Fixture::with_workspaces(0);
    for index in 0..MAX_REGISTERED_SKILLS {
        fixture.add(&format!("skill-{index}"));
    }
    let revision = fixture.revision();
    assert_eq!(
        refusal(fixture.store.add_skill(&registration("one-more"), revision)),
        SkillRefusal::TooManySkills
    );
    assert_eq!(fixture.revision(), revision);
}

/// Each field bound refuses at add time without a record.
#[test]
fn given_out_of_bound_fields_when_added_then_each_is_refused() {
    let (mut fixture, _) = Fixture::with_workspaces(0);
    let revision = fixture.revision();
    let long = |count: usize| "x".repeat(count);
    let mut cases: Vec<(&str, SkillRegistration)> = Vec::new();
    let mut push = |label, change: &dyn Fn(&mut SkillRegistration)| {
        let mut value = registration("reviewer");
        change(&mut value);
        cases.push((label, value));
    };
    push("empty description", &|r| r.description = String::new());
    push("long description", &|r| r.description = long(1025));
    push("tab in description", &|r| r.description = "a\tb".into());
    push("cr in description", &|r| r.description = "a\rb".into());
    push("empty license", &|r| r.license = Some(String::new()));
    push("long license", &|r| r.license = Some(long(257)));
    push("empty compatibility", &|r| {
        r.compatibility = Some(String::new())
    });
    push("long compatibility", &|r| r.compatibility = Some(long(501)));
    push("lf in license", &|r| r.license = Some("a\nb".into()));
    push("many metadata", &|r| {
        r.metadata = (0..17).map(|n| (format!("k{n}"), String::new())).collect();
    });
    push("empty metadata key", &|r| {
        r.metadata = BTreeMap::from([(String::new(), "v".to_owned())]);
    });
    push("long metadata key", &|r| {
        r.metadata = BTreeMap::from([(long(65), "v".to_owned())]);
    });
    push("long metadata value", &|r| {
        r.metadata = BTreeMap::from([("k".to_owned(), long(257))]);
    });
    push("many tools", &|r| {
        r.allowed_tools = (0..33).map(|n| format!("t{n}")).collect();
    });
    push("long tools", &|r| {
        r.allowed_tools = (0..5).map(|_| long(250)).collect();
    });
    push("tool with space", &|r| r.allowed_tools = vec!["a b".into()]);
    push("short digest", &|r| r.package_sha256 = "abc".into());
    push("upper digest", &|r| {
        r.skill_md_sha256 = DIGEST_A.to_uppercase()
    });
    push("non hex digest", &|r| r.skill_md_sha256 = "g".repeat(64));
    push("zero md", &|r| r.skill_md_bytes = 0);
    push("big md", &|r| r.skill_md_bytes = MAX_SKILL_MD_BYTES + 1);
    push("zero files", &|r| r.file_count = 0);
    push("many files", &|r| r.file_count = MAX_SKILL_FILES + 1);
    push("big package", &|r| {
        r.total_bytes = MAX_SKILL_PACKAGE_BYTES + 1
    });
    push("package below md", &|r| {
        r.total_bytes = r.skill_md_bytes - 1
    });
    push("empty source", &|r| r.source = String::new());
    push("long source", &|r| r.source = long(1025));
    push("newline source", &|r| r.source = "a\nb".into());
    push("nil id", &|r| r.skill_id = Uuid::nil());
    for (label, value) in cases {
        let result = fixture.store.add_skill(&value, revision);
        assert!(
            matches!(
                result,
                Err(ControlError::Skill(SkillRefusal::InvalidField(_)))
            ),
            "{label}: {result:?}"
        );
    }
    assert_eq!(fixture.revision(), revision);
    let mut edge = registration("reviewer");
    edge.description = format!("{}\n{}", long(500), long(523));
    edge.license = Some(long(256));
    edge.compatibility = Some(long(500));
    edge.metadata = (0..16).map(|n| (format!("{n:0>64}"), long(256))).collect();
    edge.allowed_tools = (0..32).map(|_| long(32)).collect();
    edge.skill_md_bytes = MAX_SKILL_MD_BYTES;
    edge.file_count = MAX_SKILL_FILES;
    edge.total_bytes = MAX_SKILL_PACKAGE_BYTES;
    edge.source = long(1024);
    fixture
        .store
        .add_skill(&edge, revision)
        .expect("every bound at its limit is accepted");
}

/// Hand-edited lines are refused at replay.
#[test]
fn given_a_tampered_journal_when_replayed_then_it_is_refused() {
    let (mut fixture, ids) = Fixture::with_workspaces(1);
    fixture.add("reviewer");
    let revision = fixture.revision();
    fixture
        .store
        .set_skill_enablement(
            &skill_name("reviewer"),
            SkillScope::Workspace {
                workspace_id: ids[0],
            },
            SkillSetting::Enabled,
            revision,
        )
        .expect("enable");
    let original = fixture.journal();
    let workspace = ids[0].to_string();
    let edits: Vec<(String, String)> = vec![
        (DIGEST_A.into(), "AAAA".into()),
        (DIGEST_B.into(), DIGEST_B.to_uppercase()),
        (r#""name":"reviewer""#.into(), r#""name":"Bad Name""#.into()),
        (
            r#""skill_md_bytes":2048"#.into(),
            r#""skill_md_bytes":0"#.into(),
        ),
        (r#""file_count":3"#.into(), r#""file_count":129"#.into()),
        (
            r#""total_bytes":4096"#.into(),
            r#""total_bytes":4194305"#.into(),
        ),
        (
            r#""license":"MIT""#.into(),
            r#""license":"MIT","extra":1"#.into(),
        ),
        (
            r#""license":"MIT""#.into(),
            r#""license":"M\u0001T""#.into(),
        ),
        (r#""setting":"enabled""#.into(), r#""setting":"on""#.into()),
        (
            r#""scope":{"kind":"workspace","workspace_id":"#.to_owned()
                + &format!(r#""{workspace}"}}"#),
            r#""scope":{"kind":"application"}"#.into(),
        ),
    ];
    // The last edit is valid and must replay; the others must not.
    let (valid, invalid) = edits.split_last().expect("edits");
    for (from, to) in invalid {
        assert!(original.contains(from.as_str()), "{from}");
        replay_refused(&original.replacen(from.as_str(), to, 1));
    }
    assert!(original.contains(valid.0.as_str()));
    let (_directory, path) = copy_journal(&original.replacen(valid.0.as_str(), &valid.1, 1));
    ControlStore::open(&path).expect("an application-scope enablement is valid");
}

/// Structural tampering: duplicates, unknown skills, inherit, and workspaces.
#[test]
fn given_structural_tampering_when_replayed_then_it_is_refused() {
    let (mut fixture, ids) = Fixture::with_workspaces(1);
    fixture.add("reviewer");
    let revision = fixture.revision();
    fixture
        .store
        .set_skill_enablement(
            &skill_name("reviewer"),
            SkillScope::Application,
            SkillSetting::Enabled,
            revision,
        )
        .expect("enable");
    let original = fixture.journal();
    let lines: Vec<&str> = original.lines().collect();
    let added = lines
        .iter()
        .position(|line| line.contains("skill_added"))
        .expect("added");
    let enablement = lines
        .iter()
        .position(|line| line.contains("skill_enablement_set"))
        .expect("enablement");

    // Duplicate name: replay the add line again under a fresh sequence is
    // refused for its envelope, so edit a second add instead.
    let mut second = fixture;
    let revision = second.revision();
    second
        .store
        .add_skill(&registration("planner"), revision)
        .expect("second skill");
    let text = second.journal();
    let renamed = text.replacen(r#""name":"planner""#, r#""name":"reviewer""#, 1);
    replay_refused(&renamed);

    // Duplicate identifier.
    let first_id = lines[added]
        .split(r#""skill_id":""#)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("id")
        .to_owned();
    let second_line = text
        .lines()
        .rfind(|line| line.contains("skill_added"))
        .expect("second add");
    let second_id = second_line
        .split(r#""skill_id":""#)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("id");
    replay_refused(&text.replacen(second_id, &first_id, 1));

    // Enablement of an unknown skill.
    let unknown = Uuid::new_v4().to_string();
    let edited = lines[enablement].replacen(&first_id, &unknown, 1);
    let mut tampered = lines.clone();
    tampered[enablement] = &edited;
    replay_refused(&(tampered.join("\n") + "\n"));

    // Inherit at the application scope.
    let inherit = lines[enablement].replacen(r#""setting":"enabled""#, r#""setting":"inherit""#, 1);
    let mut tampered = lines.clone();
    tampered[enablement] = &inherit;
    replay_refused(&(tampered.join("\n") + "\n"));

    // A workspace scope naming a workspace that does not exist.
    let ghost = lines[enablement].replacen(
        r#""scope":{"kind":"application"}"#,
        &format!(
            r#""scope":{{"kind":"workspace","workspace_id":"{}"}}"#,
            Uuid::new_v4()
        ),
        1,
    );
    assert_ne!(ghost, lines[enablement]);
    let mut tampered = lines;
    tampered[enablement] = &ghost;
    replay_refused(&(tampered.join("\n") + "\n"));
    assert_eq!(ids.len(), 1);
}

/// Removal and enablement lines for a skill that was never added are refused.
#[test]
fn given_a_removal_for_an_unregistered_skill_when_replayed_then_it_is_refused() {
    let (mut fixture, _) = Fixture::with_workspaces(0);
    let record = fixture.add("reviewer");
    let revision = fixture.revision();
    fixture
        .store
        .remove_skill(&skill_name("reviewer"), revision)
        .expect("remove");
    let text = fixture.journal();
    let removal = text
        .lines()
        .rfind(|line| line.contains("skill_removed"))
        .expect("removal");
    let edited = removal.replacen(
        &record.skill_id().to_string(),
        &Uuid::new_v4().to_string(),
        1,
    );
    replay_refused(&text.replacen(removal, &edited, 1));
}

/// A journal with no skill record has no skill fields and replays to the
/// same bytes.
#[test]
fn given_a_journal_without_skills_when_replayed_then_bytes_are_identical() {
    let (mut fixture, _) = Fixture::with_workspaces(1);
    let encoded = serde_json::to_value(fixture.store.snapshot()).expect("snapshot");
    assert!(encoded.get("skills").is_none());
    assert!(encoded.get("skill_enablement").is_none());
    let revision = fixture.revision();
    fixture
        .store
        .add_provider_profile(
            &crate::control::ProviderProfileDeclaration {
                name: "work".into(),
                kind: "openai".into(),
                auth: "api_key".into(),
                endpoint: None,
            },
            revision,
        )
        .expect("profile");
    let Fixture { data, store, .. } = fixture;
    drop(store);
    let bytes = fs::read_to_string(data.join(FILE_NAME)).expect("journal text");
    assert!(!bytes.contains("skill"));
    let store = ControlStore::open(&data).expect("replay");
    let mut again = String::new();
    for entry in &store.entries {
        again.push_str(&serde_json::to_string(entry).expect("event JSON"));
        again.push('\n');
    }
    assert_eq!(again, bytes);
}

/// The snapshot a caller holds keeps answering after the store changes.
#[test]
fn given_a_snapshot_when_the_skill_is_disabled_later_then_the_copy_is_unchanged() {
    let (mut fixture, ids) = Fixture::with_workspaces(1);
    fixture.add("reviewer");
    let name = skill_name("reviewer");
    let revision = fixture.revision();
    fixture
        .store
        .set_skill_enablement(
            &name,
            SkillScope::Application,
            SkillSetting::Enabled,
            revision,
        )
        .expect("enable");
    let copy = fixture.store.snapshot();
    let revision = fixture.revision();
    fixture
        .store
        .set_skill_enablement(
            &name,
            SkillScope::Application,
            SkillSetting::Disabled,
            revision,
        )
        .expect("disable");
    assert!(copy.effective_skill(ids[0], &name).is_some());
    assert!(fixture.store.effective_skill(ids[0], &name).is_none());
}
