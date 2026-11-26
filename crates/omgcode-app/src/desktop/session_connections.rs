//! The connection and model chosen for each session (ADR-0071).
//!
//! The file is `<data_root>/desktop-v1/session-connections.json`. It maps a
//! session identifier to the provider the operator chose for that session, the
//! model chosen for it, and the reasoning effort chosen for it (ADR-0102). It
//! holds no secret, no credential, and no permission: a profile name, the
//! endpoint the operator confirmed, a model name, and an effort name.
//!
//! # Trust
//!
//! Only this process writes the file, and only after a native confirmation of
//! the provider (`connection_select`) or for a model name or effort that
//! grants nothing (`session_model_select`, `session_effort_select`). The file
//! is **not tamper-evident**: a process
//! running as the same user can edit it, as it can edit the settings journal.
//! The edit cannot widen what a turn reaches, because every turn resolves the
//! session's profile through core again and refuses the turn with
//! `session_connection_stale` unless the profile is still selectable and its
//! endpoint still equals the recorded one. An edit can therefore only choose a
//! profile that is already recorded and selectable.
//!
//! # Storage contract
//!
//! - A read is bounded to [`MAX_FILE_BYTES`] and parsed strictly: unknown
//!   fields, a different version, or a value over its bound make the file
//!   unreadable, never partially applied. An unreadable file is reported and
//!   treated as empty, so it never blocks chat. The next recorded choice
//!   replaces it.
//! - At most [`MAX_SESSIONS`] sessions are recorded. A choice for a session
//!   that has no entry is refused with `session_choices` beyond that; an
//!   existing entry can always change.
//! - A write goes to a new temporary file in the same directory (created
//!   exclusively, mode 0600 on Unix), is flushed, and is renamed over the
//!   final name, as the chat preferences are written. One mutex serializes the
//!   read-modify-write cycles of this service, and an exclusive lock on the
//!   dedicated `session-connections.lock` file serializes them across desktop
//!   processes that share the data folder, waiting at most two seconds and then
//!   failing with `busy`. Both are held for file input and output only, never
//!   across a dialog or a network request. Reads take no lock: a write is an
//!   atomic rename, so a read sees the old file or the new one, never a mix.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use omgcode_core::control::{ControlStore, ProviderKind};
use omgcode_core::provider::{ModelEndpoint, ReasoningEffort};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::Inner;
use super::error::DesktopError;
use super::preferences::MAX_MODEL_BYTES;
use crate::{
    ConnectionSource, LocalConnection, ProfileCheckError, profile_selectability,
    resolve_local_connection, resolve_profile_connection,
};

/// The directory under the data root that holds desktop-only files.
const DIRECTORY: &str = "desktop-v1";
/// The file name.
const FILE_NAME: &str = "session-connections.json";
/// The only file format version this build reads and writes.
const VERSION: u32 = 1;
/// Most sessions that keep a recorded choice.
pub(super) const MAX_SESSIONS: usize = 4096;
/// Largest file read or written.
pub(super) const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Longest profile name or endpoint stored. Core bounds both far below this.
const MAX_TEXT_BYTES: usize = 1024;

/// The provider recorded for a session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ChosenConnection {
    /// No provider choice: the session follows the global selection made in
    /// `settings`, or the default endpoint. A model choice alone records this,
    /// so a model never pins a provider that the operator did not confirm.
    Follow {},
    /// The operator chose the default endpoint (the launch endpoint with
    /// `OMGCODE_LM_TOKEN`) for this session. `endpoint` is the one confirmed.
    Default {
        /// The launch default endpoint at the time of the choice.
        endpoint: String,
    },
    /// The operator chose a recorded profile for this session.
    Profile {
        /// The profile name.
        name: String,
        /// The endpoint shown in the confirmation. A turn is refused when the
        /// profile's endpoint is no longer this one.
        endpoint: String,
    },
}

/// One session's recorded choice.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionRecord {
    /// The provider.
    pub(super) connection: ChosenConnection,
    /// The model; empty when none is chosen.
    pub(super) model: String,
    /// The reasoning effort; `None` sends none (ADR-0102). Omitted from the
    /// file when none is chosen, so a file without efforts keeps the version
    /// 1 shape that earlier builds read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) reasoning_effort: Option<ReasoningEffort>,
}

