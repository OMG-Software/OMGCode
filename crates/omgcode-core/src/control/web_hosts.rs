//! The per-workspace allowlist of hosts the model `web_fetch` tool may reach
//! without a per-call approval.
//!
//! A pattern is configuration only. It never authorizes a fetch by itself: the
//! workspace's `web_fetch` preference and the turn's own checks decide that.
//! Patterns are ASCII DNS names; an operator types punycode for an
//! internationalized name. Public suffixes are not consulted, so a pattern
//! such as `*.co.uk` is accepted and reaches every host beneath it.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::{Action, ControlError, ControlSnapshot, ControlStore, WorkspaceRegistration};
use crate::policy::WorkspaceId;

/// The most host patterns one workspace may record.
pub const MAX_WEB_FETCH_HOSTS: usize = 32;
/// The longest host pattern, without the wildcard prefix.
pub const MAX_WEB_HOST_BYTES: usize = 253;
const MAX_LABEL_BYTES: usize = 63;

/// Name suffixes that never name a public web host. A pattern equal to one of
/// them, or beneath one, is refused.
const RESERVED_SUFFIXES: [&str; 7] = [
    "localhost",
    "local",
    "internal",
    "home.arpa",
    "lan",
    "localdomain",
    "onion",
];

/// Why a host pattern or an allowlist change was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WebHostRefusal {
    /// The pattern is empty.
    Empty,
    /// The pattern contains a byte outside ASCII. Use punycode.
    NotAscii,
    /// The pattern is longer than 253 bytes.
    TooLong,
    /// A label is empty, longer than 63 bytes, or starts or ends with a hyphen.
    BadLabel,
    /// The pattern contains a character other than `a-z`, `0-9`, `-`, `.`, or
    /// one leading `*.`.
    BadCharacter,
    /// The pattern is, or ends like, an IP address.
    IpLiteral,
    /// The name is local or private by convention, such as `localhost`.
    Reserved,
    /// The name has fewer than two labels after any `*.`.
    TooFewLabels,
    /// Stored text that is not in canonical form, so a stored pattern has one
    /// spelling. Operator input is normalised instead.
    NotCanonical,
    /// The workspace already lists this pattern.
    Duplicate,
    /// The workspace does not list this pattern.
    NotListed,
    /// The workspace already lists the most patterns allowed.
    TooMany,
}

impl WebHostRefusal {
    /// A stable machine-readable code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "web_host_empty",
            Self::NotAscii => "web_host_not_ascii",
            Self::TooLong => "web_host_too_long",
            Self::BadLabel => "web_host_bad_label",
            Self::BadCharacter => "web_host_bad_character",
            Self::IpLiteral => "web_host_ip_literal",
            Self::Reserved => "web_host_reserved",
            Self::TooFewLabels => "web_host_too_few_labels",
            Self::NotCanonical => "web_host_not_canonical",
            Self::Duplicate => "web_host_duplicate",
            Self::NotListed => "web_host_not_listed",
            Self::TooMany => "web_host_too_many",
        }
    }

    /// A short fixed sentence that never repeats the rejected input.
    const fn reason(self) -> &'static str {
        match self {
            Self::Empty => "the host pattern is empty",
            Self::NotAscii => {
                "the host pattern must be ASCII; use punycode for an international name"
            }
            Self::TooLong => "the host pattern is longer than 253 bytes",
            Self::BadLabel => {
                "each label must be 1 to 63 bytes and must not start or end with a hyphen"
            }
            Self::BadCharacter => "use only letters, digits, hyphens, dots, and one leading `*.`",
            Self::IpLiteral => "an IP address is not a host pattern",
            Self::Reserved => "local and private names are not allowed",
            Self::TooFewLabels => "the pattern needs at least two labels after any `*.`",
            Self::NotCanonical => "the stored host pattern is not in canonical form",
            Self::Duplicate => "the workspace already lists this host pattern",
            Self::NotListed => "the workspace does not list this host pattern",
            Self::TooMany => "a workspace may list at most 32 host patterns",
        }
    }
}

impl fmt::Display for WebHostRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.reason())
    }
}

impl std::error::Error for WebHostRefusal {}

