use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::*;
use crate::control::ControlStore;
use crate::policy::{Access, PermissionSet, SessionBinding, WorkspaceId};
use crate::replacement::{
    CheckpointCompaction, CheckpointDeletion, CheckpointError, CheckpointInspection,
    CheckpointRetention, RecordedArtifact, RecordedReplacement, ReplacementKind,
    ReplacementOutcome, ReplacementProgress,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use uuid::Uuid;

const ORIGINAL: &[u8] = b"captured original bytes\n";
const PROPOSED: &[u8] = b"complete proposed replacement\n";

struct Fixture {
    data: PathBuf,
    workspace: PathBuf,
    workspace_id: WorkspaceId,
    session_id: Uuid,
    control: ControlStore,
    _temporary: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("create isolated fixture");
        let data = temporary.path().join("application-data");
        let root = temporary.path().join("development-root");
        let workspace = root.join("workspace");
        fs::create_dir(&root).expect("create development root");
        fs::create_dir(&workspace).expect("create workspace");
        fs::write(workspace.join("source.txt"), ORIGINAL).expect("write source");
        set_source_security(&workspace.join("source.txt"));
        #[cfg(windows)]
        make_private_root(&data);

        let mut control = ControlStore::open(&data).expect("open control store");
        control
            .set_development_root(&root, control.revision())
            .expect("set development root");
        let registration = control
            .register_workspace(&workspace, control.revision())
            .expect("register workspace");
        control
            .set_permissions(
                registration.id(),
                PermissionSet {
                    read: Access::Allow,
                    write: Access::Prompt,
                    ..PermissionSet::default()
                },
                control.revision(),
            )
            .expect("enable read and write");

        Self {
            data,
            workspace,
            workspace_id: registration.id(),
            session_id: Uuid::new_v4(),
            control,
            _temporary: temporary,
        }
    }

    fn binding(&self) -> SessionBinding {
        self.control
            .bind_session(self.session_id, self.workspace_id)
            .expect("bind session")
    }

    fn target(&self) -> FileTarget {
        FileTarget {
            session_id: self.session_id,
            workspace_id: self.workspace_id,
            path: PathBuf::from("source.txt"),
        }
    }

    fn apply(
        &mut self,
        binding: &SessionBinding,
        hook: impl FnOnce(&mut Store),
    ) -> Result<ReplacementReceipt, FileError> {
        let target = self.target();
        FileBroker::new(&mut self.control).apply_with_preparation_hook(
            binding,
            &target,
            &artifact(ORIGINAL),
            PROPOSED,
            hook,
        )
    }

    fn inspect(&mut self) -> Result<CheckpointInspection, FileError> {
        FileBroker::new(&mut self.control)
            .checkpoint_inspect(CheckpointStore::Workspace(self.workspace_id))
    }

    /// The workspace's checkpoint folder (ADR-0070).
    fn store_dir(&self) -> PathBuf {
        self.workspace.join(".OMGCode")
    }

    fn operation_dir(&self, id: Uuid) -> PathBuf {
        self.store_dir().join("replacement-v1").join(id.to_string())
    }
}

/// Unix fixtures use a distinct mode. Windows sources keep their inherited DACL.
fn set_source_security(path: &Path) {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o640)).expect("set source mode");
    #[cfg(windows)]
    let _ = path;
}

/// Reads the security that a replacement must preserve, as comparable text.
fn source_security(path: &Path) -> String {
    #[cfg(unix)]
    {
        format!(
            "{:o}",
            fs::metadata(path).unwrap().permissions().mode() & 0o777
        )
    }
    #[cfg(windows)]
    {
        icacls(&[path.as_os_str()])
    }
}

/// Runs `icacls` and returns its output with the path line normalized away.
#[cfg(windows)]
fn icacls(arguments: &[&std::ffi::OsStr]) -> String {
    let output = std::process::Command::new("icacls")
        .args(arguments)
        .output()
        .expect("icacls");
    assert!(output.status.success(), "icacls fixture failed");
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let path = arguments[0].to_string_lossy().into_owned();
    text.replace(&path, "")
}

/// Gives the data root the private ACL the Windows store requires.
#[cfg(windows)]
fn make_private_root(data: &Path) {
    let sid = current_user_sid();
    fs::create_dir(data).expect("application data");
    icacls(&[
        data.as_os_str(),
        "/inheritance:r".as_ref(),
        "/grant:r".as_ref(),
        format!("*{sid}:(OI)(CI)F").as_ref(),
    ]);
}

#[cfg(windows)]
fn current_user_sid() -> String {
    let output = std::process::Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .expect("whoami");
    assert!(output.status.success(), "whoami fixture failed");
    let identity = String::from_utf8_lossy(&output.stdout).into_owned();
    identity
        .trim()
        .rsplit(',')
        .next()
        .expect("SID column")
        .trim_matches('"')
        .to_owned()
}

/// Gives `path` exactly this protected DACL: full access for the current user
/// and read access for BUILTIN\Users. It replaces every existing entry, so the
/// result does not depend on the runner's default ACL.
#[cfg(windows)]
fn grant_users_read(path: &Path) {
    let dacl = format!("D:P(A;;FA;;;{})(A;;FR;;;BU)", current_user_sid());
    run_security_script(
        "$dacl = [System.Security.AccessControl.FileSecurity]::new(); \
         $dacl.SetSecurityDescriptorSddlForm($env:OMG_SDDL, 'Access'); \
         [System.IO.File]::SetAccessControl($env:OMG_TO, $dacl)",
        &[("OMG_SDDL", dacl.as_ref()), ("OMG_TO", path.as_os_str())],
    );
}

/// Reports whether `path` grants BUILTIN\Users access.
#[cfg(windows)]
fn grants_users(path: &Path) -> bool {
    let output = std::process::Command::new("icacls")
        .arg(path)
        .args(["/findsid", "*S-1-5-32-545"])
        .output()
        .expect("icacls");
    assert!(output.status.success(), "icacls fixture failed");
    String::from_utf8_lossy(&output.stdout).contains(&*path.to_string_lossy())
}

/// Reads a file's security exactly as recovery compares it with the record.
#[cfg(windows)]
fn native_security(path: &Path) -> omgcode_platform::private_store::NativeSecurity {
    let file = fs::File::open(path).expect("open file for its security");
    omgcode_platform::private_store::stage_metadata(&file)
        .expect("read native security")
        .security
}

/// Copies the owner, group, and protected DACL of `from` onto `to`, as an
/// interrupted commit attempt would leave a stage.
///
/// .NET writes access entries in its own canonical order, while recovery
/// compares descriptors exactly. `from` must have been written by
/// `grant_users_read`, which uses the same API, so its order already matches.
#[cfg(windows)]
fn copy_security(from: &Path, to: &Path) {
    run_security_script(
        "$parts = [System.Security.AccessControl.AccessControlSections]'Owner,Group,Access'; \
         $sddl = [System.IO.File]::GetAccessControl($env:OMG_FROM).GetSecurityDescriptorSddlForm($parts); \
         $copy = [System.Security.AccessControl.FileSecurity]::new(); \
         $copy.SetSecurityDescriptorSddlForm($sddl, $parts); \
         [System.IO.File]::SetAccessControl($env:OMG_TO, $copy)",
        &[("OMG_FROM", from.as_os_str()), ("OMG_TO", to.as_os_str())],
    );
}

/// Runs a Windows PowerShell security fixture with values passed through the
/// environment.
///
/// It uses .NET only, no cmdlets: a module path inherited from another shell
/// can stop Windows PowerShell loading them. `Set-Acl` would also write the
/// audit SACL, which needs elevation.
#[cfg(windows)]
fn run_security_script(script: &str, values: &[(&str, &std::ffi::OsStr)]) {
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env_remove("PSModulePath")
        .envs(values.iter().copied())
        .output()
        .expect("powershell");
    assert!(
        output.status.success() && output.stderr.is_empty(),
        "security fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn artifact(bytes: &[u8]) -> RecordedArtifact {
    RecordedArtifact {
        bytes: u64::try_from(bytes.len()).expect("bounded test bytes"),
        sha256: Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    }
}

fn uncertain_id(result: Result<ReplacementReceipt, FileError>) -> Uuid {
    match result {
        Err(FileError::Checkpoint(CheckpointError::Uncertain {
            operation_id: Some(id),
            ..
        })) => id,
        other => panic!("expected an uncertain replacement with an operation ID: {other:?}"),
    }
}

fn operation(
    inspection: &CheckpointInspection,
    id: Uuid,
) -> &crate::replacement::RecordedReplacement {
    inspection
        .operations
        .iter()
        .find(|operation| operation.operation_id == id)
        .expect("operation retained in inspection")
}

fn assert_payloads(fixture: &Fixture, id: Uuid) {
    let directory = fixture.operation_dir(id);
    assert_eq!(fs::read(directory.join("original")).unwrap(), ORIGINAL);
    assert_eq!(fs::read(directory.join("proposed")).unwrap(), PROPOSED);
}

fn assert_stage(fixture: &Fixture, id: Uuid, expected: bool) {
    let path = fixture.operation_dir(id).join("stage");
    assert_eq!(path.exists(), expected, "stage presence: {path:?}");
    if expected {
        assert_eq!(fs::read(path).expect("read retained stage"), PROPOSED);
    }
}

/// An edit cannot reach a Windows target in place while its source handle
/// denies write sharing, so only the entry swap is a possible conflict there.
fn independent_edit_cases() -> &'static [bool] {
    if cfg!(windows) {
        &[true]
    } else {
        &[false, true]
    }
}

