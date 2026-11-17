//! Listing, lookup, and full-text search over the index.

use std::path::PathBuf;

use rusqlite::Row;
use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

use super::snippet::{MARK_END, MARK_START, match_expression, parse_snippet};
use super::{IndexError, IndexedSession, IndexedState, SessionIndex};
use crate::session::{NativePath, strict_journal_session_id};

/// The most sessions one listing returns.
const MAX_LISTING: usize = 5000;
/// The most hits one search returns.
const MAX_HITS: usize = 200;
/// Tokens of context around a match in a snippet.
const SNIPPET_TOKENS: i64 = 24;
/// The code recorded for a session UUID carried by two journal files.
const CODE_DUPLICATE: &str = "duplicate_session_id";

/// The columns `IndexedSession` needs, in the order `RawSession::read` expects.
/// Statements alias `journal` as `j`.
macro_rules! session_columns {
    () => {
        "j.session_id, j.state, j.invalid_code, j.title, j.workspace_id, \
         j.workspace_encoding, j.workspace_units, j.folder_display, j.created_ns, \
         j.updated_ns, j.archived, j.active_turn, j.event_count, j.incomplete_tail"
    };
}

/// A listing statement with a fixed `ORDER BY`. The order text is a literal at
/// the call site, never caller input.
macro_rules! list_sql {
    ($order:literal) => {
        concat!(
            "SELECT ",
            session_columns!(),
            " FROM journal j WHERE j.archived = ?1 ORDER BY ",
            $order,
            " LIMIT ?2"
        )
    };
}

/// Which sessions a listing or search covers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveView {
    /// Sessions that are not archived, including those with unknown status
    /// because their journal is busy or invalid and was never read.
    Active,
    /// Archived sessions.
    Archived,
}

impl ArchiveView {
    fn archived_flag(self) -> i64 {
        match self {
            Self::Active => 0,
            Self::Archived => 1,
        }
    }
}

/// The key a listing sorts by. Rows without a value for the key sort last in
/// either direction, and ties break by session UUID ascending.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSort {
    /// Time of the last complete event.
    Updated,
    /// Time of the `session.started` event.
    Created,
    /// Title, compared case-insensitively by Unicode lowercase.
    Title,
}

/// A listing request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionQuery {
    /// Archived or active sessions.
    pub view: ArchiveView,
    /// The sort key.
    pub sort: SessionSort,
    /// Sort largest or latest first, or last to first for titles.
    pub descending: bool,
    /// The most sessions to return, clamped to 1 through 5000.
    pub limit: usize,
}

/// One page of a listing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SessionListing {
    /// Sessions in the requested order, at most `limit`.
    pub sessions: Vec<IndexedSession>,
    /// How many sessions the view holds before `limit` applied.
    pub total: u64,
}

/// What kind of text a search hit matched.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchRole {
    /// A user message.
    User,
    /// An assistant message.
    Assistant,
    /// A tool result message.
    Tool,
    /// A context compaction summary.
    Summary,
    /// The session title.
    Title,
}

/// A run of snippet text, plain or matched.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SnippetPart {
    /// Transcript text. It never contains the reserved marker code points.
    pub text: String,
    /// Whether the search matched this run.
    pub matched: bool,
}

/// One matching text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SearchHit {
    /// The session the text belongs to.
    pub session: IndexedSession,
    /// The journal sequence number, or `None` for a title.
    pub seq: Option<u64>,
    /// What kind of text matched.
    pub role: SearchRole,
    /// About 24 tokens around the match.
    pub snippet: Vec<SnippetPart>,
}

/// Search results.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SearchListing {
    /// Hits ordered by relevance, then newest session, then position.
    pub hits: Vec<SearchHit>,
    /// Whether more hits exist than `limit` allowed.
    pub truncated: bool,
}

/// The stored values of one `journal` row before validation.
struct RawSession {
    session_id: String,
    state: String,
    invalid_code: Option<String>,
    title: Option<String>,
    workspace_id: Option<String>,
    workspace_encoding: Option<String>,
    workspace_units: Option<Vec<u8>>,
    folder_display: Option<String>,
    created_ns: Option<i64>,
    updated_ns: Option<i64>,
    archived: i64,
    active_turn: Option<String>,
    event_count: i64,
    incomplete_tail: i64,
}