/// The reasoning efforts a request to `endpoint` may carry (ADR-0102): every
/// documented value, or none when the endpoint takes no effort or is not a
/// valid endpoint. The server still decides which values its model accepts.
pub(super) fn reasoning_efforts(endpoint: &str) -> Vec<ReasoningEffort> {
    if ModelEndpoint::parse(endpoint).is_ok_and(|parsed| parsed.takes_reasoning_effort()) {
        ReasoningEffort::ALL.to_vec()
    } else {
        Vec::new()
    }
}

/// Every recorded choice, by session.
pub(super) type Records = BTreeMap<Uuid, SessionRecord>;

/// The file's on-disk shape.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RecordsFile {
    version: u32,
    sessions: Records,
}

/// Why the file could not be used. Every case reads as an empty file.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum LoadError {
    /// The file could not be opened or read, or is not a regular file.
    Io,
    /// The file is larger than [`MAX_FILE_BYTES`].
    TooLarge,
    /// The file is not valid JSON of the expected shape and version.
    Format,
    /// A value is over its bound.
    Invalid,
}

fn file_path(data_root: &Path) -> PathBuf {
    data_root.join(DIRECTORY).join(FILE_NAME)
}

/// Check every bound. The same check runs on load and on save.
fn check(records: &Records) -> Result<(), &'static str> {
    if records.len() > MAX_SESSIONS {
        return Err("Too many sessions have a recorded choice.");
    }
    for record in records.values() {
        let model = &record.model;
        if model.len() > MAX_MODEL_BYTES || model.chars().any(char::is_control) {
            return Err("A recorded model name is not valid.");
        }
        let texts: [&str; 2] = match &record.connection {
            ChosenConnection::Follow {} => ["", ""],
            ChosenConnection::Default { endpoint } => [endpoint, ""],
            ChosenConnection::Profile { name, endpoint } => [name, endpoint],
        };
        if texts
            .iter()
            .any(|text| text.len() > MAX_TEXT_BYTES || text.chars().any(char::is_control))
        {
            return Err("A recorded connection is not valid.");
        }
    }
    Ok(())
}

/// Read the recorded choices. `Ok(None)` means no file exists.
///
/// # Errors
///
/// A [`LoadError`] when the file exists but cannot be used.
pub(super) fn load(data_root: &Path) -> Result<Option<Records>, LoadError> {
    let path = file_path(data_root);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(LoadError::Io),
    };
    if !metadata.file_type().is_file() {
        return Err(LoadError::Io);
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(LoadError::TooLarge);
    }
    let file = File::open(&path).map_err(|_| LoadError::Io)?;
    let mut bytes = Vec::new();
    // One byte more than the bound shows a file that grew after the check.
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| LoadError::Io)?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_FILE_BYTES) {
        return Err(LoadError::TooLarge);
    }
    let parsed: RecordsFile = serde_json::from_slice(&bytes).map_err(|_| LoadError::Format)?;
    if parsed.version != VERSION {
        return Err(LoadError::Format);
    }
    check(&parsed.sessions).map_err(|_| LoadError::Invalid)?;
    Ok(Some(parsed.sessions))
}

/// The recorded choices, empty when there is no file or it cannot be used,
/// with whether it could not be used.
pub(super) fn load_or_empty(data_root: &Path) -> (Records, bool) {
    match load(data_root) {
        Ok(Some(records)) => (records, false),
        Ok(None) => (Records::new(), false),
        Err(_) => (Records::new(), true),
    }
}

/// Replace the file with `records`.
///
/// # Errors
///
/// `session_choices` when a bound is exceeded or the file cannot be written. A
/// failed save leaves the previous file in place.
pub(super) fn save(data_root: &Path, records: &Records) -> Result<(), DesktopError> {
    check(records).map_err(|_| DesktopError::session_choices())?;
    let bytes = serde_json::to_vec_pretty(&RecordsFile {
        version: VERSION,
        sessions: records.clone(),
    })
    .map_err(|_| DesktopError::session_choices())?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_FILE_BYTES) {
        return Err(DesktopError::session_choices());
    }
    let directory = data_root.join(DIRECTORY);
    create_directory(&directory).map_err(|_| DesktopError::session_choices())?;
    replace(&directory, &directory.join(temporary_name()), &bytes)
        .map_err(|_| DesktopError::session_choices())?;
    sync_directory(&directory).map_err(|_| DesktopError::session_choices())
}

