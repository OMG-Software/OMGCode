//! Per-workspace checkpoint folders and their recorded ledger heads (ADR-0070).
//!
//! A workspace store lives in the workspace's own `.OMGCode` folder. The
//! control journal binds the folder by native identity and creation time.
//! The data directory, outside every workspace, records the store's ledger
//! head before and after each ledger change, so a ledger that something else
//! rewrote, rolled back, or extended is refused before the store is used.
//! None of this grants access; the broker refuses the folder's name.

use std::ffi::OsStr;
use std::io::{self, Write};

use omgcode_platform::broker::RESERVED_STORE_NAME;
use omgcode_platform::private_store::PrivateDirectory;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{CheckpointError, open_root, read_bounded};
use crate::control::ControlStore;
use crate::policy::WorkspaceId;
use crate::replacement::RecordedArtifact;
use crate::workspace::DirectoryRecord;

/// The data-directory folder holding one head record per workspace store.
const HEADS: &str = "checkpoint-heads-v1";
/// A head record is a few hundred bytes; anything larger is invalid.
const HEAD_LIMIT: usize = 4096;
/// The folder's own ignore file. It ignores everything in the folder,
/// itself included, so the operator's `.gitignore` is never edited.
pub(super) const GITIGNORE: &str = ".gitignore";
const GITIGNORE_BYTES: &[u8] = b"*\n";
const UNREGISTERED: &str = "an unregistered .OMGCode folder already exists in this workspace; \
     move or delete it, or empty it, before the next file write";
const ACCESS_CHANGED: &str = "the checkpoint folder's owner or access list changed";

/// Which checkpoint store a request uses (ADR-0070).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointStore {
    /// The data-directory store that versions before 0.49.0 wrote. New
    /// operations never go here; existing entries stay inspectable,
    /// acknowledgeable, deletable, compactable, and restorable.
    Legacy,
    /// The protected `.OMGCode` folder inside one registered workspace.
    Workspace(WorkspaceId),
}

/// Where a store lives and how its ledger is anchored.
pub(super) struct Location {
    /// The registered root directory: the data directory or the bound folder.
    pub(super) root: DirectoryRecord,
    /// The control store this checkpoint store belongs to.
    pub(super) control_id: Uuid,
    /// The ledger head record, for a workspace store only.
    pub(super) head: Option<HeadSlot>,
}

/// Resolve one store. An unbound workspace has no store yet: `None`.
pub(super) fn locate(
    control: &ControlStore,
    store: CheckpointStore,
) -> Result<Option<Location>, CheckpointError> {
    let (data, control_id) = control.replacement_context()?;
    Ok(match store {
        CheckpointStore::Legacy => Some(Location {
            root: data,
            control_id,
            head: None,
        }),
        CheckpointStore::Workspace(workspace_id) => control
            .workspace_store(workspace_id)
            .cloned()
            .map(|root| Location {
                root,
                control_id,
                head: Some(HeadSlot { data, workspace_id }),
            }),
    })
}

/// Open a bound folder and require the recorded identity and creation time.
/// A missing folder and a replaced or re-permissioned one are distinct errors.
pub(super) fn open_bound_root(
    expected: &DirectoryRecord,
) -> Result<PrivateDirectory, CheckpointError> {
    let root = match PrivateDirectory::open(&expected.live_path()?) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(CheckpointError::StoreMissing);
        }
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            return Err(CheckpointError::StoreChanged(ACCESS_CHANGED));
        }
        Err(error) => return Err(error.into()),
    };
    if root.identity()? != expected.native_identity() || !expected.matches_created(root.created()?)
    {
        return Err(CheckpointError::StoreChanged(
            "the checkpoint folder was replaced",
        ));
    }
    // A root open accepts any DACL limited to trusted principals. A bound
    // folder must keep the exact owner-only protection it was created with,
    // or a later inheritable grant on the workspace would reach it.
    match root.require_owner_only() {
        Ok(()) => Ok(root),
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            Err(CheckpointError::StoreChanged(ACCESS_CHANGED))
        }
        Err(error) => Err(error.into()),
    }
}