#[test]
fn independent_edits_after_checkpoint_conflict_and_preserve_evidence() {
    for &replace_inode in independent_edit_cases() {
        let mut fixture = Fixture::new();
        let binding = fixture.binding();
        let target = fixture.workspace.join("source.txt");
        let workspace = fixture.workspace.clone();
        let security = source_security(&target);
        let independent = if replace_inode {
            ORIGINAL
        } else {
            b"independent in-place edit".as_slice()
        };
        let result = fixture.apply(&binding, |_store| {
            if replace_inode {
                let replacement = workspace.join("independent.txt");
                fs::write(&replacement, independent).expect("write independent inode");
                set_source_security(&replacement);
                fs::rename(replacement, &target).expect("replace target inode");
            } else {
                fs::write(&target, independent).expect("edit target in place");
            }
        });
        assert!(
            matches!(
                result,
                Err(FileError::Checkpoint(CheckpointError::Conflict))
            ),
            "expected conflict for inode replacement={replace_inode}, got {result:?}"
        );
        assert_eq!(fs::read(&target).unwrap(), independent);
        assert_eq!(source_security(&target), security);

        let inspection = fixture.inspect().expect("inspect conflict evidence");
        assert_eq!(inspection.pending, None);
        let record = inspection
            .operations
            .iter()
            .find(|record| record.progress == ReplacementProgress::StagePrepared)
            .expect("stage-prepared conflict record");
        assert_eq!(record.outcome, Some(ReplacementOutcome::Conflict));
        assert_stage(&fixture, record.operation_id, true);
        assert_payloads(&fixture, record.operation_id);
    }
}

#[test]
fn publication_faults_reopen_with_exact_progress_and_no_automatic_retry() {
    let cases = [
        (
            "stage_prepared_sync",
            ReplacementProgress::StagePrepared,
            false,
            None,
        ),
        (
            "commit_intent_sync",
            ReplacementProgress::CommitIntended,
            false,
            None,
        ),
        (
            "before_native_commit",
            ReplacementProgress::CommitIntended,
            false,
            None,
        ),
        (
            "after_native_commit",
            ReplacementProgress::CommitIntended,
            true,
            None,
        ),
        (
            "publication_outcome_sync",
            ReplacementProgress::CommitIntended,
            true,
            Some(ReplacementOutcome::Succeeded),
        ),
    ];
    for (fault, progress, applied, outcome) in cases {
        let mut fixture = Fixture::new();
        let binding = fixture.binding();
        let security = source_security(&fixture.workspace.join("source.txt"));
        let id = uncertain_id(fixture.apply(&binding, |store| {
            store.inject_publication_fault(fault);
        }));
        assert_eq!(
            fs::read(fixture.workspace.join("source.txt")).unwrap(),
            if applied { PROPOSED } else { ORIGINAL }
        );
        assert_eq!(
            source_security(&fixture.workspace.join("source.txt")),
            security
        );
        assert_stage(&fixture, id, !applied);
        assert_payloads(&fixture, id);

        let inspection = fixture.inspect().expect("reopen exact ledger evidence");
        assert_eq!(inspection.pending, outcome.is_none().then_some(id));
        let record = operation(&inspection, id);
        assert_eq!(record.progress, progress);
        assert_eq!(record.outcome, outcome);

        if fault == "after_native_commit" {
            FileBroker::new(&mut fixture.control)
                .checkpoint_acknowledge(CheckpointStore::Workspace(fixture.workspace_id), id)
                .expect("acknowledge without claiming success");
            let acknowledged = fixture.inspect().expect("inspect acknowledgement");
            let record = operation(&acknowledged, id);
            assert!(record.acknowledged);
            assert_eq!(record.outcome, None);
            assert_eq!(acknowledged.pending, None);
            assert_eq!(
                fs::read(fixture.workspace.join("source.txt")).unwrap(),
                PROPOSED
            );
        }
    }
}

#[test]
fn partial_publication_records_fail_closed_without_changing_evidence() {
    for (fault, applied) in [
        ("stage_prepared_partial_write", false),
        ("commit_intent_partial_write", false),
        ("publication_outcome_partial_write", true),
    ] {
        let mut fixture = Fixture::new();
        let binding = fixture.binding();
        let id = uncertain_id(fixture.apply(&binding, |store| {
            store.inject_publication_fault(fault);
        }));
        let target = fixture.workspace.join("source.txt");
        assert_eq!(
            fs::read(&target).unwrap(),
            if applied { PROPOSED } else { ORIGINAL }
        );
        assert_stage(&fixture, id, !applied);
        assert_payloads(&fixture, id);

        let ledger = fixture
            .store_dir()
            .join("replacement-v1")
            .join("ledger.jsonl");
        let before = fs::read(&ledger).expect("read partial ledger evidence");
        assert!(fixture.inspect().is_err(), "{fault} must not replay");
        assert!(
            FileBroker::new(&mut fixture.control)
                .checkpoint_acknowledge(CheckpointStore::Workspace(fixture.workspace_id), id)
                .is_err(),
            "{fault} must not acknowledge corrupt evidence"
        );
        assert_eq!(fs::read(&ledger).expect("ledger preserved"), before);
        assert_eq!(
            fs::read(&target).expect("target preserved"),
            if applied { PROPOSED } else { ORIGINAL }
        );
        assert_payloads(&fixture, id);
    }
}

/// Change the stage's security so it no longer matches its recorded source.
fn tamper_stage_security(stage: &Path) {
    #[cfg(unix)]
    fs::set_permissions(stage, fs::Permissions::from_mode(0o600)).expect("tamper stage mode");
    #[cfg(windows)]
    icacls(&[
        stage.as_os_str(),
        "/grant".as_ref(),
        "*S-1-1-0:(R)".as_ref(),
    ]);
}

#[test]
fn tampered_stage_metadata_fails_closed_and_preserves_the_bytes() {
    let mut fixture = Fixture::new();
    let binding = fixture.binding();
    let id = uncertain_id(fixture.apply(&binding, |store| {
        store.inject_publication_fault("before_native_commit");
    }));
    let stage = fixture.operation_dir(id).join("stage");
    let stage_bytes = fs::read(&stage).expect("read retained stage");
    tamper_stage_security(&stage);
    let security = source_security(&stage);
    let ledger = fixture
        .store_dir()
        .join("replacement-v1")
        .join("ledger.jsonl");
    let ledger_bytes = fs::read(&ledger).expect("read complete ledger");

    assert!(
        fixture.inspect().is_err(),
        "stage metadata tampering must fail closed"
    );
    assert!(
        FileBroker::new(&mut fixture.control)
            .checkpoint_acknowledge(CheckpointStore::Workspace(fixture.workspace_id), id)
            .is_err(),
        "tampered stage cannot be acknowledged"
    );
    assert_eq!(fs::read(&stage).expect("stage preserved"), stage_bytes);
    assert_eq!(source_security(&stage), security);
    assert_eq!(fs::read(&ledger).expect("ledger preserved"), ledger_bytes);
    assert_payloads(&fixture, id);
    assert_eq!(
        fs::read(fixture.workspace.join("source.txt")).unwrap(),
        ORIGINAL
    );
}

/// A crash after the stage takes the source security, but before the rename,
/// leaves that security on a retained stage. Recovery accepts it only after
/// commit intent.
#[cfg(windows)]
#[test]
fn an_interrupted_attempt_with_the_source_security_reopens_after_commit_intent() {
    let mut fixture = Fixture::new();
    let binding = fixture.binding();
    let source = fixture.workspace.join("source.txt");
    let twin = fixture.workspace.join("twin.txt");
    fs::write(&twin, b"twin").expect("twin");
    grant_users_read(&source);
    grant_users_read(&twin);
    let id = uncertain_id(fixture.apply(&binding, |store| {
        store.inject_publication_fault("before_native_commit");
    }));
    let stage = fixture.operation_dir(id).join("stage");
    assert!(
        !grants_users(&stage),
        "the stage is private before the attempt"
    );
    copy_security(&twin, &stage);
    assert!(grants_users(&stage), "fixture recreates the crash state");
    assert_eq!(
        native_security(&stage),
        native_security(&source),
        "fixture gives the stage exactly the source security"
    );

    let inspection = fixture
        .inspect()
        .expect("an interrupted attempt is accepted after commit intent");
    assert_eq!(
        operation(&inspection, id).progress,
        ReplacementProgress::CommitIntended
    );
    assert_eq!(fs::read(&stage).expect("stage preserved"), PROPOSED);
    assert_eq!(fs::read(&source).expect("target unchanged"), ORIGINAL);
}

/// Before commit intent the stage must still be private, even when its
/// security exactly matches the source.
#[cfg(windows)]
#[test]
fn a_conflict_stage_with_the_source_security_fails_closed() {
    let mut fixture = Fixture::new();
    let binding = fixture.binding();
    let target = fixture.workspace.join("source.txt");
    let twin = fixture.workspace.join("twin.txt");
    fs::write(&twin, b"twin").expect("twin");
    grant_users_read(&target);
    grant_users_read(&twin);
    let workspace = fixture.workspace.clone();
    let result = fixture.apply(&binding, |_store| {
        let replacement = workspace.join("independent.txt");
        fs::write(&replacement, ORIGINAL).expect("write independent file");
        fs::rename(replacement, workspace.join("source.txt")).expect("replace target");
    });
    assert!(matches!(
        result,
        Err(FileError::Checkpoint(CheckpointError::Conflict))
    ));
    let inspection = fixture.inspect().expect("inspect conflict evidence");
    let id = inspection
        .operations
        .iter()
        .find(|record| record.progress == ReplacementProgress::StagePrepared)
        .expect("stage-prepared conflict record")
        .operation_id;
    let stage = fixture.operation_dir(id).join("stage");
    assert!(!grants_users(&stage), "a conflict retains a private stage");
    copy_security(&twin, &stage);
    assert!(
        fixture.inspect().is_err(),
        "a non-private stage before commit intent fails closed"
    );
    assert_eq!(fs::read(&stage).expect("stage preserved"), PROPOSED);
}