/// Write `bytes` through the new file `temporary` and rename it over the
/// final file in `directory`. The temporary file is removed on failure only
/// when this call created it.
fn replace(directory: &Path, temporary: &Path, bytes: &[u8]) -> io::Result<()> {
    let file = create_new(temporary)?;
    let written =
        write_synced(file, bytes).and_then(|()| fs::rename(temporary, directory.join(FILE_NAME)));
    if written.is_err() {
        // Best-effort cleanup of the file this call created; it holds no
        // secret. A failed removal leaves a stray file that no load reads.
        let _ = fs::remove_file(temporary);
    }
    written
}

/// Distinguishes saves within this process, even within one clock tick.
static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temporary_name() -> String {
    let sequence = SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("{FILE_NAME}.{}-{sequence}-{nanos}.tmp", std::process::id())
}

fn create_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

fn create_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn write_synced(mut file: File, bytes: &[u8]) -> io::Result<()> {
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

/// Windows has no directory handle for this in the standard library. The
/// rename replaces the file atomically; durability after power loss is not
/// claimed.
#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// How long a write waits for another process that holds the file lock.
const LOCK_WAIT: Duration = Duration::from_secs(2);
/// The pause between lock attempts.
const LOCK_POLL: Duration = Duration::from_millis(20);
/// The lock file next to the data file. It holds nothing; only its lock matters.
const LOCK_NAME: &str = "session-connections.lock";

/// One read-modify-write cycle of the file (see `update_session_record`).
fn update_record(
    data_root: &Path,
    session_id: Uuid,
    change: impl FnOnce(Option<&SessionRecord>) -> SessionRecord,
    wait: Duration,
) -> Result<(), DesktopError> {
    let _lock = lock_exclusive(data_root, wait)?;
    let (mut records, _) = load_or_empty(data_root);
    if !records.contains_key(&session_id) && records.len() >= MAX_SESSIONS {
        return Err(DesktopError::session_choices());
    }
    let updated = change(records.get(&session_id));
    records.insert(session_id, updated);
    save(data_root, &records)
}

/// Hold the cross-process exclusive lock until the returned handle drops.
///
/// Two desktop processes can share one data folder, and the in-process mutex
/// cannot order them. The lock is on a dedicated file, never on the data file,
/// because the data file is replaced by rename. The wait is bounded so a
/// stuck holder cannot freeze a command.
///
/// # Errors
///
/// `busy` when another holder keeps the lock for `wait`; `session_choices`
/// when the lock file cannot be opened or locked.
fn lock_exclusive(data_root: &Path, wait: Duration) -> Result<File, DesktopError> {
    let directory = data_root.join(DIRECTORY);
    create_directory(&directory).map_err(|_| DesktopError::session_choices())?;
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(directory.join(LOCK_NAME))
        .map_err(|_| DesktopError::session_choices())?;
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) => {
                if started.elapsed() >= wait {
                    return Err(DesktopError::busy());
                }
                std::thread::sleep(LOCK_POLL);
            }
            Err(TryLockError::Error(_)) => return Err(DesktopError::session_choices()),
        }
    }
}

/// Check a model id the way `chat_send` does: at most 256 bytes and no control
/// character. An empty id clears the session's model.
pub(super) fn check_model(model: &str) -> Result<(), DesktopError> {
    if model.len() > MAX_MODEL_BYTES {
        return Err(DesktopError::invalid_request("The model name is too long."));
    }
    if model.chars().any(char::is_control) {
        return Err(DesktopError::invalid_request(
            "The model name contains a control character.",
        ));
    }
    Ok(())
}

/// A session's resolved connection for one request.
///
/// `Debug` prints the endpoint and the token's fixed redaction.
#[derive(Debug)]
pub(super) struct SessionConnection {
    /// The endpoint and optional token.
    pub(super) connection: LocalConnection,
    /// The profile kind, or `None` for the launch or default endpoint.
    pub(super) kind: Option<ProviderKind>,
}

impl Inner {
    /// The recorded choices and whether the file was unreadable.
    pub(super) fn session_records(&self) -> (Records, bool) {
        load_or_empty(&self.data_root)
    }