/// Bind the workspace's `.OMGCode` folder on its first checkpoint.
///
/// The folder is created with the private owner-only access, or adopted if
/// an existing one is private and empty. Its live identity is then recorded
/// in the control journal. Anything else in the way is refused. The caller
/// holds the control writer and has just checked write permission.
pub(crate) fn ensure(
    control: &mut ControlStore,
    workspace_id: WorkspaceId,
) -> Result<(), CheckpointError> {
    if control.workspace_store(workspace_id).is_some() {
        return Ok(());
    }
    let workspace = control.file_scope(workspace_id)?.workspace;
    let parent = workspace.live_path()?;
    let path = parent.join(RESERVED_STORE_NAME);
    let name = OsStr::new(RESERVED_STORE_NAME);
    let root = match PrivateDirectory::create_in(&parent, workspace.native_identity(), name) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let root = PrivateDirectory::open(&path)
                .map_err(|_| CheckpointError::StoreChanged(UNREGISTERED))?;
            // Only the exact owner-only folder that creation makes is adopted.
            // A folder whose DACL merely names trusted principals could later
            // inherit a workspace container grant.
            root.require_owner_only()
                .map_err(|_| CheckpointError::StoreChanged(UNREGISTERED))?;
            // Only an empty folder is adopted. A bounded inventory that
            // overflows or holds an unexpected name is not empty either.
            match root.names(1) {
                Ok(names) if names.is_empty() => root,
                Ok(_) => return Err(CheckpointError::StoreChanged(UNREGISTERED)),
                Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                    return Err(CheckpointError::StoreChanged(UNREGISTERED));
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    };
    let record = DirectoryRecord::capture(&path)?;
    if root.identity()? != record.native_identity() || !record.matches_created(root.created()?) {
        return Err(CheckpointError::StoreChanged(
            "the new checkpoint folder changed while it was registered",
        ));
    }
    let (data, _) = control.replacement_context()?;
    // A forgotten store's head would refuse the new ledger. It is removed
    // before the binding, so a crash between the two leaves nothing bound.
    HeadSlot { data, workspace_id }.discard()?;
    control.bind_workspace_store(workspace_id, record)?;
    Ok(())
}

/// Create the folder's `.gitignore` if it is absent. An existing one is
/// left as it is; it is a convenience for Git, not a security control.
pub(super) fn write_gitignore(root: &PrivateDirectory) -> Result<(), CheckpointError> {
    let name = OsStr::new(GITIGNORE);
    if root.exists(name)? {
        return Ok(());
    }
    let mut file = root.create_file(name)?;
    file.write_all(GITIGNORE_BYTES)?;
    file.sync_all()?;
    root.sync()?;
    Ok(())
}

/// The digest of `existing` followed by `appended`, without copying either.
pub(super) fn ledger_descriptor(
    existing: &[u8],
    appended: &[u8],
) -> Result<RecordedArtifact, CheckpointError> {
    let length = existing
        .len()
        .checked_add(appended.len())
        .ok_or(CheckpointError::Limit)?;
    let mut hasher = Sha256::new();
    hasher.update(existing);
    hasher.update(appended);
    let sha256 = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(RecordedArtifact {
        bytes: u64::try_from(length).map_err(|_| CheckpointError::Limit)?,
        sha256,
    })
}

/// One recorded ledger head. `next` is announced before a ledger change and
/// becomes `committed` after it syncs, so a crash leaves either one current.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct HeadRecord {
    schema_version: u32,
    replacement_store_id: Uuid,
    committed: Option<RecordedArtifact>,
    next: Option<RecordedArtifact>,
}

/// The place of one workspace store's head record in the data directory.
pub(super) struct HeadSlot {
    data: DirectoryRecord,
    workspace_id: WorkspaceId,
}