impl RawSession {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            session_id: row.get(0)?,
            state: row.get(1)?,
            invalid_code: row.get(2)?,
            title: row.get(3)?,
            workspace_id: row.get(4)?,
            workspace_encoding: row.get(5)?,
            workspace_units: row.get(6)?,
            folder_display: row.get(7)?,
            created_ns: row.get(8)?,
            updated_ns: row.get(9)?,
            archived: row.get(10)?,
            active_turn: row.get(11)?,
            event_count: row.get(12)?,
            incomplete_tail: row.get(13)?,
        })
    }

    /// Validate every stored value. The database is a cache that may have been
    /// altered, so nothing is trusted.
    fn into_session(self) -> Result<IndexedSession, IndexError> {
        let state = match self.state.as_str() {
            "ok" => IndexedState::Ok,
            "busy" => IndexedState::Busy,
            "invalid" => IndexedState::Invalid,
            _ => return Err(IndexError::InvalidIndexEntry("unknown journal state")),
        };
        Ok(IndexedSession {
            session_id: parse_uuid(&self.session_id)?,
            state,
            invalid_code: self.invalid_code,
            title: self.title,
            workspace_id: self.workspace_id.as_deref().map(parse_uuid).transpose()?,
            workspace: decode_workspace(self.workspace_encoding, self.workspace_units)?,
            folder_display: self.folder_display,
            created_at: self.created_ns.map(timestamp).transpose()?,
            updated_at: self.updated_ns.map(timestamp).transpose()?,
            archived: self.archived != 0,
            active_turn: self.active_turn.as_deref().map(parse_uuid).transpose()?,
            event_count: u64::try_from(self.event_count)
                .map_err(|_| IndexError::InvalidIndexEntry("negative event count"))?,
            incomplete_tail: self.incomplete_tail != 0,
        })
    }
}

fn parse_uuid(text: &str) -> Result<Uuid, IndexError> {
    Uuid::try_parse(text).map_err(|_| IndexError::InvalidIndexEntry("malformed UUID"))
}

fn timestamp(nanos: i64) -> Result<OffsetDateTime, IndexError> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(nanos))
        .map_err(|_| IndexError::InvalidIndexEntry("timestamp out of range"))
}

fn decode_workspace(
    encoding: Option<String>,
    units: Option<Vec<u8>>,
) -> Result<Option<NativePath>, IndexError> {
    match (encoding.as_deref(), units) {
        (None, None) => Ok(None),
        (Some("unix_bytes"), Some(bytes)) => Ok(Some(NativePath::UnixBytes(bytes))),
        (Some("windows_utf16"), Some(bytes)) => {
            if bytes.len() % 2 != 0 {
                return Err(IndexError::InvalidIndexEntry("odd UTF-16 byte count"));
            }
            let (pairs, _) = bytes.as_chunks::<2>();
            let units = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
            Ok(Some(NativePath::WindowsUtf16(units)))
        }
        _ => Err(IndexError::InvalidIndexEntry("malformed workspace path")),
    }
}

