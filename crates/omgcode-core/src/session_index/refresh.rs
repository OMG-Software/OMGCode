//! Incremental, bounded refresh of the index from the journals.
//!
//! A refresh inspects at most a caller-chosen number of journals, one at a time,
//! with no database transaction open. It then writes each result in its own
//! short transaction. Journals are opened only through
//! `SessionInspection::inspect`, which never takes the writer lock.

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, named_params};
use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

use super::snippet::sanitize_text;
use super::{IndexError, IndexedSession, SessionIndex};
use crate::session::{
    self, DiscoveredJournal, MessageRole, NativePath, SessionError, SessionInspection,
    TranscriptEntryKind,
};

/// The most journals one refresh call inspects, however large the request.
const MAX_JOURNALS_PER_REFRESH: usize = 1000;

/// Stable code for a session UUID that two journal files carry.
const CODE_DUPLICATE: &str = "duplicate_session_id";
/// Stable code for an operating-system failure. The journal is retried.
const CODE_IO: &str = "io_error";

/// What one `refresh` call did. Counts of `busy` and `invalid` cover only the
/// journals this call inspected or reclassified.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct RefreshReport {
    /// Journals found by discovery.
    pub discovered: u64,
    /// Journals this call opened for inspection.
    pub inspected: u64,
    /// Journals skipped because size, modification time, and path match a
    /// settled row.
    pub unchanged: u64,
    /// Journals this call found held by a writer.
    pub busy: u64,
    /// Journals this call classified as invalid, including duplicates.
    pub invalid: u64,
    /// Rows removed because their journal is no longer discovered.
    pub removed: u64,
    /// Journals that need inspection but exceeded this call's budget. Call
    /// again to continue.
    pub remaining: u64,
    /// Links and reparse points that discovery skipped.
    pub skipped_links: u64,
    /// Entries that did not match the journal layout.
    pub unrecognized: u64,
}

/// The facts a stored row contributes to the unchanged check.
struct Existing {
    relative_path: String,
    size: i64,
    modified_ns: i64,
    state: String,
    invalid_code: Option<String>,
}

impl Existing {
    /// A row is settled when nothing about the file changed and the last result
    /// will not change without the file changing.
    fn is_settled_for(&self, journal: &DiscoveredJournal, relative_path: &str) -> bool {
        let same_file = self.relative_path == relative_path
            && self.size == size_value(journal.size)
            && self.modified_ns == modified_value(journal.modified);
        if !same_file {
            return false;
        }
        match self.state.as_str() {
            "ok" => true,
            "invalid" => !matches!(
                self.invalid_code.as_deref(),
                Some(CODE_IO | CODE_DUPLICATE) | None
            ),
            _ => false,
        }
    }

    fn is_settled_duplicate(&self, journal: &DiscoveredJournal, relative_path: &str) -> bool {
        self.state == "invalid"
            && self.invalid_code.as_deref() == Some(CODE_DUPLICATE)
            && self.relative_path == relative_path
            && self.size == size_value(journal.size)
            && self.modified_ns == modified_value(journal.modified)
    }
}

/// The result of inspecting one journal, ready to store.
enum Outcome {
    Indexed(Box<Indexed>),
    Busy,
    Invalid {
        code: String,
        /// Whether to record size and modification time so the journal is not
        /// inspected again until it changes.
        record_stat: bool,
    },
}

/// Metadata and searchable texts of an inspected journal.
struct Indexed {
    metadata: Metadata,
    rows: Vec<TextRow>,
}

struct Metadata {
    title: String,
    title_key: String,
    workspace_id: String,
    workspace_encoding: &'static str,
    workspace_units: Vec<u8>,
    folder_display: String,
    created_ns: i64,
    updated_ns: i64,
    archived: bool,
    active_turn: Option<String>,
    event_count: i64,
    schema_version: i64,
    incomplete_tail: bool,
}

struct TextRow {
    text: String,
    seq: Option<i64>,
    role: &'static str,
}

/// The values for one `journal` row. `metadata` is `None` for busy or invalid.
struct JournalRow<'a> {
    session_id: &'a str,
    relative_path: &'a str,
    size: i64,
    modified_ns: i64,
    state: &'static str,
    invalid_code: Option<&'a str>,
    metadata: Option<&'a Metadata>,
    fts: Option<(i64, i64)>,
}

