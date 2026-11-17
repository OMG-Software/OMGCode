//! A derived, rebuildable index over session journals.
//!
//! The JSONL journals stay the only source of truth. This index is a cache that
//! lets a session tree list titles, folders, times, and status without
//! re-reading every journal, and lets the user search transcripts. Losing,
//! deleting, or corrupting the index never loses data: [`SessionIndex::open`]
//! discards an unusable database and the next [`SessionIndex::refresh`]
//! rebuilds it from the journals. The index grants no authority. Every value it
//! returns is untrusted historical data, and a stored path is revalidated
//! before [`SessionIndex::journal_path`] returns it.
//!
//! # Storage
//!
//! The index is one SQLite database, `<data_root>/session-index-v1/index.sqlite3`,
//! built with the bundled SQLite and its FTS5 extension. It uses the default
//! rollback journal (not WAL) and these settings on every connection:
//! `application_id` [`APPLICATION_ID`], `user_version` 1, `foreign_keys=ON`,
//! `trusted_schema=OFF`, `secure_delete=ON` (the file holds transcript text),
//! a two-second busy timeout, `SQLITE_OPEN_NOFOLLOW`, no URI filenames, no
//! shared cache, and extension loading disabled. All statements are
//! parameterized. A sort order comes from a closed enum that maps to static SQL.
//! The directory and the database file are refused when either is a symbolic
//! link or Windows reparse point ([`IndexError::UnsafeLocation`]).
//!
//! A database that is not SQLite, is corrupt at open, or carries another
//! `application_id` or `user_version` is deleted together with its `-journal`
//! file, and an empty index is created. Nothing outside `session-index-v1/` is
//! touched. The schema is numbered as migration 1; a later migration can be
//! appended to the migration list.
//!
//! # What is indexed
//!
//! Per journal: the title, every message (user, assistant, tool), and a
//! compaction summary, each as one searchable text. Tool-call arguments, request
//! manifests, outcomes, and failure codes are not indexed. Journals are read only
//! through [`SessionInspection::inspect`], which never takes the writer lock.
//! The code points U+E000 and U+E001 are reserved as snippet markers, so they
//! are replaced with U+FFFD before text enters the index.
//!
//! # Concurrency
//!
//! A [`SessionIndex`] owns one connection and is used from one thread at a time
//! (`&mut self` for refresh). Two processes may open the same index; SQLite's
//! own locking arbitrates and contention that outlasts the busy timeout surfaces
//! as [`IndexError::Busy`]. Each transaction covers exactly one journal, and a
//! journal is inspected before its transaction opens, never inside it.
//!
//! # Busy and invalid journals
//!
//! A journal that a writer holds (Windows) keeps its previous metadata and text
//! with state `busy` and is retried on every refresh. A journal that fails
//! validation becomes `invalid` with a stable code, loses its text, and is not
//! inspected again until its size or modification time changes. An operating
//! system failure marks the journal `invalid` with `io_error` and is retried.
//! One bad journal never fails a refresh.

mod query;
mod refresh;
mod schema;
mod snippet;

use std::io;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Serialize;
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::session::{NativePath, SessionError};

pub use query::{
    ArchiveView, SearchHit, SearchListing, SearchRole, SessionListing, SessionQuery, SessionSort,
    SnippetPart,
};
pub use refresh::RefreshReport;

/// SQLite `application_id` stamped into the database header: the ASCII bytes
/// `OMGI` read as a big-endian integer. A database with another value is not
/// this index and is rebuilt.
pub const APPLICATION_ID: i32 = 0x4f4d_4749;

/// Directory below the data root that holds the index. The `v1` names the
/// on-disk layout; a different layout would use a new directory.
pub const INDEX_DIRECTORY: &str = "session-index-v1";

/// Database file name inside [`INDEX_DIRECTORY`].
pub const INDEX_FILE: &str = "index.sqlite3";

