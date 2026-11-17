//! The `Authorization` header for a bound registry credential. See ADR-0056.
//!
//! The plaintext stays in zeroizing buffers until `HeaderValue` copies it, and
//! the value is marked sensitive so `Debug` and HTTP/2 header compression
//! treat it as such. The copy inside the HTTP stack is outside this module's
//! control.

use reqwest::header::HeaderValue;

use crate::control::{PackageAuth, Secret, Zeroizing};

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Build the header value: `Bearer <secret>` or `Basic <base64(secret)>`.
///
/// Returns `None` when the bytes are not a legal header value. Bind-time
/// validation already limits secrets to visible ASCII, so this is a second
/// check, not the policy.
pub(super) fn header_value(auth: PackageAuth, secret: &Secret) -> Option<HeaderValue> {
    // Reserve the exact length first: a reallocation would free a partial
    // copy of the secret without wiping it.
    let secret_len = secret.expose().len();
    let capacity = match auth {
        PackageAuth::Bearer => b"Bearer ".len().checked_add(secret_len)?,
        PackageAuth::Basic => secret_len
            .div_ceil(3)
            .checked_mul(4)?
            .checked_add(b"Basic ".len())?,
    };
    let mut buffer: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::with_capacity(capacity));
    match auth {
        PackageAuth::Bearer => {
            buffer.extend_from_slice(b"Bearer ");
            buffer.extend_from_slice(secret.expose());
        }
        PackageAuth::Basic => {
            buffer.extend_from_slice(b"Basic ");
            encode_base64(secret.expose(), &mut buffer);
        }
    }
    debug_assert_eq!(buffer.len(), capacity);
    let mut value = HeaderValue::from_bytes(&buffer).ok()?;
    value.set_sensitive(true);
    Some(value)
}

/// Standard RFC 4648 base64 with padding, appended to `output`.
fn encode_base64(input: &[u8], output: &mut Vec<u8>) {
    for chunk in input.chunks(3) {
        let first = chunk.first().copied().unwrap_or(0);
        let second = chunk.get(1).copied();
        let third = chunk.get(2).copied();
        let group = (u32::from(first) << 16)
            | (u32::from(second.unwrap_or(0)) << 8)
            | u32::from(third.unwrap_or(0));
        let sextets = [
            (group >> 18) & 0x3f,
            (group >> 12) & 0x3f,
            (group >> 6) & 0x3f,
            group & 0x3f,
        ];
        let kept = match chunk.len() {
            1 => 2,
            2 => 3,
            _ => 4,
        };
        for (index, sextet) in sextets.iter().enumerate() {
            let symbol = if index < kept {
                // The sextet is masked to six bits, so the lookup is in range.
                ALPHABET
                    .get(usize::try_from(*sextet).unwrap_or(0))
                    .copied()
                    .unwrap_or(b'=')
            } else {
                b'='
            };
            output.push(symbol);
        }
    }
}

#[cfg(test)]
mod tests;