impl SessionIndex {
    /// Bring the index up to date with the journals on disk.
    ///
    /// Discovers journals, removes rows for vanished ones, then inspects at most
    /// `max_journals` (clamped to 1 through 1000) whose size, modification time,
    /// or path changed, or whose last result was `busy` or an operating-system
    /// failure. Journals left over are counted in `remaining`; call again until
    /// it is zero. A journal held by a writer keeps its earlier data with state
    /// `busy`. An invalid journal is recorded and not re-inspected until it
    /// changes. Two files carrying one session UUID make that session invalid
    /// with `duplicate_session_id` and neither file's text is indexed.
    ///
    /// # Errors
    ///
    /// Returns discovery failures as [`IndexError::Session`], and database
    /// failures, including [`IndexError::Busy`]. A failure in one journal is
    /// recorded for that journal and never fails the refresh. Work committed
    /// before an error stays committed.
    pub fn refresh(&mut self, max_journals: usize) -> Result<RefreshReport, IndexError> {
        let budget = max_journals.clamp(1, MAX_JOURNALS_PER_REFRESH);
        let discovery = session::discover(&self.data_root)?;
        let mut report = RefreshReport {
            discovered: count(discovery.journals.len()),
            skipped_links: discovery.skipped_links,
            unrecognized: discovery.unrecognized,
            ..RefreshReport::default()
        };
        let existing = load_existing(&self.connection)?;

        let present: HashSet<Uuid> = discovery
            .journals
            .iter()
            .map(|journal| journal.session_id)
            .collect();
        for key in existing.keys() {
            let still_present = Uuid::try_parse(key).is_ok_and(|id| present.contains(&id));
            if !still_present {
                remove_session(&mut self.connection, key)?;
                report.removed += 1;
            }
        }

        let mut copies: HashMap<Uuid, u64> = HashMap::new();
        for journal in &discovery.journals {
            *copies.entry(journal.session_id).or_default() += 1;
        }
        let mut handled_duplicates: HashSet<Uuid> = HashSet::new();
        for journal in &discovery.journals {
            let relative = relative_text(journal)?;
            let key = journal.session_id.as_hyphenated().to_string();
            let row = existing.get(&key);
            if copies.get(&journal.session_id).copied().unwrap_or(0) > 1 {
                if !handled_duplicates.insert(journal.session_id) {
                    continue;
                }
                if row.is_some_and(|row| row.is_settled_duplicate(journal, &relative)) {
                    report.unchanged += 1;
                } else {
                    self.record_duplicate(journal, &relative)?;
                    report.invalid += 1;
                }
                continue;
            }
            if row.is_some_and(|row| row.is_settled_for(journal, &relative)) {
                report.unchanged += 1;
            } else if report.inspected >= count(budget) {
                report.remaining += 1;
            } else {
                report.inspected += 1;
                match self.index_journal(journal, &relative, row)? {
                    IndexedAs::Ok => {}
                    IndexedAs::Busy => report.busy += 1,
                    IndexedAs::Invalid => report.invalid += 1,
                }
            }
        }
        Ok(report)
    }

    /// Re-inspect exactly one session now, whatever its size and modification
    /// time. Use it right after the application's own writer closes a journal.
    ///
    /// Returns `None` when no journal has `session_id`, and removes a stale row.
    /// A journal held by a writer yields its row with state `busy`.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`refresh`](Self::refresh).
    pub fn refresh_session(
        &mut self,
        session_id: Uuid,
    ) -> Result<Option<IndexedSession>, IndexError> {
        let discovery = session::discover(&self.data_root)?;
        let matches: Vec<&DiscoveredJournal> = discovery
            .journals
            .iter()
            .filter(|journal| journal.session_id == session_id)
            .collect();
        let key = session_id.as_hyphenated().to_string();
        let existing = load_existing_one(&self.connection, &key)?;
        match matches.as_slice() {
            [] => {
                remove_session(&mut self.connection, &key)?;
                return Ok(None);
            }
            [journal] => {
                let relative = relative_text(journal)?;
                self.index_journal(journal, &relative, existing.as_ref())?;
            }
            [first, ..] => {
                let relative = relative_text(first)?;
                self.record_duplicate(first, &relative)?;
            }
        }
        self.session(session_id)
    }