/// One validated allowlist entry: an exact DNS name, or `*.` and a DNS name
/// that matches its proper subdomains only.
///
/// Validation lowercases the text and removes one trailing dot, so equal
/// patterns have equal [`Self::as_str`] text. Deserialization validates, so a
/// settings log cannot hold an invalid pattern.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct WebHostPattern {
    /// The canonical text, including any `*.` prefix.
    text: String,
    wildcard: bool,
}

impl TryFrom<String> for WebHostPattern {
    type Error = WebHostRefusal;

    /// Decode stored text, which must already be canonical.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let parsed = Self::parse(&value)?;
        if parsed.text == value {
            Ok(parsed)
        } else {
            Err(WebHostRefusal::NotCanonical)
        }
    }
}

impl From<WebHostPattern> for String {
    fn from(pattern: WebHostPattern) -> Self {
        pattern.text
    }
}

impl WebHostPattern {
    /// Validate operator-typed pattern text.
    ///
    /// # Errors
    ///
    /// Returns the [`WebHostRefusal`] for the first rule the text breaks.
    pub fn parse(text: &str) -> Result<Self, WebHostRefusal> {
        if text.is_empty() {
            return Err(WebHostRefusal::Empty);
        }
        if !text.is_ascii() {
            return Err(WebHostRefusal::NotAscii);
        }
        let lowered = text.to_ascii_lowercase();
        let unwrapped = lowered.strip_suffix('.').unwrap_or(&lowered);
        let (wildcard, name) = match unwrapped.strip_prefix("*.") {
            Some(name) => (true, name),
            None => (false, unwrapped),
        };
        if name.is_empty() {
            return Err(WebHostRefusal::Empty);
        }
        if name.len() > MAX_WEB_HOST_BYTES {
            return Err(WebHostRefusal::TooLong);
        }
        if !name.bytes().all(|byte| is_name_byte(byte) || byte == b'.') {
            return Err(WebHostRefusal::BadCharacter);
        }
        let mut count = 0_usize;
        let mut last = "";
        for label in name.split('.') {
            if label.is_empty()
                || label.len() > MAX_LABEL_BYTES
                || label.starts_with('-')
                || label.ends_with('-')
            {
                return Err(WebHostRefusal::BadLabel);
            }
            count += 1;
            last = label;
        }
        if looks_numeric(last) {
            return Err(WebHostRefusal::IpLiteral);
        }
        if RESERVED_SUFFIXES.iter().any(|suffix| {
            name == *suffix
                || name
                    .strip_suffix(suffix)
                    .is_some_and(|rest| rest.ends_with('.'))
        }) {
            return Err(WebHostRefusal::Reserved);
        }
        if count < 2 {
            return Err(WebHostRefusal::TooFewLabels);
        }
        let text = if wildcard {
            format!("*.{name}")
        } else {
            name.to_owned()
        };
        Ok(Self { text, wildcard })
    }

    /// The canonical text, such as `docs.example.com` or `*.example.com`.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Whether the pattern matches subdomains instead of one exact name.
    pub const fn is_wildcard(&self) -> bool {
        self.wildcard
    }

    /// Whether `host` is named by this pattern.
    ///
    /// The comparison ignores ASCII case and one trailing dot on `host`. An
    /// exact pattern matches only the identical name. A wildcard matches any
    /// proper subdomain, at any depth, and never the bare name. `host` is not
    /// resolved and is not trusted: text that is not a plain DNS name matches
    /// nothing.
    pub fn matches(&self, host: &str) -> bool {
        let lowered = host.to_ascii_lowercase();
        let host = lowered.strip_suffix('.').unwrap_or(&lowered);
        if !self.wildcard {
            return host == self.text;
        }
        let Some(suffix) = self.text.strip_prefix("*.") else {
            return false;
        };
        host.strip_suffix(suffix)
            .and_then(|prefix| prefix.strip_suffix('.'))
            .is_some_and(|prefix| {
                prefix
                    .split('.')
                    .all(|label| !label.is_empty() && label.bytes().all(is_name_byte))
            })
    }
}

/// A byte allowed inside a label.
const fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
}