#[test]
fn apply_rejects_foreign_and_stale_bindings_and_either_denied_preference() {
    let mut fixture = Fixture::new();
    let live = fixture.binding();
    let revision = fixture.control.revision();
    let mismatches = [
        SessionBinding::new(
            Uuid::new_v4(),
            fixture.session_id,
            fixture.workspace_id,
            revision,
        ),
        SessionBinding::new(
            live.store_id(),
            Uuid::new_v4(),
            fixture.workspace_id,
            revision,
        ),
        SessionBinding::new(
            live.store_id(),
            fixture.session_id,
            WorkspaceId::fresh(),
            revision,
        ),
    ];
    for binding in mismatches {
        assert!(matches!(
            fixture.apply(&binding, |_| {}),
            Err(FileError::Checkpoint(CheckpointError::Denied(_)))
        ));
    }
    for (read, write) in [
        (Access::Deny, Access::Prompt),
        (Access::Allow, Access::Deny),
    ] {
        let stale = fixture.binding();
        fixture
            .control
            .set_permissions(
                fixture.workspace_id,
                PermissionSet {
                    read,
                    write,
                    ..PermissionSet::default()
                },
                fixture.control.revision(),
            )
            .expect("change current preferences");
        let fresh = fixture.binding();
        for binding in [&stale, &fresh] {
            assert!(matches!(
                fixture.apply(binding, |_| {}),
                Err(FileError::Checkpoint(CheckpointError::Denied(_)))
            ));
        }
    }
    assert_eq!(
        fs::read(fixture.workspace.join("source.txt")).expect("unchanged source"),
        ORIGINAL
    );
    // A denial precedes the workspace checkpoint folder (ADR-0070).
    assert!(!fixture.store_dir().exists());
    assert!(!fixture.data.join("file-operations-v1.jsonl").exists());
}

fn apply_bytes(
    fixture: &mut Fixture,
    current: &[u8],
    proposed: &[u8],
) -> Result<ReplacementReceipt, FileError> {
    let binding = fixture.binding();
    let target = fixture.target();
    FileBroker::new(&mut fixture.control).apply_replacement(
        &binding,
        &target,
        &artifact(current),
        proposed,
    )
}

fn applied_id(fixture: &mut Fixture, current: &[u8], proposed: &[u8]) -> Uuid {
    apply_bytes(fixture, current, proposed)
        .expect("apply")
        .operation_id
        .expect("changed bytes create an operation")
}

fn restore(
    fixture: &mut Fixture,
    checkpoint: Uuid,
    current: &[u8],
) -> Result<ReplacementReceipt, FileError> {
    let binding = fixture.binding();
    let target = fixture.target();
    FileBroker::new(&mut fixture.control).restore_checkpoint(
        &binding,
        &target,
        checkpoint,
        &artifact(current),
    )
}

fn delete(fixture: &mut Fixture, id: Uuid) -> Result<CheckpointDeletion, FileError> {
    FileBroker::new(&mut fixture.control)
        .checkpoint_delete(CheckpointStore::Workspace(fixture.workspace_id), id)
}

fn checkpoint_error<T: std::fmt::Debug>(result: Result<T, FileError>) -> CheckpointError {
    match result {
        Err(FileError::Checkpoint(error)) => error,
        other => panic!("expected a checkpoint error: {other:?}"),
    }
}

#[test]
fn restore_reverts_through_a_new_checkpoint_and_preserves_security() {
    let mut fixture = Fixture::new();
    let target = fixture.workspace.join("source.txt");
    let security = source_security(&target);
    let applied = applied_id(&mut fixture, ORIGINAL, PROPOSED);
    assert_eq!(fs::read(&target).unwrap(), PROPOSED);

    let receipt = restore(&mut fixture, applied, PROPOSED).expect("restore");
    assert!(receipt.changed && receipt.applied);
    assert_eq!(receipt.original, artifact(PROPOSED));
    assert_eq!(receipt.proposed, artifact(ORIGINAL));
    let restored = receipt.operation_id.expect("restore is a new operation");
    assert_ne!(restored, applied);
    assert_eq!(fs::read(&target).unwrap(), ORIGINAL);
    assert_eq!(source_security(&target), security);

    // The bytes that the restore replaced are themselves checkpointed.
    let directory = fixture.operation_dir(restored);
    assert_eq!(fs::read(directory.join("original")).unwrap(), PROPOSED);
    assert_eq!(fs::read(directory.join("proposed")).unwrap(), ORIGINAL);
    assert_payloads(&fixture, applied);
    let inspection = fixture.inspect().expect("inspect restore");
    assert_eq!(inspection.pending, None);
    assert_eq!(
        operation(&inspection, restored).outcome,
        Some(ReplacementOutcome::Succeeded)
    );
    assert_eq!(
        operation(&inspection, applied).retention,
        CheckpointRetention::Retained
    );

    // Restoring bytes equal to the current file changes nothing.
    let unchanged = restore(&mut fixture, applied, ORIGINAL).expect("unchanged restore");
    assert!(!unchanged.changed && !unchanged.applied);
    assert_eq!(unchanged.operation_id, None);
    assert_eq!(fixture.inspect().expect("inspect").operations.len(), 2);
}

#[test]
fn restore_refuses_stale_content_foreign_targets_and_missing_checkpoints() {
    let mut fixture = Fixture::new();
    let target = fixture.workspace.join("source.txt");
    let applied = applied_id(&mut fixture, ORIGINAL, PROPOSED);

    // A later edit is never overwritten merely because a checkpoint exists.
    let later = b"later independent edit\n".as_slice();
    fs::write(&target, later).expect("later edit");
    assert!(matches!(
        checkpoint_error(restore(&mut fixture, applied, PROPOSED)),
        CheckpointError::Conflict
    ));
    assert_eq!(fs::read(&target).unwrap(), later);
    assert!(matches!(
        checkpoint_error(restore(&mut fixture, Uuid::new_v4(), later)),
        CheckpointError::Unavailable
    ));

    // A checkpoint restores only its recorded workspace and exact target.
    let other_path = fixture.workspace.join("other.txt");
    fs::write(&other_path, later).expect("other target");
    let binding = fixture.binding();
    let other = FileTarget {
        path: PathBuf::from("other.txt"),
        ..fixture.target()
    };
    assert!(matches!(
        checkpoint_error(FileBroker::new(&mut fixture.control).restore_checkpoint(
            &binding,
            &other,
            applied,
            &artifact(later),
        )),
        CheckpointError::TargetMismatch
    ));
    assert_eq!(fs::read(&other_path).unwrap(), later);

    // There is no restore-as-create.
    fs::remove_file(&target).expect("remove target");
    assert!(restore(&mut fixture, applied, later).is_err());
    assert!(!target.exists());

    // Current preferences still apply.
    fs::write(&target, later).expect("recreate target");
    fixture
        .control
        .set_permissions(
            fixture.workspace_id,
            PermissionSet {
                read: Access::Allow,
                write: Access::Deny,
                ..PermissionSet::default()
            },
            fixture.control.revision(),
        )
        .expect("deny write");
    assert!(matches!(
        checkpoint_error(restore(&mut fixture, applied, later)),
        CheckpointError::Denied(_)
    ));
    assert_eq!(fs::read(&target).unwrap(), later);
    assert_eq!(fixture.inspect().expect("inspect").operations.len(), 1);
    assert_payloads(&fixture, applied);
}

#[test]
fn deletion_removes_resolved_entries_and_keeps_their_history() {
    let mut fixture = Fixture::new();
    let binding = fixture.binding();
    let target = fixture.workspace.join("source.txt");
    let workspace = fixture.workspace.clone();
    // A conflict retains its stage; deletion removes it with the entry.
    let conflict = fixture.apply(&binding, |_| {
        let replacement = workspace.join("independent.txt");
        fs::write(&replacement, ORIGINAL).expect("independent inode");
        set_source_security(&replacement);
        fs::rename(replacement, workspace.join("source.txt")).expect("swap target");
    });
    assert!(matches!(
        checkpoint_error(conflict),
        CheckpointError::Conflict
    ));
    let conflict = fixture.inspect().expect("inspect conflict").operations[0].operation_id;
    assert_stage(&fixture, conflict, true);
    let applied = applied_id(&mut fixture, ORIGINAL, PROPOSED);
    let before = fixture.inspect().expect("inspect").managed_bytes;

    for id in [conflict, applied] {
        let deleted = delete(&mut fixture, id).expect("delete resolved entry");
        assert_eq!(deleted.operation_id, id);
        assert!(!deleted.resumed);
        assert!(!fixture.operation_dir(id).exists());
    }
    let inspection = fixture.inspect().expect("inspect deletions");
    assert!(inspection.managed_bytes < before);
    assert_eq!(inspection.pending_deletion, None);
    for (id, outcome) in [
        (conflict, ReplacementOutcome::Conflict),
        (applied, ReplacementOutcome::Succeeded),
    ] {
        let record = operation(&inspection, id);
        assert_eq!(record.retention, CheckpointRetention::Deleted);
        assert_eq!(record.outcome, Some(outcome));
    }
    assert_eq!(fs::read(&target).unwrap(), PROPOSED);
    assert!(matches!(
        checkpoint_error(delete(&mut fixture, applied)),
        CheckpointError::Unavailable
    ));
    assert!(matches!(
        checkpoint_error(restore(&mut fixture, applied, PROPOSED)),
        CheckpointError::Unavailable
    ));
    applied_id(&mut fixture, PROPOSED, ORIGINAL);
}

#[test]
fn deletion_frees_a_retained_slot_without_evicting_history() {
    let mut fixture = Fixture::new();
    let mut first = None;
    for index in 0..64 {
        let (current, proposed) = if index % 2 == 0 {
            (ORIGINAL, PROPOSED)
        } else {
            (PROPOSED, ORIGINAL)
        };
        let id = applied_id(&mut fixture, current, proposed);
        first.get_or_insert(id);
    }
    assert!(matches!(
        checkpoint_error(apply_bytes(&mut fixture, ORIGINAL, PROPOSED)),
        CheckpointError::Limit
    ));
    delete(&mut fixture, first.expect("first operation")).expect("free one slot");
    applied_id(&mut fixture, ORIGINAL, PROPOSED);
    assert_eq!(fixture.inspect().expect("inspect").operations.len(), 65);
}

