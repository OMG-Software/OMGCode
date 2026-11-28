//! Single-use review tokens (ADR-0046, decision 4).
//!
//! A token proves that `approval_detail` showed the operator the exact request
//! an approval is bound to. It is random, bound to one approval and its digest,
//! lives for [`TOKEN_LIFETIME`], and is removed by the first attempt to redeem
//! it, successful or not. Tokens live in memory only. This module is pure: the
//! caller supplies every `Instant`, so expiry is testable without sleeping.
//!
//! A token for a web fetch also keeps the host core's review offered for
//! **Allow host** (ADR-0110). Redeeming returns it, so that decision uses the
//! host core found and never one a webview sends.

use std::time::{Duration, Instant};

use omgcode_core::control::ReviewedHost;
use uuid::Uuid;

/// How long a token stays valid. A token is expired at exactly this age.
pub(super) const TOKEN_LIFETIME: Duration = Duration::from_secs(120);

/// Most tokens kept at once. A new token evicts the oldest beyond this.
pub(super) const MAX_OUTSTANDING_TOKENS: usize = 64;

/// Why a redeem attempt failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RedeemError {
    /// No token exists for the approval, none was presented, or it is too old.
    Expired,
    /// The presented token or the approval's digest differs from the issued one.
    Mismatch,
}

/// One outstanding token.
struct Entry {
    approval: Uuid,
    token: String,
    digest: String,
    /// The host Allow host would list, when the review offered it.
    host: Option<ReviewedHost>,
    issued: Instant,
}

/// The outstanding tokens, oldest first. A token is a credential for one
/// approve, so this type has no `Debug` output.
#[derive(Default)]
pub(super) struct ReviewTokens {
    entries: Vec<Entry>,
}

impl ReviewTokens {
    /// Issue a fresh token for `approval`, replacing any earlier one. `host`
    /// is the host the review offered for Allow host, if any.
    ///
    /// Expired tokens are dropped first, then the oldest is evicted while the
    /// table would exceed [`MAX_OUTSTANDING_TOKENS`].
    pub(super) fn issue(
        &mut self,
        approval: Uuid,
        digest: &str,
        host: Option<ReviewedHost>,
        now: Instant,
    ) -> String {
        self.entries.retain(|entry| entry.approval != approval);
        self.entries.retain(|entry| !is_expired(entry, now));
        while self.entries.len() >= MAX_OUTSTANDING_TOKENS {
            self.entries.remove(0);
        }
        let token = Uuid::new_v4().to_string();
        self.entries.push(Entry {
            approval,
            token: token.clone(),
            digest: digest.to_owned(),
            host,
            issued: now,
        });
        token
    }

    /// Use the token for `approval`. The entry is removed on every attempt.
    /// On success, returns the host the review offered for Allow host.
    ///
    /// `presented` is `None` when the caller sent no token, which is `Expired`.
    pub(super) fn redeem(
        &mut self,
        approval: Uuid,
        presented: Option<&str>,
        digest: &str,
        now: Instant,
    ) -> Result<Option<ReviewedHost>, RedeemError> {
        let position = self
            .entries
            .iter()
            .position(|entry| entry.approval == approval);
        let entry = match position {
            Some(index) => self.entries.remove(index),
            None => return Err(RedeemError::Expired),
        };
        if is_expired(&entry, now) {
            return Err(RedeemError::Expired);
        }
        let Some(presented) = presented else {
            return Err(RedeemError::Expired);
        };
        let token_matches = constant_time_equal(presented.as_bytes(), entry.token.as_bytes());
        let digest_matches = constant_time_equal(digest.as_bytes(), entry.digest.as_bytes());
        if token_matches && digest_matches {
            Ok(entry.host)
        } else {
            Err(RedeemError::Mismatch)
        }
    }

    /// How many tokens are outstanding.
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Whether `entry` has reached [`TOKEN_LIFETIME`] at `now`.
fn is_expired(entry: &Entry, now: Instant) -> bool {
    now.saturating_duration_since(entry.issued) >= TOKEN_LIFETIME
}

/// Compare two byte strings without stopping at the first difference.
///
/// Lengths are not secret: tokens and digests have fixed lengths.
fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let difference = left
        .iter()
        .zip(right)
        .fold(0_u8, |accumulated, (a, b)| accumulated | (a ^ b));
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "aa";

    fn id() -> Uuid {
        Uuid::new_v4()
    }

    #[test]
    fn a_token_redeems_once() {
        let mut tokens = ReviewTokens::default();
        let start = Instant::now();
        let approval = id();
        let token = tokens.issue(approval, DIGEST, None, start);
        assert_eq!(
            tokens.redeem(approval, Some(&token), DIGEST, start),
            Ok(None)
        );
        assert_eq!(
            tokens.redeem(approval, Some(&token), DIGEST, start),
            Err(RedeemError::Expired)
        );
    }