    /// Inspect `journal` with no transaction open, then store the result.
    fn index_journal(
        &mut self,
        journal: &DiscoveredJournal,
        relative: &str,
        existing: Option<&Existing>,
    ) -> Result<IndexedAs, IndexError> {
        let outcome = inspect(journal);
        let key = journal.session_id.as_hyphenated().to_string();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        clear_path_conflicts(&transaction, &key, relative)?;
        let size = size_value(journal.size);
        let modified_ns = modified_value(journal.modified);
        let stored = match outcome {
            Outcome::Indexed(indexed) => {
                delete_text(&transaction, &key)?;
                let fts = insert_text(&transaction, &key, &indexed.rows)?;
                write_row(
                    &transaction,
                    &JournalRow {
                        session_id: &key,
                        relative_path: relative,
                        size,
                        modified_ns,
                        state: "ok",
                        invalid_code: None,
                        metadata: Some(&indexed.metadata),
                        fts,
                    },
                )?;
                IndexedAs::Ok
            }
            Outcome::Busy => {
                // Keep earlier metadata and text, and keep the old size and
                // time so the next refresh retries.
                let changed = transaction.execute(
                    "UPDATE journal SET state = 'busy', invalid_code = NULL, relative_path = ?2 \
                     WHERE session_id = ?1",
                    rusqlite::params![key, relative],
                )?;
                if changed == 0 {
                    write_row(
                        &transaction,
                        &JournalRow {
                            session_id: &key,
                            relative_path: relative,
                            size: 0,
                            modified_ns: 0,
                            state: "busy",
                            invalid_code: None,
                            metadata: None,
                            fts: None,
                        },
                    )?;
                }
                IndexedAs::Busy
            }
            Outcome::Invalid { code, record_stat } => {
                delete_text(&transaction, &key)?;
                let (size, modified_ns) = if record_stat {
                    (size, modified_ns)
                } else {
                    existing.map_or((0, 0), |row| (row.size, row.modified_ns))
                };
                write_row(
                    &transaction,
                    &JournalRow {
                        session_id: &key,
                        relative_path: relative,
                        size,
                        modified_ns,
                        state: "invalid",
                        invalid_code: Some(&code),
                        metadata: None,
                        fts: None,
                    },
                )?;
                IndexedAs::Invalid
            }
        };
        transaction.commit()?;
        Ok(stored)
    }

    /// Mark a session invalid because two files carry its UUID. Neither file is
    /// inspected and no text stays indexed.
    fn record_duplicate(
        &mut self,
        first: &DiscoveredJournal,
        relative: &str,
    ) -> Result<(), IndexError> {
        let key = first.session_id.as_hyphenated().to_string();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        clear_path_conflicts(&transaction, &key, relative)?;
        delete_text(&transaction, &key)?;
        write_row(
            &transaction,
            &JournalRow {
                session_id: &key,
                relative_path: relative,
                size: size_value(first.size),
                modified_ns: modified_value(first.modified),
                state: "invalid",
                invalid_code: Some(CODE_DUPLICATE),
                metadata: None,
                fts: None,
            },
        )?;
        transaction.commit()?;
        Ok(())
    }
}

/// How a stored journal ended up, for the report.
enum IndexedAs {
    Ok,
    Busy,
    Invalid,
}

/// Inspect one journal and turn the result into a storable outcome. No error
/// from one journal escapes: it becomes an outcome for that journal.
fn inspect(journal: &DiscoveredJournal) -> Outcome {
    match SessionInspection::inspect(&journal.path) {
        Ok(inspection) => {
            if inspection.session_id != journal.session_id {
                return Outcome::Invalid {
                    code: "session_id_mismatch".to_owned(),
                    record_stat: true,
                };
            }
            match build_indexed(&inspection) {
                Some(indexed) => Outcome::Indexed(Box::new(indexed)),
                None => Outcome::Invalid {
                    code: "timestamp_out_of_range".to_owned(),
                    record_stat: true,
                },
            }
        }
        Err(SessionError::Busy) => Outcome::Busy,
        Err(SessionError::InvalidJournal(detail)) => Outcome::Invalid {
            code: format!("invalid_journal: {detail}"),
            record_stat: true,
        },
        Err(SessionError::LimitReached) => Outcome::Invalid {
            code: "limit_reached".to_owned(),
            record_stat: true,
        },
        Err(_) => Outcome::Invalid {
            code: CODE_IO.to_owned(),
            record_stat: false,
        },
    }
}

