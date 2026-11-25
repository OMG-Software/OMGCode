//! The `web_fetch` host allowlist changes from the `settings` window.
//!
//! Each method calls the same `ControlStore` operation as the trusted
//! operator path (`add_web_fetch_host`, `remove_web_fetch_host`) with the
//! same expected-revision check and the same pattern validation, and records
//! the same event. The window adds no second rule:
//!
//! - Rust parses the pattern first. An invalid pattern is refused with core's
//!   stable `web_host_*` code before any dialog opens, and the dialog shows
//!   the canonical text core parsed, not the typed text.
//! - Adding or removing a pattern is confirmed natively. The confirmation
//!   names the workspace folder and the pattern, and says what a wildcard
//!   covers.
//! - A listed host grants nothing by itself. It only lets a `web_fetch` call
//!   skip its approval while the workspace's `web_fetch` preference is
//!   `allow`.
//!
//! As for every settings change, the store is released before the dialog and
//! the change is applied with the revision the operator confirmed, so a
//! change made meanwhile is refused as `stale_revision`.

use omgcode_core::control::{
    ControlError, MAX_WEB_FETCH_HOSTS, MAX_WEB_HOST_BYTES, WebHostPattern, WebHostRefusal,
};
use omgcode_core::policy::WorkspaceId;
use omgcode_core::turn::display_safe_line;
use serde::Deserialize;
use uuid::Uuid;

use super::DesktopService;
use super::error::DesktopError;
use super::settings_changes::{ChangeGuard, Confirmation, SettingsPrompt, check_revision, confirm};
use super::state::folder_of_directory;
use super::wire::SettingsOverview;

/// Longest typed pattern examined, in bytes. Core's limit is the 253-byte
/// name plus a two-byte `*.` prefix; a longer text is refused as too long
/// without being parsed.
const MAX_TYPED_PATTERN_BYTES: usize = MAX_WEB_HOST_BYTES + 2;

/// Arguments of `settings_add_web_fetch_host` and
/// `settings_remove_web_fetch_host`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WebHostChangeRequest {
    /// The settings revision the window last read.
    pub revision: u64,
    /// A registered workspace.
    pub workspace_id: Uuid,
    /// The pattern as typed or listed. Core normalises it.
    pub pattern: String,
}

/// Parse a typed pattern, refusing an over-long text without parsing it.
fn parse_pattern(text: &str) -> Result<WebHostPattern, DesktopError> {
    if text.len() > MAX_TYPED_PATTERN_BYTES {
        return Err(refused(WebHostRefusal::TooLong));
    }
    WebHostPattern::parse(text).map_err(refused)
}

fn refused(refusal: WebHostRefusal) -> DesktopError {
    DesktopError::from_control(&ControlError::WebHost(refusal))
}

/// The sentence a wildcard adds, or nothing for an exact name.
fn coverage_text(pattern: &WebHostPattern) -> String {
    if pattern.is_wildcard() {
        let base = pattern.as_str().trim_start_matches("*.");
        format!(
            "\n\n{} covers every subdomain of {base}, at any depth, but not {base} itself.",
            display_safe_line(pattern.as_str())
        )
    } else {
        String::new()
    }
}

/// The native confirmation for adding a pattern.
fn add_confirmation(workspace_folder: &str, pattern: &WebHostPattern) -> Confirmation {
    Confirmation {
        title: "Allow a web host",
        message: format!(
            "Add the host pattern {} to the allowed web hosts of the workspace\n{workspace_folder}?{}\n\nThe list grants nothing while web fetch is deny. Under allow, the model's fetches from a listed host run without asking you. Under prompt, and for hosts not listed, every fetch still asks.",
            display_safe_line(pattern.as_str()),
            coverage_text(pattern),
        ),
    }
}

/// The native confirmation for removing a pattern.
fn remove_confirmation(workspace_folder: &str, pattern: &WebHostPattern) -> Confirmation {
    Confirmation {
        title: "Stop allowing a web host",
        message: format!(
            "Remove the host pattern {} from the allowed web hosts of the workspace\n{workspace_folder}?{}\n\nFetches from a host it named then ask for approval again.",
            display_safe_line(pattern.as_str()),
            coverage_text(pattern),
        ),
    }
}

