//! Read-only review of the recorded request an approval is bound to (ADR-0046).
//!
//! An approval record stores a digest and a short summary, never the tool
//! arguments. A reviewer that shows arguments next to an Approve control must
//! show only arguments that reproduce that digest. This module is the one
//! place that does the matching, using the same argument parser, request
//! construction, edit preparation, and digest functions as the tool dispatcher.
//! It grants nothing: it never requests, decides, or consumes an approval.

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;
use serde_json::json;
use thiserror::Error;

use super::{
    PreparedEdit, ToolArguments, ToolCall, ToolName, command_digest, command_run_request,
    edit_digest, edit_target, is_hidden_format, prepare_edit, read_edit_source,
};
use crate::control::{ApprovalOperation, ApprovalStatus, ControlError, ControlStore};
use crate::file_audit::FileAuditError;
use crate::files::FileError;
use crate::replacement::CheckpointError;

/// Most distinct edit requests evaluated for one review.
///
/// Each evaluation reads a workspace file through the audited broker and
/// writes one audit record, so the work and the audit growth are bounded.
const MAX_REVIEW_EDIT_CANDIDATES: usize = 8;

/// One recorded model tool request offered as a candidate for review.
///
/// Both fields are untrusted model data, usually copied from the session
/// journal's recorded tool requests. Offering a candidate grants nothing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReviewCandidate<'a> {
    /// The requested tool name.
    pub tool: &'a str,
    /// The exact arguments JSON string that was recorded.
    pub arguments: &'a str,
}

/// What core found for one approval record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApprovalReview {
    /// A recorded request reproduces the approval's digest under the current
    /// settings, so a retry of it would consume the approval.
    Exact(ReviewedRequest),
    /// Candidates of the right tool exist, or the settings revision changed,
    /// but nothing reproduces the digest. The approval cannot be consumed by
    /// any request core can show.
    Stale(StaleReason),
    /// No recorded request of the right tool exists, the operation has no
    /// reviewable tool, or the settings store or workspace is absent.
    NotFound,
}

/// Why a review is stale.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleReason {
    /// The settings revision changed after the approval was requested. The
    /// record can never be consumed.
    SettingsChanged,
    /// No candidate reproduces the digest, for example because the target
    /// file changed after the request.
    NoMatchingRequest,
}

/// The request an approval is bound to, rendered for a person to read.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReviewedRequest {
    /// The wire name of the tool that makes the request.
    pub tool: String,
    /// Labelled values, each already passed through [`display_safe`].
    pub fields: Vec<ReviewField>,
}

/// One labelled, display-safe value of a reviewed request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReviewField {
    /// A fixed English label.
    pub label: &'static str,
    /// Display-safe text. It may contain `\n` only when `multiline` is true.
    pub value: String,
    /// Whether the value is meant to be shown as several lines.
    pub multiline: bool,
}