/// Whether a last label would make a browser or resolver read the whole name
/// as an IPv4 address: all digits, or a hexadecimal `0x` form.
fn looks_numeric(label: &str) -> bool {
    label.bytes().all(|byte| byte.is_ascii_digit())
        || label
            .strip_prefix("0x")
            .is_some_and(|digits| digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

impl ControlStore {
    /// The host patterns this workspace allows `web_fetch` to reach without a
    /// per-call approval, in the order they were added. Empty for an unknown
    /// workspace. Configuration only.
    pub fn web_fetch_hosts(&self, workspace_id: WorkspaceId) -> &[WebHostPattern] {
        self.workspace(workspace_id)
            .map_or(&[], |workspace| workspace.web_fetch_hosts.as_slice())
    }

    /// Whether any pattern of this workspace names `host`. It does not consult
    /// the `web_fetch` preference, which the caller applies.
    pub fn web_fetch_host_allowed(&self, workspace_id: WorkspaceId, host: &str) -> bool {
        self.web_fetch_hosts(workspace_id)
            .iter()
            .any(|pattern| pattern.matches(host))
    }

    /// Add one host pattern to a workspace's allowlist. This advances the
    /// settings revision and grants no authority by itself.
    ///
    /// # Errors
    ///
    /// Refuses a stale revision, an unknown workspace, an invalid or duplicate
    /// pattern, and a workspace that already lists 32.
    pub fn add_web_fetch_host(
        &mut self,
        workspace_id: WorkspaceId,
        pattern: &str,
        expected_revision: u64,
    ) -> Result<WebHostPattern, ControlError> {
        self.check_revision(expected_revision)?;
        if self.workspace(workspace_id).is_none() {
            return Err(ControlError::UnknownWorkspace);
        }
        let pattern = WebHostPattern::parse(pattern).map_err(ControlError::WebHost)?;
        self.append(
            Action::WebFetchHostAdded {
                workspace_id,
                pattern: pattern.clone(),
            },
            self.next_revision()?,
        )?;
        Ok(pattern)
    }

    /// Remove one host pattern, compared in canonical form. This advances the
    /// settings revision.
    ///
    /// # Errors
    ///
    /// Refuses a stale revision, an unknown workspace, an invalid pattern, and
    /// a pattern the workspace does not list.
    pub fn remove_web_fetch_host(
        &mut self,
        workspace_id: WorkspaceId,
        pattern: &str,
        expected_revision: u64,
    ) -> Result<WebHostPattern, ControlError> {
        self.check_revision(expected_revision)?;
        if self.workspace(workspace_id).is_none() {
            return Err(ControlError::UnknownWorkspace);
        }
        let pattern = WebHostPattern::parse(pattern).map_err(ControlError::WebHost)?;
        self.append(
            Action::WebFetchHostRemoved {
                workspace_id,
                pattern: pattern.clone(),
            },
            self.next_revision()?,
        )?;
        Ok(pattern)
    }
}

/// Validate and apply one allowlist action during replay or append preflight.
/// The patterns were validated when they were decoded.
pub(super) fn apply(state: &mut ControlSnapshot, action: &Action) -> Result<(), ControlError> {
    match action {
        Action::WebFetchHostAdded {
            workspace_id,
            pattern,
        } => {
            let workspace = registration_mut(state, *workspace_id)?;
            if workspace.web_fetch_hosts.contains(pattern) {
                return Err(ControlError::WebHost(WebHostRefusal::Duplicate));
            }
            if workspace.web_fetch_hosts.len() >= MAX_WEB_FETCH_HOSTS {
                return Err(ControlError::WebHost(WebHostRefusal::TooMany));
            }
            workspace.web_fetch_hosts.push(pattern.clone());
        }
        Action::WebFetchHostRemoved {
            workspace_id,
            pattern,
        } => {
            let workspace = registration_mut(state, *workspace_id)?;
            let index = workspace
                .web_fetch_hosts
                .iter()
                .position(|existing| existing == pattern)
                .ok_or(ControlError::WebHost(WebHostRefusal::NotListed))?;
            workspace.web_fetch_hosts.remove(index);
        }
        _ => return Err(ControlError::Invalid("not a web host action")),
    }
    Ok(())
}

/// The registration an allowlist action names, or a replay failure.
fn registration_mut(
    state: &mut ControlSnapshot,
    workspace_id: WorkspaceId,
) -> Result<&mut WorkspaceRegistration, ControlError> {
    state
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == workspace_id)
        .ok_or(ControlError::Invalid("web host for an unknown workspace"))
}

#[cfg(test)]
mod tests;