#[test]
fn unresolved_operations_must_be_acknowledged_before_deletion() {
    let mut fixture = Fixture::new();
    let binding = fixture.binding();
    let id = uncertain_id(fixture.apply(&binding, |store| {
        store.inject_publication_fault("after_native_commit");
    }));
    assert!(matches!(
        checkpoint_error(delete(&mut fixture, id)),
        CheckpointError::Pending(pending) if pending == id
    ));
    assert_payloads(&fixture, id);
    FileBroker::new(&mut fixture.control)
        .checkpoint_acknowledge(CheckpointStore::Workspace(fixture.workspace_id), id)
        .expect("acknowledge");
    assert!(!delete(&mut fixture, id).expect("delete").resumed);
    let inspection = fixture.inspect().expect("inspect");
    let record = operation(&inspection, id);
    assert!(record.acknowledged);
    assert_eq!(record.outcome, None);
    assert_eq!(record.retention, CheckpointRetention::Deleted);
    assert!(!fixture.operation_dir(id).exists());
}

#[test]
fn an_interrupted_deletion_blocks_other_work_until_repeated() {
    for fault in [
        "deletion_intent_sync",
        "deletion_artifact",
        "deletion_directory",
    ] {
        let mut fixture = Fixture::new();
        let target = fixture.workspace.join("source.txt");
        let first = applied_id(&mut fixture, ORIGINAL, PROPOSED);
        let second = applied_id(&mut fixture, PROPOSED, ORIGINAL);
        let error = crate::replacement::store::delete_with_fault(
            &fixture.control,
            CheckpointStore::Workspace(fixture.workspace_id),
            first,
            fault,
        )
        .expect_err(fault);
        assert!(
            matches!(
                error,
                CheckpointError::Uncertain {
                    operation_id: Some(id),
                    ..
                } if id == first
            ),
            "{fault}: {error:?}"
        );
        let inspection = fixture.inspect().expect("reopen");
        assert_eq!(inspection.pending_deletion, Some(first), "{fault}");
        assert_eq!(inspection.pending, None);
        assert_eq!(
            operation(&inspection, first).retention,
            CheckpointRetention::Deleting
        );

        let blocked = |error: CheckpointError| matches!(error, CheckpointError::DeletionPending(id) if id == first);
        assert!(blocked(checkpoint_error(apply_bytes(
            &mut fixture,
            ORIGINAL,
            PROPOSED
        ))));
        assert!(blocked(checkpoint_error(restore(
            &mut fixture,
            second,
            ORIGINAL
        ))));
        assert!(blocked(checkpoint_error(delete(&mut fixture, second))));
        assert!(
            FileBroker::new(&mut fixture.control)
                .checkpoint_acknowledge(CheckpointStore::Workspace(fixture.workspace_id), first)
                .is_err()
        );
        assert_eq!(fs::read(&target).unwrap(), ORIGINAL);
        assert!(fixture.operation_dir(second).join("original").exists());

        let finished = delete(&mut fixture, first).expect("repeat the same deletion");
        assert!(finished.resumed, "{fault}");
        assert!(!fixture.operation_dir(first).exists());
        let inspection = fixture.inspect().expect("inspect");
        assert_eq!(inspection.pending_deletion, None);
        assert_eq!(
            operation(&inspection, first).retention,
            CheckpointRetention::Deleted
        );
        assert_eq!(
            operation(&inspection, second).retention,
            CheckpointRetention::Retained
        );
    }

    // A completion record that became visible despite a sync error stands.
    let mut fixture = Fixture::new();
    let id = applied_id(&mut fixture, ORIGINAL, PROPOSED);
    let error = crate::replacement::store::delete_with_fault(
        &fixture.control,
        CheckpointStore::Workspace(fixture.workspace_id),
        id,
        "deletion_outcome_sync",
    )
    .expect_err("outcome sync fault");
    assert_eq!(error.code(), "checkpoint_uncertain");
    let inspection = fixture.inspect().expect("inspect");
    assert_eq!(inspection.pending_deletion, None);
    assert_eq!(
        operation(&inspection, id).retention,
        CheckpointRetention::Deleted
    );
    assert!(!fixture.operation_dir(id).exists());
}

#[test]
fn partial_deletion_records_fail_closed_without_further_removal() {
    for (fault, removed) in [
        ("deletion_intent_partial_write", false),
        ("deletion_outcome_partial_write", true),
    ] {
        let mut fixture = Fixture::new();
        let id = applied_id(&mut fixture, ORIGINAL, PROPOSED);
        let error = crate::replacement::store::delete_with_fault(
            &fixture.control,
            CheckpointStore::Workspace(fixture.workspace_id),
            id,
            fault,
        )
        .expect_err(fault);
        assert_eq!(error.code(), "checkpoint_uncertain", "{fault}");
        let ledger = fixture
            .store_dir()
            .join("replacement-v1")
            .join("ledger.jsonl");
        let before = fs::read(&ledger).expect("partial ledger evidence");
        assert!(fixture.inspect().is_err(), "{fault} must not replay");
        assert!(delete(&mut fixture, id).is_err(), "{fault}");
        assert_eq!(fs::read(&ledger).expect("ledger preserved"), before);
        assert_eq!(fixture.operation_dir(id).exists(), !removed, "{fault}");
        if !removed {
            assert_payloads(&fixture, id);
        }
    }
}

#[test]
fn deletion_never_removes_an_unknown_name() {
    let mut fixture = Fixture::new();
    let id = applied_id(&mut fixture, ORIGINAL, PROPOSED);
    crate::replacement::store::delete_with_fault(
        &fixture.control,
        CheckpointStore::Workspace(fixture.workspace_id),
        id,
        "deletion_artifact",
    )
    .expect_err("interrupted deletion");
    let unexpected = fixture.operation_dir(id).join("unexpected");
    fs::write(&unexpected, b"operator data").expect("unknown name");
    assert_eq!(
        checkpoint_error(delete(&mut fixture, id)).code(),
        "checkpoint_invalid"
    );
    assert_eq!(fs::read(&unexpected).unwrap(), b"operator data");
    assert!(fixture.inspect().is_err());
}

fn compact(fixture: &mut Fixture) -> Result<CheckpointCompaction, FileError> {
    FileBroker::new(&mut fixture.control)
        .checkpoint_compact(CheckpointStore::Workspace(fixture.workspace_id))
}

fn ledger_path(fixture: &Fixture) -> PathBuf {
    fixture
        .store_dir()
        .join("replacement-v1")
        .join("ledger.jsonl")
}

fn candidate_path(fixture: &Fixture) -> PathBuf {
    fixture
        .store_dir()
        .join("replacement-v1")
        .join("ledger.compact")
}

fn record_count(bytes: &[u8]) -> usize {
    bytes.iter().filter(|byte| **byte == b'\n').count()
}

fn retained_operations(inspection: &CheckpointInspection) -> Vec<RecordedReplacement> {
    inspection
        .operations
        .iter()
        .filter(|operation| operation.retention == CheckpointRetention::Retained)
        .cloned()
        .collect()
}

/// Leave one retained entry and one deleted entry's history. The source ends
/// with its original bytes.
fn retained_and_deleted(fixture: &mut Fixture) -> (Uuid, Uuid) {
    let retained = applied_id(fixture, ORIGINAL, PROPOSED);
    let deleted = applied_id(fixture, PROPOSED, ORIGINAL);
    delete(fixture, deleted).expect("delete one entry");
    (retained, deleted)
}

/// Writes synthesized ledger records with the store's scope and sequence.
struct Padding {
    header: serde_json::Value,
    revision: u64,
    workspace_id: Uuid,
    bytes: Vec<u8>,
    /// Records in `bytes`, counted as they are pushed.
    records: usize,
}

impl Padding {
    fn push(&mut self, kind: &str, payload: serde_json::Value) {
        let record = serde_json::json!({
            "schema_version": 1,
            "control_store_id": self.header["control_store_id"],
            "replacement_store_id": self.header["replacement_store_id"],
            "event_id": Uuid::new_v4(),
            "sequence": self.records,
            "timestamp": "2026-11-17T12:00:00Z",
            "action": {"type": kind, "payload": payload},
        });
        serde_json::to_writer(&mut self.bytes, &record).expect("record JSON");
        self.bytes.push(b'\n');
        self.records += 1;
    }

    /// Record one operation that failed before preparing an entry.
    fn failed(&mut self) -> Uuid {
        let operation_id = Uuid::new_v4();
        self.push(
            "replacement_started",
            serde_json::json!({
                "operation_id": operation_id,
                "session_id": Uuid::new_v4(),
                "workspace_id": self.workspace_id,
                "revision": self.revision,
                "target": crate::session::NativePath::from_path(Path::new("padding.txt"))
                    .expect("native target"),
                "expected": {"bytes": 1, "sha256": "a".repeat(64)},
                "proposed": {"bytes": 1, "sha256": "b".repeat(64)},
                "artifacts": {
                    "original": "original", "proposed": "proposed",
                    "manifest": "manifest.json", "stage": "stage"
                }
            }),
        );
        self.push(
            "replacement_finished",
            serde_json::json!({
                "operation_id": operation_id, "outcome": "failed", "code": "padding"
            }),
        );
        operation_id
    }
}

/// Append deleted operations, four records each, then at most one failed
/// operation without an entry, until the ledger holds at least `minimum`
/// records. Real failures and deletions write these same records. Returns the
/// failed operation, which stays retained and is carried by compaction.
fn pad_ledger(fixture: &Fixture, minimum: usize) -> Option<Uuid> {
    let path = ledger_path(fixture);
    let bytes = fs::read(&path).expect("ledger");
    let header = serde_json::from_slice(
        bytes
            .split_inclusive(|byte| *byte == b'\n')
            .next()
            .expect("header record"),
    )
    .expect("header JSON");
    let mut padding = Padding {
        header,
        revision: fixture.control.revision(),
        workspace_id: *fixture.workspace_id.as_uuid(),
        records: record_count(&bytes),
        bytes,
    };
    while padding.records + 4 <= minimum {
        let operation_id = padding.failed();
        padding.push(
            "checkpoint_deletion_started",
            serde_json::json!({"operation_id": operation_id}),
        );
        padding.push(
            "checkpoint_deleted",
            serde_json::json!({"operation_id": operation_id}),
        );
    }
    let failed = (padding.records < minimum).then(|| padding.failed());
    fs::write(&path, padding.bytes).expect("write padded ledger");
    crate::replacement::store::trust_ledger_for_test(&fixture.control, fixture.workspace_id);
    failed
}