/// Reduce an inspection to the stored metadata and searchable texts. Returns
/// `None` when a timestamp does not fit the stored integer.
fn build_indexed(inspection: &SessionInspection) -> Option<Indexed> {
    let (workspace_encoding, workspace_units) = encode_workspace(&inspection.workspace);
    let folder_display = inspection
        .workspace
        .to_path_buf()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    let title = sanitize_text(&inspection.title);
    let metadata = Metadata {
        title_key: title.to_lowercase(),
        title,
        workspace_id: inspection.workspace_id.as_hyphenated().to_string(),
        workspace_encoding,
        workspace_units,
        folder_display,
        created_ns: nanos(inspection.created_at)?,
        updated_ns: nanos(inspection.updated_at)?,
        archived: inspection.archived,
        active_turn: inspection
            .active_turn
            .map(|turn| turn.as_hyphenated().to_string()),
        event_count: i64::try_from(inspection.event_count).ok()?,
        schema_version: i64::from(inspection.schema_version),
        incomplete_tail: inspection.incomplete_tail,
    };
    let mut rows = Vec::new();
    if !inspection.title.is_empty() {
        rows.push(TextRow {
            text: sanitize_text(&inspection.title),
            seq: None,
            role: "title",
        });
    }
    for entry in &inspection.entries {
        let seq = i64::try_from(entry.seq).ok();
        match &entry.kind {
            TranscriptEntryKind::Message { role, text, .. } => rows.push(TextRow {
                text: sanitize_text(text),
                seq,
                role: match role {
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                    MessageRole::Tool => "tool",
                },
            }),
            TranscriptEntryKind::ContextCompacted { record } => rows.push(TextRow {
                text: sanitize_text(&record.summary),
                seq,
                role: "summary",
            }),
            _ => {}
        }
    }
    Some(Indexed { metadata, rows })
}

/// Store a workspace path as an encoding tag and unit bytes that round-trip.
fn encode_workspace(workspace: &NativePath) -> (&'static str, Vec<u8>) {
    match workspace {
        NativePath::UnixBytes(bytes) => ("unix_bytes", bytes.clone()),
        NativePath::WindowsUtf16(units) => (
            "windows_utf16",
            units.iter().flat_map(|unit| unit.to_le_bytes()).collect(),
        ),
    }
}

fn nanos(time: OffsetDateTime) -> Option<i64> {
    i64::try_from(time.unix_timestamp_nanos()).ok()
}

fn count(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn size_value(size: u64) -> i64 {
    i64::try_from(size).unwrap_or(i64::MAX)
}

fn modified_value(modified: SystemTime) -> i64 {
    match modified.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_nanos()).unwrap_or(i64::MAX),
        Err(before) => i64::try_from(before.duration().as_nanos())
            .map(|nanos| -nanos)
            .unwrap_or(i64::MIN),
    }
}

/// The forward-slash form of the journal's path below the data root.
fn relative_text(journal: &DiscoveredJournal) -> Result<String, IndexError> {
    let mut text = String::new();
    for component in journal.relative_path.components() {
        let part = component
            .as_os_str()
            .to_str()
            .ok_or(IndexError::InvalidIndexEntry("journal path is not UTF-8"))?;
        if !text.is_empty() {
            text.push('/');
        }
        text.push_str(part);
    }
    Ok(text)
}

fn load_existing(connection: &Connection) -> Result<HashMap<String, Existing>, IndexError> {
    let mut statement = connection.prepare(
        "SELECT session_id, relative_path, size, modified_ns, state, invalid_code FROM journal",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            Existing {
                relative_path: row.get(1)?,
                size: row.get(2)?,
                modified_ns: row.get(3)?,
                state: row.get(4)?,
                invalid_code: row.get(5)?,
            },
        ))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (key, existing) = row?;
        map.insert(key, existing);
    }
    Ok(map)
}

fn load_existing_one(connection: &Connection, key: &str) -> Result<Option<Existing>, IndexError> {
    Ok(connection
        .query_row(
            "SELECT relative_path, size, modified_ns, state, invalid_code FROM journal \
             WHERE session_id = ?1",
            [key],
            |row| {
                Ok(Existing {
                    relative_path: row.get(0)?,
                    size: row.get(1)?,
                    modified_ns: row.get(2)?,
                    state: row.get(3)?,
                    invalid_code: row.get(4)?,
                })
            },
        )
        .optional()?)
}

/// Remove a session's row and text in one transaction.
fn remove_session(connection: &mut Connection, key: &str) -> Result<(), IndexError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    delete_text(&transaction, key)?;
    transaction.execute("DELETE FROM journal WHERE session_id = ?1", [key])?;
    transaction.commit()?;
    Ok(())
}