impl SessionIndex {
    /// List sessions in one view with a fixed, deterministic order.
    ///
    /// `total` counts the whole view before `limit` (clamped to 1 through 5000)
    /// applies. Busy and invalid sessions are listed with their state.
    ///
    /// # Errors
    ///
    /// Returns database failures and [`IndexError::InvalidIndexEntry`] when a
    /// stored row fails validation.
    pub fn sessions(&self, query: &SessionQuery) -> Result<SessionListing, IndexError> {
        let sql = match (query.sort, query.descending) {
            (SessionSort::Updated, false) => {
                list_sql!("j.updated_ns IS NULL, j.updated_ns ASC, j.session_id ASC")
            }
            (SessionSort::Updated, true) => {
                list_sql!("j.updated_ns IS NULL, j.updated_ns DESC, j.session_id ASC")
            }
            (SessionSort::Created, false) => {
                list_sql!("j.created_ns IS NULL, j.created_ns ASC, j.session_id ASC")
            }
            (SessionSort::Created, true) => {
                list_sql!("j.created_ns IS NULL, j.created_ns DESC, j.session_id ASC")
            }
            (SessionSort::Title, false) => {
                list_sql!("j.title_key IS NULL, j.title_key ASC, j.session_id ASC")
            }
            (SessionSort::Title, true) => {
                list_sql!("j.title_key IS NULL, j.title_key DESC, j.session_id ASC")
            }
        };
        let limit = i64::try_from(query.limit.clamp(1, MAX_LISTING)).unwrap_or(i64::MAX);
        let archived = query.view.archived_flag();
        let total: i64 = self.connection.query_row(
            "SELECT count(*) FROM journal WHERE archived = ?1",
            [archived],
            |row| row.get(0),
        )?;
        let mut statement = self.connection.prepare(sql)?;
        let raws = statement
            .query_map(rusqlite::params![archived, limit], RawSession::read)?
            .collect::<Result<Vec<_>, _>>()?;
        let sessions = raws
            .into_iter()
            .map(RawSession::into_session)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SessionListing {
            sessions,
            total: u64::try_from(total).unwrap_or(0),
        })
    }

    /// Look up one session by UUID.
    ///
    /// # Errors
    ///
    /// Returns database failures and [`IndexError::InvalidIndexEntry`].
    pub fn session(&self, session_id: Uuid) -> Result<Option<IndexedSession>, IndexError> {
        let key = session_id.as_hyphenated().to_string();
        let mut statement = self.connection.prepare(concat!(
            "SELECT ",
            session_columns!(),
            " FROM journal j WHERE j.session_id = ?1"
        ))?;
        let mut rows = statement.query_map([key], RawSession::read)?;
        rows.next()
            .transpose()?
            .map(RawSession::into_session)
            .transpose()
    }

    /// Resolve a session UUID to its journal path for a later writer open.
    ///
    /// The database is a cache and could have been altered, so the stored
    /// relative path is revalidated before it is joined to the data root: it
    /// must be exactly `sessions/<YYYY>/<MM>/<DD>/<file>` where `<file>` is the
    /// writer's exact journal name carrying `session_id`. Nothing on the
    /// filesystem is touched, and the journal may no longer exist.
    ///
    /// Returns `None` when the index has no row for the session.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::InvalidIndexEntry`] for a stored path that fails
    /// validation, and for a session whose UUID two journal files carry.
    pub fn journal_path(&self, session_id: Uuid) -> Result<Option<PathBuf>, IndexError> {
        let key = session_id.as_hyphenated().to_string();
        let mut statement = self
            .connection
            .prepare("SELECT relative_path, invalid_code FROM journal WHERE session_id = ?1")?;
        let mut rows = statement.query_map([key], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        let Some(row) = rows.next().transpose()? else {
            return Ok(None);
        };
        let (relative_path, invalid_code) = row;
        if invalid_code.as_deref() == Some(CODE_DUPLICATE) {
            return Err(IndexError::InvalidIndexEntry(
                "more than one journal has this session UUID",
            ));
        }
        let mut path = self.data_root.clone();
        for part in validated_components(&relative_path, session_id)? {
            path.push(part);
        }
        Ok(Some(path))
    }

    /// Search transcripts for `text`.
    ///
    /// The text is never FTS5 syntax. Whitespace-separated terms (at most 16,
    /// at most 256 characters in all) must all match, case- and
    /// diacritic-insensitively, and the last term also matches as a prefix.
    /// Only `ok` and `busy` sessions in `view` can match. `limit` is clamped to
    /// 1 through 200. Results order by relevance, then most recently updated
    /// session, then session and position.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::InvalidQuery`] for unusable text, database
    /// failures, and [`IndexError::InvalidIndexEntry`].
    pub fn search(
        &self,
        text: &str,
        view: ArchiveView,
        limit: usize,
    ) -> Result<SearchListing, IndexError> {
        let expression = match_expression(text)?;
        let limit = limit.clamp(1, MAX_HITS);
        let fetch = i64::try_from(limit + 1).unwrap_or(i64::MAX);
        let mut statement = self.connection.prepare(concat!(
            "SELECT ",
            session_columns!(),
            ", entry_text.seq, entry_text.role, snippet(entry_text, 0, ?4, ?5, '', ?6) \
             FROM entry_text JOIN journal j ON j.session_id = entry_text.session_id \
             WHERE entry_text MATCH ?1 AND j.state IN ('ok', 'busy') AND j.archived = ?2 \
             ORDER BY rank, j.updated_ns DESC, j.session_id ASC, entry_text.seq ASC \
             LIMIT ?3"
        ))?;
        let start = MARK_START.to_string();
        let end = MARK_END.to_string();
        let raws = statement
            .query_map(
                rusqlite::params![
                    expression,
                    view.archived_flag(),
                    fetch,
                    start,
                    end,
                    SNIPPET_TOKENS
                ],
                |row| {
                    Ok((
                        RawSession::read(row)?,
                        row.get::<_, Option<i64>>(14)?,
                        row.get::<_, String>(15)?,
                        row.get::<_, String>(16)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut hits = Vec::new();
        for (raw, seq, role, marked) in raws {
            hits.push(SearchHit {
                session: raw.into_session()?,
                seq: seq
                    .map(u64::try_from)
                    .transpose()
                    .map_err(|_| IndexError::InvalidIndexEntry("negative sequence number"))?,
                role: parse_role(&role)?,
                snippet: parse_snippet(&marked),
            });
        }
        let truncated = hits.len() > limit;
        hits.truncate(limit);
        Ok(SearchListing { hits, truncated })
    }
}

fn parse_role(role: &str) -> Result<SearchRole, IndexError> {
    match role {
        "user" => Ok(SearchRole::User),
        "assistant" => Ok(SearchRole::Assistant),
        "tool" => Ok(SearchRole::Tool),
        "summary" => Ok(SearchRole::Summary),
        "title" => Ok(SearchRole::Title),
        _ => Err(IndexError::InvalidIndexEntry("unknown text role")),
    }
}

/// Check a stored relative path against the exact layout the writer produces
/// and return its components.
fn validated_components(relative: &str, session_id: Uuid) -> Result<Vec<&str>, IndexError> {
    const INVALID: IndexError = IndexError::InvalidIndexEntry("stored journal path is not valid");
    if relative.contains(['\\', '\0']) {
        return Err(INVALID);
    }
    let parts: Vec<&str> = relative.split('/').collect();
    let [sessions, year, month, day, file] = parts.as_slice() else {
        return Err(INVALID);
    };
    let layout_ok = *sessions == "sessions"
        && has_ascii_digits(year, 4)
        && has_ascii_digits(month, 2)
        && has_ascii_digits(day, 2);
    if !layout_ok || strict_journal_session_id(file) != Some(session_id) {
        return Err(INVALID);
    }
    Ok(parts)
}

fn has_ascii_digits(text: &str, width: usize) -> bool {
    text.len() == width && text.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::validated_components;
    use uuid::Uuid;

    #[test]
    fn only_the_exact_writer_layout_for_the_same_session_validates() {
        let id = Uuid::new_v4();
        let other = Uuid::new_v4();
        let good = format!("sessions/2026/11/16/omgcode-01-02-03-004Z-{id}.jsonl");
        assert_eq!(validated_components(&good, id).expect("valid").len(), 5);
        let bad = [
            "../../evil.jsonl".to_owned(),
            format!("/sessions/2026/11/16/omgcode-01-02-03-004Z-{id}.jsonl"),
            format!("sessions\\2026\\11\\16\\omgcode-01-02-03-004Z-{id}.jsonl"),
            format!("sessions/2026/11/16/omgcode-01-02-03-004Z-{other}.jsonl"),
            format!("sessions/2026/11/16/extra/omgcode-01-02-03-004Z-{id}.jsonl"),
            format!("sessions/2026/11/../16/omgcode-01-02-03-004Z-{id}.jsonl"),
            format!("sessions/26/11/16/omgcode-01-02-03-004Z-{id}.jsonl"),
            format!("sessions/2026/11/16/omgcode-01-02-03-004Z-{id}.jsonl\0"),
            format!("other/2026/11/16/omgcode-01-02-03-004Z-{id}.jsonl"),
            String::new(),
        ];
        for path in bad {
            assert!(validated_components(&path, id).is_err(), "{path:?}");
        }
    }
}