#[test]
fn compaction_carries_retained_state_and_drops_deleted_history() {
    let mut fixture = Fixture::new();
    let (retained, deleted) = retained_and_deleted(&mut fixture);
    let before = fixture.inspect().expect("inspect");
    let old = fs::read(ledger_path(&fixture)).expect("ledger");
    let header = old
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .expect("header")
        .to_vec();

    let result = compact(&mut fixture).expect("compact");
    assert!(result.compacted);
    assert_eq!((result.carried, result.dropped), (1, 1));
    assert_eq!(result.previous_records, record_count(&old));
    assert_eq!(result.previous_bytes, old.len());
    let new = fs::read(ledger_path(&fixture)).expect("compacted ledger");
    assert_eq!(result.records, 3);
    assert_eq!(record_count(&new), 3);
    assert_eq!(result.bytes, new.len());
    assert!(new.len() < old.len());
    assert!(
        new.starts_with(&header),
        "the header record is kept exactly"
    );
    assert!(!candidate_path(&fixture).exists());

    let after = fixture.inspect().expect("inspect compacted ledger");
    assert_eq!(after.operations, retained_operations(&before));
    assert!(
        after
            .operations
            .iter()
            .all(|entry| entry.operation_id != deleted)
    );
    let compacted_from = after.compacted_from.clone().expect("compaction recorded");
    assert_eq!(compacted_from.ledger, artifact(&old));
    assert_eq!(
        compacted_from.records,
        u64::try_from(record_count(&old)).unwrap()
    );
    assert_eq!(after.managed_bytes, result.managed_bytes);
    assert_payloads(&fixture, retained);

    // Nothing is left to drop, so a second request writes nothing.
    let again = compact(&mut fixture).expect("compact again");
    assert!(!again.compacted);
    assert_eq!(fs::read(ledger_path(&fixture)).unwrap(), new);

    // New work appends after the carried record, and a carried entry
    // restores and deletes like any other.
    let target = fixture.workspace.join("source.txt");
    let applied = applied_id(&mut fixture, ORIGINAL, PROPOSED);
    restore(&mut fixture, retained, PROPOSED).expect("restore a carried entry");
    assert_eq!(fs::read(&target).unwrap(), ORIGINAL);
    delete(&mut fixture, retained).expect("delete a carried entry");
    let chained_from = fs::read(ledger_path(&fixture)).expect("ledger");
    let chained = compact(&mut fixture).expect("compact a compacted ledger");
    assert!(chained.compacted);
    assert_eq!(chained.dropped, 1);
    let inspection = fixture.inspect().expect("inspect");
    assert_eq!(
        inspection.compacted_from.clone().expect("chained").ledger,
        artifact(&chained_from)
    );
    assert_eq!(
        operation(&inspection, applied).retention,
        CheckpointRetention::Retained
    );
    assert_eq!(inspection.operations.len(), 2);
}

#[test]
fn compaction_is_refused_while_an_operation_or_deletion_is_unresolved() {
    let mut fixture = Fixture::new();
    let binding = fixture.binding();
    let pending = uncertain_id(fixture.apply(&binding, |store| {
        store.inject_publication_fault("commit_intent_sync");
    }));
    let ledger = fs::read(ledger_path(&fixture)).expect("ledger");
    assert_eq!(
        checkpoint_error(compact(&mut fixture)).code(),
        "checkpoint_pending"
    );
    assert_eq!(fs::read(ledger_path(&fixture)).unwrap(), ledger);
    FileBroker::new(&mut fixture.control)
        .checkpoint_acknowledge(CheckpointStore::Workspace(fixture.workspace_id), pending)
        .expect("acknowledge");

    let id = applied_id(&mut fixture, ORIGINAL, PROPOSED);
    crate::replacement::store::delete_with_fault(
        &fixture.control,
        CheckpointStore::Workspace(fixture.workspace_id),
        id,
        "deletion_artifact",
    )
    .expect_err("interrupted deletion");
    let ledger = fs::read(ledger_path(&fixture)).expect("ledger");
    assert_eq!(
        checkpoint_error(compact(&mut fixture)).code(),
        "checkpoint_deletion_pending"
    );
    assert_eq!(fs::read(ledger_path(&fixture)).unwrap(), ledger);
    assert!(!candidate_path(&fixture).exists());
}

#[test]
fn compaction_faults_keep_a_valid_ledger_and_are_not_retried() {
    for (fault, replaced) in [
        ("compaction_partial_write", false),
        ("compaction_sync", false),
        ("compaction_rename", false),
        ("compaction_directory_sync", true),
    ] {
        let mut fixture = Fixture::new();
        let (retained, _) = retained_and_deleted(&mut fixture);
        let before = fixture.inspect().expect("inspect");
        let old = fs::read(ledger_path(&fixture)).expect("ledger");

        let error = crate::replacement::store::compact_with_fault(
            &fixture.control,
            CheckpointStore::Workspace(fixture.workspace_id),
            fault,
        )
        .expect_err(fault);
        assert_eq!(error.code(), "checkpoint_uncertain", "{fault}");
        // Either ledger reopens with the same retained state.
        let after = fixture.inspect().expect(fault);
        assert_eq!(
            retained_operations(&after),
            retained_operations(&before),
            "{fault}"
        );
        assert_eq!(after.compacted_from.is_some(), replaced, "{fault}");
        assert_eq!(
            fs::read(ledger_path(&fixture)).unwrap() == old,
            !replaced,
            "{fault}"
        );
        assert_eq!(candidate_path(&fixture).exists(), !replaced, "{fault}");

        // Ordinary work continues beside a stale candidate, which only a
        // later explicit compaction removes.
        applied_id(&mut fixture, ORIGINAL, PROPOSED);
        assert_eq!(candidate_path(&fixture).exists(), !replaced, "{fault}");
        let result = compact(&mut fixture).expect(fault);
        assert!(result.compacted, "{fault}");
        assert!(!candidate_path(&fixture).exists(), "{fault}");
        assert_payloads(&fixture, retained);
    }
}

#[test]
fn a_full_ledger_is_compacted_before_apply_and_before_deletion() {
    // The record bound in `replacement::replay`.
    const MAX_RECORDS: usize = 65_536;
    let mut fixture = Fixture::new();
    let (retained, _) = retained_and_deleted(&mut fixture);

    // Apply needs six more records and deletion two.
    let failed = pad_ledger(&fixture, MAX_RECORDS - 5);
    let padded = fs::read(ledger_path(&fixture)).expect("padded ledger");
    assert!((MAX_RECORDS - 5..=MAX_RECORDS).contains(&record_count(&padded)));
    let inspection = fixture.inspect().expect("a padded ledger is valid");
    assert!(inspection.operations.len() > 16_000);

    let applied = applied_id(&mut fixture, ORIGINAL, PROPOSED);
    let inspection = fixture.inspect().expect("inspect");
    let compacted_from = inspection.compacted_from.clone().expect("compacted first");
    assert_eq!(compacted_from.ledger, artifact(&padded));
    let mut kept = inspection
        .operations
        .iter()
        .map(|entry| entry.operation_id)
        .collect::<Vec<_>>();
    kept.sort();
    let mut expected = vec![retained, applied];
    expected.extend(failed);
    expected.sort();
    assert_eq!(kept, expected);
    assert!(record_count(&fs::read(ledger_path(&fixture)).unwrap()) < 16);

    pad_ledger(&fixture, MAX_RECORDS - 1);
    let padded = fs::read(ledger_path(&fixture)).expect("padded ledger");
    delete(&mut fixture, retained).expect("deletion compacts first");
    let inspection = fixture.inspect().expect("inspect");
    assert_eq!(
        inspection
            .compacted_from
            .clone()
            .expect("compacted again")
            .ledger,
        artifact(&padded)
    );
    assert_eq!(
        operation(&inspection, retained).retention,
        CheckpointRetention::Deleted
    );
    assert_eq!(
        operation(&inspection, applied).retention,
        CheckpointRetention::Retained
    );
    assert!(!fixture.operation_dir(retained).exists());
}

// ---------------------------------------------------------------------------
// Checkpointed removal and restore of a removed file (ADR-0055)
// ---------------------------------------------------------------------------

fn remove_hooked(
    fixture: &mut Fixture,
    expected: &[u8],
    hook: impl FnOnce(&mut Store),
) -> Result<ReplacementReceipt, FileError> {
    let binding = fixture.binding();
    let target = fixture.target();
    FileBroker::new(&mut fixture.control).removal_with_preparation_hook(
        &binding,
        &target,
        &artifact(expected),
        hook,
    )
}

fn remove(fixture: &mut Fixture, expected: &[u8]) -> Result<ReplacementReceipt, FileError> {
    remove_hooked(fixture, expected, |_| {})
}

fn restore_removed(
    fixture: &mut Fixture,
    checkpoint: Uuid,
) -> Result<crate::files::FileCreated, FileError> {
    let binding = fixture.binding();
    let target = fixture.target();
    FileBroker::new(&mut fixture.control).restore_removed_file(&binding, &target, checkpoint)
}

fn removed_id(fixture: &mut Fixture, expected: &[u8]) -> Uuid {
    remove(fixture, expected)
        .expect("removal")
        .operation_id
        .expect("a removal creates an operation")
}