    /// Change one session's record under the service's write lock.
    ///
    /// `change` receives the existing record, or `None`, and returns the new
    /// one. A new entry beyond [`MAX_SESSIONS`] is refused. An unreadable file
    /// is replaced.
    ///
    /// # Errors
    ///
    /// `session_choices` for the bound or a failed write, `internal` for a
    /// poisoned lock.
    pub(super) fn update_session_record(
        &self,
        session_id: Uuid,
        change: impl FnOnce(Option<&SessionRecord>) -> SessionRecord,
    ) -> Result<(), DesktopError> {
        let _guard = self
            .session_choices
            .lock()
            .map_err(|_| DesktopError::poisoned())?;
        update_record(&self.data_root, session_id, change, LOCK_WAIT)
    }

    /// The connection for one session's next request (ADR-0071 decision 7).
    ///
    /// Order: a launch argument fixes the connection for every session; else
    /// the session's recorded provider, re-resolved through core now; else the
    /// global selection of ADR-0064. Nothing is cached.
    ///
    /// # Errors
    ///
    /// `session_connection_stale` when the session's recorded profile is gone,
    /// is not selectable, or has another endpoint than the one recorded (or
    /// the default endpoint changed since a default choice); otherwise the
    /// application layer's `credential`, `configuration`, and `provider`
    /// codes.
    pub(super) fn resolve_session(
        &self,
        session_id: Uuid,
    ) -> Result<SessionConnection, DesktopError> {
        if self.token.is_fixed() {
            let connection = self
                .token
                .resolve(&self.data_root, &self.endpoint)
                .map_err(|error| DesktopError::from_app(&error))?;
            return Ok(SessionConnection {
                connection,
                kind: None,
            });
        }
        let (records, _) = self.session_records();
        match records.get(&session_id).map(|record| &record.connection) {
            None | Some(ChosenConnection::Follow {}) => self.resolve_global(),
            Some(ChosenConnection::Default { endpoint }) => {
                if *endpoint != self.endpoint {
                    return Err(DesktopError::session_connection_stale());
                }
                let connection = resolve_local_connection(
                    &self.data_root,
                    &ConnectionSource::Environment {
                        endpoint: self.endpoint.clone(),
                    },
                )
                .map_err(|error| DesktopError::from_app(&error))?;
                Ok(SessionConnection {
                    connection,
                    kind: None,
                })
            }
            Some(ChosenConnection::Profile { name, endpoint }) => {
                self.resolve_recorded_profile(name, endpoint)
            }
        }
    }

    /// The global selection (or the default endpoint), as before ADR-0071.
    fn resolve_global(&self) -> Result<SessionConnection, DesktopError> {
        // One read gives the endpoint, the token, and the kind, so they always
        // describe the same profile even if the selection changes meanwhile.
        let (connection, kind) = self
            .token
            .resolve_with_kind(&self.data_root, &self.endpoint)
            .map_err(|error| DesktopError::from_app(&error))?;
        Ok(SessionConnection { connection, kind })
    }

    /// Re-resolve a session's recorded profile and compare its endpoint.
    fn resolve_recorded_profile(
        &self,
        name: &str,
        recorded: &str,
    ) -> Result<SessionConnection, DesktopError> {
        match profile_selectability(&self.data_root, name) {
            Ok(()) => {}
            Err(ProfileCheckError::NotSelectable(_)) => {
                return Err(DesktopError::session_connection_stale());
            }
            Err(ProfileCheckError::Unavailable(error)) => {
                return Err(DesktopError::from_app(&error));
            }
        }
        let resolved = resolve_profile_connection(&self.data_root, name)
            .map_err(|error| DesktopError::from_app(&error))?;
        if resolved.endpoint.as_str() != recorded {
            return Err(DesktopError::session_connection_stale());
        }
        Ok(SessionConnection {
            connection: LocalConnection {
                endpoint: resolved.endpoint.as_str().to_owned(),
                token: resolved.token,
                plan: resolved.plan,
            },
            kind: Some(resolved.kind),
        })
    }
}

