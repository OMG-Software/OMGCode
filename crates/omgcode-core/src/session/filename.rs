//! Journal file naming shared by the writer, `SessionJournal::open`, the
//! read-only inspector, and discovery.
//!
//! A name is `omgcode-<HH>-<MM>-<SS>-<mmm>Z-<uuid>.jsonl`. The name is only a
//! locator. Replay still requires every event to carry the same session UUID.

use time::OffsetDateTime;
use uuid::Uuid;

use super::SessionError;

const NAME_PREFIX: &str = "omgcode-";
const NAME_SUFFIX: &str = ".jsonl";
const UUID_TEXT_BYTES: usize = 36;

/// Format the file name the writer creates for a new session at `at`.
pub(super) fn journal_file_name(at: OffsetDateTime, session_id: Uuid) -> String {
    format!(
        "{NAME_PREFIX}{:02}-{:02}-{:02}-{:03}Z-{session_id}{NAME_SUFFIX}",
        at.hour(),
        at.minute(),
        at.second(),
        at.millisecond()
    )
}

/// Extract the session UUID the way `SessionJournal::open` always has.
///
/// This accepts any UUID spelling that `Uuid::parse_str` accepts and does not
/// check the time fields. Replay then rejects any event whose UUID differs.
pub(super) fn lenient_session_id(filename: &str) -> Result<Uuid, SessionError> {
    let id_text = filename
        .strip_prefix(NAME_PREFIX)
        .and_then(|name| name.strip_suffix(NAME_SUFFIX))
        .and_then(|name| name.rsplit_once("Z-"))
        .map(|(_, id)| id)
        .ok_or(SessionError::InvalidJournal(
            "filename does not contain a session UUID",
        ))?;
    Uuid::parse_str(id_text)
        .map_err(|_| SessionError::InvalidJournal("filename has an invalid session UUID"))
}

/// Parse exactly the name the writer produces, or return `None`.
///
/// The time fields must be fixed-width ASCII digits within range, and the UUID
/// must be a lowercase hyphenated version 4 value.
pub(super) fn parse_strict(name: &str) -> Option<Uuid> {
    let body = name.strip_prefix(NAME_PREFIX)?.strip_suffix(NAME_SUFFIX)?;
    let (stamp, id_text) = body.split_once("Z-")?;
    let mut parts = stamp.split('-');
    let hour = fixed_digits(parts.next()?, 2)?;
    let minute = fixed_digits(parts.next()?, 2)?;
    let second = fixed_digits(parts.next()?, 2)?;
    let millisecond = fixed_digits(parts.next()?, 3)?;
    if parts.next().is_some() || hour > 23 || minute > 59 || second > 59 || millisecond > 999 {
        return None;
    }
    if id_text.len() != UUID_TEXT_BYTES {
        return None;
    }
    let id = Uuid::try_parse(id_text).ok()?;
    if id.get_version_num() != 4 || id.as_hyphenated().to_string() != id_text {
        return None;
    }
    Some(id)
}

/// Parse a field of exactly `width` ASCII digits.
fn fixed_digits(text: &str, width: usize) -> Option<u16> {
    if text.len() != width || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{journal_file_name, lenient_session_id, parse_strict};
    use time::OffsetDateTime;
    use uuid::Uuid;

    #[test]
    fn writer_names_round_trip_through_the_strict_parser() {
        let id = Uuid::new_v4();
        let name = journal_file_name(OffsetDateTime::now_utc(), id);
        assert_eq!(parse_strict(&name), Some(id));
        assert_eq!(lenient_session_id(&name).expect("lenient"), id);
    }

    #[test]
    fn strict_parser_rejects_every_deviation_from_the_writer_format() {
        let id = Uuid::new_v4();
        let good = format!("omgcode-01-02-03-004Z-{id}.jsonl");
        assert_eq!(parse_strict(&good), Some(id));
        let upper = format!(
            "omgcode-01-02-03-004Z-{}.jsonl",
            id.to_string().to_uppercase()
        );
        let simple = format!("omgcode-01-02-03-004Z-{}.jsonl", id.simple());
        let nil = "omgcode-01-02-03-004Z-00000000-0000-0000-0000-000000000000.jsonl";
        let bad = [
            upper,
            simple,
            nil.to_owned(),
            format!("omgcode-24-02-03-004Z-{id}.jsonl"),
            format!("omgcode-01-60-03-004Z-{id}.jsonl"),
            format!("omgcode-01-02-60-004Z-{id}.jsonl"),
            format!("omgcode-1-02-03-004Z-{id}.jsonl"),
            format!("omgcode-01-02-03-4Z-{id}.jsonl"),
            format!("omgcode-01-02-03-004-{id}.jsonl"),
            format!("omgcode-01-02-03-004-05Z-{id}.jsonl"),
            format!("omgcode-+1-02-03-004Z-{id}.jsonl"),
            format!("omgcode-01-02-03-004Z-{id}.json"),
            format!("omgcode-01-02-03-004Z-{id}.jsonl.bak"),
            format!("other-01-02-03-004Z-{id}.jsonl"),
        ];
        for name in bad {
            assert_eq!(parse_strict(&name), None, "{name}");
        }
    }
}