/// Delete another session's row that claims `relative_path`. A name carries its
/// own UUID, so this only happens when the stored rows were altered.
fn clear_path_conflicts(
    connection: &Connection,
    key: &str,
    relative_path: &str,
) -> Result<(), IndexError> {
    let mut statement = connection
        .prepare("SELECT session_id FROM journal WHERE relative_path = ?1 AND session_id <> ?2")?;
    let others = statement
        .query_map([relative_path, key], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<String>, _>>()?;
    for other in others {
        delete_text(connection, &other)?;
        connection.execute("DELETE FROM journal WHERE session_id = ?1", [other])?;
    }
    Ok(())
}

/// Delete the session's `entry_text` rows by the rowid range its row records.
/// The session filter keeps an altered range from deleting another session's
/// text.
fn delete_text(connection: &Connection, key: &str) -> Result<(), IndexError> {
    let range: Option<(Option<i64>, Option<i64>)> = connection
        .query_row(
            "SELECT fts_first, fts_last FROM journal WHERE session_id = ?1",
            [key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((Some(first), Some(last))) = range {
        connection.execute(
            "DELETE FROM entry_text WHERE rowid BETWEEN ?1 AND ?2 AND session_id = ?3",
            rusqlite::params![first, last, key],
        )?;
    }
    Ok(())
}

/// Insert searchable texts. Inside the write transaction the rowids are
/// assigned in increasing order, so the first and last bound all of them.
fn insert_text(
    connection: &Connection,
    key: &str,
    rows: &[TextRow],
) -> Result<Option<(i64, i64)>, IndexError> {
    let mut statement = connection
        .prepare("INSERT INTO entry_text (text, session_id, seq, role) VALUES (?1, ?2, ?3, ?4)")?;
    let mut bounds: Option<(i64, i64)> = None;
    for row in rows {
        statement.execute(rusqlite::params![row.text, key, row.seq, row.role])?;
        let rowid = connection.last_insert_rowid();
        bounds = Some(bounds.map_or((rowid, rowid), |(first, _)| (first, rowid)));
    }
    Ok(bounds)
}

/// Insert or replace the `journal` row for one session.
fn write_row(connection: &Connection, row: &JournalRow<'_>) -> Result<(), IndexError> {
    let metadata = row.metadata;
    let (first, last) = row.fts.unzip();
    connection.execute(
        "INSERT INTO journal (
             session_id, relative_path, size, modified_ns, state, invalid_code,
             title, title_key, workspace_id, workspace_encoding, workspace_units,
             folder_display, created_ns, updated_ns, archived, active_turn,
             event_count, schema_version, incomplete_tail, fts_first, fts_last
         ) VALUES (
             :session_id, :relative_path, :size, :modified_ns, :state, :invalid_code,
             :title, :title_key, :workspace_id, :workspace_encoding, :workspace_units,
             :folder_display, :created_ns, :updated_ns, :archived, :active_turn,
             :event_count, :schema_version, :incomplete_tail, :fts_first, :fts_last
         ) ON CONFLICT (session_id) DO UPDATE SET
             relative_path = excluded.relative_path, size = excluded.size,
             modified_ns = excluded.modified_ns, state = excluded.state,
             invalid_code = excluded.invalid_code, title = excluded.title,
             title_key = excluded.title_key, workspace_id = excluded.workspace_id,
             workspace_encoding = excluded.workspace_encoding,
             workspace_units = excluded.workspace_units,
             folder_display = excluded.folder_display, created_ns = excluded.created_ns,
             updated_ns = excluded.updated_ns, archived = excluded.archived,
             active_turn = excluded.active_turn, event_count = excluded.event_count,
             schema_version = excluded.schema_version,
             incomplete_tail = excluded.incomplete_tail,
             fts_first = excluded.fts_first, fts_last = excluded.fts_last",
        named_params! {
            ":session_id": row.session_id,
            ":relative_path": row.relative_path,
            ":size": row.size,
            ":modified_ns": row.modified_ns,
            ":state": row.state,
            ":invalid_code": row.invalid_code,
            ":title": metadata.map(|m| m.title.as_str()),
            ":title_key": metadata.map(|m| m.title_key.as_str()),
            ":workspace_id": metadata.map(|m| m.workspace_id.as_str()),
            ":workspace_encoding": metadata.map(|m| m.workspace_encoding),
            ":workspace_units": metadata.map(|m| m.workspace_units.as_slice()),
            ":folder_display": metadata.map(|m| m.folder_display.as_str()),
            ":created_ns": metadata.map(|m| m.created_ns),
            ":updated_ns": metadata.map(|m| m.updated_ns),
            ":archived": metadata.is_some_and(|m| m.archived),
            ":active_turn": metadata.and_then(|m| m.active_turn.as_deref()),
            ":event_count": metadata.map_or(0, |m| m.event_count),
            ":schema_version": metadata.map(|m| m.schema_version),
            ":incomplete_tail": metadata.is_some_and(|m| m.incomplete_tail),
            ":fts_first": first,
            ":fts_last": last,
        },
    )?;
    Ok(())
}