impl HeadSlot {
    fn name(&self) -> String {
        format!("{}.json", self.workspace_id.as_uuid())
    }

    fn heads(&self, create: bool) -> Result<Option<PrivateDirectory>, CheckpointError> {
        let root = open_root(&self.data)?;
        let name = OsStr::new(HEADS);
        if root.exists(name)? {
            return Ok(Some(root.open_dir(name)?));
        }
        if !create {
            return Ok(None);
        }
        Ok(Some(root.create_dir(name)?))
    }

    /// Read the recorded head, if any. A malformed record is an error.
    pub(super) fn open(&self) -> Result<Option<HeadFile>, CheckpointError> {
        let Some(directory) = self.heads(false)? else {
            return Ok(None);
        };
        let name = self.name();
        if !directory.exists(OsStr::new(&name))? {
            return Ok(None);
        }
        let mut file = directory.open_file(OsStr::new(&name), false)?;
        let bytes = read_bounded(&mut file, HEAD_LIMIT)?;
        let value = crate::storage::parse_strict_json(&bytes).map_err(CheckpointError::Json)?;
        let record: HeadRecord = serde_json::from_value(value).map_err(CheckpointError::Json)?;
        if record.schema_version != 1 {
            return Err(CheckpointError::Invalid(
                "unsupported checkpoint head record",
            ));
        }
        Ok(Some(HeadFile {
            directory,
            name,
            record,
        }))
    }

    /// Announce the first ledger of a new store before it is written.
    pub(super) fn begin(
        &self,
        replacement_store_id: Uuid,
        next: RecordedArtifact,
    ) -> Result<HeadFile, CheckpointError> {
        let directory = self.heads(true)?.ok_or(CheckpointError::Invalid(
            "checkpoint head folder is missing",
        ))?;
        let record = HeadRecord {
            schema_version: 1,
            replacement_store_id,
            committed: None,
            next: None,
        };
        let mut head = HeadFile {
            directory,
            name: self.name(),
            record,
        };
        head.announce(next)?;
        Ok(head)
    }

    /// Remove the head of a forgotten store before a new folder is bound.
    fn discard(&self) -> Result<(), CheckpointError> {
        let Some(directory) = self.heads(false)? else {
            return Ok(());
        };
        let name = self.name();
        for candidate in [temporary_name(&name), name] {
            if directory.exists(OsStr::new(&candidate))? {
                directory.remove_file(OsStr::new(&candidate))?;
                directory.sync()?;
            }
        }
        Ok(())
    }
}

/// Open a bound store's recorded head, for tests.
#[cfg(test)]
fn head_for_test(control: &ControlStore, workspace_id: WorkspaceId) -> HeadFile {
    locate(control, CheckpointStore::Workspace(workspace_id))
        .expect("locate the store")
        .expect("a bound store")
        .head
        .expect("a workspace head slot")
        .open()
        .expect("open the head record")
        .expect("a recorded head")
}

/// The digest a head records for these ledger bytes, for tests.
#[cfg(test)]
pub(crate) fn ledger_digest_for_test(ledger: &[u8]) -> RecordedArtifact {
    ledger_descriptor(ledger, &[]).expect("ledger digest")
}

/// The committed and announced ledgers of a bound store's head, for tests.
#[cfg(test)]
pub(crate) fn head_state_for_test(
    control: &ControlStore,
    workspace_id: WorkspaceId,
) -> (Option<RecordedArtifact>, Option<RecordedArtifact>) {
    let head = head_for_test(control, workspace_id);
    (head.record.committed, head.record.next)
}

/// Rewrite a bound store's head through its ordinary atomic write.
///
/// Tests use this to stand in for a store that grew through ordinary
/// appends, or for a write that stopped between its announce and commit
/// steps. Nothing in the product may.
#[cfg(test)]
pub(crate) fn set_head_for_test(
    control: &ControlStore,
    workspace_id: WorkspaceId,
    committed: Option<RecordedArtifact>,
    next: Option<RecordedArtifact>,
) {
    let mut head = head_for_test(control, workspace_id);
    let mut record = head.record.clone();
    record.committed = committed;
    record.next = next;
    head.write(record).expect("rewrite the head record");
}

