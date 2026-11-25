//! Address policy and the pinning DNS resolver.
//!
//! The model chooses the host, so a name can resolve to the user's own
//! machine or network. The resolver below is the only name resolution the
//! fetch client uses. It validates the whole answer and returns only that
//! answer, so the connection goes to exactly the addresses that were checked.
//! There is no second lookup between the check and the connect (DNS
//! rebinding), and a host that returns even one non-global address is refused
//! whole, so a mixed answer cannot be used to reach the private one.
//!
//! The policy is a deny list over a global-unicast allow list. It is a
//! prototype table, not a claim that every reserved range is listed; IPv6
//! outside `2000::/3` is refused wholesale for that reason.

use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// The most addresses taken from one lookup. A larger answer is cut, and only
/// the addresses kept are checked and returned.
const MAX_RESOLVED_ADDRESSES: usize = 32;

/// IPv4 ranges that are not global unicast: network address and prefix length.
const BLOCKED_V4: [([u8; 4], u8); 16] = [
    ([0, 0, 0, 0], 8),
    ([10, 0, 0, 0], 8),
    ([100, 64, 0, 0], 10),
    ([127, 0, 0, 0], 8),
    ([169, 254, 0, 0], 16),
    ([172, 16, 0, 0], 12),
    ([192, 0, 0, 0], 24),
    ([192, 0, 2, 0], 24),
    ([192, 88, 99, 0], 24),
    ([192, 168, 0, 0], 16),
    ([198, 18, 0, 0], 15),
    ([198, 51, 100, 0], 24),
    ([203, 0, 113, 0], 24),
    // Multicast, then the reserved block that holds the broadcast address.
    ([224, 0, 0, 0], 4),
    ([240, 0, 0, 0], 4),
    ([255, 255, 255, 255], 32),
];

/// IPv6 ranges refused even where they overlap the allowed `2000::/3`, or lie
/// outside it, so the reason for each refusal stays visible: network address
/// and prefix length.
const BLOCKED_V6: [([u16; 8], u8); 13] = [
    // Unspecified, loopback, IPv4-compatible.
    ([0, 0, 0, 0, 0, 0, 0, 0], 96),
    // IPv4-mapped. The embedded IPv4 address is refused whatever it is.
    ([0, 0, 0, 0, 0, 0xffff, 0, 0], 96),
    // NAT64.
    ([0x64, 0xff9b, 0, 0, 0, 0, 0, 0], 96),
    ([0x64, 0xff9b, 1, 0, 0, 0, 0, 0], 48),
    // IETF protocol assignments, including Teredo.
    ([0x2001, 0, 0, 0, 0, 0, 0, 0], 23),
    // Documentation.
    ([0x2001, 0xdb8, 0, 0, 0, 0, 0, 0], 32),
    ([0x3fff, 0, 0, 0, 0, 0, 0, 0], 20),
    // 6to4.
    ([0x2002, 0, 0, 0, 0, 0, 0, 0], 16),
    // Segment routing.
    ([0x5f00, 0, 0, 0, 0, 0, 0, 0], 16),
    // Unique local, link-local, site-local, multicast.
    ([0xfc00, 0, 0, 0, 0, 0, 0, 0], 7),
    ([0xfe80, 0, 0, 0, 0, 0, 0, 0], 10),
    ([0xfec0, 0, 0, 0, 0, 0, 0, 0], 10),
    ([0xff00, 0, 0, 0, 0, 0, 0, 0], 8),
];

/// Why an address set is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressRefusal {
    /// The lookup returned no address.
    Empty,
    /// At least one address is not global unicast.
    NotGlobalUnicast,
}

/// Report whether `ip` is a global unicast address this module allows.
///
/// IPv4 is allowed unless a blocked range holds it. IPv6 is allowed only
/// inside `2000::/3` and outside every blocked range.
pub fn is_global_unicast(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(address) => v4_allowed(address),
        IpAddr::V6(address) => v6_allowed(address),
    }
}

fn v4_allowed(address: Ipv4Addr) -> bool {
    let bits = address.to_bits();
    !BLOCKED_V4.iter().any(|(network, prefix)| {
        let mask = u32::MAX
            .checked_shl(32_u32.saturating_sub(u32::from(*prefix)))
            .unwrap_or(0);
        bits & mask == Ipv4Addr::from(*network).to_bits() & mask
    })
}

