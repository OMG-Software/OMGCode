//! `SKILL.md` parsing: delimiters, event-level frontmatter checks, field rules.
//!
//! This module is pure. It reads no file and runs nothing. Its input is
//! untrusted bytes; its output is either a typed refusal or a validated
//! [`SkillManifest`] and body. The YAML parser is used at the event level only.
//! No value tree is built, so no anchor, alias, tag, or merge key is ever
//! resolved. The accepted subset is documented in `docs/skills.md`.

use std::collections::BTreeMap;

use saphyr_parser::{Event, Parser, ScalarStyle};
use thiserror::Error;

use super::{SkillName, SkillNameError};

/// Maximum size of `SKILL.md` in bytes.
pub const MAX_SKILL_MD_BYTES: usize = 64 * 1024;
/// Maximum size of the frontmatter in bytes.
pub const MAX_FRONTMATTER_BYTES: usize = 8 * 1024;
/// Maximum size of the Markdown body in bytes. This is also the load bound.
pub const MAX_BODY_BYTES: usize = 32 * 1024;
/// Maximum `description` length in characters.
pub const MAX_DESCRIPTION_CHARS: usize = 1024;
/// Maximum `license` length in characters.
pub const MAX_LICENSE_CHARS: usize = 256;
/// Maximum `compatibility` length in characters.
pub const MAX_COMPATIBILITY_CHARS: usize = 500;
/// Maximum number of `metadata` entries.
pub const MAX_METADATA_ENTRIES: usize = 16;
/// Maximum `metadata` key length in characters.
pub const MAX_METADATA_KEY_CHARS: usize = 64;
/// Maximum `metadata` value length in characters.
pub const MAX_METADATA_VALUE_CHARS: usize = 256;
/// Maximum `allowed-tools` length in characters.
pub const MAX_ALLOWED_TOOLS_CHARS: usize = 1024;
/// Maximum number of whitespace-separated `allowed-tools` entries.
pub const MAX_ALLOWED_TOOLS_ENTRIES: usize = 32;

/// Longest untrusted key text a diagnostic may carry, in characters.
const MAX_DIAGNOSTIC_CHARS: usize = 64;

/// Why a `SKILL.md` was refused. Each variant has a stable [`code`](Self::code).
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum SkillPackageError {
    #[error("SKILL.md is larger than {MAX_SKILL_MD_BYTES} bytes")]
    FileTooLarge,
    #[error("SKILL.md is not valid UTF-8")]
    NotUtf8,
    #[error("SKILL.md starts with a byte order mark")]
    ByteOrderMark,
    #[error("SKILL.md contains a NUL character")]
    NulCharacter,
    #[error("SKILL.md does not start with a line that is exactly ---")]
    MissingOpeningDelimiter,
    #[error("SKILL.md frontmatter has no closing line that is exactly ---")]
    MissingClosingDelimiter,
    #[error("SKILL.md frontmatter is larger than {MAX_FRONTMATTER_BYTES} bytes")]
    FrontmatterTooLarge,
    #[error("SKILL.md body is empty")]
    EmptyBody,
    #[error("SKILL.md body is larger than {MAX_BODY_BYTES} bytes")]
    BodyTooLarge,
    #[error("SKILL.md frontmatter is not valid YAML: {0}")]
    YamlSyntax(String),
    #[error("SKILL.md frontmatter has no document")]
    EmptyFrontmatter,
    #[error("SKILL.md frontmatter has more than one YAML document")]
    MultipleDocuments,
    #[error("SKILL.md frontmatter is not a block mapping")]
    NotBlockMapping,
    #[error("SKILL.md frontmatter uses a YAML alias")]
    Alias,
    #[error("SKILL.md frontmatter uses a YAML anchor")]
    Anchor,
    #[error("SKILL.md frontmatter uses a YAML tag")]
    Tag,
    #[error("SKILL.md frontmatter uses a sequence")]
    Sequence,
    #[error("SKILL.md frontmatter key `{0}` has a nested or flow mapping value")]
    NestedMapping(String),
    #[error("SKILL.md frontmatter has a key that is not a plain scalar")]
    KeyNotPlainScalar,
    #[error("SKILL.md frontmatter value of `{0}` is not a scalar")]
    ValueNotScalar(String),
    #[error("SKILL.md frontmatter repeats key `{0}`")]
    DuplicateKey(String),
    #[error("SKILL.md frontmatter has unknown key `{0}`")]
    UnknownKey(String),
    #[error("SKILL.md frontmatter has no value for required field `{0}`")]
    MissingField(&'static str),
    #[error("SKILL.md field `{field}` must have {min} to {max} characters")]
    FieldLength {
        field: &'static str,
        min: usize,
        max: usize,
    },
    #[error("SKILL.md field `{0}` contains a control character")]
    ControlCharacter(&'static str),
    #[error("SKILL.md name is invalid: {0}")]
    InvalidName(SkillNameError),
    #[error("SKILL.md metadata has more than {MAX_METADATA_ENTRIES} entries")]
    TooManyMetadataEntries,
    #[error("SKILL.md allowed-tools has more than {MAX_ALLOWED_TOOLS_ENTRIES} entries")]
    TooManyAllowedTools,
}

impl SkillPackageError {
    /// Stable machine-readable code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::FileTooLarge => "skill_md_too_large",
            Self::NotUtf8 => "skill_md_not_utf8",
            Self::ByteOrderMark => "skill_md_byte_order_mark",
            Self::NulCharacter => "skill_md_nul_character",
            Self::MissingOpeningDelimiter => "frontmatter_missing_opening",
            Self::MissingClosingDelimiter => "frontmatter_missing_closing",
            Self::FrontmatterTooLarge => "frontmatter_too_large",
            Self::EmptyBody => "body_empty",
            Self::BodyTooLarge => "body_too_large",
            Self::YamlSyntax(_) => "frontmatter_yaml_syntax",
            Self::EmptyFrontmatter => "frontmatter_empty",
            Self::MultipleDocuments => "frontmatter_multiple_documents",
            Self::NotBlockMapping => "frontmatter_not_block_mapping",
            Self::Alias => "frontmatter_alias",
            Self::Anchor => "frontmatter_anchor",
            Self::Tag => "frontmatter_tag",
            Self::Sequence => "frontmatter_sequence",
            Self::NestedMapping(_) => "frontmatter_nested_mapping",
            Self::KeyNotPlainScalar => "frontmatter_key_not_plain_scalar",
            Self::ValueNotScalar(_) => "frontmatter_value_not_scalar",
            Self::DuplicateKey(_) => "frontmatter_duplicate_key",
            Self::UnknownKey(_) => "frontmatter_unknown_key",
            Self::MissingField(_) => "field_missing",
            Self::FieldLength { .. } => "field_length",
            Self::ControlCharacter(_) => "field_control_character",
            Self::InvalidName(error) => error.code(),
            Self::TooManyMetadataEntries => "metadata_too_many_entries",
            Self::TooManyAllowedTools => "allowed_tools_too_many_entries",
        }
    }
}

