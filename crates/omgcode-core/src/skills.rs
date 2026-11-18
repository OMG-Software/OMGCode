//! Skill packages: validated, immutable copies of operator-selected folders.
//!
//! A skill is untrusted task text. Importing, registering, and enabling a skill
//! never grants a permission, approves a tool call, or runs a package file.
//! The control journal is the only registry; a package directory without a
//! registry record is inert. See `docs/skills.md` and ADR-0057.

use std::fmt;

mod import;
mod package;
mod store;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use import::{
    MAX_DIRECTORY_ENTRIES, MAX_PACKAGE_BYTES, MAX_PACKAGE_DEPTH, MAX_PACKAGE_DIRECTORIES,
    MAX_PACKAGE_FILE_BYTES, MAX_PACKAGE_FILES, SkillImportError,
};
pub use package::{
    MAX_ALLOWED_TOOLS_ENTRIES, MAX_BODY_BYTES, MAX_SKILL_MD_BYTES, ParsedSkill, SkillManifest,
    SkillPackageError, parse_skill_md,
};
pub use store::{
    ImportedPackage, LoadedSkill, PackageFile, SkillLoadError, SkillStoreError, import_package,
    load_skill_body, remove_package,
};

/// Maximum length of a skill name in bytes (Agent Skills specification).
pub const MAX_NAME_BYTES: usize = 64;
/// Maximum number of skills the registry holds at once.
pub const MAX_REGISTERED_SKILLS: usize = 64;
/// Maximum number of skills one chat turn may select.
pub const MAX_SELECTED_SKILLS: usize = 4;

/// A validated skill name.
///
/// The Agent Skills specification allows 1 to 64 characters from lowercase
/// ASCII letters, digits, and hyphens, with no leading, trailing, or
/// consecutive hyphen. The name must also equal the package directory's name.
/// OMGCode accepts only the ASCII subset, so every valid name is a portable
/// file name on Windows and Linux.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SkillName(String);

/// Why a candidate skill name was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillNameError {
    Empty,
    TooLong,
    InvalidCharacter,
    EdgeHyphen,
    ConsecutiveHyphens,
}

impl SkillNameError {
    /// Stable machine-readable code.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::Empty => "name_empty",
            Self::TooLong => "name_too_long",
            Self::InvalidCharacter => "name_invalid_character",
            Self::EdgeHyphen => "name_edge_hyphen",
            Self::ConsecutiveHyphens => "name_consecutive_hyphens",
        }
    }
}

impl fmt::Display for SkillNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Empty => "skill name is empty",
            Self::TooLong => "skill name is longer than 64 bytes",
            Self::InvalidCharacter => {
                "skill name may contain only lowercase ASCII letters, digits, and hyphens"
            }
            Self::EdgeHyphen => "skill name must not start or end with a hyphen",
            Self::ConsecutiveHyphens => "skill name must not contain consecutive hyphens",
        };
        formatter.write_str(text)
    }
}

impl std::error::Error for SkillNameError {}

impl SkillName {
    /// Validates `value` as a skill name.
    ///
    /// # Errors
    ///
    /// Returns the first rule the value breaks.
    pub fn new(value: &str) -> Result<Self, SkillNameError> {
        if value.is_empty() {
            return Err(SkillNameError::Empty);
        }
        if value.len() > MAX_NAME_BYTES {
            return Err(SkillNameError::TooLong);
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(SkillNameError::InvalidCharacter);
        }
        if value.starts_with('-') || value.ends_with('-') {
            return Err(SkillNameError::EdgeHyphen);
        }
        if value.contains("--") {
            return Err(SkillNameError::ConsecutiveHyphens);
        }
        Ok(Self(value.to_owned()))
    }

    /// The validated name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SkillName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for SkillName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SkillName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::new(&value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_specification_examples_when_validated_then_valid_names_pass_and_invalid_fail() {
        assert!(SkillName::new("pdf-processing").is_ok());
        assert!(SkillName::new("a").is_ok());
        assert!(SkillName::new(&"a".repeat(64)).is_ok());
        assert_eq!(SkillName::new(""), Err(SkillNameError::Empty));
        assert_eq!(
            SkillName::new(&"a".repeat(65)),
            Err(SkillNameError::TooLong)
        );
        assert_eq!(
            SkillName::new("PDF-Processing"),
            Err(SkillNameError::InvalidCharacter)
        );
        assert_eq!(SkillName::new("-pdf"), Err(SkillNameError::EdgeHyphen));
        assert_eq!(SkillName::new("pdf-"), Err(SkillNameError::EdgeHyphen));
        assert_eq!(
            SkillName::new("pdf--processing"),
            Err(SkillNameError::ConsecutiveHyphens)
        );
        assert_eq!(
            SkillName::new("caf\u{e9}"),
            Err(SkillNameError::InvalidCharacter)
        );
        assert_eq!(SkillName::new("a/b"), Err(SkillNameError::InvalidCharacter));
    }

    #[test]
    fn given_an_invalid_name_in_json_when_deserialized_then_it_is_refused() {
        let refused: Result<SkillName, _> = serde_json::from_str("\"Bad\"");
        assert!(refused.is_err());
        let accepted: SkillName = serde_json::from_str("\"good-name\"").expect("valid name");
        assert_eq!(accepted.as_str(), "good-name");
    }
}