/// A review that could not be completed. Messages hold no file contents,
/// request text, or absolute paths; the codes are fixed strings.
#[derive(Debug, Error)]
pub enum ReviewError {
    /// Another process holds the settings store or the file-operation store.
    /// Retry later; nothing was changed.
    #[error("the settings or file-operation store is in use by another process")]
    Busy,
    /// The settings store could not be read.
    #[error("the settings store could not be read ({0})")]
    Control(&'static str),
    /// A review file read failed in a way that is not a plain non-match.
    #[error("the review file read could not complete ({0})")]
    Files(&'static str),
}

/// Find the recorded request that an approval record is bound to.
///
/// `candidates` are the recorded tool requests of the approval's session in
/// journal order, oldest first. They are untrusted; the result depends only on
/// whether a candidate reproduces `approval.request_sha256`, so a candidate
/// from another session or workspace can match only by reproducing the digest
/// of the approval's own session and workspace.
///
/// 1. Only [`ApprovalOperation::Command`] and [`ApprovalOperation::FileWrite`]
///    are reviewable. Others give [`ApprovalReview::NotFound`].
/// 2. The existing settings store is opened once, without retries. A missing
///    store or unregistered workspace gives `NotFound`, a busy store gives
///    [`ReviewError::Busy`], and a settings revision that differs from the
///    approval's gives `Stale(SettingsChanged)`.
/// 3. Only candidates whose tool is the operation's tool are considered; with
///    none, the result is `NotFound`.
/// 4. They are tried newest first, each parsed by [`ToolCall::parse`]. A
///    candidate that fails to parse is skipped. A command candidate is built
///    into a run request by the dispatcher's own constructor. An edit candidate
///    is read live through the audited broker under the live `read`
///    requirement and prepared by the dispatcher's own edit preparation. At
///    most eight distinct edit candidates (distinct by exact arguments) are
///    evaluated, newest first. A candidate whose preparation fails for a
///    recoverable reason (missing file, no match, ambiguous match, too large,
///    read denied) is not a match.
/// 5. If none reproduces the digest, the result is `Stale(NoMatchingRequest)`.
///
/// The fields of an `Exact` result come from the request that reproduced the
/// digest, not from the newest candidate and not from re-reading the JSON.
///
/// # Side effects
///
/// None on approvals. The function never requests, decides, or consumes a
/// record, never applies an edit, never creates a checkpoint, and never starts
/// a process. For an edit candidate it reads one workspace file through the
/// broker, which appends one audit record per read; nothing else is written.
///
/// # Locking
///
/// The settings store is opened once, the same way the dispatcher opens it. A
/// command review releases it before comparing candidates. An edit review
/// holds it while it reads, as dispatch does, because the broker needs the
/// writer; the broker takes its own subordinate stores one at a time under the
/// documented order (settings, then file audit). The settings store is never
/// held across anything but those bounded reads and is dropped before return.
/// A store held elsewhere gives [`ReviewError::Busy`]; the function does not
/// retry.
///
/// # Not validated
///
/// Whether `approval` is still pending, approved, or unexpired. The caller
/// shows state separately. A consumed or expired record can still be `Exact`.
/// An `Exact` result is a snapshot: a file can change before an approval is
/// decided, and core still checks the binding when the call is consumed.
pub fn review_approval(
    data_directory: &Path,
    approval: &ApprovalStatus,
    candidates: &[ReviewCandidate<'_>],
) -> Result<ApprovalReview, ReviewError> {
    let tool = match approval.operation {
        ApprovalOperation::Command => ToolName::Command,
        ApprovalOperation::FileWrite => ToolName::EditFile,
        ApprovalOperation::GitPush | ApprovalOperation::Network => {
            return Ok(ApprovalReview::NotFound);
        }
    };
    let control = match ControlStore::open_existing(data_directory) {
        Ok(Some(control)) => control,
        Ok(None) => return Ok(ApprovalReview::NotFound),
        Err(ControlError::Busy) => return Err(ReviewError::Busy),
        Err(error) => return Err(ReviewError::Control(control_code(&error))),
    };
    if approval.settings_revision != control.revision() {
        return Ok(ApprovalReview::Stale(StaleReason::SettingsChanged));
    }
    if control.workspace(approval.workspace_id).is_none() {
        return Ok(ApprovalReview::NotFound);
    }
    let mut relevant = candidates
        .iter()
        .rev()
        .filter(|candidate| candidate.tool == tool.as_str())
        .peekable();
    if relevant.peek().is_none() {
        return Ok(ApprovalReview::NotFound);
    }
    let found = match tool {
        ToolName::Command => {
            drop(control);
            review_command(approval, relevant)
        }
        _ => review_edit(control, approval, relevant)?,
    };
    Ok(found.map_or(
        ApprovalReview::Stale(StaleReason::NoMatchingRequest),
        ApprovalReview::Exact,
    ))
}

/// Parse a candidate with the dispatcher's strict parser.
///
/// The parser takes a whole provider call, so the candidate is wrapped in one
/// with a fixed identifier. Everything else it checks applies unchanged.
fn parse_candidate(candidate: &ReviewCandidate<'_>, tool: ToolName) -> Option<ToolCall> {
    let value = json!({
        "id": "review",
        "type": "function",
        "function": {"name": candidate.tool, "arguments": candidate.arguments},
    });
    ToolCall::parse(&value)
        .ok()
        .filter(|call| call.name == tool)
}

/// Find the newest command candidate that reproduces the approval's digest.
fn review_command<'a>(
    approval: &ApprovalStatus,
    candidates: impl Iterator<Item = &'a ReviewCandidate<'a>>,
) -> Option<ReviewedRequest> {
    for candidate in candidates {
        let Some(call) = parse_candidate(candidate, ToolName::Command) else {
            continue;
        };
        let ToolArguments::Command {
            profile,
            program,
            arguments,
            cwd,
        } = &call.arguments
        else {
            continue;
        };
        let request = command_run_request(
            approval.workspace_id,
            profile,
            program,
            arguments,
            cwd.as_ref(),
        );
        if command_digest(approval.session_id, &request) == approval.request_sha256 {
            return Some(ReviewedRequest {
                tool: ToolName::Command.as_str().to_owned(),
                fields: command_fields(&request),
            });
        }
    }
    None
}

/// Find the newest edit candidate that reproduces the approval's digest.
///
/// Evaluates at most [`MAX_REVIEW_EDIT_CANDIDATES`] distinct candidates.
fn review_edit<'a>(
    mut control: ControlStore,
    approval: &ApprovalStatus,
    candidates: impl Iterator<Item = &'a ReviewCandidate<'a>>,
) -> Result<Option<ReviewedRequest>, ReviewError> {
    let binding = control
        .bind_session(approval.session_id, approval.workspace_id)
        .map_err(|error| match error {
            ControlError::Busy => ReviewError::Busy,
            other => ReviewError::Control(control_code(&other)),
        })?;
    let mut seen = HashSet::new();
    let mut evaluated = 0usize;
    for candidate in candidates {
        let Some(call) = parse_candidate(candidate, ToolName::EditFile) else {
            continue;
        };
        let ToolArguments::EditFile {
            path,
            old_text,
            new_text,
        } = &call.arguments
        else {
            continue;
        };
        if !seen.insert(candidate.arguments) {
            continue;
        }
        if evaluated == MAX_REVIEW_EDIT_CANDIDATES {
            break;
        }
        evaluated += 1;
        let target = edit_target(approval.session_id, approval.workspace_id, path);
        let current = match read_edit_source(&mut control, &binding, &target) {
            Ok(bytes) => bytes,
            Err(error) => {
                classify_review_read(error)?;
                continue;
            }
        };
        let Ok(prepared) = prepare_edit(path, old_text, new_text, &current) else {
            continue;
        };
        let digest = edit_digest(
            approval.session_id,
            approval.workspace_id,
            &prepared.normalized,
            &prepared.current_artifact,
            &prepared.proposed_artifact,
        );
        if digest == approval.request_sha256 {
            return Ok(Some(ReviewedRequest {
                tool: ToolName::EditFile.as_str().to_owned(),
                fields: edit_fields(&prepared, old_text, new_text),
            }));
        }
    }
    Ok(None)
}

/// Decide whether a failed review read is a plain non-match (`Ok`) or leaves
/// the review unable to continue (`Err`).
///
/// Permission denial, a missing or unsuitable file, and a file over the edit
/// bound mean this candidate cannot be what the approval is bound to. A busy
/// store, an unresolved file operation, or an audit failure mean core could
/// not look, so claiming the approval is stale would be wrong.
fn classify_review_read(error: FileError) -> Result<(), ReviewError> {
    match error {
        FileError::Denied(_)
        | FileError::Native(_)
        | FileError::InvalidPath(_)
        | FileError::Invalid(_) => Ok(()),
        FileError::Control(ControlError::Busy) | FileError::Checkpoint(CheckpointError::Busy) => {
            Err(ReviewError::Busy)
        }
        FileError::Control(other) => Err(ReviewError::Control(control_code(&other))),
        FileError::Audit(source) => {
            if matches!(
                source.downcast_ref::<FileAuditError>(),
                Some(FileAuditError::Busy)
            ) {
                Err(ReviewError::Busy)
            } else {
                Err(ReviewError::Files("audit_failed"))
            }
        }
        FileError::Pending { .. } => Err(ReviewError::Files("operation_pending")),
        _ => Err(ReviewError::Files("read_failed")),
    }
}

/// A fixed, content-free code for a settings-store failure.
const fn control_code(error: &ControlError) -> &'static str {
    match error {
        ControlError::Io(_) => "io",
        ControlError::Invalid(_) => "invalid",
        ControlError::Busy => "busy",
        ControlError::UnknownWorkspace => "unknown_workspace",
        ControlError::DirectoryChanged => "directory_changed",
        _ => "unavailable",
    }
}