/// Validated skill metadata. Every field has passed the bounds in this module.
///
/// `allowed_tools` is advisory text for the operator. It never narrows or
/// widens any permission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillManifest {
    pub name: SkillName,
    pub description: String,
    pub license: Option<String>,
    pub compatibility: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub allowed_tools: Vec<String>,
}

/// A validated `SKILL.md`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedSkill {
    pub manifest: SkillManifest,
    /// The Markdown body. It is untrusted task text.
    pub body: String,
    /// Byte offset of `body` within the original `SKILL.md`.
    pub body_offset: usize,
}

/// Parse and validate the bytes of a `SKILL.md`.
///
/// This does not compare the name with a directory name; the importer does.
///
/// # Errors
///
/// Returns the first rule the bytes break.
pub fn parse_skill_md(bytes: &[u8]) -> Result<ParsedSkill, SkillPackageError> {
    if bytes.len() > MAX_SKILL_MD_BYTES {
        return Err(SkillPackageError::FileTooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| SkillPackageError::NotUtf8)?;
    if text.starts_with('\u{feff}') {
        return Err(SkillPackageError::ByteOrderMark);
    }
    if text.contains('\0') {
        return Err(SkillPackageError::NulCharacter);
    }
    let (frontmatter, body_offset) = split_frontmatter(text)?;
    let body = text
        .get(body_offset..)
        .ok_or(SkillPackageError::EmptyBody)?;
    if body.trim().is_empty() {
        return Err(SkillPackageError::EmptyBody);
    }
    if body.len() > MAX_BODY_BYTES {
        return Err(SkillPackageError::BodyTooLarge);
    }
    let fields = read_frontmatter(frontmatter)?;
    Ok(ParsedSkill {
        manifest: build_manifest(fields)?,
        body: body.to_owned(),
        body_offset,
    })
}

/// Split the text into frontmatter and the byte offset where the body starts.
fn split_frontmatter(text: &str) -> Result<(&str, usize), SkillPackageError> {
    let after_opening =
        strip_line(text, "---").ok_or(SkillPackageError::MissingOpeningDelimiter)?;
    let mut offset = after_opening;
    while offset <= text.len() {
        let rest = text.get(offset..).unwrap_or("");
        let (line, next) = match rest.find('\n') {
            Some(index) => (
                &rest[..index],
                offset.saturating_add(index).saturating_add(1),
            ),
            None => (rest, text.len().saturating_add(1)),
        };
        if line.strip_suffix('\r').unwrap_or(line) == "---" {
            let frontmatter = text.get(after_opening..offset).unwrap_or("");
            if frontmatter.len() > MAX_FRONTMATTER_BYTES {
                return Err(SkillPackageError::FrontmatterTooLarge);
            }
            return Ok((frontmatter, next.min(text.len())));
        }
        if next > text.len() {
            break;
        }
        offset = next;
    }
    Err(SkillPackageError::MissingClosingDelimiter)
}

/// If the first line of `text` is exactly `marker`, return the offset after it.
fn strip_line(text: &str, marker: &str) -> Option<usize> {
    let rest = text.strip_prefix(marker)?;
    if rest.starts_with("\r\n") {
        Some(marker.len().checked_add(2)?)
    } else if rest.starts_with('\n') {
        Some(marker.len().checked_add(1)?)
    } else {
        None
    }
}

/// Raw fields after event-level checks and before field rules.
#[derive(Default)]
struct RawFields {
    name: Option<String>,
    description: Option<String>,
    license: Option<String>,
    compatibility: Option<String>,
    metadata: Option<Vec<(String, String)>>,
    allowed_tools: Option<String>,
}

/// One scalar with the facts the field rules need.
struct Scalar {
    text: String,
    plain: bool,
}

impl Scalar {
    /// A plain empty scalar, `~`, or `null` is YAML null: the value is absent.
    fn is_null(&self) -> bool {
        self.plain && matches!(self.text.as_str(), "" | "~" | "null")
    }
}

/// A node the strict event reader accepts.
enum Node {
    Scalar(Scalar),
    MappingStart { flow: bool },
    MappingEnd,
}

/// Pulls events and refuses every construct outside the accepted subset.
struct Events<'a> {
    parser: Parser<'a, saphyr_parser::StrInput<'a>>,
    source: &'a str,
}

