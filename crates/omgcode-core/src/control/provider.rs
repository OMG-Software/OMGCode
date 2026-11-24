//! Operator-declared provider connection profiles.
//!
//! A profile records a name, a provider kind, an authentication mode, and
//! optionally an endpoint. It stores no account, token, password, or other
//! credential. Its status can name the opaque identifier of a credential bound
//! through `credential`, never the secret.
//!
//! Every kind is served by one adapter that speaks the OpenAI-compatible Chat
//! Completions API (ADR-0064, ADR-0071, ADR-0092). An `lm_studio` or
//! `ollama_local` profile may record a loopback endpoint that passes
//! `provider::validate_base_url`. A named hosted kind accepts exactly its one
//! fixed HTTPS endpoint ([`ProviderKind::fixed_endpoint`]), and an
//! `openai_compatible` profile accepts an HTTPS base URL the operator typed,
//! which must pass `provider::validate_custom_url`. The text is stored exactly
//! as validated and is fixed for the life of the profile: no operation
//! changes it, so a bound credential cannot be moved to another destination.
//! Replay applies the same validation. The adapter reads a selected profile
//! only through `ControlStore::provider_credential_binding` (ADR-0047,
//! ADR-0048). See `docs/provider-authentication.md`, ADR-0038, and ADR-0042.
//!
//! The store also records which profile is the *active* connection: the one a
//! desktop process started without a connection argument uses for each turn
//! (ADR-0059 decision 7, ADR-0064). Only a profile that records an endpoint,
//! and for a hosted kind has a bound credential, can be selected. Removing the selected profile clears the
//! selection in the same event, so the selection never names a missing
//! profile.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::credential::{self, CredentialBinding, ProviderProfileRemoval};
use super::{Action, ControlError, ControlSnapshot, ControlStore, valid_profile_name, valid_uuid};
use crate::provider::{
    ANTHROPIC_ENDPOINT, DEEPSEEK_ENDPOINT, GOOGLE_ENDPOINT, META_ENDPOINT, MINIMAX_ENDPOINT,
    MOONSHOT_ENDPOINT, OLLAMA_CLOUD_ENDPOINT, OPENAI_ENDPOINT, OPENROUTER_ENDPOINT,
    TENCENT_ENDPOINT, THINKING_MACHINES_ENDPOINT, XAI_ENDPOINT, Z_AI_ENDPOINT, matches_fixed,
    validate_base_url, validate_custom_url,
};

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
///
/// A journal that records a kind added in 0.59.0 cannot be read by 0.58.0 or
/// earlier, which refuse the unknown spelling.
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
    #[serde(rename = "google")]
    Google,
    #[serde(rename = "meta")]
    Meta,
    #[serde(rename = "moonshot")]
    Moonshot,
    #[serde(rename = "tencent")]
    Tencent,
    #[serde(rename = "thinking_machines")]
    ThinkingMachines,
    #[serde(rename = "minimax")]
    MiniMax,
    #[serde(rename = "z_ai")]
    ZAi,
    #[serde(rename = "deepseek")]
    DeepSeek,
    #[serde(rename = "xai")]
    XAi,
    /// Any service that speaks the OpenAI-compatible API at an HTTPS base URL
    /// the operator types when adding the profile.
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible,
}

impl ProviderKind {
    /// Every kind: the local kinds, then the named hosted providers, then
    /// `openai_compatible`. The CLI reports kinds in this order; the desktop
    /// dropdown keeps its own order (`NEW_CONNECTION_KINDS` in the UI's
    /// `providers.ts`), which lists Ollama before LM Studio.
    pub const ALL: [Self; 16] = [
        Self::LmStudio,
        Self::OllamaLocal,
        Self::OllamaCloud,
        Self::Anthropic,
        Self::OpenAi,
        Self::Google,
        Self::Meta,
        Self::Moonshot,
        Self::Tencent,
        Self::ThinkingMachines,
        Self::MiniMax,
        Self::OpenRouter,
        Self::ZAi,
        Self::DeepSeek,
        Self::XAi,
        Self::OpenAiCompatible,
    ];