/// A failure while opening, refreshing, or querying the session index.
///
/// No variant embeds transcript text or an absolute path.
#[derive(Debug, Error)]
pub enum IndexError {
    /// Another connection held a conflicting lock past the busy timeout.
    #[error("session index is busy")]
    Busy,
    /// The caller's search text cannot be used.
    #[error("invalid search query: {0}")]
    InvalidQuery(&'static str),
    /// A stored value failed validation. The database is a cache that can be
    /// tampered with, so nothing stored is trusted.
    #[error("invalid session index entry: {0}")]
    InvalidIndexEntry(&'static str),
    /// The index directory or database file is a link, a reparse point, or not
    /// the expected kind of file.
    #[error("unsafe session index location: {0}")]
    UnsafeLocation(&'static str),
    /// Journal discovery failed.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// A filesystem operation on the index location failed.
    #[error("session index I/O failed: {0}")]
    Io(#[from] io::Error),
    /// SQLite reported any other failure.
    #[error("session index database operation failed")]
    Sqlite(#[source] rusqlite::Error),
}

impl From<rusqlite::Error> for IndexError {
    fn from(error: rusqlite::Error) -> Self {
        use rusqlite::ErrorCode;
        if let rusqlite::Error::SqliteFailure(failure, _) = &error
            && matches!(
                failure.code,
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
            )
        {
            return Self::Busy;
        }
        Self::Sqlite(error)
    }
}

/// How a journal was last classified by the index.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexedState {
    /// Inspected successfully. Metadata and text are current as of the stored
    /// size and modification time.
    Ok,
    /// A writer held the journal at the last attempt. Metadata and text, if any,
    /// are from an earlier inspection.
    Busy,
    /// The journal failed validation or could not be read. It has no text.
    Invalid,
}

/// One session as the index last saw it. Everything here is untrusted
/// historical data and grants no authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct IndexedSession {
    /// The session UUID taken from the journal file name.
    pub session_id: Uuid,
    /// How the journal was last classified.
    pub state: IndexedState,
    /// Stable code and static detail for an `Invalid` journal.
    pub invalid_code: Option<String>,
    /// Session title, when the journal has been inspected.
    pub title: Option<String>,
    /// Workspace association recorded at creation.
    pub workspace_id: Option<Uuid>,
    /// Lossless workspace path recorded at creation. Descriptive only.
    pub workspace: Option<NativePath>,
    /// Lossy workspace path text for display. Never reopen a path from it.
    pub folder_display: Option<String>,
    /// Time of the `session.started` event.
    #[serde(with = "time::serde::rfc3339::option")]
    pub created_at: Option<OffsetDateTime>,
    /// Time of the last complete event.
    #[serde(with = "time::serde::rfc3339::option")]
    pub updated_at: Option<OffsetDateTime>,
    /// Whether the session was archived after its last complete event. `false`
    /// when unknown.
    pub archived: bool,
    /// The turn left active by the last complete event, if any.
    pub active_turn: Option<Uuid>,
    /// Number of complete events, including `session.started`.
    pub event_count: u64,
    /// Whether bytes followed the last complete record when inspected.
    pub incomplete_tail: bool,
}

/// A handle to the derived session index. See the module documentation.
#[derive(Debug)]
pub struct SessionIndex {
    data_root: PathBuf,
    connection: Connection,
}

impl SessionIndex {
    /// Open the index below `data_root`, creating it when absent.
    ///
    /// `data_root` must come from trusted application settings. The index lives
    /// in `<data_root>/session-index-v1/`. A database that is not SQLite, is
    /// corrupt, or has another `application_id` or `user_version` is deleted
    /// (only the index's own files) and recreated empty; call
    /// [`refresh`](Self::refresh) to repopulate it.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::UnsafeLocation`] when the index directory or file is
    /// a link or reparse point or is not the expected kind of object,
    /// [`IndexError::Busy`] when another connection holds the database past the
    /// busy timeout, [`IndexError::Io`] for filesystem failures, and
    /// [`IndexError::Sqlite`] for other database failures.
    pub fn open(data_root: &Path) -> Result<Self, IndexError> {
        let connection = schema::open_connection(data_root)?;
        Ok(Self {
            data_root: data_root.to_path_buf(),
            connection,
        })
    }

    /// The data root this index describes journals for.
    pub fn data_root(&self) -> &Path {
        &self.data_root
    }
}