impl<'a> Events<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            parser: Parser::new_from_str(source),
            source,
        }
    }

    /// Next raw event; a syntax error or an early end is a refusal.
    fn next(&mut self) -> Result<(Event<'a>, saphyr_parser::Span), SkillPackageError> {
        match self.parser.next() {
            Some(Ok(item)) => Ok(item),
            Some(Err(error)) => Err(SkillPackageError::YamlSyntax(sanitize(&error.to_string()))),
            None => Err(SkillPackageError::EmptyFrontmatter),
        }
    }

    /// Next event that must be a scalar or mapping boundary. Sequences,
    /// aliases, anchors, and tags are refused wherever they appear.
    fn node(&mut self) -> Result<Node, SkillPackageError> {
        let (event, span) = self.next()?;
        match event {
            Event::Alias(_) => Err(SkillPackageError::Alias),
            Event::Scalar(value, style, anchor, tag) => {
                if anchor != 0 {
                    return Err(SkillPackageError::Anchor);
                }
                if tag.is_some() {
                    return Err(SkillPackageError::Tag);
                }
                Ok(Node::Scalar(Scalar {
                    text: value.into_owned(),
                    plain: style == ScalarStyle::Plain,
                }))
            }
            Event::SequenceStart(..) => Err(SkillPackageError::Sequence),
            Event::MappingStart(anchor, tag) => {
                if anchor != 0 {
                    return Err(SkillPackageError::Anchor);
                }
                if tag.is_some() {
                    return Err(SkillPackageError::Tag);
                }
                let flow = self.source.chars().nth(span.start.index()) == Some('{');
                Ok(Node::MappingStart { flow })
            }
            Event::MappingEnd => Ok(Node::MappingEnd),
            // These cannot start a node. Treat a stray one as a syntax error.
            Event::Nothing
            | Event::StreamStart
            | Event::StreamEnd
            | Event::DocumentStart(_)
            | Event::DocumentEnd
            | Event::SequenceEnd => {
                Err(SkillPackageError::YamlSyntax("unexpected event".to_owned()))
            }
        }
    }
}