#[test]
fn removal_checkpoints_the_original_and_removes_only_the_file() {
    let mut fixture = Fixture::new();
    let bystander = fixture.workspace.join("bystander.txt");
    fs::write(&bystander, b"keep").expect("bystander");
    let target = fixture.workspace.join("source.txt");

    let receipt = remove(&mut fixture, ORIGINAL).expect("removal");
    assert!(receipt.changed && receipt.applied);
    assert_eq!(receipt.original, artifact(ORIGINAL));
    assert_eq!(receipt.proposed, artifact(b""));
    let id = receipt.operation_id.expect("operation");
    assert!(!target.exists());
    assert_eq!(fs::read(&bystander).unwrap(), b"keep");

    // The private entry keeps the original and the empty proposal, and has no stage.
    let directory = fixture.operation_dir(id);
    assert_eq!(fs::read(directory.join("original")).unwrap(), ORIGINAL);
    assert_eq!(fs::read(directory.join("proposed")).unwrap(), b"");
    assert!(directory.join("manifest.json").exists());
    assert!(!directory.join("stage").exists());
    let inspection = fixture.inspect().expect("inspect removal");
    assert_eq!(inspection.pending, None);
    let record = operation(&inspection, id);
    assert_eq!(record.kind, ReplacementKind::Remove);
    assert_eq!(record.progress, ReplacementProgress::CommitIntended);
    assert_eq!(record.outcome, Some(ReplacementOutcome::Succeeded));
    assert!(record.stage_identity.is_none());
    assert_eq!(record.proposed, artifact(b""));
    assert_eq!(record.retention, CheckpointRetention::Retained);
}

#[test]
fn removal_refuses_a_wrong_precondition_a_missing_target_and_denied_preferences() {
    let mut fixture = Fixture::new();
    let target = fixture.workspace.join("source.txt");
    // A different size or digest is a conflict before any private evidence.
    assert!(matches!(
        checkpoint_error(remove(&mut fixture, b"other bytes")),
        CheckpointError::Conflict
    ));
    assert!(target.exists());
    assert!(fixture.inspect().expect("inspect").operations.is_empty());

    // Current preferences still apply.
    fixture
        .control
        .set_permissions(
            fixture.workspace_id,
            PermissionSet {
                read: Access::Allow,
                write: Access::Deny,
                ..PermissionSet::default()
            },
            fixture.control.revision(),
        )
        .expect("deny write");
    assert!(matches!(
        checkpoint_error(remove(&mut fixture, ORIGINAL)),
        CheckpointError::Denied(_)
    ));
    assert!(target.exists());
    fixture
        .control
        .set_permissions(
            fixture.workspace_id,
            PermissionSet {
                read: Access::Allow,
                write: Access::Prompt,
                ..PermissionSet::default()
            },
            fixture.control.revision(),
        )
        .expect("restore preferences");

    // There is nothing to remove from a missing target.
    fs::remove_file(&target).expect("remove target");
    assert!(remove(&mut fixture, ORIGINAL).is_err());
    assert!(fixture.inspect().expect("inspect").operations.is_empty());
}

#[test]
fn a_target_replaced_after_the_checkpoint_conflicts_and_is_not_removed() {
    let mut fixture = Fixture::new();
    let target = fixture.workspace.join("source.txt");
    let workspace = fixture.workspace.clone();
    let result = remove_hooked(&mut fixture, ORIGINAL, |_store| {
        let replacement = workspace.join("independent.txt");
        fs::write(&replacement, ORIGINAL).expect("independent inode, equal bytes");
        set_source_security(&replacement);
        fs::rename(replacement, &target).expect("replace target inode");
    });
    assert!(matches!(
        result,
        Err(FileError::Checkpoint(CheckpointError::Conflict))
    ));
    assert_eq!(
        fs::read(fixture.workspace.join("source.txt")).unwrap(),
        ORIGINAL
    );
    let inspection = fixture.inspect().expect("inspect conflict");
    assert_eq!(inspection.pending, None);
    let record = &inspection.operations[0];
    assert_eq!(record.kind, ReplacementKind::Remove);
    assert_eq!(record.progress, ReplacementProgress::CheckpointPrepared);
    assert_eq!(record.outcome, Some(ReplacementOutcome::Conflict));
    assert!(
        !fixture
            .operation_dir(record.operation_id)
            .join("stage")
            .exists()
    );
}

#[test]
fn removal_faults_leave_exact_unresolved_evidence_and_are_not_retried() {
    for (fault, removed) in [
        ("commit_intent_sync", false),
        ("before_native_commit", false),
        ("after_native_commit", true),
        ("publication_outcome_sync", true),
    ] {
        let mut fixture = Fixture::new();
        let target = fixture.workspace.join("source.txt");
        let result = remove_hooked(&mut fixture, ORIGINAL, |store| {
            store.inject_publication_fault(fault);
        });
        let id = uncertain_id(result);
        assert_eq!(target.exists(), !removed, "{fault}");
        assert_eq!(
            fs::read(fixture.operation_dir(id).join("original")).unwrap(),
            ORIGINAL,
            "{fault}"
        );
        let inspection = fixture.inspect().expect("reopen exact evidence");
        let record = operation(&inspection, id);
        assert_eq!(record.kind, ReplacementKind::Remove, "{fault}");
        assert_eq!(
            record.progress,
            ReplacementProgress::CommitIntended,
            "{fault}"
        );
        let outcome_recorded = fault == "publication_outcome_sync";
        assert_eq!(record.outcome.is_some(), outcome_recorded, "{fault}");
        assert_eq!(
            inspection.pending,
            (!outcome_recorded).then_some(id),
            "{fault}"
        );
        // Nothing is retried while the operation is unresolved.
        if !outcome_recorded {
            assert!(remove(&mut fixture, ORIGINAL).is_err(), "{fault}");
            FileBroker::new(&mut fixture.control)
                .checkpoint_acknowledge(CheckpointStore::Workspace(fixture.workspace_id), id)
                .expect("acknowledge without claiming success");
            assert_eq!(fixture.inspect().expect("inspect").pending, None);
        }
    }
}

#[test]
fn restoring_a_removal_recreates_the_exact_bytes_and_never_replaces() {
    let mut fixture = Fixture::new();
    let target = fixture.workspace.join("source.txt");
    let id = removed_id(&mut fixture, ORIGINAL);
    assert!(!target.exists());

    let created = restore_removed(&mut fixture, id).expect("restore the removed file");
    assert_eq!(created.bytes_written, ORIGINAL.len());
    assert_eq!(fs::read(&target).unwrap(), ORIGINAL);
    // Nothing is destroyed by a restore, so no checkpoint operation is added.
    let inspection = fixture.inspect().expect("inspect");
    assert_eq!(inspection.operations.len(), 1);
    assert_eq!(inspection.pending, None);

    // The name is taken now, whatever it holds.
    fs::write(&target, b"someone else").expect("another writer");
    assert!(matches!(
        checkpoint_error(restore_removed(&mut fixture, id)),
        CheckpointError::Conflict
    ));
    assert_eq!(fs::read(&target).unwrap(), b"someone else");
}

#[test]
fn a_removed_non_utf8_file_is_checkpointed_and_restored_exactly() {
    let mut fixture = Fixture::new();
    let bytes: &[u8] = &[0xff, 0xfe, 0x00, 0x80, 0x01];
    let target = fixture.workspace.join("source.txt");
    fs::write(&target, bytes).expect("binary source");
    set_source_security(&target);
    let id = removed_id(&mut fixture, bytes);
    assert_eq!(
        fs::read(fixture.operation_dir(id).join("original")).unwrap(),
        bytes
    );
    // Reopening validates the entry again without interpreting its bytes.
    assert_eq!(fixture.inspect().expect("inspect").operations.len(), 1);
    restore_removed(&mut fixture, id).expect("restore");
    assert_eq!(fs::read(&target).unwrap(), bytes);
}

#[test]
fn the_two_restore_routes_refuse_the_other_kind_and_foreign_targets() {
    let mut fixture = Fixture::new();
    let target = fixture.workspace.join("source.txt");
    let applied = applied_id(&mut fixture, ORIGINAL, PROPOSED);
    // A replacement checkpoint cannot be restored as a removal.
    fs::remove_file(&target).expect("remove target");
    assert!(matches!(
        checkpoint_error(restore_removed(&mut fixture, applied)),
        CheckpointError::Unavailable
    ));
    assert!(!target.exists());
    fs::write(&target, PROPOSED).expect("recreate target");

    let removed = removed_id(&mut fixture, PROPOSED);
    // A removal checkpoint cannot be restored over a file.
    fs::write(&target, b"current").expect("current file");
    assert!(matches!(
        checkpoint_error(restore(&mut fixture, removed, b"current")),
        CheckpointError::Unavailable
    ));
    assert_eq!(fs::read(&target).unwrap(), b"current");
    fs::remove_file(&target).expect("clear the name");

    // It restores only its recorded workspace and exact target.
    let binding = fixture.binding();
    let other = FileTarget {
        path: PathBuf::from("other.txt"),
        ..fixture.target()
    };
    assert!(matches!(
        checkpoint_error(
            FileBroker::new(&mut fixture.control).restore_removed_file(&binding, &other, removed)
        ),
        CheckpointError::TargetMismatch
    ));
    assert!(!fixture.workspace.join("other.txt").exists());
    assert!(matches!(
        checkpoint_error(restore_removed(&mut fixture, Uuid::new_v4())),
        CheckpointError::Unavailable
    ));
}

#[test]
fn a_removal_checkpoint_can_be_deleted_and_then_cannot_restore() {
    let mut fixture = Fixture::new();
    let id = removed_id(&mut fixture, ORIGINAL);
    let deletion = delete(&mut fixture, id).expect("delete the checkpoint");
    assert!(!deletion.resumed);
    assert!(!fixture.operation_dir(id).exists());
    let inspection = fixture.inspect().expect("history stays");
    let record = operation(&inspection, id);
    assert_eq!(record.kind, ReplacementKind::Remove);
    assert_eq!(record.retention, CheckpointRetention::Deleted);
    assert!(matches!(
        checkpoint_error(restore_removed(&mut fixture, id)),
        CheckpointError::Unavailable
    ));
}

