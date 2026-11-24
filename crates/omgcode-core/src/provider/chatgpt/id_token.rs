//! ID-token validation for the ChatGPT sign-in (ADR-0093).
//!
//! The token must be a compact JWS signed with RS256 by a key in OpenAI's
//! published key set, for this client, from this attempt (the nonce), and not
//! expired. Only the subject and an optional email leave this module; the
//! token itself is dropped by the caller.

use base64::Engine as _;
use rsa::pkcs1v15::{Signature, VerifyingKey};
use rsa::signature::Verifier as _;
use rsa::{BoxedUint, PublicKey as RsaPublicKey};
use serde::Deserialize;
use serde_json::Value;

use super::{SignInError, valid_identifier};
use crate::storage::parse_strict_json;

/// The longest accepted ID token.
const MAX_ID_TOKEN_BYTES: usize = 16 * 1024;
/// The most keys a key set may hold.
const MAX_KEYS: usize = 32;
/// Accepted RSA modulus sizes in bytes: 2048 to 4096 bits.
const MODULUS_BYTES: std::ops::RangeInclusive<usize> = 256..=512;
/// Allowed clock difference when checking `exp`.
const LEEWAY_SECONDS: u64 = 60;
/// The longest kept display email.
const MAX_EMAIL_BYTES: usize = 256;

/// The verified account.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct Identity {
    pub(super) subject: String,
    pub(super) email: Option<String>,
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    kid: Option<String>,
}

#[derive(Deserialize)]
struct KeySet {
    keys: Vec<Value>,
}

#[derive(Deserialize)]
struct Jwk {
    kty: String,
    kid: Option<String>,
    alg: Option<String>,
    #[serde(rename = "use")]
    usage: Option<String>,
    n: Option<String>,
    e: Option<String>,
}

#[derive(Deserialize)]
struct Claims {
    iss: Option<String>,
    aud: Option<Value>,
    exp: Option<u64>,
    nonce: Option<String>,
    sub: Option<String>,
    email: Option<String>,
}

const fn invalid(reason: &'static str) -> SignInError {
    SignInError::IdToken(reason)
}

/// Validate `token` and return its account.
pub(super) fn validate(
    token: &str,
    key_set: &[u8],
    issuer: &str,
    client_id: &str,
    nonce: &str,
    now: u64,
) -> Result<Identity, SignInError> {
    if token.len() > MAX_ID_TOKEN_BYTES {
        return Err(invalid("too long"));
    }
    let mut parts = token.split('.');
    let (Some(header_text), Some(payload_text), Some(signature_text), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(invalid("not a signed token"));
    };
    let header: Header = decode_json(header_text)?;
    if header.alg != "RS256" {
        return Err(invalid("not signed with RS256"));
    }
    let kid = header.kid.ok_or(invalid("no key identifier"))?;
    let key = find_key(key_set, &kid)?;
    let signature_bytes = decode(signature_text)?;
    let signature =
        Signature::try_from(signature_bytes.as_slice()).map_err(|_| invalid("bad signature"))?;
    let signed = token
        .get(..header_text.len() + 1 + payload_text.len())
        .ok_or(invalid("not a signed token"))?;
    VerifyingKey::<rsa::sha2::Sha256>::new(key)
        .verify(signed.as_bytes(), &signature)
        .map_err(|_| invalid("bad signature"))?;
    let claims: Claims = decode_json(payload_text)?;
    if claims.iss.as_deref() != Some(issuer) {
        return Err(invalid("wrong issuer"));
    }
    let audience_matches = match &claims.aud {
        Some(Value::String(audience)) => audience == client_id,
        Some(Value::Array(audiences)) => audiences
            .iter()
            .any(|audience| audience.as_str() == Some(client_id)),
        _ => false,
    };
    if !audience_matches {
        return Err(invalid("wrong audience"));
    }
    let expires = claims.exp.ok_or(invalid("no expiry"))?;
    if expires.saturating_add(LEEWAY_SECONDS) <= now {
        return Err(invalid("expired"));
    }
    if claims.nonce.as_deref() != Some(nonce) {
        return Err(invalid("wrong nonce"));
    }
    let subject = claims
        .sub
        .filter(|subject| valid_identifier(subject))
        .ok_or(invalid("no subject"))?;
    let email = claims.email.filter(|email| {
        !email.is_empty() && email.len() <= MAX_EMAIL_BYTES && !email.chars().any(char::is_control)
    });
    Ok(Identity { subject, email })
}

fn decode(text: &str) -> Result<Vec<u8>, SignInError> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| invalid("bad encoding"))
}

fn decode_json<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T, SignInError> {
    let bytes = decode(text)?;
    let value = parse_strict_json(&bytes).map_err(|_| invalid("bad JSON"))?;
    serde_json::from_value(value).map_err(|_| invalid("bad JSON"))
}

/// Find exactly one usable RSA signing key with this identifier.
fn find_key(key_set: &[u8], kid: &str) -> Result<RsaPublicKey, SignInError> {
    let value = parse_strict_json(key_set).map_err(|_| SignInError::Malformed)?;
    let set: KeySet = serde_json::from_value(value).map_err(|_| SignInError::Malformed)?;
    if set.keys.len() > MAX_KEYS {
        return Err(SignInError::Malformed);
    }
    let mut found = None;
    for key in set.keys {
        let Ok(key) = serde_json::from_value::<Jwk>(key) else {
            continue;
        };
        if key.kid.as_deref() != Some(kid) {
            continue;
        }
        if found.is_some() {
            return Err(invalid("the key identifier names more than one key"));
        }
        found = Some(key);
    }
    let key = found.ok_or(invalid("unknown signing key"))?;
    if key.kty != "RSA"
        || key.alg.as_deref().is_some_and(|alg| alg != "RS256")
        || key.usage.as_deref().is_some_and(|usage| usage != "sig")
    {
        return Err(invalid("the signing key is not an RS256 key"));
    }
    let modulus = decode(key.n.as_deref().ok_or(invalid("bad signing key"))?)?;
    let exponent = decode(key.e.as_deref().ok_or(invalid("bad signing key"))?)?;
    let modulus = strip_leading_zeros(&modulus);
    if !MODULUS_BYTES.contains(&modulus.len()) || exponent.is_empty() || exponent.len() > 8 {
        return Err(invalid("bad signing key"));
    }
    RsaPublicKey::new(
        BoxedUint::from_be_slice_vartime(modulus),
        BoxedUint::from_be_slice_vartime(&exponent),
    )
    .map_err(|_| invalid("bad signing key"))
}

fn strip_leading_zeros(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(bytes.len());
    bytes.get(start..).unwrap_or_default()
}