/// Read the single block mapping and collect raw, still unvalidated fields.
fn read_frontmatter(frontmatter: &str) -> Result<RawFields, SkillPackageError> {
    let mut events = Events::new(frontmatter);
    if !matches!(events.next()?.0, Event::StreamStart) {
        return Err(SkillPackageError::YamlSyntax("unexpected event".to_owned()));
    }
    let (first, _) = events.next()?;
    match first {
        Event::StreamEnd => return Err(SkillPackageError::EmptyFrontmatter),
        Event::DocumentStart(_) => {}
        _ => return Err(SkillPackageError::YamlSyntax("unexpected event".to_owned())),
    }
    match events.node()? {
        Node::MappingStart { flow: false } => {}
        Node::MappingStart { flow: true } | Node::Scalar(_) | Node::MappingEnd => {
            return Err(SkillPackageError::NotBlockMapping);
        }
    }
    let mut fields = RawFields::default();
    let mut seen: Vec<String> = Vec::new();
    loop {
        let key = match events.node()? {
            Node::MappingEnd => break,
            Node::Scalar(key) if key.plain => key.text,
            Node::Scalar(_) | Node::MappingStart { .. } => {
                return Err(SkillPackageError::KeyNotPlainScalar);
            }
        };
        if seen.contains(&key) {
            return Err(SkillPackageError::DuplicateKey(sanitize(&key)));
        }
        seen.push(key.clone());
        let known = matches!(
            key.as_str(),
            "name" | "description" | "license" | "compatibility" | "metadata" | "allowed-tools"
        );
        if !known {
            return Err(SkillPackageError::UnknownKey(sanitize(&key)));
        }
        let value = events.node()?;
        if key == "metadata" {
            fields.metadata = match value {
                Node::Scalar(scalar) if scalar.is_null() => None,
                Node::Scalar(_) | Node::MappingEnd => {
                    return Err(SkillPackageError::ValueNotScalar(key));
                }
                Node::MappingStart { flow: true } => {
                    return Err(SkillPackageError::NestedMapping(key));
                }
                Node::MappingStart { flow: false } => Some(read_metadata(&mut events)?),
            };
            continue;
        }
        let scalar = match value {
            Node::Scalar(scalar) => scalar,
            Node::MappingStart { .. } => return Err(SkillPackageError::NestedMapping(key)),
            Node::MappingEnd => return Err(SkillPackageError::ValueNotScalar(key)),
        };
        let slot = match key.as_str() {
            "name" => &mut fields.name,
            "description" => &mut fields.description,
            "license" => &mut fields.license,
            "compatibility" => &mut fields.compatibility,
            _ => &mut fields.allowed_tools,
        };
        // YAML null is an absent field; the field rules decide if that is allowed.
        *slot = if scalar.is_null() {
            None
        } else {
            Some(scalar.text)
        };
    }
    let (after, _) = events.next()?;
    if !matches!(after, Event::DocumentEnd) {
        return Err(SkillPackageError::YamlSyntax("unexpected event".to_owned()));
    }
    match events.next()?.0 {
        Event::StreamEnd => Ok(fields),
        Event::DocumentStart(_) => Err(SkillPackageError::MultipleDocuments),
        _ => Err(SkillPackageError::YamlSyntax("unexpected event".to_owned())),
    }
}

/// Read the `metadata` block mapping: scalar keys to scalar values only.
fn read_metadata(events: &mut Events<'_>) -> Result<Vec<(String, String)>, SkillPackageError> {
    let mut entries: Vec<(String, String)> = Vec::new();
    loop {
        let key = match events.node()? {
            Node::MappingEnd => return Ok(entries),
            Node::Scalar(key) => key.text,
            Node::MappingStart { .. } => {
                return Err(SkillPackageError::NestedMapping("metadata".to_owned()));
            }
        };
        if entries.iter().any(|(existing, _)| *existing == key) {
            return Err(SkillPackageError::DuplicateKey(sanitize(&key)));
        }
        if entries.len() >= MAX_METADATA_ENTRIES {
            return Err(SkillPackageError::TooManyMetadataEntries);
        }
        let value = match events.node()? {
            Node::Scalar(value) => value.text,
            Node::MappingStart { .. } | Node::MappingEnd => {
                return Err(SkillPackageError::NestedMapping("metadata".to_owned()));
            }
        };
        entries.push((key, value));
    }
}

/// Apply the field rules to the raw fields.
fn build_manifest(fields: RawFields) -> Result<SkillManifest, SkillPackageError> {
    let name = fields.name.ok_or(SkillPackageError::MissingField("name"))?;
    reject_controls("name", &name, false)?;
    let name = SkillName::new(&name).map_err(SkillPackageError::InvalidName)?;
    let description = fields
        .description
        .ok_or(SkillPackageError::MissingField("description"))?;
    reject_controls("description", &description, true)?;
    check_chars("description", &description, 1, MAX_DESCRIPTION_CHARS)?;
    let license = optional("license", fields.license, 1, MAX_LICENSE_CHARS)?;
    let compatibility = optional(
        "compatibility",
        fields.compatibility,
        1,
        MAX_COMPATIBILITY_CHARS,
    )?;
    let mut metadata = BTreeMap::new();
    for (key, value) in fields.metadata.unwrap_or_default() {
        reject_controls("metadata", &key, false)?;
        reject_controls("metadata", &value, false)?;
        check_chars("metadata", &key, 1, MAX_METADATA_KEY_CHARS)?;
        check_chars("metadata", &value, 0, MAX_METADATA_VALUE_CHARS)?;
        if metadata.insert(key.clone(), value).is_some() {
            return Err(SkillPackageError::DuplicateKey(sanitize(&key)));
        }
    }
    let allowed_tools = match optional(
        "allowed-tools",
        fields.allowed_tools,
        1,
        MAX_ALLOWED_TOOLS_CHARS,
    )? {
        None => Vec::new(),
        Some(text) => {
            let entries: Vec<String> = text.split_ascii_whitespace().map(str::to_owned).collect();
            if entries.is_empty() {
                return Err(SkillPackageError::FieldLength {
                    field: "allowed-tools",
                    min: 1,
                    max: MAX_ALLOWED_TOOLS_CHARS,
                });
            }
            if entries.len() > MAX_ALLOWED_TOOLS_ENTRIES {
                return Err(SkillPackageError::TooManyAllowedTools);
            }
            entries
        }
    };
    Ok(SkillManifest {
        name,
        description,
        license,
        compatibility,
        metadata,
        allowed_tools,
    })
}

