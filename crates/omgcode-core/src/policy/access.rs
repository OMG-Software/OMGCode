//! Three-state workspace preferences: deny, prompt, or allow (ADR-0062).
//!
//! A preference is a policy input only. `Prompt` and `Allow` both permit an
//! operator-initiated trusted command. They differ only for model-initiated
//! tool calls: `Prompt` requires a trusted single-use approval for each call,
//! and `Allow` runs the call without one. `Deny` refuses both.
//!
//! Persisted settings keep the exact bytes written before this type existed.
//! Each preference stores `false` for `Deny`, `true` for its legacy enabled
//! meaning, and a string only for the one state a boolean could not express.

use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// How a workspace preference treats an operation.
///
/// The variants are ordered from most to least restrictive, so the most
/// restrictive of several preferences is their minimum.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// Refuse the operation.
    Deny,
    /// Permit the operation; each model-initiated call needs an approval.
    Prompt,
    /// Permit the operation; model-initiated calls need no approval.
    Allow,
}

impl Access {
    /// Every state, from most to least restrictive.
    pub const ALL: [Self; 3] = [Self::Deny, Self::Prompt, Self::Allow];

    /// Return whether the preference permits the operation at all.
    pub const fn permits(self) -> bool {
        !matches!(self, Self::Deny)
    }

    /// Return the more restrictive of two states.
    #[must_use]
    pub const fn most_restrictive(self, other: Self) -> Self {
        if (self as u8) <= (other as u8) {
            self
        } else {
            other
        }
    }

    /// The stored and displayed spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Prompt => "prompt",
            Self::Allow => "allow",
        }
    }
}

impl fmt::Display for Access {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The supplied text is not `deny`, `prompt`, or `allow`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccessParseError;

impl fmt::Display for AccessParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected deny, prompt, or allow")
    }
}

impl std::error::Error for AccessParseError {}

impl FromStr for Access {
    type Err = AccessParseError;

    /// Parse the exact lowercase spelling only.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "deny" => Ok(Self::Deny),
            "prompt" => Ok(Self::Prompt),
            "allow" => Ok(Self::Allow),
            _ => Err(AccessParseError),
        }
    }
}

/// One persisted preference value: a legacy flag or a state name.
#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum Stored {
    Flag(bool),
    State(Access),
}

/// Encode `access` with `legacy` as the meaning of a stored `true`.
fn encode<S: Serializer>(access: Access, legacy: Access, serializer: S) -> Result<S::Ok, S::Error> {
    let stored = match access {
        Access::Deny => Stored::Flag(false),
        state if state == legacy => Stored::Flag(true),
        state => Stored::State(state),
    };
    stored.serialize(serializer)
}

/// Decode one canonical value. A name that a flag could have expressed is
/// refused, so every state has exactly one stored form.
fn decode<'de, D: Deserializer<'de>>(legacy: Access, deserializer: D) -> Result<Access, D::Error> {
    match Stored::deserialize(deserializer)? {
        Stored::Flag(false) => Ok(Access::Deny),
        Stored::Flag(true) => Ok(legacy),
        Stored::State(state) if state != Access::Deny && state != legacy => Ok(state),
        Stored::State(_) => Err(D::Error::custom("non-canonical preference value")),
    }
}

/// `read`: a stored `true` means `Allow`, because model reads never needed
/// an approval before this type existed.
pub(crate) mod legacy_allow {
    use super::{Access, Deserializer, Serializer, decode, encode};

    #[allow(clippy::trivially_copy_pass_by_ref)] // serde passes the field by reference.
    pub(crate) fn serialize<S: Serializer>(
        access: &Access,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        encode(*access, Access::Allow, serializer)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Access, D::Error> {
        decode(Access::Allow, deserializer)
    }
}

/// Every other preference: a stored `true` means `Prompt`, which keeps the
/// approval that model writes and commands needed before this type existed
/// and makes no future model network or host-service tool unattended.
pub(crate) mod legacy_prompt {
    use super::{Access, Deserializer, Serializer, decode, encode};