#[test]
fn compaction_carries_a_removal_and_it_restores_afterwards() {
    let mut fixture = Fixture::new();
    // One deleted entry makes the ledger shrinkable.
    let applied = applied_id(&mut fixture, ORIGINAL, PROPOSED);
    delete(&mut fixture, applied).expect("delete a replacement checkpoint");
    let removed = removed_id(&mut fixture, PROPOSED);
    let before = fixture.inspect().expect("inspect");
    let result = compact(&mut fixture).expect("compact");
    assert!(result.compacted);
    assert_eq!((result.carried, result.dropped), (1, 1));
    let after = fixture.inspect().expect("inspect compacted ledger");
    assert_eq!(after.operations, retained_operations(&before));
    assert_eq!(operation(&after, removed).kind, ReplacementKind::Remove);
    let ledger = fs::read_to_string(ledger_path(&fixture)).expect("ledger");
    assert!(ledger.contains("\"kind\":\"remove\""), "{ledger}");

    restore_removed(&mut fixture, removed).expect("restore after compaction");
    assert_eq!(
        fs::read(fixture.workspace.join("source.txt")).unwrap(),
        PROPOSED
    );
}

#[test]
fn a_replacement_record_stays_byte_identical_without_a_kind() {
    let mut fixture = Fixture::new();
    applied_id(&mut fixture, ORIGINAL, PROPOSED);
    let ledger = fs::read_to_string(ledger_path(&fixture)).expect("ledger");
    assert!(!ledger.contains("\"kind\""), "{ledger}");
}

#[test]
fn a_stage_file_in_a_removal_entry_is_refused_on_reopen() {
    let mut fixture = Fixture::new();
    let id = removed_id(&mut fixture, ORIGINAL);
    fs::write(fixture.operation_dir(id).join("stage"), b"unexpected").expect("planted stage");
    assert!(matches!(
        fixture.inspect(),
        Err(FileError::Checkpoint(CheckpointError::Invalid(_)))
    ));
}

// ADR-0070: the per-workspace `.OMGCode` checkpoint folder.

/// The data-directory head record of the fixture's workspace store.
fn head_path(fixture: &Fixture) -> PathBuf {
    fixture
        .data
        .join("checkpoint-heads-v1")
        .join(format!("{}.json", fixture.workspace_id.as_uuid()))
}

fn source_bytes(fixture: &Fixture) -> Vec<u8> {
    fs::read(fixture.workspace.join("source.txt")).expect("read source")
}

/// Create a directory with the owner-only access a private store requires.
fn make_private_dir(path: &Path) {
    #[cfg(windows)]
    make_private_root(path);
    #[cfg(unix)]
    {
        fs::create_dir(path).expect("create private directory");
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("owner-only mode");
    }
}

/// Creates `.OMGCode` exactly as trusted creation does: owner-only and
/// exclusive, through the platform private-directory backend.
fn create_exact_store_folder(fixture: &Fixture) {
    let scope = fixture
        .control
        .file_scope(fixture.workspace_id)
        .expect("workspace scope");
    omgcode_platform::private_store::PrivateDirectory::create_in(
        &fixture.workspace,
        scope.workspace.native_identity(),
        std::ffi::OsStr::new(".OMGCode"),
    )
    .expect("create an exactly private folder");
}

fn store_changed(error: &CheckpointError) -> &'static str {
    match error {
        CheckpointError::StoreChanged(message) => message,
        other => panic!("expected a changed checkpoint store: {other:?}"),
    }
}

const UNREGISTERED_PREFIX: &str = "an unregistered .OMGCode folder already exists";

#[test]
fn given_a_first_write_when_applied_then_a_private_self_ignoring_folder_is_bound() {
    let mut fixture = Fixture::new();
    assert_eq!(
        fixture.inspect().expect("inspect unused").state,
        crate::replacement::CheckpointState::Unused
    );
    assert!(!fixture.store_dir().exists());

    let id = applied_id(&mut fixture, ORIGINAL, PROPOSED);

    let store = fixture.store_dir();
    assert_eq!(
        fs::read(store.join(".gitignore")).expect("ignore file"),
        b"*\n"
    );
    assert_payloads(&fixture, id);
    // Opening validates the owner-only access list or mode.
    omgcode_platform::private_store::PrivateDirectory::open(&store).expect("private folder");
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&store)
            .expect("folder metadata")
            .permissions()
            .mode()
            & 0o7777,
        0o700
    );
    let bound = fixture
        .control
        .workspace_store(fixture.workspace_id)
        .expect("bound folder")
        .live_path()
        .expect("bound path");
    assert_eq!(
        fs::canonicalize(bound).expect("bound path"),
        fs::canonicalize(&store).expect("store path")
    );
    assert!(head_path(&fixture).exists());
    // The legacy data-directory store stays unused.
    assert!(!fixture.data.join("replacement-v1").exists());
    assert!(!fixture.data.join("replacement-init-v1.jsonl").exists());
}

#[test]
fn given_a_deleted_folder_when_writing_then_the_store_is_missing_and_not_recreated() {
    let mut fixture = Fixture::new();
    applied_id(&mut fixture, ORIGINAL, PROPOSED);
    fs::remove_dir_all(fixture.store_dir()).expect("delete the folder");

    let error = checkpoint_error(apply_bytes(&mut fixture, PROPOSED, ORIGINAL));
    assert!(matches!(error, CheckpointError::StoreMissing), "{error:?}");
    assert_eq!(error.code(), "checkpoint_store_missing");
    assert!(matches!(
        checkpoint_error(fixture.inspect()),
        CheckpointError::StoreMissing
    ));
    assert!(!fixture.store_dir().exists());
    assert_eq!(source_bytes(&fixture), PROPOSED);
}

#[test]
fn given_a_replaced_folder_when_writing_then_the_store_is_refused_as_changed() {
    let mut fixture = Fixture::new();
    let id = applied_id(&mut fixture, ORIGINAL, PROPOSED);
    let moved = fixture.workspace.join("moved-store");
    fs::rename(fixture.store_dir(), &moved).expect("move the folder");
    make_private_dir(&fixture.store_dir());

    let error = checkpoint_error(apply_bytes(&mut fixture, PROPOSED, ORIGINAL));
    assert_eq!(store_changed(&error), "the checkpoint folder was replaced");
    assert_eq!(error.code(), "checkpoint_store_changed");
    assert_eq!(
        fs::read_dir(fixture.store_dir())
            .expect("list new folder")
            .count(),
        0
    );
    assert_eq!(
        fs::read(
            moved
                .join("replacement-v1")
                .join(id.to_string())
                .join("original")
        )
        .expect("moved evidence"),
        ORIGINAL
    );
    assert_eq!(source_bytes(&fixture), PROPOSED);
}

#[test]
fn given_a_rolled_back_ledger_or_a_missing_head_when_opening_then_the_store_is_refused() {
    let mut fixture = Fixture::new();
    applied_id(&mut fixture, ORIGINAL, PROPOSED);
    let ledger = ledger_path(&fixture);
    let earlier = fs::read(&ledger).expect("earlier ledger");
    let later = applied_id(&mut fixture, PROPOSED, ORIGINAL);

    // Roll the folder back to its state before the second write.
    fs::write(&ledger, &earlier).expect("roll the ledger back");
    fs::remove_dir_all(fixture.operation_dir(later)).expect("remove the later entry");
    let differs = "the checkpoint ledger differs from its recorded head";
    assert_eq!(store_changed(&checkpoint_error(fixture.inspect())), differs);
    let error = checkpoint_error(apply_bytes(&mut fixture, ORIGINAL, PROPOSED));
    assert_eq!(store_changed(&error), differs);
    assert_eq!(fs::read(&ledger).expect("ledger unchanged"), earlier);
    assert_eq!(source_bytes(&fixture), ORIGINAL);

    // A store whose head record is gone is refused too.
    fs::remove_file(head_path(&fixture)).expect("remove the head record");
    assert_eq!(
        store_changed(&checkpoint_error(fixture.inspect())),
        "the checkpoint ledger's head record is missing"
    );
}

#[test]
fn given_a_bound_folder_whose_access_changed_when_opening_then_the_store_is_refused() {
    let mut fixture = Fixture::new();
    applied_id(&mut fixture, ORIGINAL, PROPOSED);
    let folder = fixture.store_dir();
    // Windows: the workspace grants only the current user, inheritably, so
    // the folder's inherited DACL names only trusted principals and the root
    // open passes. Only the exact owner-only check refuses it.
    #[cfg(windows)]
    {
        icacls(&[
            fixture.workspace.as_os_str(),
            "/inheritance:r".as_ref(),
            "/grant:r".as_ref(),
            format!("*{}:(OI)(CI)F", current_user_sid()).as_ref(),
        ]);
        icacls(&[folder.as_os_str(), "/inheritance:e".as_ref()]);
        omgcode_platform::private_store::PrivateDirectory::open(&folder)
            .expect("the inherited DACL passes the trusted-root open");
    }
    #[cfg(unix)]
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o750)).expect("widen the folder");

    let changed = "the checkpoint folder's owner or access list changed";
    assert_eq!(store_changed(&checkpoint_error(fixture.inspect())), changed);
    let error = checkpoint_error(apply_bytes(&mut fixture, PROPOSED, ORIGINAL));
    assert_eq!(store_changed(&error), changed);
    assert_eq!(source_bytes(&fixture), PROPOSED);
}

fn head_state(fixture: &Fixture) -> (Option<RecordedArtifact>, Option<RecordedArtifact>) {
    crate::replacement::store::head_state_for_test(&fixture.control, fixture.workspace_id)
}

fn ledger_digest(fixture: &Fixture) -> RecordedArtifact {
    crate::replacement::store::ledger_digest_for_test(
        &fs::read(ledger_path(fixture)).expect("read the ledger"),
    )
}