/// The endpoint text a session record keeps for a profile: the shared rule's
/// normal form, so it equals what a later resolution returns.
pub(super) fn recorded_endpoint(store: &ControlStore, name: &str) -> Option<String> {
    let status = store.provider_profile(name)?;
    let endpoint = status.profile().endpoint()?;
    ModelEndpoint::parse(endpoint.as_str())
        .ok()
        .map(|parsed| parsed.as_str().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "omgcode-session-connections-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("scratch directory");
        path
    }

    fn profile_record(model: &str) -> SessionRecord {
        SessionRecord {
            connection: ChosenConnection::Profile {
                name: "cloud".to_owned(),
                endpoint: "https://ollama.com/v1".to_owned(),
            },
            model: model.to_owned(),
            reasoning_effort: None,
        }
    }

    #[test]
    fn records_round_trip_through_the_file() {
        let root = scratch("round-trip");
        let mut records = Records::new();
        records.insert(Uuid::from_u128(1), profile_record("glm-5.1:cloud"));
        records.insert(
            Uuid::from_u128(2),
            SessionRecord {
                connection: ChosenConnection::Default {
                    endpoint: "http://127.0.0.1:1234/v1".to_owned(),
                },
                model: String::new(),
                reasoning_effort: None,
            },
        );
        records.insert(
            Uuid::from_u128(3),
            SessionRecord {
                connection: ChosenConnection::Follow {},
                model: "qwen".to_owned(),
                reasoning_effort: Some(ReasoningEffort::XHigh),
            },
        );
        assert_eq!(load(&root), Ok(None));
        save(&root, &records).expect("save");
        assert_eq!(load(&root), Ok(Some(records)));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn an_effort_is_omitted_when_none_and_a_file_without_one_still_loads() {
        // ADR-0102: a record without an effort keeps the shape earlier builds
        // wrote and read, and a file they wrote loads with no effort.
        let root = scratch("effort-shape");
        let mut records = Records::new();
        records.insert(Uuid::from_u128(1), profile_record("m"));
        save(&root, &records).expect("save");
        let text = fs::read_to_string(file_path(&root)).expect("read");
        assert!(!text.contains("reasoning_effort"), "{text}");
        fs::write(
            file_path(&root),
            r#"{"version":1,"sessions":{"00000000-0000-0000-0000-000000000002":{"connection":{"kind":"follow"},"model":"qwen"}}}"#,
        )
        .expect("write an earlier build's file");
        let loaded = load(&root).expect("load").expect("records");
        assert_eq!(
            loaded
                .get(&Uuid::from_u128(2))
                .map(|record| record.reasoning_effort),
            Some(None)
        );
        // A recorded effort is its wire name; any other name makes the file
        // unreadable, never partly applied.
        let mut records = Records::new();
        records.insert(
            Uuid::from_u128(3),
            SessionRecord {
                reasoning_effort: Some(ReasoningEffort::Max),
                ..profile_record("m")
            },
        );
        save(&root, &records).expect("save");
        let text = fs::read_to_string(file_path(&root)).expect("read");
        assert!(text.contains(r#""reasoning_effort": "max""#), "{text}");
        fs::write(file_path(&root), text.replace(r#""max""#, r#""maximum""#))
            .expect("write an unknown effort");
        assert_eq!(load(&root), Err(LoadError::Format));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn only_endpoints_that_take_an_effort_offer_one() {
        assert_eq!(
            reasoning_efforts("http://127.0.0.1:1234/v1"),
            ReasoningEffort::ALL.to_vec()
        );
        assert_eq!(
            reasoning_efforts("https://api.openai.com/v1"),
            ReasoningEffort::ALL.to_vec()
        );
        // A typed `openai_compatible` address takes one, unless its text is a
        // named provider's fixed endpoint: that rule wins (ADR-0102).
        assert_eq!(
            reasoning_efforts("https://llm.example.com/v1"),
            ReasoningEffort::ALL.to_vec()
        );
        for endpoint in [
            "https://api.anthropic.com/v1",
            "https://api.anthropic.com/v1/",
            "https://api.deepseek.com",
            "not an endpoint",
            "",
        ] {
            assert!(reasoning_efforts(endpoint).is_empty(), "{endpoint}");
        }
    }

    #[test]
    fn a_damaged_file_reads_as_empty_and_is_reported() {
        let root = scratch("damaged");
        let directory = root.join(DIRECTORY);
        fs::create_dir_all(&directory).expect("directory");
        let path = directory.join(FILE_NAME);
        for text in [
            "not json",
            r#"{"version":2,"sessions":{}}"#,
            r#"{"version":1,"sessions":{},"extra":1}"#,
            r#"{"version":1,"sessions":{"00000000-0000-0000-0000-000000000001":{"connection":{"kind":"follow","token":"x"},"model":""}}}"#,
            r#"{"version":1,"sessions":{"00000000-0000-0000-0000-000000000001":{"connection":{"kind":"follow"},"model":"a\u0000b"}}}"#,
        ] {
            fs::write(&path, text).expect("write");
            assert!(load(&root).is_err(), "{text}");
            let (records, damaged) = load_or_empty(&root);
            assert!(records.is_empty() && damaged, "{text}");
        }
        // A directory in place of the file is refused, never followed.
        fs::remove_file(&path).expect("remove");
        fs::create_dir(&path).expect("directory at the name");
        assert_eq!(load(&root), Err(LoadError::Io));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn an_oversized_file_is_refused() {
        let root = scratch("oversized");
        let directory = root.join(DIRECTORY);
        fs::create_dir_all(&directory).expect("directory");
        let padding = " ".repeat(usize::try_from(MAX_FILE_BYTES).expect("size") + 1);
        fs::write(
            directory.join(FILE_NAME),
            format!(r#"{{"version":1,"sessions":{{}}}}{padding}"#),
        )
        .expect("write");
        assert_eq!(load(&root), Err(LoadError::TooLarge));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn the_session_bound_and_value_bounds_are_enforced_on_save() {
        let root = scratch("bounds");
        let mut records = Records::new();
        for index in 0..=MAX_SESSIONS {
            records.insert(
                Uuid::from_u128(u128::try_from(index).expect("index")),
                profile_record(""),
            );
        }
        assert_eq!(
            save(&root, &records).expect_err("over the bound").code(),
            "session_choices"
        );
        assert!(!file_path(&root).exists());
        let mut records = Records::new();
        records.insert(Uuid::from_u128(1), profile_record(&"m".repeat(257)));
        assert!(save(&root, &records).is_err());
        let mut records = Records::new();
        records.insert(Uuid::from_u128(1), profile_record("bad\nmodel"));
        assert!(save(&root, &records).is_err());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn a_model_name_follows_the_chat_send_rule() {
        assert!(check_model("").is_ok());
        assert!(check_model("glm-5.1:cloud").is_ok());
        assert!(check_model(&"m".repeat(256)).is_ok());
        assert_eq!(
            check_model(&"m".repeat(257)).expect_err("long").code(),
            "invalid_request"
        );
        assert_eq!(
            check_model("a\u{7}b").expect_err("control").code(),
            "invalid_request"
        );
    }

    #[test]
    fn writers_without_a_shared_mutex_keep_each_others_sessions() {
        // Two independent writers stand for two desktop processes: they share
        // no mutex, only the data folder.
        let root = scratch("two-writers");
        let writers: Vec<_> = (0..2u128)
            .map(|writer| {
                let root = root.clone();
                std::thread::spawn(move || {
                    for step in 0..15u128 {
                        let id = Uuid::from_u128(writer * 100 + step);
                        update_record(&root, id, |_| profile_record("m"), Duration::from_secs(30))
                            .expect("update");
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().expect("writer");
        }
        let (records, damaged) = load_or_empty(&root);
        assert!(!damaged);
        assert_eq!(records.len(), 30);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn a_write_fails_with_busy_while_another_handle_holds_the_lock() {
        let root = scratch("held-lock");
        let held = lock_exclusive(&root, Duration::from_millis(50)).expect("first lock");
        let started = Instant::now();
        let error = update_record(
            &root,
            Uuid::from_u128(1),
            |_| profile_record("m"),
            Duration::from_millis(100),
        )
        .expect_err("busy");
        assert_eq!(error.code(), "busy");
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the wait is bounded"
        );
        assert!(load(&root).expect("load").is_none(), "nothing was written");
        // Once the holder lets go, the same write succeeds.
        drop(held);
        update_record(
            &root,
            Uuid::from_u128(1),
            |_| profile_record("m"),
            Duration::from_millis(100),
        )
        .expect("after release");
        assert_eq!(load_or_empty(&root).0.len(), 1);
        fs::remove_dir_all(root).expect("cleanup");
    }
}
