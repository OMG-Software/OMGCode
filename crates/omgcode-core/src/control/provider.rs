//! Operator-declared provider connection profiles.
//!
//! A profile records a name, a provider kind, an authentication mode, and
//! optionally an endpoint. It stores no account, token, password, or other
//! credential. Its status can name the opaque identifier of a credential bound
//! through `credential`, never the secret.
//!
//! An `lm_studio` or `ollama_local` profile may record the endpoint the local
//! adapter uses. Both servers speak the same OpenAI-compatible Chat
//! Completions API, so one adapter and one rule serve both (ADR-0063). The
//! text must pass the rule the local adapter applies to every request
//! (`provider::validate_base_url`), is stored exactly as validated, and is
//! fixed for the life of the profile: no operation changes it, so a bound
//! credential cannot be moved to another destination. Every other kind refuses
//! an endpoint until an adapter defines its rule. Replay applies the same
//! validation. The local adapter reads a selected profile only through
//! `ControlStore::provider_credential_binding` (ADR-0047, ADR-0048). See
//! `docs/provider-authentication.md`, ADR-0038, and ADR-0042.
//!
//! The store also records which local profile is the *active* connection: the
//! one a desktop process started without a connection argument uses for each
//! turn (ADR-0059 decision 7, ADR-0063). Only a local profile that records an
//! endpoint can be selected. Removing the selected profile clears the
//! selection in the same event, so the selection never names a missing
//! profile.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::credential::{self, CredentialBinding, ProviderProfileRemoval};
use super::{Action, ControlError, ControlSnapshot, ControlStore, valid_profile_name, valid_uuid};
use crate::provider::validate_base_url;

/// Most recorded provider profiles.
pub const MAX_PROVIDER_PROFILES: usize = 32;

/// Most bytes of a recorded provider endpoint, checked before it is parsed.
pub const MAX_PROVIDER_ENDPOINT_BYTES: usize = 256;

/// The endpoint a profile records, exactly as it was validated.
///
/// Only [`ProviderProfile`] holds one. A value read from a journal is checked
/// by replay before the profile is used, and the store never returns an
/// unchecked profile. It is not a secret.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ProviderEndpoint(pub(super) String);

impl ProviderEndpoint {
    /// The validated text, unchanged.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A provider that a profile may name.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ProviderKind {
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "ollama_local")]
    OllamaLocal,
    #[serde(rename = "ollama_cloud")]
    OllamaCloud,
    #[serde(rename = "open_router")]
    OpenRouter,
    #[serde(rename = "lm_studio")]
    LmStudio,
}

impl ProviderKind {
    /// The stored spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::OllamaLocal => "ollama_local",
            Self::OllamaCloud => "ollama_cloud",
            Self::OpenRouter => "open_router",
            Self::LmStudio => "lm_studio",
        }
    }

    /// Whether the local OpenAI-compatible adapter serves this kind: LM Studio
    /// and Ollama on this computer (ADR-0063).
    pub const fn is_local(self) -> bool {
        matches!(self, Self::LmStudio | Self::OllamaLocal)
    }

    /// Parse a stored or command-line spelling, case-insensitively.
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "openai" => Some(Self::OpenAi),
            "anthropic" => Some(Self::Anthropic),
            "ollama_local" => Some(Self::OllamaLocal),
            "ollama_cloud" => Some(Self::OllamaCloud),
            "open_router" => Some(Self::OpenRouter),
            "lm_studio" => Some(Self::LmStudio),
            _ => None,
        }
    }
}

/// How a provider connection would authenticate, when implemented.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAuth {
    /// A provider-supported subscription sign-in.
    Subscription,
    /// A separately billed API key.
    ApiKey,
    /// No provider credential; a local or unauthenticated endpoint.
    None,
}