    /// The stored spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::OllamaLocal => "ollama_local",
            Self::OllamaCloud => "ollama_cloud",
            Self::OpenRouter => "open_router",
            Self::LmStudio => "lm_studio",
            Self::Google => "google",
            Self::Meta => "meta",
            Self::Moonshot => "moonshot",
            Self::Tencent => "tencent",
            Self::ThinkingMachines => "thinking_machines",
            Self::MiniMax => "minimax",
            Self::ZAi => "z_ai",
            Self::DeepSeek => "deepseek",
            Self::XAi => "xai",
            Self::OpenAiCompatible => "openai_compatible",
        }
    }

    /// Whether this kind is a server on this computer, LM Studio or Ollama,
    /// reached at a loopback endpoint (ADR-0064).
    pub const fn is_local(self) -> bool {
        matches!(self, Self::LmStudio | Self::OllamaLocal)
    }

    /// Whether this kind is a hosted service: every request leaves this
    /// computer over HTTPS and carries a stored API key (ADR-0071, ADR-0092).
    pub const fn is_hosted(self) -> bool {
        !self.is_local()
    }

    /// The one endpoint a named hosted kind accepts, or `None` for a local
    /// kind or `openai_compatible`, whose endpoint the operator supplies.
    pub const fn fixed_endpoint(self) -> Option<&'static str> {
        match self {
            Self::LmStudio | Self::OllamaLocal | Self::OpenAiCompatible => None,
            Self::OllamaCloud => Some(OLLAMA_CLOUD_ENDPOINT),
            Self::Anthropic => Some(ANTHROPIC_ENDPOINT),
            Self::OpenAi => Some(OPENAI_ENDPOINT),
            Self::Google => Some(GOOGLE_ENDPOINT),
            Self::Meta => Some(META_ENDPOINT),
            Self::Moonshot => Some(MOONSHOT_ENDPOINT),
            Self::Tencent => Some(TENCENT_ENDPOINT),
            Self::ThinkingMachines => Some(THINKING_MACHINES_ENDPOINT),
            Self::MiniMax => Some(MINIMAX_ENDPOINT),
            Self::OpenRouter => Some(OPENROUTER_ENDPOINT),
            Self::ZAi => Some(Z_AI_ENDPOINT),
            Self::DeepSeek => Some(DEEPSEEK_ENDPOINT),
            Self::XAi => Some(XAI_ENDPOINT),
        }
    }

    /// Parse a stored or command-line spelling, case-insensitively.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.to_ascii_lowercase();
        Self::ALL.into_iter().find(|kind| kind.as_str() == text)
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
    /// The endpoint to record: a loopback endpoint for a local kind, the
    /// kind's fixed endpoint for a named hosted kind, or an HTTPS base URL for
    /// `openai_compatible`. It is stored exactly as given.
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
        let endpoint = match (&declaration.endpoint, auth, kind.is_local()) {
            (Some(text), _, _) => Some(text.clone()),
            // A hosted API-key profile with no endpoint could never be used,
            // and its endpoint cannot be added later. A named kind takes its
            // one documented endpoint, as the desktop records it (ADR-0092).
            (None, ProviderAuth::ApiKey, false) => Some(
                kind.fixed_endpoint()
                    .ok_or(ControlError::ProviderRefused(
                        "an openai_compatible profile needs an endpoint",
                    ))?
                    .to_owned(),
            ),
            (None, _, _) => None,
        };
        let profile = ProviderProfile {
            id: Uuid::new_v4(),
            name: declaration.name.clone(),
            kind,
            auth,
            endpoint: endpoint.map(ProviderEndpoint),
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

    /// The active connection, if one is selected (ADR-0064). It always names a
    /// recorded profile with an endpoint, local or hosted (ADR-0092).
    pub fn active_provider_profile(&self) -> Option<&ProviderProfileStatus> {
        let id = self.snapshot.active_provider?;
        self.snapshot
            .provider_profiles
            .iter()
            .find(|status| status.profile.id == id)
    }

    /// Check that the named profile can be chosen as a connection now
    /// (ADR-0071, ADR-0092): a profile that records an endpoint and, for a
    /// hosted kind, has a bound credential. It reads no
    /// native storage and so cannot show that the stored secret still exists;
    /// a turn without a usable credential fails with the `credential` errors.
    /// Replay applies the same rule without the credential check.
    ///
    /// # Errors
    ///
    /// `UnknownProvider` for an unknown name and `ProviderRefused` for a
    /// profile that cannot be chosen.
    pub fn check_provider_selectable(
        &self,
        name: &str,
    ) -> Result<&ProviderProfileStatus, ControlError> {
        let status = self
            .provider_profile(name)
            .ok_or(ControlError::UnknownProvider)?;
        if !selectable(&status.profile) {
            return Err(ControlError::ProviderRefused(
                "only a provider profile with a recorded endpoint can be the active connection",
            ));
        }
        if status.profile.kind.is_hosted() && status.credential.is_none() {
            return Err(ControlError::ProviderRefused(
                "a hosted provider profile needs a bound credential before it can be chosen",
            ));
        }
        Ok(status)
    }

    /// Select the active connection by profile name, or return to the
    /// default endpoint with `None`. This advances the settings revision.
    ///
    /// The profile must pass [`Self::check_provider_selectable`]: it records an
    /// endpoint and, if hosted, has a bound credential (ADR-0092). Selecting
    /// grants nothing by itself: each turn still resolves the endpoint and any
    /// credential from that one profile (ADR-0048). Selecting the current choice again is refused, so every
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
            Some(name) => Some(self.check_provider_selectable(name)?.profile.id),
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

/// Whether a profile can be the active connection: it records the endpoint
/// its turns use, which replay has already checked against its kind's rule
/// (ADR-0071, ADR-0092). Replay applies exactly this; the credential is not
/// read there.
const fn selectable(profile: &ProviderProfile) -> bool {
    profile.endpoint.is_some()
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
    if text.len() > MAX_PROVIDER_ENDPOINT_BYTES {
        return Err(ControlError::ProviderRefused(
            "the provider endpoint is longer than 256 bytes",
        ));
    }
    if kind.is_local() {
        return validate_base_url(text).map(|_| ()).map_err(|_| {
            ControlError::ProviderRefused(
                "a local provider endpoint must be http://<loopback IP>:<port>/v1 with no user information, query, or fragment",
            )
        });
    }
    match kind.fixed_endpoint() {
        Some(fixed) if matches_fixed(text, fixed) => Ok(()),
        Some(_) => Err(ControlError::ProviderRefused(
            "this provider accepts only its one documented endpoint",
        )),
        None => validate_custom_url(text).map(|_| ()).map_err(|_| {
            ControlError::ProviderRefused(
                "an openai_compatible endpoint must be an https:// base URL with a lower-case host and no user information, query, fragment, percent-encoding, default port, or empty or dot path segment",
            )
        }),
    }
}

/// The authentication modes a kind may record. `subscription` stays
/// recordable for OpenAI and Anthropic so journals written by 0.58.0 and
/// earlier replay, but no adapter uses it; every other hosted kind is `api_key` only.
fn auth_allowed(kind: ProviderKind, auth: ProviderAuth) -> bool {
    match kind {
        ProviderKind::OpenAi | ProviderKind::Anthropic => {
            matches!(auth, ProviderAuth::Subscription | ProviderAuth::ApiKey)
        }
        ProviderKind::OllamaLocal => auth == ProviderAuth::None,
        ProviderKind::LmStudio => matches!(auth, ProviderAuth::None | ProviderAuth::ApiKey),
        ProviderKind::OllamaCloud
        | ProviderKind::OpenRouter
        | ProviderKind::Google
        | ProviderKind::Meta
        | ProviderKind::Moonshot
        | ProviderKind::Tencent
        | ProviderKind::ThinkingMachines
        | ProviderKind::MiniMax
        | ProviderKind::ZAi
        | ProviderKind::DeepSeek
        | ProviderKind::XAi
        | ProviderKind::OpenAiCompatible => auth == ProviderAuth::ApiKey,
    }
}

#[cfg(test)]
mod tests;