    #[allow(clippy::trivially_copy_pass_by_ref)] // serde passes the field by reference.
    pub(crate) fn serialize<S: Serializer>(
        access: &Access,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        encode(*access, Access::Prompt, serializer)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Access, D::Error> {
        decode(Access::Prompt, deserializer)
    }
}

/// One of the five model file-write tools.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WriteTool {
    /// `workspace_edit_file`.
    Edit,
    /// `workspace_multi_edit_file`.
    MultiEdit,
    /// `workspace_create_file`.
    Create,
    /// `workspace_rename_file`.
    Rename,
    /// `workspace_delete_file`.
    Delete,
}

impl WriteTool {
    /// Every write tool, in the order they are offered.
    pub const ALL: [Self; 5] = [
        Self::Edit,
        Self::MultiEdit,
        Self::Create,
        Self::Rename,
        Self::Delete,
    ];

    /// The stored and displayed name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::MultiEdit => "multi_edit",
            Self::Create => "create",
            Self::Rename => "rename",
            Self::Delete => "delete",
        }
    }
}

/// Per-tool limits on the five model file-write tools.
///
/// Each value can only narrow the workspace `write` preference: the effective
/// state of a tool is the more restrictive of the two. Every value defaults
/// to `Allow`, which adds no restriction, so the field is omitted from
/// persisted settings until one value is narrowed.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriteToolAccess {
    /// Limit on `workspace_edit_file`.
    pub edit: Access,
    /// Limit on `workspace_multi_edit_file`.
    pub multi_edit: Access,
    /// Limit on `workspace_create_file`.
    pub create: Access,
    /// Limit on `workspace_rename_file`.
    pub rename: Access,
    /// Limit on `workspace_delete_file`.
    pub delete: Access,
}

impl Default for WriteToolAccess {
    /// Add no restriction to the workspace `write` preference.
    fn default() -> Self {
        Self {
            edit: Access::Allow,
            multi_edit: Access::Allow,
            create: Access::Allow,
            rename: Access::Allow,
            delete: Access::Allow,
        }
    }
}

impl WriteToolAccess {
    /// Return the limit recorded for `tool`.
    pub const fn get(self, tool: WriteTool) -> Access {
        match tool {
            WriteTool::Edit => self.edit,
            WriteTool::MultiEdit => self.multi_edit,
            WriteTool::Create => self.create,
            WriteTool::Rename => self.rename,
            WriteTool::Delete => self.delete,
        }
    }

    /// Replace the limit for `tool`.
    pub const fn set(&mut self, tool: WriteTool, access: Access) {
        match tool {
            WriteTool::Edit => self.edit = access,
            WriteTool::MultiEdit => self.multi_edit = access,
            WriteTool::Create => self.create = access,
            WriteTool::Rename => self.rename = access,
            WriteTool::Delete => self.delete = access,
        }
    }

    /// Report whether no tool is narrowed, so the field can stay unrecorded.
    pub(crate) fn is_unrestricted(&self) -> bool {
        *self == Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_order_from_most_to_least_restrictive() {
        assert!(Access::Deny < Access::Prompt && Access::Prompt < Access::Allow);
        for left in Access::ALL {
            for right in Access::ALL {
                assert_eq!(left.most_restrictive(right), left.min(right));
            }
        }
    }

    #[test]
    fn only_deny_refuses() {
        assert!(!Access::Deny.permits());
        assert!(Access::Prompt.permits());
        assert!(Access::Allow.permits());
    }

    #[test]
    fn names_round_trip_and_nothing_else_parses() {
        for access in Access::ALL {
            assert_eq!(access.as_str().parse::<Access>(), Ok(access));
        }
        for bad in ["", "Allow", "ALLOW", "yes", "true", " allow"] {
            assert_eq!(bad.parse::<Access>(), Err(AccessParseError));
        }
    }

    #[test]
    fn write_tool_limits_default_to_no_restriction() {
        let limits = WriteToolAccess::default();
        assert!(limits.is_unrestricted());
        for tool in WriteTool::ALL {
            assert_eq!(limits.get(tool), Access::Allow);
        }
        let mut narrowed = limits;
        narrowed.set(WriteTool::Delete, Access::Prompt);
        assert!(!narrowed.is_unrestricted());
        assert_eq!(narrowed.get(WriteTool::Delete), Access::Prompt);
        assert_eq!(narrowed.get(WriteTool::Edit), Access::Allow);
    }
}