impl ProviderAuth {
    /// The stored spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Subscription => "subscription",
            Self::ApiKey => "api_key",
            Self::None => "none",
        }
    }

    /// Parse a stored or command-line spelling, case-insensitively.
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "subscription" => Some(Self::Subscription),
            "api_key" | "api-key" => Some(Self::ApiKey),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// One recorded provider connection shape. Credentials are never stored here.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderProfile {
    id: Uuid,
    name: String,
    kind: ProviderKind,
    auth: ProviderAuth,
    /// Written only when present, so a profile without one keeps the bytes it
    /// had before endpoints existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) endpoint: Option<ProviderEndpoint>,
}

impl ProviderProfile {
    /// The recorded endpoint, if any. It never changes for this profile.
    pub const fn endpoint(&self) -> Option<&ProviderEndpoint> {
        self.endpoint.as_ref()
    }

    /// Stable profile identifier.
    pub const fn id(&self) -> Uuid {
        self.id
    }

    /// The operator's name for the profile.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The provider kind.
    pub const fn kind(&self) -> ProviderKind {
        self.kind
    }

    /// The authentication mode.
    pub const fn auth(&self) -> ProviderAuth {
        self.auth
    }
}

/// A profile, the settings revision that recorded it, and its credential
/// binding. The binding holds an opaque identifier, never a secret.
#[derive(Clone, Debug, Serialize)]
pub struct ProviderProfileStatus {
    #[serde(flatten)]
    pub(super) profile: ProviderProfile,
    revision: u64,
    #[serde(serialize_with = "credential::serialize_state")]
    pub(super) credential: Option<CredentialBinding>,
}

impl ProviderProfileStatus {
    /// The recorded profile.
    pub const fn profile(&self) -> &ProviderProfile {
        &self.profile
    }

    /// The settings revision of the event that added it.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// The bound credential, if any.
    pub const fn credential(&self) -> Option<&CredentialBinding> {
        self.credential.as_ref()
    }
}

/// An operator's provider profile declaration, before parsing.
#[derive(Clone, Debug, Default)]
pub struct ProviderProfileDeclaration {
    /// The profile name, with the same rules as other profile names.
    pub name: String,
    /// A provider spelling such as `openai` or `ollama_local`.
    pub kind: String,
    /// `subscription`, `api_key`, or `none`.
    pub auth: String,
    /// The endpoint to record. Only `lm_studio` and `ollama_local` accept one
    /// for now, and it must pass the local adapter's rule. It is stored
    /// exactly as given.
    pub endpoint: Option<String>,
}

impl ControlStore {
    /// Every recorded provider profile, in the order they were added.
    pub fn provider_profiles(&self) -> &[ProviderProfileStatus] {
        &self.snapshot.provider_profiles
    }

    /// The profile with this name, compared case-insensitively.
    pub fn provider_profile(&self, name: &str) -> Option<&ProviderProfileStatus> {
        self.snapshot
            .provider_profiles
            .iter()
            .find(|status| status.profile.name.eq_ignore_ascii_case(name))
    }

    /// Record a provider connection shape. This advances the settings revision
    /// and records no credential or runtime authority.
    pub fn add_provider_profile(
        &mut self,
        declaration: &ProviderProfileDeclaration,
        expected_revision: u64,
    ) -> Result<ProviderProfile, ControlError> {
        self.check_revision(expected_revision)?;
        if self.provider_profiles().len() >= MAX_PROVIDER_PROFILES {
            return Err(ControlError::ProviderRefused(
                "at most 32 provider profiles may be recorded",
            ));
        }
        if self.provider_profile(&declaration.name).is_some() {
            return Err(ControlError::ProviderRefused(
                "a provider profile with this name already exists; remove it first",
            ));
        }
        let kind = ProviderKind::parse(&declaration.kind)
            .ok_or(ControlError::ProviderRefused("unknown provider kind"))?;
        let auth = ProviderAuth::parse(&declaration.auth).ok_or(ControlError::ProviderRefused(
            "authentication must be subscription, api_key, or none",
        ))?;
        let profile = ProviderProfile {
            id: Uuid::new_v4(),
            name: declaration.name.clone(),
            kind,
            auth,
            endpoint: declaration.endpoint.clone().map(ProviderEndpoint),
        };
        validate_profile(&profile)?;
        self.append(
            Action::ProviderProfileAdded {
                profile: profile.clone(),
            },
            self.next_revision()?,
        )?;
        Ok(profile)
    }