    #[test]
    fn a_token_is_valid_just_before_the_lifetime_and_expired_at_it() {
        let start = Instant::now();
        let approval = id();
        let mut tokens = ReviewTokens::default();
        let token = tokens.issue(approval, DIGEST, None, start);
        let almost = start + TOKEN_LIFETIME - Duration::from_millis(1);
        assert_eq!(
            tokens.redeem(approval, Some(&token), DIGEST, almost),
            Ok(None)
        );

        let token = tokens.issue(approval, DIGEST, None, start);
        assert_eq!(
            tokens.redeem(approval, Some(&token), DIGEST, start + TOKEN_LIFETIME),
            Err(RedeemError::Expired)
        );
    }

    #[test]
    fn issuing_again_replaces_the_earlier_token() {
        let start = Instant::now();
        let approval = id();
        let mut tokens = ReviewTokens::default();
        let first = tokens.issue(approval, DIGEST, None, start);
        let second = tokens.issue(approval, DIGEST, None, start);
        assert_ne!(first, second);
        assert_eq!(tokens.len(), 1);
        assert_eq!(
            tokens.redeem(approval, Some(&first), DIGEST, start),
            Err(RedeemError::Mismatch)
        );
        // The failed attempt removed the entry, so the second token is gone too.
        assert_eq!(
            tokens.redeem(approval, Some(&second), DIGEST, start),
            Err(RedeemError::Expired)
        );
    }

    #[test]
    fn a_wrong_token_or_digest_is_a_mismatch_and_removes_the_entry() {
        let start = Instant::now();
        let approval = id();
        let mut tokens = ReviewTokens::default();
        let token = tokens.issue(approval, DIGEST, None, start);
        assert_eq!(
            tokens.redeem(approval, Some("not-the-token"), DIGEST, start),
            Err(RedeemError::Mismatch)
        );
        assert_eq!(
            tokens.redeem(approval, Some(&token), DIGEST, start),
            Err(RedeemError::Expired)
        );

        let token = tokens.issue(approval, DIGEST, None, start);
        assert_eq!(
            tokens.redeem(approval, Some(&token), "bb", start),
            Err(RedeemError::Mismatch)
        );
        assert_eq!(tokens.len(), 0);
    }

    #[test]
    fn a_missing_token_is_expired_and_removes_the_entry() {
        let start = Instant::now();
        let approval = id();
        let mut tokens = ReviewTokens::default();
        let token = tokens.issue(approval, DIGEST, None, start);
        assert_eq!(
            tokens.redeem(approval, None, DIGEST, start),
            Err(RedeemError::Expired)
        );
        assert_eq!(
            tokens.redeem(approval, Some(&token), DIGEST, start),
            Err(RedeemError::Expired)
        );
    }

    #[test]
    fn a_token_for_one_approval_does_not_redeem_another() {
        let start = Instant::now();
        let (a, b) = (id(), id());
        let mut tokens = ReviewTokens::default();
        let token_a = tokens.issue(a, DIGEST, None, start);
        assert_eq!(
            tokens.redeem(b, Some(&token_a), DIGEST, start),
            Err(RedeemError::Expired)
        );
        assert_eq!(tokens.redeem(a, Some(&token_a), DIGEST, start), Ok(None));
    }

    #[test]
    fn the_sixty_fifth_token_evicts_the_oldest() {
        let start = Instant::now();
        let mut tokens = ReviewTokens::default();
        let approvals: Vec<Uuid> = (0..=MAX_OUTSTANDING_TOKENS).map(|_| id()).collect();
        let issued: Vec<String> = approvals
            .iter()
            .map(|approval| tokens.issue(*approval, DIGEST, None, start))
            .collect();
        assert_eq!(tokens.len(), MAX_OUTSTANDING_TOKENS);
        assert_eq!(
            tokens.redeem(approvals[0], Some(&issued[0]), DIGEST, start),
            Err(RedeemError::Expired)
        );
        assert_eq!(
            tokens.redeem(approvals[1], Some(&issued[1]), DIGEST, start),
            Ok(None)
        );
    }

    #[test]
    fn expired_tokens_are_dropped_when_a_new_one_is_issued() {
        let start = Instant::now();
        let mut tokens = ReviewTokens::default();
        for _ in 0..10 {
            tokens.issue(id(), DIGEST, None, start);
        }
        tokens.issue(id(), DIGEST, None, start + TOKEN_LIFETIME);
        assert_eq!(tokens.len(), 1);
    }

    #[test]
    fn tokens_are_random_version_four_identifiers() {
        let start = Instant::now();
        let mut tokens = ReviewTokens::default();
        let token = tokens.issue(id(), DIGEST, None, start);
        let parsed = Uuid::try_parse(&token).expect("a uuid token");
        assert_eq!(parsed.get_version_num(), 4);
    }
}