/// Validate an optional text field when present.
fn optional(
    field: &'static str,
    value: Option<String>,
    min: usize,
    max: usize,
) -> Result<Option<String>, SkillPackageError> {
    let Some(value) = value else {
        return Ok(None);
    };
    reject_controls(field, &value, false)?;
    check_chars(field, &value, min, max)?;
    Ok(Some(value))
}

/// Refuse every ASCII control character, allowing LF only when `allow_lf`.
fn reject_controls(
    field: &'static str,
    value: &str,
    allow_lf: bool,
) -> Result<(), SkillPackageError> {
    if value
        .chars()
        .any(|character| character.is_ascii_control() && !(allow_lf && character == '\n'))
    {
        return Err(SkillPackageError::ControlCharacter(field));
    }
    Ok(())
}

/// Count characters, not bytes, against the specification's limits.
fn check_chars(
    field: &'static str,
    value: &str,
    min: usize,
    max: usize,
) -> Result<(), SkillPackageError> {
    let count = value.chars().count();
    if count < min || count > max {
        return Err(SkillPackageError::FieldLength { field, min, max });
    }
    Ok(())
}

/// Make untrusted text safe and short for a diagnostic.
fn sanitize(text: &str) -> String {
    text.chars()
        .take(MAX_DIAGNOSTIC_CHARS)
        .flat_map(char::escape_debug)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PULL_REQUEST_REVIEW: &[u8] =
        include_bytes!("../../../../skills/pull-request-review/SKILL.md");

    fn skill(frontmatter: &str, body: &str) -> Vec<u8> {
        format!("---\n{frontmatter}\n---\n{body}").into_bytes()
    }

    fn refusal(bytes: &[u8]) -> SkillPackageError {
        parse_skill_md(bytes).expect_err("the package must be refused")
    }

    fn refusal_code(frontmatter: &str) -> &'static str {
        refusal(&skill(frontmatter, "Body.\n")).code()
    }

    const MINIMAL: &str = "name: demo\ndescription: A demo skill.";

    #[test]
    fn given_the_bundled_skill_when_parsed_then_the_manifest_and_body_are_returned() {
        let parsed = parse_skill_md(PULL_REQUEST_REVIEW).expect("bundled skill parses");
        assert_eq!(parsed.manifest.name.as_str(), "pull-request-review");
        assert!(
            parsed
                .manifest
                .description
                .starts_with("Review a pull request")
        );
        assert!(
            parsed.body.starts_with("\n# Pull Request Review")
                || parsed.body.contains("# Pull Request Review")
        );
        assert_eq!(
            &PULL_REQUEST_REVIEW[parsed.body_offset..],
            parsed.body.as_bytes()
        );
    }

    #[test]
    fn given_all_optional_fields_when_parsed_then_each_is_returned() {
        let frontmatter = "name: demo\ndescription: \"Does things: well.\"\nlicense: MIT\ncompatibility: Needs nothing\nmetadata:\n  version: \"1.0\"\n  author: me\nallowed-tools: Bash(git:*)  Read";
        let parsed = parse_skill_md(&skill(frontmatter, "Body.\n")).expect("parses");
        let manifest = parsed.manifest;
        assert_eq!(manifest.license.as_deref(), Some("MIT"));
        assert_eq!(manifest.compatibility.as_deref(), Some("Needs nothing"));
        assert_eq!(
            manifest.metadata.get("version").map(String::as_str),
            Some("1.0")
        );
        assert_eq!(manifest.metadata.len(), 2);
        assert_eq!(manifest.allowed_tools, ["Bash(git:*)", "Read"]);
    }

    #[test]
    fn given_a_plain_number_when_parsed_then_it_is_literal_text() {
        let frontmatter = "name: demo\ndescription: 1.0\nmetadata:\n  count: 007\n  flag: true";
        let manifest = parse_skill_md(&skill(frontmatter, "Body.\n"))
            .expect("parses")
            .manifest;
        assert_eq!(manifest.description, "1.0");
        assert_eq!(
            manifest.metadata.get("count").map(String::as_str),
            Some("007")
        );
        assert_eq!(
            manifest.metadata.get("flag").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn given_crlf_delimiters_when_parsed_then_the_body_starts_after_the_closing_line() {
        let bytes = b"---\r\nname: demo\r\ndescription: A demo.\r\n---\r\nBody text\r\n";
        let parsed = parse_skill_md(bytes).expect("CRLF parses");
        assert_eq!(parsed.body, "Body text\r\n");
        assert_eq!(parsed.manifest.description, "A demo.");
    }

    #[test]
    fn given_a_block_description_when_parsed_then_line_feeds_are_kept() {
        let frontmatter = "name: demo\ndescription: |\n  line one\n  line two";
        let manifest = parse_skill_md(&skill(frontmatter, "Body.\n"))
            .expect("parses")
            .manifest;
        assert_eq!(manifest.description, "line one\nline two\n");
    }

    #[test]
    fn given_a_utf8_byte_order_mark_when_parsed_then_it_is_refused() {
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend(skill(MINIMAL, "Body.\n"));
        assert_eq!(refusal(&bytes), SkillPackageError::ByteOrderMark);
    }

    #[test]
    fn given_invalid_utf8_when_parsed_then_it_is_refused() {
        let mut bytes = skill(MINIMAL, "Body.\n");
        bytes.push(0xff);
        assert_eq!(refusal(&bytes), SkillPackageError::NotUtf8);
    }

    #[test]
    fn given_a_nul_character_when_parsed_then_it_is_refused() {
        assert_eq!(
            refusal(&skill(MINIMAL, "Body\0.\n")),
            SkillPackageError::NulCharacter
        );
    }

    #[test]
    fn given_a_missing_opening_delimiter_when_parsed_then_it_is_refused() {
        for bytes in [
            &b"name: demo\n---\nBody\n"[..],
            b"--- \nname: demo\n---\nBody\n",
            b"----\nname: demo\n---\nBody\n",
            b"\n---\nname: demo\n---\nBody\n",
            b"",
        ] {
            assert_eq!(refusal(bytes), SkillPackageError::MissingOpeningDelimiter);
        }
    }

    #[test]
    fn given_a_missing_closing_delimiter_when_parsed_then_it_is_refused() {
        for bytes in [
            &b"---\nname: demo\ndescription: x\nBody\n"[..],
            b"---\nname: demo\ndescription: x\n--- \nBody\n",
            b"---\nname: demo\ndescription: x\n----\nBody\n",
        ] {
            assert_eq!(refusal(bytes), SkillPackageError::MissingClosingDelimiter);
        }
    }

    #[test]
    fn given_a_closing_delimiter_at_the_end_when_parsed_then_the_body_is_empty() {
        let bytes = b"---\nname: demo\ndescription: x\n---";
        assert_eq!(refusal(bytes), SkillPackageError::EmptyBody);
        assert_eq!(
            refusal(&skill(MINIMAL, " \n\t\n")),
            SkillPackageError::EmptyBody
        );
    }

    #[test]
    fn given_an_oversized_frontmatter_when_parsed_then_it_is_refused() {
        let padding = "x".repeat(MAX_FRONTMATTER_BYTES);
        let frontmatter = format!("{MINIMAL}\n# {padding}");
        assert_eq!(
            refusal(&skill(&frontmatter, "Body.\n")),
            SkillPackageError::FrontmatterTooLarge
        );
    }

    #[test]
    fn given_oversized_body_or_file_when_parsed_then_each_is_refused() {
        let body = "b".repeat(MAX_BODY_BYTES + 1);
        assert_eq!(
            refusal(&skill(MINIMAL, &body)),
            SkillPackageError::BodyTooLarge
        );
        let exact = "b".repeat(MAX_BODY_BYTES - 1);
        parse_skill_md(&skill(MINIMAL, &format!("{exact}\n"))).expect("body at the bound");
        let file = vec![b'a'; MAX_SKILL_MD_BYTES + 1];
        assert_eq!(refusal(&file), SkillPackageError::FileTooLarge);
    }

    #[test]
    fn given_an_alias_when_parsed_then_it_is_refused() {
        // The parser rejects an alias with no anchor; an anchored one is
        // refused at the anchor, before the alias is reached.
        assert_eq!(
            refusal_code("name: demo\ndescription: *x"),
            "frontmatter_yaml_syntax"
        );
        assert_eq!(
            refusal_code("name: demo\nlicense: &x MIT\ndescription: *x"),
            "frontmatter_anchor"
        );
    }

    #[test]
    fn given_any_anchor_when_parsed_then_it_is_refused() {
        assert_eq!(
            refusal_code("name: demo\ndescription: &x text"),
            "frontmatter_anchor"
        );
        assert_eq!(
            refusal_code("name: demo\ndescription: text\nmetadata: &m\n  a: b"),
            "frontmatter_anchor"
        );
    }

    #[test]
    fn given_any_tag_when_parsed_then_it_is_refused() {
        for frontmatter in [
            "name: demo\ndescription: !!str text",
            "name: demo\ndescription: !!python/object:os.system text",
            "name: demo\ndescription: !custom text",
            "name: demo\ndescription: !<tag:example.com,2026:x> text",
            "name: !!str demo\ndescription: text",
        ] {
            assert_eq!(
                refusal_code(frontmatter),
                "frontmatter_tag",
                "{frontmatter}"
            );
        }
    }

    #[test]
    fn given_a_sequence_when_parsed_then_it_is_refused() {
        for frontmatter in [
            "name: demo\ndescription: text\nlicense:\n  - MIT",
            "name: demo\ndescription: text\nlicense: [MIT]",
            "- name\n- description",
        ] {
            assert_eq!(
                refusal_code(frontmatter),
                "frontmatter_sequence",
                "{frontmatter}"
            );
        }
    }

    #[test]
    fn given_a_flow_or_nested_mapping_when_parsed_then_it_is_refused() {
        assert_eq!(
            refusal_code("name: demo\ndescription: text\nlicense:\n  a: b"),
            "frontmatter_nested_mapping"
        );
        assert_eq!(
            refusal_code("name: demo\ndescription: text\nlicense: {a: b}"),
            "frontmatter_nested_mapping"
        );
        assert_eq!(
            refusal_code("name: demo\ndescription: text\nmetadata: {a: b}"),
            "frontmatter_nested_mapping"
        );
        assert_eq!(
            refusal_code("name: demo\ndescription: text\nmetadata:\n  a:\n    b: c"),
            "frontmatter_nested_mapping"
        );
        assert_eq!(
            refusal_code("{name: demo, description: text}"),
            "frontmatter_not_block_mapping"
        );
    }

    #[test]
    fn given_a_non_mapping_document_when_parsed_then_it_is_refused() {
        assert_eq!(refusal_code("just text"), "frontmatter_not_block_mapping");
        assert_eq!(refusal_code(""), "frontmatter_empty");
        assert_eq!(refusal_code("# only a comment"), "frontmatter_empty");
    }

    #[test]
    fn given_multiple_documents_when_parsed_then_they_are_refused() {
        assert_eq!(
            refusal_code("name: demo\ndescription: text\n...\n--- extra"),
            "frontmatter_multiple_documents"
        );
        assert_eq!(
            refusal_code("name: demo\ndescription: text\n--- \nname: other"),
            "frontmatter_multiple_documents"
        );
    }

    #[test]
    fn given_duplicate_keys_when_parsed_then_they_are_refused_at_every_level() {
        assert_eq!(
            refusal_code("name: demo\ndescription: a\ndescription: b"),
            "frontmatter_duplicate_key"
        );
        assert_eq!(
            refusal_code("name: demo\ndescription: a\nmetadata:\n  k: 1\n  k: 2"),
            "frontmatter_duplicate_key"
        );
        assert_eq!(
            refusal_code("name: demo\ndescription: a\nmetadata:\n  k: 1\n  \"k\": 2"),
            "frontmatter_duplicate_key"
        );
    }

    #[test]
    fn given_an_unknown_top_level_key_when_parsed_then_it_is_refused() {
        assert_eq!(
            refusal_code("name: demo\ndescription: a\nowner: me"),
            "frontmatter_unknown_key"
        );
    }

    #[test]
    fn given_a_non_plain_key_when_parsed_then_it_is_refused() {
        assert_eq!(
            refusal_code("\"name\": demo\ndescription: a"),
            "frontmatter_key_not_plain_scalar"
        );
    }

    #[test]
    fn given_a_yaml_syntax_error_when_parsed_then_it_is_refused() {
        assert_eq!(
            refusal_code("name: demo\ndescription: a: b"),
            "frontmatter_yaml_syntax"
        );
        assert_eq!(
            refusal_code("name: demo\ndescription: \"unterminated"),
            "frontmatter_yaml_syntax"
        );
    }

    #[test]
    fn given_required_fields_that_are_null_when_parsed_then_they_are_missing() {
        for value in ["", "~", "null"] {
            assert_eq!(
                refusal(&skill(&format!("name: {value}\ndescription: a"), "B\n")),
                SkillPackageError::MissingField("name"),
                "name {value:?}"
            );
            assert_eq!(
                refusal(&skill(&format!("name: demo\ndescription: {value}"), "B\n")),
                SkillPackageError::MissingField("description"),
                "description {value:?}"
            );
        }
        assert_eq!(
            refusal(&skill("name: demo", "B\n")),
            SkillPackageError::MissingField("description")
        );
        // A quoted empty string is text, so the length rule refuses it instead.
        assert_eq!(
            refusal_code("name: demo\ndescription: \"\""),
            "field_length"
        );
    }

    #[test]
    fn given_an_invalid_name_when_parsed_then_the_name_rule_code_is_reported() {
        assert_eq!(
            refusal_code("name: Demo\ndescription: a"),
            "name_invalid_character"
        );
        assert_eq!(
            refusal_code("name: -demo\ndescription: a"),
            "name_edge_hyphen"
        );
        assert_eq!(
            refusal_code("name: de--mo\ndescription: a"),
            "name_consecutive_hyphens"
        );
    }

    #[test]
    fn given_fields_at_and_past_their_bounds_when_parsed_then_the_bounds_hold() {
        let ok = |frontmatter: String| parse_skill_md(&skill(&frontmatter, "B\n"));
        let long = |count: usize| "a".repeat(count);
        assert!(ok(format!("name: demo\ndescription: {}", long(1024))).is_ok());
        assert_eq!(
            refusal_code(&format!("name: demo\ndescription: {}", long(1025))),
            "field_length"
        );
        assert!(
            ok(format!(
                "name: demo\ndescription: a\nlicense: {}",
                long(256)
            ))
            .is_ok()
        );
        assert_eq!(
            refusal_code(&format!(
                "name: demo\ndescription: a\nlicense: {}",
                long(257)
            )),
            "field_length"
        );
        assert!(
            ok(format!(
                "name: demo\ndescription: a\ncompatibility: {}",
                long(500)
            ))
            .is_ok()
        );
        assert_eq!(
            refusal_code(&format!(
                "name: demo\ndescription: a\ncompatibility: {}",
                long(501)
            )),
            "field_length"
        );
        assert!(
            ok(format!(
                "name: demo\ndescription: a\nmetadata:\n  {}: {}",
                long(64),
                long(256)
            ))
            .is_ok()
        );
        assert_eq!(
            refusal_code(&format!(
                "name: demo\ndescription: a\nmetadata:\n  {}: x",
                long(65)
            )),
            "field_length"
        );
        assert_eq!(
            refusal_code(&format!(
                "name: demo\ndescription: a\nmetadata:\n  k: {}",
                long(257)
            )),
            "field_length"
        );
        assert!(
            ok("name: demo\ndescription: a\nmetadata:\n  k: \"\"".to_owned()).is_ok(),
            "an empty metadata value is allowed"
        );
        assert_eq!(
            refusal_code(&format!(
                "name: demo\ndescription: a\nallowed-tools: {}",
                long(1025)
            )),
            "field_length"
        );
    }

    #[test]
    fn given_characters_beyond_ascii_when_counted_then_characters_not_bytes_are_used() {
        let description = "\u{e9}".repeat(1024);
        parse_skill_md(&skill(
            &format!("name: demo\ndescription: {description}"),
            "B\n",
        ))
        .expect("1024 characters of two bytes each");
    }

    #[test]
    fn given_too_many_metadata_entries_or_tools_when_parsed_then_they_are_refused() {
        let entries = |count: usize| {
            (0..count)
                .map(|index| format!("  k{index}: v"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        parse_skill_md(&skill(
            &format!("{MINIMAL}\nmetadata:\n{}", entries(16)),
            "B\n",
        ))
        .expect("16 entries");
        assert_eq!(
            refusal_code(&format!("{MINIMAL}\nmetadata:\n{}", entries(17))),
            "metadata_too_many_entries"
        );
        let tools = |count: usize| {
            (0..count)
                .map(|index| format!("t{index}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        parse_skill_md(&skill(
            &format!("{MINIMAL}\nallowed-tools: {}", tools(32)),
            "B\n",
        ))
        .expect("32 tools");
        assert_eq!(
            refusal_code(&format!("{MINIMAL}\nallowed-tools: {}", tools(33))),
            "allowed_tools_too_many_entries"
        );
        assert_eq!(
            refusal_code(&format!("{MINIMAL}\nallowed-tools: \"   \"")),
            "field_length"
        );
    }

    #[test]
    fn given_control_characters_when_parsed_then_they_are_refused_except_a_description_line_feed() {
        for frontmatter in [
            "name: demo\ndescription: \"a\\tb\"",
            "name: demo\ndescription: \"a\\rb\"",
            "name: demo\ndescription: \"a\\x1bb\"",
            "name: demo\ndescription: \"a\\x7fb\"",
            "name: demo\ndescription: a\nlicense: \"a\\nb\"",
            "name: demo\ndescription: a\ncompatibility: \"a\\nb\"",
            "name: demo\ndescription: a\nallowed-tools: \"a\\nb\"",
            "name: demo\ndescription: a\nmetadata:\n  \"k\\nk\": v",
            "name: demo\ndescription: a\nmetadata:\n  k: \"v\\tv\"",
            "name: \"demo\\n\"\ndescription: a",
        ] {
            assert_eq!(
                refusal_code(frontmatter),
                "field_control_character",
                "{frontmatter}"
            );
        }
        parse_skill_md(&skill("name: demo\ndescription: \"a\\nb\"", "B\n"))
            .expect("a description may contain a line feed");
    }

    #[test]
    fn given_untrusted_key_text_when_refused_then_the_diagnostic_is_escaped_and_short() {
        let key = format!("bad\\x1b{}", "k".repeat(200));
        let error = refusal(&skill(&format!("{MINIMAL}\n{key}: v"), "B\n"));
        let text = error.to_string();
        assert!(!text.contains('\u{1b}'));
        assert!(text.len() < 300, "{text}");
    }
}