    /// The active local connection, if one is selected (ADR-0063). It always
    /// names a recorded local profile with an endpoint.
    pub fn active_provider_profile(&self) -> Option<&ProviderProfileStatus> {
        let id = self.snapshot.active_provider?;
        self.snapshot
            .provider_profiles
            .iter()
            .find(|status| status.profile.id == id)
    }

    /// Select the active local connection by profile name, or return to the
    /// default endpoint with `None`. This advances the settings revision.
    ///
    /// The profile must be an `lm_studio` or `ollama_local` profile that
    /// records an endpoint. Selecting grants nothing by itself: each turn
    /// still resolves the endpoint and any credential from that one profile
    /// (ADR-0048). Selecting the current choice again is refused, so every
    /// recorded event is a change.
    ///
    /// # Errors
    ///
    /// `Conflict` for a stale revision, `UnknownProvider` for an unknown
    /// name, `ProviderRefused` for a profile that cannot be selected or an
    /// unchanged selection, and the append errors.
    pub fn select_active_provider(
        &mut self,
        name: Option<&str>,
        expected_revision: u64,
    ) -> Result<(), ControlError> {
        self.check_revision(expected_revision)?;
        let profile_id = match name {
            None => None,
            Some(name) => {
                let status = self
                    .provider_profile(name)
                    .ok_or(ControlError::UnknownProvider)?;
                if !selectable(&status.profile) {
                    return Err(ControlError::ProviderRefused(
                        "only an lm_studio or ollama_local profile with a recorded endpoint can be the active connection",
                    ));
                }
                Some(status.profile.id)
            }
        };
        if profile_id == self.snapshot.active_provider {
            return Err(ControlError::ProviderRefused(
                "this is already the active connection",
            ));
        }
        self.append(
            Action::ActiveProviderSelected { profile_id },
            self.next_revision()?,
        )
    }

    /// Remove a provider profile by name. This advances the settings revision.
    ///
    /// A bound credential's binding is removed by the same recorded event,
    /// which names its identifier. The caller then deletes the native item
    /// with [`credential::delete_unbound_credential`]. Removing the active
    /// connection's profile returns the selection to the default endpoint.
    pub fn remove_provider_profile(
        &mut self,
        name: &str,
        expected_revision: u64,
    ) -> Result<ProviderProfileRemoval, ControlError> {
        self.check_revision(expected_revision)?;
        let status = self
            .provider_profile(name)
            .ok_or(ControlError::UnknownProvider)?;
        let profile_id = status.profile.id;
        let credential = status.credential.clone();
        self.append(
            Action::ProviderProfileRemoved {
                profile_id,
                credential_id: credential.as_ref().map(|binding| binding.id().clone()),
            },
            self.next_revision()?,
        )?;
        Ok(ProviderProfileRemoval {
            profile_id,
            credential,
        })
    }
}

