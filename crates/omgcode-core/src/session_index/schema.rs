//! Opening the database, its pragmas, and its numbered migrations.

use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use rusqlite::config::DbConfig;
use rusqlite::{Connection, ErrorCode, OpenFlags, TransactionBehavior};

use super::{APPLICATION_ID, INDEX_DIRECTORY, INDEX_FILE, IndexError};
use crate::storage::{self, StorageError};

/// How long a statement waits for another connection's lock.
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);

/// Migration 1: the first schema. Append later migrations to `MIGRATIONS`;
/// never edit an applied one. The index is a cache, so a version that is not
/// in `1..=MIGRATIONS.len()` is rebuilt rather than migrated.
///
/// Times are UTC nanoseconds since the Unix epoch so they sort numerically.
/// `workspace_units` holds `NativePath` units: raw bytes for `unix_bytes`, or
/// little-endian UTF-16 code units for `windows_utf16`. `fts_first` and
/// `fts_last` bound the `entry_text` rowids written for the journal so they can
/// be deleted by range instead of by scanning the full-text table.
pub(super) const MIGRATION_1: &str = "
CREATE TABLE journal (
    session_id TEXT NOT NULL PRIMARY KEY,
    relative_path TEXT NOT NULL UNIQUE,
    size INTEGER NOT NULL,
    modified_ns INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('ok', 'busy', 'invalid')),
    invalid_code TEXT,
    title TEXT,
    title_key TEXT,
    workspace_id TEXT,
    workspace_encoding TEXT CHECK (workspace_encoding IN ('unix_bytes', 'windows_utf16')),
    workspace_units BLOB,
    folder_display TEXT,
    created_ns INTEGER,
    updated_ns INTEGER,
    archived INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1)),
    active_turn TEXT,
    event_count INTEGER NOT NULL DEFAULT 0,
    schema_version INTEGER,
    incomplete_tail INTEGER NOT NULL DEFAULT 0 CHECK (incomplete_tail IN (0, 1)),
    fts_first INTEGER,
    fts_last INTEGER
) STRICT;
CREATE INDEX journal_archived_updated ON journal (archived, updated_ns);
CREATE VIRTUAL TABLE entry_text USING fts5(
    text,
    session_id UNINDEXED,
    seq UNINDEXED,
    role UNINDEXED,
    tokenize = 'unicode61 remove_diacritics 2'
);
";

/// Ordered migrations. Migration `n` is `MIGRATIONS[n - 1]`.
const MIGRATIONS: [&str; 1] = [MIGRATION_1];

/// The `user_version` after every migration has run.
const SCHEMA_VERSION: i64 = 1;
const _: () = assert!(
    MIGRATIONS.len() == 1,
    "update SCHEMA_VERSION with MIGRATIONS"
);

/// What opening one database file found.
enum Opened {
    Ready(Connection),
    /// The file is not a usable index of this version. It is a cache, so the
    /// caller deletes it and starts again.
    Rebuild,
}

/// Open or create the index database below `data_root`.
pub(super) fn open_connection(data_root: &Path) -> Result<Connection, IndexError> {
    let directory = data_root.join(INDEX_DIRECTORY);
    check_location(&directory, LocationKind::Directory)?;
    storage::create_private_directories(&directory).map_err(storage_error)?;
    check_location(&directory, LocationKind::Directory)?;
    let database = std::path::absolute(directory.join(INDEX_FILE))?;
    if database
        .to_str()
        .is_some_and(|text| text.starts_with("file:"))
    {
        // The bundled SQLite is built with URI filenames on by default. An
        // absolute native path never starts this way, but refuse it anyway.
        return Err(IndexError::UnsafeLocation(
            "index path looks like a URI filename",
        ));
    }
    check_location(&database, LocationKind::File)?;
    match try_open(&database)? {
        Opened::Ready(connection) => Ok(connection),
        Opened::Rebuild => {
            remove_index_files(&directory)?;
            check_location(&database, LocationKind::File)?;
            match try_open(&database)? {
                Opened::Ready(connection) => Ok(connection),
                Opened::Rebuild => Err(IndexError::InvalidIndexEntry(
                    "a newly created index failed validation",
                )),
            }
        }
    }
}

#[derive(Clone, Copy)]
enum LocationKind {
    Directory,
    File,
}

