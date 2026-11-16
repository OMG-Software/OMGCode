//! Session tree, search, transcript, and lifecycle commands.

use std::fmt::Write as _;

use omgcode_core::session::{NativePath, SessionInspection, SessionJournal, TranscriptEntryKind};
use omgcode_core::session_index::SnippetPart as IndexSnippetPart;
use omgcode_core::session_index::{
    ArchiveView, IndexedSession, SearchRole, SessionQuery, SessionSort,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::error::DesktopError;
use super::state::{folder_of, folder_of_path, format_time, summary_of, summary_of_inspection};
use super::wire::{
    EntryKind, SearchHit, SearchRequest, SearchResult, SearchRoleDto, SessionRequest,
    SessionSummary, SessionTree, SnippetPart, TranscriptEntry, TranscriptPage, TranscriptRequest,
    TreeRequest, TreeSort, TreeView, WorkspaceGroup, source_label,
};
use super::{DesktopService, Inner};

/// Most sessions one tree listing returns.
const LISTING_LIMIT: usize = 5000;
/// Most search hits one request returns.
const MAX_SEARCH_HITS: usize = 200;
/// Most transcript entries one page returns.
const MAX_PAGE_ENTRIES: usize = 200;
/// Most bytes of text one transcript page carries. A page always holds at
/// least one entry.
const MAX_PAGE_TEXT_BYTES: usize = 1024 * 1024;
/// Failure code recorded when the operator closes an open turn.
const INTERRUPT_CODE: &str = "operator_interrupted";
/// Characters of a group key that make the opaque group identifier.
const GROUP_ID_CHARS: usize = 16;

impl DesktopService {
    /// List sessions grouped by their recorded workspace folder.
    ///
    /// The index is refreshed first, within a time bound. Groups appear in the
    /// order their first session appears in the sorted listing, and sessions
    /// keep the listing order. At most 5000 sessions are listed;
    /// `indexed_sessions` counts the whole view.
    ///
    /// # Errors
    ///
    /// `busy` when the index is held elsewhere, `internal` otherwise.
    pub fn session_tree(&self, request: TreeRequest) -> Result<SessionTree, DesktopError> {
        let inner = &self.inner;
        let running = inner.running_sessions()?;
        let query = SessionQuery {
            view: archive_view(request.view),
            sort: match request.sort {
                TreeSort::Updated => SessionSort::Updated,
                TreeSort::Created => SessionSort::Created,
                TreeSort::Title => SessionSort::Title,
            },
            descending: request.descending,
            limit: LISTING_LIMIT,
        };
        let (listing, skipped_entries) = inner.with_index(|state| {
            state.refresh_bounded(false)?;
            let listing = state
                .index
                .sessions(&query)
                .map_err(|error| DesktopError::from_index(&error))?;
            Ok((listing, state.skipped_entries))
        })?;

        let mut groups: Vec<WorkspaceGroup> = Vec::new();
        let mut positions: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for session in &listing.sessions {
            let key = group_key(session.workspace.as_ref());
            let summary = summary_of(session, &running);
            if let Some(index) = positions.get(&key).copied()
                && let Some(group) = groups.get_mut(index)
            {
                group.sessions.push(summary);
                continue;
            }
            positions.insert(key.clone(), groups.len());
            groups.push(WorkspaceGroup {
                group_id: key.chars().take(GROUP_ID_CHARS).collect(),
                folder_display: folder_of(session),
                sessions: vec![summary],
            });
        }
        Ok(SessionTree {
            groups,
            indexed_sessions: listing.total,
            skipped_entries,
        })
    }

    /// Search transcript text.
    ///
    /// The index is refreshed first. The query is never search-engine syntax;
    /// an unusable query is `invalid_request`. The limit is clamped to 1
    /// through 200.
    ///
    /// # Errors
    ///
    /// `invalid_request`, `busy`, or `internal`.
    pub fn session_search(&self, request: SearchRequest) -> Result<SearchResult, DesktopError> {
        let inner = &self.inner;
        let running = inner.running_sessions()?;
        let limit = usize::try_from(request.limit)
            .unwrap_or(MAX_SEARCH_HITS)
            .clamp(1, MAX_SEARCH_HITS);
        let listing = inner.with_index(|state| {
            state.refresh_bounded(false)?;
            state
                .index
                .search(&request.query, archive_view(request.view), limit)
                .map_err(|error| DesktopError::from_index(&error))
        })?;
        let hits = listing
            .hits
            .iter()
            .map(|hit| SearchHit {
                session: summary_of(&hit.session, &running),
                folder_display: folder_of(&hit.session),
                seq: hit.seq,
                role: match hit.role {
                    SearchRole::User => SearchRoleDto::User,
                    SearchRole::Assistant => SearchRoleDto::Assistant,
                    SearchRole::Tool => SearchRoleDto::Tool,
                    SearchRole::Summary => SearchRoleDto::Summary,
                    SearchRole::Title => SearchRoleDto::Title,
                },
                snippet: hit.snippet.iter().map(snippet_part).collect(),
            })
            .collect();
        Ok(SearchResult {
            hits,
            truncated: listing.truncated,
        })
    }

    /// Read one page of a session's transcript without the writer lock.
    ///
    /// Returns entries with a sequence number above `after_seq` (all when it is
    /// `null`), at most `limit` (clamped to 1 through 200) and at most 1 MiB of
    /// text, always at least one. `next_after_seq` is the last returned
    /// sequence number when more entries remain.
    ///
    /// # Errors
    ///
    /// `not_found`; `busy` when a turn runs here for the session or another
    /// process holds the journal; `invalid_journal` when it fails validation.
    pub fn session_transcript(
        &self,
        request: TranscriptRequest,
    ) -> Result<TranscriptPage, DesktopError> {
        let inner = &self.inner;
        if inner.is_running(request.session_id)? {
            return Err(DesktopError::busy());
        }
        let path = inner.resolve_path(request.session_id)?;
        let inspection = SessionInspection::inspect(&path)
            .map_err(|error| DesktopError::from_session(&error))?;
        let limit = usize::try_from(request.limit)
            .unwrap_or(MAX_PAGE_ENTRIES)
            .clamp(1, MAX_PAGE_ENTRIES);
        let mut entries = Vec::new();
        let mut text_bytes = 0_usize;
        let mut more = false;
        for entry in inspection
            .entries
            .iter()
            .filter(|entry| request.after_seq.is_none_or(|after| entry.seq > after))
        {
            let kind = entry_kind(&entry.kind);
            let size = entry_text_bytes(&kind);
            let next_total = text_bytes.saturating_add(size);
            if !entries.is_empty() && (entries.len() >= limit || next_total > MAX_PAGE_TEXT_BYTES) {
                more = true;
                break;
            }
            let timestamp = format_time(entry.timestamp)
                .ok_or_else(|| DesktopError::internal("A recorded time could not be formatted."))?;
            text_bytes = next_total;
            entries.push(TranscriptEntry {
                seq: entry.seq,
                timestamp,
                kind,
            });
        }
        let next_after_seq = if more {
            entries.last().map(|entry| entry.seq)
        } else {
            None
        };
        Ok(TranscriptPage {
            session: summary_of_inspection(&inspection),
            folder_display: folder_of_path(&inspection.workspace),
            entries,
            next_after_seq,
            incomplete_tail: inspection.incomplete_tail,
        })
    }

    /// Archive an idle session. Archiving an archived session changes nothing.
    ///
    /// # Errors
    ///
    /// `turn_running`, `turn_open`, `busy`, `not_found`, or `invalid_journal`.
    pub fn session_archive(&self, request: SessionRequest) -> Result<SessionSummary, DesktopError> {
        self.lifecycle(request.session_id, |journal| {
            journal
                .archive()
                .map_err(|error| DesktopError::from_session(&error))
        })
    }

    /// Restore an archived session. Restoring an active session changes nothing.
    ///
    /// # Errors
    ///
    /// `turn_running`, `busy`, `not_found`, or `invalid_journal`.
    pub fn session_restore(&self, request: SessionRequest) -> Result<SessionSummary, DesktopError> {
        self.lifecycle(request.session_id, |journal| {
            journal
                .restore()
                .map_err(|error| DesktopError::from_session(&error))
        })
    }

    /// Close a session's unfinished turn with the failure code
    /// `operator_interrupted`. It starts no work and replays nothing.
    ///
    /// # Errors
    ///
    /// `invalid_request` when no turn is open, `turn_running`, `busy`,
    /// `not_found`, or `invalid_journal`.
    pub fn session_interrupt(
        &self,
        request: SessionRequest,
    ) -> Result<SessionSummary, DesktopError> {
        self.lifecycle(request.session_id, |journal| {
            let Some(turn) = journal.snapshot().active_turn else {
                return Err(DesktopError::invalid_request("No turn is open."));
            };
            journal
                .fail_turn(turn, INTERRUPT_CODE)
                .map_err(|error| DesktopError::from_session(&error))
        })
    }

    /// Open the journal, apply one change, release it, refresh the index for
    /// this session, and summarize it.
    fn lifecycle(
        &self,
        session_id: Uuid,
        change: impl FnOnce(&mut SessionJournal) -> Result<(), DesktopError>,
    ) -> Result<SessionSummary, DesktopError> {
        let inner = &self.inner;
        if inner.is_running(session_id)? {
            return Err(DesktopError::turn_running());
        }
        let path = inner.resolve_path(session_id)?;
        let mut journal =
            SessionJournal::open(&path).map_err(|error| DesktopError::from_session(&error))?;
        let changed = change(&mut journal);
        drop(journal);
        changed?;
        refreshed_summary(inner, session_id)
    }
}

/// Refresh one session in the index and summarize it.
pub(super) fn refreshed_summary(
    inner: &Inner,
    session_id: Uuid,
) -> Result<SessionSummary, DesktopError> {
    let running = inner.running_sessions()?;
    let session: IndexedSession = inner
        .with_index(|state| {
            state
                .index
                .refresh_session(session_id)
                .map_err(|error| DesktopError::from_index(&error))
        })?
        .ok_or_else(|| DesktopError::not_found("session"))?;
    Ok(summary_of(&session, &running))
}

const fn archive_view(view: TreeView) -> ArchiveView {
    match view {
        TreeView::Active => ArchiveView::Active,
        TreeView::Archived => ArchiveView::Archived,
    }
}

fn snippet_part(part: &IndexSnippetPart) -> SnippetPart {
    SnippetPart {
        text: part.text.clone(),
        matched: part.matched,
    }
}

/// The grouping key of a recorded workspace path: the hex SHA-256 over the
/// path's encoding tag and units (exact equality of paths, up to a hash
/// collision), or the literal `unknown` when the journal recorded none. The
/// group identifier is the first 16 characters of the key.
fn group_key(path: Option<&NativePath>) -> String {
    let Some(path) = path else {
        return String::from("unknown");
    };
    let mut hasher = Sha256::new();
    match path {
        NativePath::UnixBytes(bytes) => {
            hasher.update(b"unix_bytes\0");
            hasher.update(bytes);
        }
        NativePath::WindowsUtf16(units) => {
            hasher.update(b"windows_utf16\0");
            for unit in units {
                hasher.update(unit.to_le_bytes());
            }
        }
    }
    let digest = hasher.finalize();
    let mut text = String::with_capacity(64);
    for byte in digest.iter() {
        // Writing to a String cannot fail.
        if write!(text, "{byte:02x}").is_err() {
            break;
        }
    }
    text
}

/// Map one recorded event to the wire union.
fn entry_kind(kind: &TranscriptEntryKind) -> EntryKind {
    match kind {
        TranscriptEntryKind::Message {
            role,
            source,
            text,
            call_id,
        } => EntryKind::Message {
            role: (*role).into(),
            source: source_label(*source).to_owned(),
            text: text.clone(),
            call_id: call_id.clone(),
        },
        TranscriptEntryKind::TurnStarted { request_id } => EntryKind::TurnStarted {
            request_id: *request_id,
        },
        TranscriptEntryKind::TurnCompleted { request_id } => EntryKind::TurnCompleted {
            request_id: *request_id,
        },
        TranscriptEntryKind::TurnFailed { request_id, code } => EntryKind::TurnFailed {
            request_id: *request_id,
            code: Some(code.clone()),
        },
        TranscriptEntryKind::ToolRequested {
            request_id,
            call_id,
            tool,
            arguments,
        } => EntryKind::ToolRequested {
            request_id: *request_id,
            call_id: call_id.clone(),
            tool: tool.clone(),
            arguments: arguments.clone(),
        },
        TranscriptEntryKind::ToolCompleted {
            request_id,
            call_id,
            outcome,
            code,
            output_bytes,
            output_sha256: _,
        } => EntryKind::ToolCompleted {
            request_id: *request_id,
            call_id: call_id.clone(),
            outcome: (*outcome).into(),
            code: code.clone(),
            output_bytes: *output_bytes,
        },
        TranscriptEntryKind::RequestManifest {
            request_id,
            model,
            prompt_version,
            tools,
        } => EntryKind::RequestManifest {
            request_id: *request_id,
            model: model.clone(),
            prompt_version: prompt_version.clone(),
            tools: tools.clone(),
        },
        TranscriptEntryKind::ContextCompacted { record } => EntryKind::ContextCompacted {
            summary: record.summary.clone(),
            compacted_messages: record.compacted_messages,
            model: record.model.clone(),
        },
        TranscriptEntryKind::SessionArchived => EntryKind::SessionArchived,
        TranscriptEntryKind::SessionRestored => EntryKind::SessionRestored,
    }
}

/// Bytes of recorded text an entry adds to a page.
fn entry_text_bytes(kind: &EntryKind) -> usize {
    match kind {
        EntryKind::Message { text, .. } => text.len(),
        EntryKind::ToolRequested {
            arguments, tool, ..
        } => arguments.len().saturating_add(tool.len()),
        EntryKind::ContextCompacted { summary, .. } => summary.len(),
        EntryKind::RequestManifest { tools, .. } => tools
            .iter()
            .fold(0_usize, |total, tool| total.saturating_add(tool.len())),
        EntryKind::TurnStarted { .. }
        | EntryKind::TurnCompleted { .. }
        | EntryKind::TurnFailed { .. }
        | EntryKind::ToolCompleted { .. }
        | EntryKind::SessionArchived
        | EntryKind::SessionRestored => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_ids_are_sixteen_hex_characters_and_depend_on_the_encoding() {
        let unix = group_key(Some(&NativePath::UnixBytes(vec![b'a'])));
        assert_eq!(unix.len(), 64);
        let windows = group_key(Some(&NativePath::WindowsUtf16(vec![u16::from(b'a')])));
        assert_eq!(unix.chars().take(GROUP_ID_CHARS).count(), 16);
        assert!(unix.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(unix, windows);
        assert_eq!(group_key(None), "unknown");
    }

    #[test]
    fn paths_that_differ_only_by_the_verbatim_prefix_are_separate_groups() {
        let units = |text: &str| NativePath::WindowsUtf16(text.encode_utf16().collect());
        let verbatim = group_key(Some(&units(r"\\?\C:\proj")));
        let plain = group_key(Some(&units(r"C:\proj")));
        assert_ne!(verbatim, plain);
        assert_ne!(
            verbatim.chars().take(GROUP_ID_CHARS).collect::<String>(),
            plain.chars().take(GROUP_ID_CHARS).collect::<String>()
        );
    }
}