/// Validate and apply one provider action during replay or append preflight.
pub(super) fn apply(
    state: &mut ControlSnapshot,
    action: &Action,
    revision: u64,
) -> Result<(), ControlError> {
    match action {
        Action::ProviderProfileAdded { profile } => {
            validate_profile(profile)
                .map_err(|_| ControlError::Invalid("invalid provider profile"))?;
            if state.provider_profiles.len() >= MAX_PROVIDER_PROFILES
                || state.provider_profiles.iter().any(|status| {
                    status.profile.id == profile.id
                        || status.profile.name.eq_ignore_ascii_case(&profile.name)
                })
            {
                return Err(ControlError::Invalid(
                    "duplicate provider profile or too many provider profiles",
                ));
            }
            state.provider_profiles.push(ProviderProfileStatus {
                profile: profile.clone(),
                revision,
                credential: None,
            });
        }
        Action::ProviderProfileRemoved {
            profile_id,
            credential_id,
        } => {
            let index = state
                .provider_profiles
                .iter()
                .position(|status| status.profile.id == *profile_id)
                .ok_or(ControlError::Invalid(
                    "removal of an unknown provider profile",
                ))?;
            // A bound credential must be named by the removal itself, so the
            // journal always shows which native item became unbound.
            let bound = state
                .provider_profiles
                .get(index)
                .and_then(|status| status.credential.as_ref())
                .map(CredentialBinding::id);
            if bound != credential_id.as_ref() {
                return Err(ControlError::Invalid(
                    "provider profile removal does not match its credential binding",
                ));
            }
            state.provider_profiles.remove(index);
            if state.active_provider == Some(*profile_id) {
                state.active_provider = None;
            }
        }
        Action::ActiveProviderSelected { profile_id } => {
            if let Some(profile_id) = profile_id {
                let selectable = state
                    .provider_profiles
                    .iter()
                    .find(|status| status.profile.id == *profile_id)
                    .is_some_and(|status| selectable(&status.profile));
                if !selectable {
                    return Err(ControlError::Invalid(
                        "the active connection names no selectable provider profile",
                    ));
                }
            }
            if state.active_provider == *profile_id {
                return Err(ControlError::Invalid(
                    "the active connection selection changes nothing",
                ));
            }
            state.active_provider = *profile_id;
        }
        _ => {}
    }
    Ok(())
}

/// Whether a profile can be the active connection: a local kind that records
/// the endpoint its turns use.
fn selectable(profile: &ProviderProfile) -> bool {
    profile.kind.is_local() && profile.endpoint.is_some()
}

/// Check a profile name and the provider/authentication pairing.
fn validate_profile(profile: &ProviderProfile) -> Result<(), ControlError> {
    if !valid_uuid(profile.id) || !valid_profile_name(&profile.name) {
        return Err(ControlError::ProviderRefused(
            "provider profile names are 1 to 64 ASCII letters, digits, '.', '-', or '_', starting with a letter or digit",
        ));
    }
    if !auth_allowed(profile.kind, profile.auth) {
        return Err(ControlError::ProviderRefused(
            "this provider does not support the selected authentication mode",
        ));
    }
    if let Some(endpoint) = &profile.endpoint {
        validate_endpoint(profile.kind, endpoint.as_str())?;
    }
    Ok(())
}

/// Check an endpoint for its provider kind. The length bound comes first, so
/// no parser sees an unbounded string.
fn validate_endpoint(kind: ProviderKind, text: &str) -> Result<(), ControlError> {
    match kind {
        ProviderKind::LmStudio | ProviderKind::OllamaLocal => {
            if text.len() > MAX_PROVIDER_ENDPOINT_BYTES {
                return Err(ControlError::ProviderRefused(
                    "the provider endpoint is longer than 256 bytes",
                ));
            }
            validate_base_url(text).map(|_| ()).map_err(|_| {
                ControlError::ProviderRefused(
                    "a local provider endpoint must be http://<loopback IP>:<port>/v1 with no user information, query, or fragment",
                )
            })
        }
        ProviderKind::OpenAi
        | ProviderKind::Anthropic
        | ProviderKind::OllamaCloud
        | ProviderKind::OpenRouter => Err(ControlError::ProviderRefused(
            "this provider kind does not accept an endpoint yet",
        )),
    }
}

fn auth_allowed(kind: ProviderKind, auth: ProviderAuth) -> bool {
    match kind {
        ProviderKind::OpenAi | ProviderKind::Anthropic => {
            matches!(auth, ProviderAuth::Subscription | ProviderAuth::ApiKey)
        }
        ProviderKind::OllamaCloud | ProviderKind::OpenRouter => auth == ProviderAuth::ApiKey,
        ProviderKind::OllamaLocal => auth == ProviderAuth::None,
        ProviderKind::LmStudio => matches!(auth, ProviderAuth::None | ProviderAuth::ApiKey),
    }
}

#[cfg(test)]
mod tests;