/// Record the ledger now in a bound folder as its committed head.
///
/// Only tests that seed a ledger directly, standing in for a store that grew
/// through ordinary appends, use this. Nothing in the product may.
#[cfg(test)]
pub(crate) fn trust_ledger_for_test(control: &ControlStore, workspace_id: WorkspaceId) {
    let location = locate(control, CheckpointStore::Workspace(workspace_id))
        .expect("locate the store")
        .expect("a bound store");
    let ledger = std::fs::read(
        location
            .root
            .live_path()
            .expect("store path")
            .join(super::DIRECTORY)
            .join(super::LEDGER),
    )
    .expect("read the seeded ledger");
    set_head_for_test(
        control,
        workspace_id,
        Some(ledger_digest_for_test(&ledger)),
        None,
    );
}

fn temporary_name(name: &str) -> String {
    format!("{name}.next")
}

/// An open head record. Every write is synced before the caller continues.
pub(super) struct HeadFile {
    directory: PrivateDirectory,
    name: String,
    record: HeadRecord,
}

impl HeadFile {
    /// Whether a ledger was ever committed. A head that only announced a
    /// first ledger records a store whose initialization never wrote one.
    pub(super) const fn has_committed(&self) -> bool {
        self.record.committed.is_some()
    }

    /// Accept a replayed ledger only when it is the committed head or the
    /// announced next one. A writable open commits an announced head.
    pub(super) fn verify(
        &mut self,
        replacement_store_id: Uuid,
        ledger: &[u8],
        writable: bool,
    ) -> Result<(), CheckpointError> {
        if self.record.replacement_store_id != replacement_store_id {
            return Err(CheckpointError::StoreChanged(
                "the checkpoint folder holds another store's ledger",
            ));
        }
        let actual = ledger_descriptor(ledger, &[])?;
        if self.record.committed.as_ref() == Some(&actual) {
            return Ok(());
        }
        if self.record.next.as_ref() == Some(&actual) {
            if writable {
                self.commit()?;
            }
            return Ok(());
        }
        Err(CheckpointError::StoreChanged(
            "the checkpoint ledger differs from its recorded head",
        ))
    }

    /// Record the ledger a change is about to produce.
    pub(super) fn announce(&mut self, next: RecordedArtifact) -> Result<(), CheckpointError> {
        let mut record = self.record.clone();
        record.next = Some(next);
        self.write(record)
    }

    /// Make the announced ledger the committed head.
    pub(super) fn commit(&mut self) -> Result<(), CheckpointError> {
        let mut record = self.record.clone();
        record.committed = Some(record.next.take().ok_or(CheckpointError::Invalid(
            "no checkpoint ledger head was announced",
        ))?);
        self.write(record)
    }

    /// Write a whole record under a temporary name and replace the old one,
    /// so a reader sees either record complete.
    fn write(&mut self, record: HeadRecord) -> Result<(), CheckpointError> {
        let mut bytes = serde_json::to_vec(&record).map_err(CheckpointError::Json)?;
        bytes.push(b'\n');
        if bytes.len() > HEAD_LIMIT {
            return Err(CheckpointError::Limit);
        }
        let name = OsStr::new(&self.name);
        if self.directory.exists(name)? {
            let temporary = temporary_name(&self.name);
            let temporary = OsStr::new(&temporary);
            if self.directory.exists(temporary)? {
                self.directory.remove_file(temporary)?;
            }
            {
                let mut file = self.directory.create_file(temporary)?;
                file.write_all(&bytes)?;
                file.sync_all()?;
            }
            self.directory.replace_file(temporary, name)?;
        } else {
            let mut file = self.directory.create_file(name)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        self.directory.sync()?;
        self.record = record;
        Ok(())
    }
}