fn v6_allowed(address: Ipv6Addr) -> bool {
    let bits = address.to_bits();
    if bits >> 125 != 0b001 {
        return false;
    }
    !BLOCKED_V6.iter().any(|(network, prefix)| {
        let mask = u128::MAX
            .checked_shl(128_u32.saturating_sub(u32::from(*prefix)))
            .unwrap_or(0);
        bits & mask == Ipv6Addr::from(*network).to_bits() & mask
    })
}

/// Refuse the whole set when it is empty or any address is not global
/// unicast. The port is not part of the policy.
pub fn check_addresses(addresses: &[SocketAddr]) -> Result<(), AddressRefusal> {
    if addresses.is_empty() {
        return Err(AddressRefusal::Empty);
    }
    if addresses
        .iter()
        .all(|address| is_global_unicast(address.ip()))
    {
        Ok(())
    } else {
        Err(AddressRefusal::NotGlobalUnicast)
    }
}

/// A resolution failure, as the client's error chain carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(super) enum ResolveError {
    /// The answer held an address the policy refuses.
    #[error("the host resolves to an address that is not allowed")]
    Blocked,
    /// The lookup failed or returned nothing.
    #[error("the host name did not resolve")]
    Lookup,
}

/// The first resolution failure of one fetch, shared between the resolver
/// and the fetcher.
///
/// `reqwest` and `hyper` wrap a resolver error in their own connect error, so
/// the resolver also records the failure here and the fetcher reads it after
/// a failed send. The error chain is checked too; either route identifies it.
#[derive(Debug, Default)]
pub(super) struct ResolveState {
    failure: Mutex<Option<ResolveError>>,
}

impl ResolveState {
    fn record(&self, error: ResolveError) {
        // A poisoned lock only means another thread panicked while holding
        // a plain `Option`; the value is still valid.
        let mut slot = self.failure.lock().unwrap_or_else(PoisonError::into_inner);
        slot.get_or_insert(error);
    }

    /// The failure recorded so far, if any.
    pub(super) fn failure(&self) -> Option<ResolveError> {
        *self.failure.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Validate the answer of a name lookup.
///
/// `lookup` is the name lookup. It is a parameter so the policy path can be
/// tested without the network.
pub(super) async fn pin_addresses<F>(lookup: F) -> Result<Vec<SocketAddr>, ResolveError>
where
    F: Future<Output = io::Result<Vec<SocketAddr>>>,
{
    let addresses = lookup.await.map_err(|_| ResolveError::Lookup)?;
    match check_addresses(&addresses) {
        Ok(()) => Ok(addresses),
        Err(AddressRefusal::Empty) => Err(ResolveError::Lookup),
        Err(AddressRefusal::NotGlobalUnicast) => Err(ResolveError::Blocked),
    }
}

/// The DNS resolver for one fetch client.
///
/// It resolves with `tokio` (the system resolver on a blocking thread),
/// applies [`check_addresses`] and hands the client only what passed. The
/// client connects to exactly those addresses. Its error carries
/// [`ResolveError`], and the failure is also recorded in the shared
/// [`ResolveState`].
#[derive(Clone, Debug)]
pub(super) struct PinningResolver {
    state: Arc<ResolveState>,
}

impl PinningResolver {
    pub(super) fn new(state: Arc<ResolveState>) -> Self {
        Self { state }
    }
}

impl Resolve for PinningResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            // Port 0 lets `reqwest` apply the URL's port.
            let lookup = async {
                tokio::net::lookup_host((host.as_str(), 0_u16))
                    .await
                    .map(|found| found.take(MAX_RESOLVED_ADDRESSES).collect::<Vec<_>>())
            };
            match pin_addresses(lookup).await {
                Ok(addresses) => {
                    let pinned: Addrs = Box::new(addresses.into_iter());
                    Ok(pinned)
                }
                Err(error) => {
                    state.record(error);
                    Err(error.into())
                }
            }
        })
    }
}

#[cfg(test)]
mod tests;