#[test]
fn given_a_head_announced_before_an_interrupted_append_when_reopened_then_the_committed_ledger_is_kept()
 {
    let mut fixture = Fixture::new();
    applied_id(&mut fixture, ORIGINAL, PROPOSED);
    let ledger = fs::read(ledger_path(&fixture)).expect("ledger");
    let committed = ledger_digest(&fixture);
    assert_eq!(head_state(&fixture), (Some(committed.clone()), None));

    // A change announced its ledger, then stopped before appending to it.
    let mut never_written = ledger.clone();
    never_written.extend_from_slice(b"{\"never\":\"appended\"}\n");
    let announced = crate::replacement::store::ledger_digest_for_test(&never_written);
    crate::replacement::store::set_head_for_test(
        &fixture.control,
        fixture.workspace_id,
        Some(committed.clone()),
        Some(announced.clone()),
    );

    fixture
        .inspect()
        .expect("a read-only open accepts the committed ledger");
    assert_eq!(head_state(&fixture), (Some(committed), Some(announced)));
    assert_eq!(fs::read(ledger_path(&fixture)).expect("ledger"), ledger);

    // The next change announces and commits over the stale announcement.
    applied_id(&mut fixture, PROPOSED, ORIGINAL);
    assert_eq!(head_state(&fixture), (Some(ledger_digest(&fixture)), None));
}

#[test]
fn given_an_append_synced_before_its_head_committed_when_reopened_then_it_is_accepted_and_committed()
 {
    let mut fixture = Fixture::new();
    applied_id(&mut fixture, ORIGINAL, PROPOSED);
    let earlier = ledger_digest(&fixture);
    applied_id(&mut fixture, PROPOSED, ORIGINAL);
    let later = ledger_digest(&fixture);

    // The append synced, but the head still names the earlier ledger as
    // committed and the synced one only as announced.
    crate::replacement::store::set_head_for_test(
        &fixture.control,
        fixture.workspace_id,
        Some(earlier.clone()),
        Some(later.clone()),
    );
    fixture
        .inspect()
        .expect("a read-only open accepts the announced ledger");
    assert_eq!(
        head_state(&fixture),
        (Some(earlier), Some(later)),
        "a read-only open never writes the head"
    );

    // A writable open commits the announced head, then the change proceeds.
    applied_id(&mut fixture, ORIGINAL, PROPOSED);
    assert_eq!(head_state(&fixture), (Some(ledger_digest(&fixture)), None));
    assert_eq!(source_bytes(&fixture), PROPOSED);
}

#[test]
fn given_a_stray_or_partial_head_record_when_opening_then_only_the_partial_one_is_refused() {
    let mut fixture = Fixture::new();
    applied_id(&mut fixture, ORIGINAL, PROPOSED);
    let head = head_path(&fixture);
    let complete = fs::read(&head).expect("head record");

    // A replacement that stopped before its rename leaves only the private
    // temporary record. The complete record is still read, and the next
    // write removes the temporary one.
    let mut stray = head.clone().into_os_string();
    stray.push(".next");
    let stray = PathBuf::from(stray);
    fs::write(&stray, &complete[..complete.len() / 2]).expect("stray temporary head");
    #[cfg(windows)]
    copy_security(&head, &stray);
    #[cfg(unix)]
    fs::set_permissions(&stray, fs::Permissions::from_mode(0o600)).expect("private stray");
    fixture
        .inspect()
        .expect("a stray temporary head is not read");
    applied_id(&mut fixture, PROPOSED, ORIGINAL);
    assert!(!stray.exists());
    assert_eq!(head_state(&fixture), (Some(ledger_digest(&fixture)), None));

    // A partial record under the head's own name, which only an interrupted
    // first write can leave, is refused rather than guessed at.
    let complete = fs::read(&head).expect("head record");
    fs::write(&head, &complete[..complete.len() / 2]).expect("truncate the head");
    assert!(
        matches!(
            checkpoint_error(fixture.inspect()),
            CheckpointError::Json(_)
        ),
        "a partial head record must be refused"
    );
    assert!(matches!(
        checkpoint_error(apply_bytes(&mut fixture, ORIGINAL, PROPOSED)),
        CheckpointError::Json(_)
    ));
    assert_eq!(source_bytes(&fixture), ORIGINAL);
}

#[test]
fn given_a_forgotten_store_when_writing_again_then_an_empty_folder_is_bound_anew() {
    let mut fixture = Fixture::new();
    let first = applied_id(&mut fixture, ORIGINAL, PROPOSED);
    let old_store = fixture.inspect().expect("inspect").replacement_store_id;

    let workspace_id = fixture.workspace_id;
    assert!(
        FileBroker::new(&mut fixture.control)
            .checkpoint_forget_store(workspace_id)
            .expect("forget")
    );
    assert!(
        !FileBroker::new(&mut fixture.control)
            .checkpoint_forget_store(workspace_id)
            .expect("forget again")
    );
    assert!(fixture.control.workspace_store(workspace_id).is_none());
    assert_eq!(
        fixture.inspect().expect("inspect forgotten").state,
        crate::replacement::CheckpointState::Unused
    );

    // The old folder stays, and blocks a new store until it is moved.
    let error = checkpoint_error(apply_bytes(&mut fixture, PROPOSED, ORIGINAL));
    assert!(
        store_changed(&error).starts_with(UNREGISTERED_PREFIX),
        "{error:?}"
    );
    assert!(fixture.control.workspace_store(workspace_id).is_none());
    assert_eq!(source_bytes(&fixture), PROPOSED);

    let moved = fixture.workspace.join("old-store");
    fs::rename(fixture.store_dir(), &moved).expect("move the old folder");
    let second = applied_id(&mut fixture, PROPOSED, ORIGINAL);
    let inspection = fixture.inspect().expect("inspect new store");
    assert_ne!(inspection.replacement_store_id, old_store);
    assert_eq!(
        inspection
            .operations
            .iter()
            .map(|operation| operation.operation_id)
            .collect::<Vec<_>>(),
        vec![second]
    );
    assert!(
        moved
            .join("replacement-v1")
            .join(first.to_string())
            .join("original")
            .exists()
    );
}

#[test]
fn given_an_existing_folder_when_first_writing_then_only_an_empty_private_one_is_adopted() {
    let mut fixture = Fixture::new();
    let folder = fixture.store_dir();

    // A folder with ordinary inherited access is never adopted, even where
    // the inherited DACL names only trusted principals.
    fs::create_dir(&folder).expect("create an ordinary folder");
    #[cfg(unix)]
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).expect("ordinary mode");
    let error = checkpoint_error(apply_bytes(&mut fixture, ORIGINAL, PROPOSED));
    assert!(
        store_changed(&error).starts_with(UNREGISTERED_PREFIX),
        "{error:?}"
    );
    fs::remove_dir(&folder).expect("remove the ordinary folder");

    // On Windows, a protected DACL for the user alone is still refused when
    // its entry is inheritable rather than the exact one creation applies.
    #[cfg(windows)]
    {
        make_private_root(&folder);
        let error = checkpoint_error(apply_bytes(&mut fixture, ORIGINAL, PROPOSED));
        assert!(
            store_changed(&error).starts_with(UNREGISTERED_PREFIX),
            "{error:?}"
        );
        fs::remove_dir(&folder).expect("remove the inheritable folder");
    }

    // Neither is an exactly private folder that already holds something.
    create_exact_store_folder(&fixture);
    fs::write(folder.join("planted"), b"planted").expect("plant a file");
    let error = checkpoint_error(apply_bytes(&mut fixture, ORIGINAL, PROPOSED));
    assert!(
        store_changed(&error).starts_with(UNREGISTERED_PREFIX),
        "{error:?}"
    );
    assert_eq!(
        fs::read(folder.join("planted")).expect("planted file"),
        b"planted"
    );
    assert!(
        fixture
            .control
            .workspace_store(fixture.workspace_id)
            .is_none()
    );
    assert_eq!(source_bytes(&fixture), ORIGINAL);

    // An empty private folder is adopted.
    fs::remove_file(folder.join("planted")).expect("remove the planted file");
    let id = applied_id(&mut fixture, ORIGINAL, PROPOSED);
    assert_payloads(&fixture, id);
    assert!(
        fixture
            .control
            .workspace_store(fixture.workspace_id)
            .is_some()
    );
}

#[test]
fn given_a_pending_legacy_operation_when_writing_then_admission_waits_and_restore_reads_it() {
    let mut fixture = Fixture::new();
    let binding = fixture.binding();
    let target = fixture.target();
    let legacy = FileBroker::new(&mut fixture.control)
        .prepare_in(
            CheckpointStore::Legacy,
            &binding,
            &target,
            &artifact(ORIGINAL),
            PROPOSED,
        )
        .expect("seed a legacy checkpoint")
        .operation_id;
    assert!(
        fixture
            .data
            .join("replacement-v1")
            .join(legacy.to_string())
            .exists()
    );
    assert!(!fixture.store_dir().exists());

    let error = checkpoint_error(apply_bytes(&mut fixture, ORIGINAL, PROPOSED));
    assert!(
        matches!(error, CheckpointError::Pending(id) if id == legacy),
        "{error:?}"
    );
    assert!(!fixture.store_dir().exists());
    assert_eq!(source_bytes(&fixture), ORIGINAL);

    FileBroker::new(&mut fixture.control)
        .checkpoint_acknowledge(CheckpointStore::Legacy, legacy)
        .expect("acknowledge the legacy operation");
    applied_id(&mut fixture, ORIGINAL, PROPOSED);

    // A legacy checkpoint restores through a new workspace checkpoint.
    let restored = restore(&mut fixture, legacy, PROPOSED)
        .expect("restore a legacy checkpoint")
        .operation_id
        .expect("a restore is a new operation");
    assert_eq!(source_bytes(&fixture), ORIGINAL);
    assert!(fixture.operation_dir(restored).exists());
    assert!(
        !fixture
            .data
            .join("replacement-v1")
            .join(restored.to_string())
            .exists()
    );
}