/// Command fields, taken from the constructed run request so the display
/// cannot diverge from what would run.
fn command_fields(request: &crate::run::RunRequest) -> Vec<ReviewField> {
    let arguments = if request.arguments.is_empty() {
        String::from("(none)")
    } else {
        request
            .arguments
            .iter()
            .enumerate()
            .map(|(index, argument)| {
                format!(
                    "{}: {}",
                    index + 1,
                    display_safe_line(&argument.to_string_lossy())
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let directory = request.working_directory.as_ref().map_or_else(
        || String::from("(workspace root)"),
        |directory| display_safe_line(&directory.to_string_lossy()),
    );
    let limits = &request.limits;
    let output = if limits.max_stdout_bytes == limits.max_stderr_bytes {
        format!("{} bytes per stream", limits.max_stdout_bytes)
    } else {
        format!(
            "stdout {} bytes, stderr {} bytes",
            limits.max_stdout_bytes, limits.max_stderr_bytes
        )
    };
    vec![
        single("Profile", display_safe_line(&request.profile)),
        single(
            "Program",
            display_safe_line(&request.program.to_string_lossy()),
        ),
        ReviewField {
            label: "Arguments",
            value: arguments,
            multiline: true,
        },
        single("Working directory", directory),
        single("Network", on_off(request.network)),
        single("Host services", on_off(request.host_services)),
        single(
            "Time limit",
            format!("{} seconds", limits.deadline.as_secs()),
        ),
        single("Output limit", output),
    ]
}

/// Edit fields. The path, sizes, and digests come from the prepared edit and
/// the old and new text from the parsed arguments that produced it.
fn edit_fields(prepared: &PreparedEdit, old_text: &str, new_text: &str) -> Vec<ReviewField> {
    vec![
        single("File", display_safe_line(&prepared.normalized)),
        ReviewField {
            label: "Old text",
            value: display_safe(old_text),
            multiline: true,
        },
        ReviewField {
            label: "New text",
            value: display_safe(new_text),
            multiline: true,
        },
        single(
            "Size",
            format!(
                "{} -> {} bytes",
                prepared.current_artifact.bytes, prepared.proposed_artifact.bytes
            ),
        ),
        single("Current SHA-256", prepared.current_artifact.sha256.clone()),
        single(
            "Proposed SHA-256",
            prepared.proposed_artifact.sha256.clone(),
        ),
    ]
}

fn single(label: &'static str, value: String) -> ReviewField {
    ReviewField {
        label,
        value,
        multiline: false,
    }
}

fn on_off(enabled: bool) -> String {
    String::from(if enabled { "on" } else { "off" })
}

/// Replace characters that can hide or reorder displayed text with visible
/// escapes, keeping line feeds so multi-line text stays readable.
///
/// Every `char::is_control` character except `\n` (so `\r`, `\t`, NUL, and
/// C1 controls) and every invisible or bidirectional formatting character that
/// the approval summaries already replace becomes `\u{XXXX}`: lowercase hex, at
/// least four digits. The ranges are listed on `is_hidden_format`; they include
/// bidirectional and zero-width marks, soft hyphen, fillers, variation
/// selectors, interlinear anchors, and tag characters. Variation selectors are
/// shown on purpose, so an emoji followed by U+FE0F displays the escape.
///
/// Limits: this does not detect look-alike letters (homoglyphs) or mixed
/// scripts, and it leaves ordinary trailing spaces as typed.
///
/// A backslash that begins the two characters `u{` would let text forge an
/// escape, so it becomes `\u{5c}`. A backslash directly before a character that
/// is itself escaped is escaped the same way, so a forged and a real escape
/// stay distinguishable. The output holds no control or hidden character other
/// than `\n`. The result is for display only; it is not reversible text and
/// not an authorization key.
pub fn display_safe(text: &str) -> String {
    escape_text(text, true)
}

/// Like [`display_safe`], but `\n` is escaped too, so the result is one line.
///
/// Use it for values that must not be able to start a new displayed line, such
/// as a command argument that could imitate the next numbered argument.
pub fn display_safe_line(text: &str) -> String {
    escape_text(text, false)
}

/// A streaming form of [`display_safe`] for text that arrives in pieces.
///
/// For any split of a string into pieces, the concatenation of every
/// [`push`](Self::push) result followed by [`finish`](Self::finish) equals
/// `display_safe` of the whole string. A backslash is judged by the characters
/// after it, so `push` holds back a trailing run of backslashes, and a trailing
/// run followed by one `u`, until more text arrives; `finish` escapes that tail
/// as the end of the text. The held tail is only backslashes and one `u`, so a
/// caller that never finishes loses at most that tail.
#[derive(Debug, Default)]
pub struct DisplaySafeStream {
    held: String,
}

impl DisplaySafeStream {
    /// Start an empty stream.
    pub const fn new() -> Self {
        Self {
            held: String::new(),
        }
    }

    /// Accept one piece and return the display-safe text that is now decided.
    pub fn push(&mut self, text: &str) -> String {
        self.held.push_str(text);
        // Backslashes then `u` at the end could still become `\u{`; backslashes
        // alone could still precede a character that is escaped. Everything
        // before them depends on no later text.
        let without_u = match self.held.strip_suffix('u') {
            Some(rest) if rest.ends_with('\\') => rest,
            _ => self.held.as_str(),
        };
        let decided = without_u.trim_end_matches('\\').len();
        let tail = self.held.split_off(decided);
        let output = escape_text(&self.held, true);
        self.held = tail;
        output
    }

    /// End the text and return the escaped held tail.
    pub fn finish(self) -> String {
        escape_text(&self.held, true)
    }
}

fn escape_text(text: &str, keep_newline: bool) -> String {
    let characters: Vec<char> = text.chars().collect();
    let mut escaped = vec![false; characters.len()];
    // Right to left, so a backslash can see whether its successor is escaped.
    for index in (0..characters.len()).rev() {
        let character = characters[index];
        escaped[index] = if character == '\\' {
            let forges =
                characters.get(index + 1) == Some(&'u') && characters.get(index + 2) == Some(&'{');
            forges || escaped.get(index + 1).copied().unwrap_or(false)
        } else {
            character.is_control() && !(keep_newline && character == '\n')
                || is_hidden_format(character)
        };
    }
    let mut output = String::with_capacity(text.len());
    for (character, escape) in characters.into_iter().zip(escaped) {
        if !escape {
            output.push(character);
        } else if character == '\\' {
            output.push_str("\\u{5c}");
        } else {
            output.push_str(&format!("\\u{{{:04x}}}", u32::from(character)));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{DisplaySafeStream, display_safe, display_safe_line};

    /// Output must hold no control or hidden character except `\n`.
    fn assert_clean(output: &str) {
        assert!(
            output
                .chars()
                .all(|c| c == '\n' || (!c.is_control() && !super::is_hidden_format(c))),
            "unsafe character in {output:?}"
        );
    }

    #[test]
    fn plain_text_and_line_feeds_pass_through_unchanged() {
        assert_eq!(
            display_safe("fn main() {\n    é 日本 😀\n}"),
            "fn main() {\n    é 日本 😀\n}"
        );
        assert_eq!(display_safe(""), "");
    }

    #[test]
    fn controls_other_than_line_feed_become_visible_escapes() {
        assert_eq!(
            display_safe("a\rb\tc\0d"),
            "a\\u{000d}b\\u{0009}c\\u{0000}d"
        );
        assert_eq!(
            display_safe("\u{7f}\u{85}\u{9f}"),
            "\\u{007f}\\u{0085}\\u{009f}"
        );
        assert_eq!(display_safe("\u{1b}[31m"), "\\u{001b}[31m");
    }

    #[test]
    fn every_hidden_format_range_boundary_is_escaped_and_neighbours_are_not() {
        let hidden = [
            '\u{061c}', '\u{200b}', '\u{200f}', '\u{2028}', '\u{202e}', '\u{2060}', '\u{2069}',
            '\u{feff}',
        ];
        for character in hidden {
            let text = format!("a{character}b");
            let escaped = display_safe(&text);
            assert_eq!(
                escaped,
                format!("a\\u{{{:04x}}}b", u32::from(character)),
                "{character:?}"
            );
            assert_clean(&escaped);
        }
        for character in [
            '\u{061b}', '\u{061d}', '\u{200a}', '\u{2010}', '\u{2027}', '\u{202f}', '\u{205f}',
            '\u{206a}', '\u{fefe}', '\u{ff00}',
        ] {
            let text = format!("a{character}b");
            assert_eq!(display_safe(&text), text, "{character:?} must pass");
        }
        // Every member of each range, not only its ends.
        for code in (0x200b..=0x200f)
            .chain(0x2028..=0x202e)
            .chain(0x2060..=0x2069)
        {
            let character = char::from_u32(code).expect("scalar");
            assert_eq!(
                display_safe(&character.to_string()),
                format!("\\u{{{code:04x}}}")
            );
        }
    }

    /// The documented inclusive ranges, written out independently of the
    /// function under test.
    const HIDDEN_RANGES: [(u32, u32); 16] = [
        (0x00ad, 0x00ad),
        (0x034f, 0x034f),
        (0x061c, 0x061c),
        (0x115f, 0x1160),
        (0x17b4, 0x17b5),
        (0x180b, 0x180f),
        (0x200b, 0x200f),
        (0x2028, 0x202e),
        (0x2060, 0x2069),
        (0x3164, 0x3164),
        (0xfe00, 0xfe0f),
        (0xfeff, 0xfeff),
        (0xffa0, 0xffa0),
        (0xfff9, 0xfffc),
        (0x1d173, 0x1d17a),
        (0xe0000, 0xe0fff),
    ];

    fn listed(code: u32) -> bool {
        HIDDEN_RANGES
            .iter()
            .any(|&(first, last)| (first..=last).contains(&code))
    }

    #[test]
    fn every_range_boundary_is_hidden_and_the_code_points_outside_are_not() {
        for (first, last) in HIDDEN_RANGES {
            for code in [first, last] {
                let character = char::from_u32(code).expect("scalar");
                assert!(super::is_hidden_format(character), "U+{code:04X}");
                assert_eq!(
                    display_safe(&format!("a{character}b")),
                    format!("a\\u{{{code:04x}}}b"),
                    "U+{code:04X}"
                );
            }
            for code in [first - 1, last + 1] {
                let character = char::from_u32(code).expect("scalar");
                assert_eq!(
                    super::is_hidden_format(character),
                    listed(code),
                    "U+{code:04X}"
                );
                if !listed(code) && !character.is_control() {
                    let text = format!("a{character}b");
                    assert_eq!(display_safe(&text), text, "U+{code:04X} must pass");
                }
            }
        }
    }

    #[test]
    fn the_hidden_set_matches_the_documented_ranges_for_every_scalar() {
        for code in 0..=0x0010_ffff_u32 {
            if let Some(character) = char::from_u32(code) {
                assert_eq!(
                    super::is_hidden_format(character),
                    listed(code),
                    "U+{code:04X}"
                );
            }
        }
    }

    #[test]
    fn ordinary_text_and_emoji_without_a_selector_are_unchanged() {
        for text in [
            "plain ASCII 123 ~!",
            "caf\u{e9} na\u{ef}ve \u{c5}ngstr\u{f6}m",
            "\u{65e5}\u{672c}\u{8a9e} \u{d55c}\u{ae00}",
            "\u{1f600} \u{1f44d} \u{2764}",
            "\u{1f44d}\u{1f3fd}",
        ] {
            assert_eq!(display_safe(text), text);
            assert_eq!(display_safe_line(text), text);
        }
    }

    #[test]
    fn a_variation_selector_after_an_emoji_is_shown_on_purpose() {
        assert_eq!(display_safe("\u{2764}\u{fe0f}"), "\u{2764}\\u{fe0f}");
        assert_eq!(
            display_safe("\u{1f600}\u{e0001}\u{e007f}"),
            "\u{1f600}\\u{e0001}\\u{e007f}"
        );
    }

    #[test]
    fn look_alikes_and_trailing_spaces_are_not_addressed() {
        // Cyrillic a, mixed script, and trailing spaces pass through. This
        // documents the limit; it is not a promise to detect them.
        assert_eq!(display_safe("p\u{430}ypal "), "p\u{430}ypal ");
    }

    #[test]
    fn a_literal_escape_cannot_forge_a_real_one() {
        let real = display_safe("\u{202e}");
        assert_eq!(real, "\\u{202e}");
        let forged = display_safe("\\u{202e}");
        assert_eq!(forged, "\\u{5c}u{202e}");
        assert_ne!(real, forged);
        // A backslash before a real escape is not left to merge with it.
        assert_eq!(display_safe("\\\u{202e}"), "\\u{5c}\\u{202e}");
        // Backslashes that do not start `u{` stay as typed.
        assert_eq!(display_safe(r"C:\dir\u1234 \n"), r"C:\dir\u1234 \n");
        // A run of backslashes ahead of `u{` is escaped as a unit.
        assert_eq!(display_safe("\\\\u{41}"), "\\u{5c}\\u{5c}u{41}");
        assert_clean(&display_safe("x\\\\u{202e}\u{202e}\\"));
    }

    #[test]
    fn mixed_hostile_text_has_no_unsafe_character_left() {
        let hostile = "ok\u{202e}evil\u{200b}\r\n\ttab\0nul\u{2028}\u{feff}\\u{1b}end\n";
        let output = display_safe(hostile);
        assert_clean(&output);
        assert_eq!(
            output.matches('\n').count(),
            2,
            "only real line feeds remain"
        );
        assert!(output.starts_with("ok\\u{202e}evil\\u{200b}\\u{000d}\n"));
    }

    /// Push every piece through one stream and join the results with `finish`.
    fn streamed(pieces: &[&str]) -> String {
        let mut stream = DisplaySafeStream::new();
        let mut output = String::new();
        for piece in pieces {
            output.push_str(&stream.push(piece));
        }
        output.push_str(&stream.finish());
        output
    }

    /// Adversarial inputs: forged escapes, backslash runs, hidden characters.
    const STREAM_CASES: &[&str] = &[
        "",
        "plain text",
        "fn main() {\n    \u{e9} \u{65e5}\u{672c} \u{1f600}\n}",
        "\\u{41}",
        "a\\\\u{202e}b",
        "\\\\\\u{",
        "\\\u{7}x",
        "x\\\n",
        "tail\\\\\\",
        "tail\\u",
        "tail\\\\u",
        "\\u\\u{",
        "\\u1234 \\n",
        "ok\u{202e}evil\u{200b}\r\n\ttab\0nul\u{2028}\u{feff}",
        "\\\u{202e}\\\\\u{200b}\\",
        "\u{2764}\u{fe0f}\\u",
        "u\\u{u{",
    ];

    #[test]
    fn streamed_display_safe_equals_the_whole_text_for_every_two_piece_split() {
        for whole in STREAM_CASES {
            let expected = display_safe(whole);
            for (cut, _) in whole.char_indices().chain([(whole.len(), ' ')]) {
                let (first, second) = whole.split_at(cut);
                assert_eq!(streamed(&[first, second]), expected, "{whole:?} at {cut}");
            }
        }
    }

    #[test]
    fn streamed_display_safe_equals_the_whole_text_for_every_three_piece_split() {
        for whole in STREAM_CASES {
            let expected = display_safe(whole);
            let boundaries: Vec<usize> = whole
                .char_indices()
                .map(|(index, _)| index)
                .chain([whole.len()])
                .collect();
            for &first_cut in &boundaries {
                for &second_cut in boundaries.iter().filter(|cut| **cut >= first_cut) {
                    let (head, rest) = whole.split_at(first_cut);
                    let (middle, tail) = rest.split_at(second_cut - first_cut);
                    assert_eq!(
                        streamed(&[head, middle, tail]),
                        expected,
                        "{whole:?} at {first_cut} and {second_cut}"
                    );
                }
            }
        }
    }

    #[test]
    fn streamed_display_safe_holds_back_only_a_backslash_tail() {
        let mut stream = DisplaySafeStream::new();
        assert_eq!(stream.push("ab\\\\"), "ab");
        assert_eq!(stream.push("u"), "");
        assert_eq!(stream.push("{"), "\\u{5c}\\u{5c}u{");
        assert_eq!(stream.push("tail\\"), "tail");
        assert_eq!(stream.finish(), "\\");
        assert_eq!(DisplaySafeStream::default().finish(), "");
    }

    #[test]
    fn the_line_variant_escapes_line_feeds_too() {
        assert_eq!(display_safe_line("a\n2: rm"), "a\\u{000a}2: rm");
        assert!(!display_safe_line("a\nb\r\n").contains('\n'));
        assert_eq!(display_safe_line("plain"), "plain");
    }
}