/// Refuse a link, a reparse point, or an object of the wrong kind. A missing
/// object is fine. A race between this check and the open is not excluded: the
/// data root must be trusted, and `SQLITE_OPEN_NOFOLLOW` covers the final
/// component on platforms that honor it.
fn check_location(path: &Path, kind: LocationKind) -> Result<(), IndexError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
        return Err(IndexError::UnsafeLocation(
            "index location is a link or reparse point",
        ));
    }
    let expected = match kind {
        LocationKind::Directory => metadata.is_dir(),
        LocationKind::File => metadata.is_file(),
    };
    if expected {
        Ok(())
    } else {
        Err(IndexError::UnsafeLocation(
            "index location is not the expected kind of file",
        ))
    }
}

#[cfg(windows)]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

fn storage_error(error: StorageError) -> IndexError {
    match error {
        StorageError::Io(error) => IndexError::Io(error),
        StorageError::Invalid(detail) => IndexError::UnsafeLocation(detail),
        StorageError::Busy => IndexError::Busy,
    }
}

/// Delete only the index's own database and rollback-journal files.
fn remove_index_files(directory: &Path) -> Result<(), IndexError> {
    for name in [INDEX_FILE, "index.sqlite3-journal"] {
        match fs::remove_file(directory.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn is_unusable_database(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if matches!(failure.code, ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt)
    )
}

/// Open `database` with the fixed flags and settings and validate its header.
fn try_open(database: &Path) -> Result<Opened, IndexError> {
    // No URI flag and no shared cache. NO_MUTEX because a connection is used by
    // one thread at a time.
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NOFOLLOW
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let mut connection = match Connection::open_with_flags(database, flags) {
        Ok(connection) => connection,
        Err(error) if is_unusable_database(&error) => return Ok(Opened::Rebuild),
        Err(error) => return Err(error.into()),
    };
    match prepare(&mut connection) {
        Ok(true) => Ok(Opened::Ready(connection)),
        Ok(false) => Ok(Opened::Rebuild),
        Err(error) if is_unusable_database(&error) => Ok(Opened::Rebuild),
        Err(error) => Err(error.into()),
    }
}

/// Apply connection settings, then validate or initialize the schema. Returns
/// `false` when the file is not a usable index.
fn prepare(connection: &mut Connection) -> rusqlite::Result<bool> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_FKEY, true)?;
    connection.pragma_update(None, "secure_delete", "ON")?;
    // The first statement that reads the file: a non-database fails here.
    let (application_id, user_version) = read_header(connection)?;
    if application_id == 0 && user_version == 0 {
        if !is_empty_database(connection)? {
            return Ok(false);
        }
        return initialize(connection);
    }
    if application_id != APPLICATION_ID || !(1..=SCHEMA_VERSION).contains(&user_version) {
        return Ok(false);
    }
    if user_version < SCHEMA_VERSION {
        migrate(connection, user_version)?;
    }
    has_expected_tables(connection)
}

fn read_header(connection: &Connection) -> rusqlite::Result<(i32, i64)> {
    let application_id = connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let user_version = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    Ok((application_id, user_version))
}

fn is_empty_database(connection: &Connection) -> rusqlite::Result<bool> {
    let objects: i64 =
        connection.query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))?;
    Ok(objects == 0)
}

fn has_expected_tables(connection: &Connection) -> rusqlite::Result<bool> {
    let tables: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name IN ('journal', 'entry_text')",
        [],
        |row| row.get(0),
    )?;
    Ok(tables == 2)
}

/// Create the schema in one immediate transaction. Another process may have
/// initialized the file while this one waited for the lock, so re-check first.
fn initialize(connection: &mut Connection) -> rusqlite::Result<bool> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (application_id, user_version) = read_header(&transaction)?;
    if application_id == 0 && user_version == 0 && is_empty_database(&transaction)? {
        transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
        migrate_in(&transaction, 0)?;
        transaction.commit()?;
        return has_expected_tables(connection);
    }
    drop(transaction);
    let (application_id, user_version) = read_header(connection)?;
    if application_id != APPLICATION_ID || user_version != SCHEMA_VERSION {
        return Ok(false);
    }
    has_expected_tables(connection)
}

/// Bring an older supported version up to date in one transaction.
fn migrate(connection: &mut Connection, from: i64) -> rusqlite::Result<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    migrate_in(&transaction, from)?;
    transaction.commit()
}

/// Run every migration after version `from` and record the new version.
fn migrate_in(connection: &Connection, from: i64) -> rusqlite::Result<()> {
    let skip = usize::try_from(from).unwrap_or(MIGRATIONS.len());
    for migration in MIGRATIONS.iter().skip(skip) {
        connection.execute_batch(migration)?;
    }
    connection.pragma_update(None, "user_version", SCHEMA_VERSION)
}