impl DesktopService {
    /// `settings` only. After a native confirmation that names the workspace
    /// folder and the canonical pattern, add one host pattern to the
    /// workspace's `web_fetch` allowlist, with the revision the window read.
    ///
    /// # Errors
    ///
    /// A `web_host_*` code (invalid, duplicate, or too many patterns) before
    /// any dialog; `unknown_workspace`; `cancelled`; `stale_revision`;
    /// `settings_change_running`; `busy`; `control`.
    pub fn settings_add_web_fetch_host(
        &self,
        request: WebHostChangeRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let pattern = parse_pattern(&request.pattern)?;
        let (workspace_id, confirmation) = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let workspace = store
                .registered_workspace(request.workspace_id)
                .ok_or_else(DesktopError::unknown_workspace)?;
            let listed = store.web_fetch_hosts(workspace.id());
            if listed.contains(&pattern) {
                return Err(refused(WebHostRefusal::Duplicate));
            }
            if listed.len() >= MAX_WEB_FETCH_HOSTS {
                return Err(refused(WebHostRefusal::TooMany));
            }
            (
                workspace.id(),
                add_confirmation(&folder_of_directory(workspace.directory_path()), &pattern),
            )
        };
        confirm(prompt, &confirmation)?;
        self.apply_web_host(workspace_id, &pattern, request.revision, true)
    }

    /// `settings` only. After a native confirmation that names the workspace
    /// folder and the canonical pattern, remove one host pattern from the
    /// workspace's `web_fetch` allowlist, with the revision the window read.
    ///
    /// # Errors
    ///
    /// A `web_host_*` code (invalid pattern, or one the workspace does not
    /// list) before any dialog; `unknown_workspace`; `cancelled`;
    /// `stale_revision`; `settings_change_running`; `busy`; `control`.
    pub fn settings_remove_web_fetch_host(
        &self,
        request: WebHostChangeRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let pattern = parse_pattern(&request.pattern)?;
        let (workspace_id, confirmation) = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let workspace = store
                .registered_workspace(request.workspace_id)
                .ok_or_else(DesktopError::unknown_workspace)?;
            if !store.web_fetch_hosts(workspace.id()).contains(&pattern) {
                return Err(refused(WebHostRefusal::NotListed));
            }
            (
                workspace.id(),
                remove_confirmation(&folder_of_directory(workspace.directory_path()), &pattern),
            )
        };
        confirm(prompt, &confirmation)?;
        self.apply_web_host(workspace_id, &pattern, request.revision, false)
    }

    /// Apply the confirmed change under the revision the operator confirmed.
    fn apply_web_host(
        &self,
        workspace_id: WorkspaceId,
        pattern: &WebHostPattern,
        revision: u64,
        add: bool,
    ) -> Result<SettingsOverview, DesktopError> {
        self.apply_checked(|store| {
            let outcome = if add {
                store.add_web_fetch_host(workspace_id, pattern.as_str(), revision)
            } else {
                store.remove_web_fetch_host(workspace_id, pattern.as_str(), revision)
            };
            outcome
                .map(|_| ())
                .map_err(|error| DesktopError::from_control(&error))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(text: &str) -> WebHostPattern {
        WebHostPattern::parse(text).expect("a valid pattern")
    }

    #[test]
    fn the_confirmations_name_the_workspace_and_the_canonical_pattern() {
        let add = add_confirmation("C:\\code\\app", &pattern("Docs.Example.COM."));
        assert_eq!(add.title, "Allow a web host");
        assert!(add.message.contains("docs.example.com"), "{}", add.message);
        assert!(add.message.contains("C:\\code\\app"));
        assert!(!add.message.contains("subdomain"));
        let remove = remove_confirmation("C:\\code\\app", &pattern("docs.example.com"));
        assert_eq!(remove.title, "Stop allowing a web host");
        assert!(remove.message.contains("C:\\code\\app"));
    }

    #[test]
    fn a_wildcard_confirmation_says_it_covers_every_subdomain() {
        let add = add_confirmation("folder", &pattern("*.example.com"));
        assert!(
            add.message.contains(
                "*.example.com covers every subdomain of example.com, at any depth, but not example.com itself."
            ),
            "{}",
            add.message
        );
    }

    #[test]
    fn an_overlong_or_invalid_pattern_is_refused_with_core_code() {
        let long = "a".repeat(MAX_TYPED_PATTERN_BYTES + 1);
        assert_eq!(
            parse_pattern(&long).expect_err("refused").code(),
            "web_host_too_long"
        );
        assert_eq!(
            parse_pattern("localhost").expect_err("refused").code(),
            "web_host_reserved"
        );
        assert!(parse_pattern("Example.com").is_ok());
    }

    #[test]
    fn a_request_refuses_unknown_fields() {
        let text = r#"{"revision":1,"workspace_id":"00000000-0000-0000-0000-000000000001","pattern":"a.example.com","path":"x"}"#;
        assert!(serde_json::from_str::<WebHostChangeRequest>(text).is_err());
    }
}
