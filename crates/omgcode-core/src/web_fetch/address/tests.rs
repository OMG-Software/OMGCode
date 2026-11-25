use super::*;
use std::net::SocketAddr;

fn v4(text: &str) -> Ipv4Addr {
    text.parse().expect("IPv4 literal")
}

fn v6(text: &str) -> Ipv6Addr {
    text.parse().expect("IPv6 literal")
}

fn allowed4(address: Ipv4Addr) -> bool {
    is_global_unicast(IpAddr::V4(address))
}

fn allowed6(address: Ipv6Addr) -> bool {
    is_global_unicast(IpAddr::V6(address))
}

/// Each blocked IPv4 range as first and last address, written out by hand
/// rather than derived from the policy table. Adjacent blocked ranges are
/// merged so every neighbour check is meaningful.
const V4_RANGES: [(&str, &str); 14] = [
    ("0.0.0.0", "0.255.255.255"),
    ("10.0.0.0", "10.255.255.255"),
    ("100.64.0.0", "100.127.255.255"),
    ("127.0.0.0", "127.255.255.255"),
    ("169.254.0.0", "169.254.255.255"),
    ("172.16.0.0", "172.31.255.255"),
    ("192.0.0.0", "192.0.0.255"),
    ("192.0.2.0", "192.0.2.255"),
    ("192.88.99.0", "192.88.99.255"),
    ("192.168.0.0", "192.168.255.255"),
    ("198.18.0.0", "198.19.255.255"),
    ("198.51.100.0", "198.51.100.255"),
    ("203.0.113.0", "203.0.113.255"),
    // 224.0.0.0/4 and 240.0.0.0/4, including the broadcast address.
    ("224.0.0.0", "255.255.255.255"),
];

#[test]
fn ipv4_blocked_ranges_hold_their_edges_and_neighbours_are_allowed() {
    for (first, last) in V4_RANGES {
        let (first, last) = (v4(first), v4(last));
        assert!(!allowed4(first), "{first} must be blocked");
        assert!(!allowed4(last), "{last} must be blocked");
        let middle = Ipv4Addr::from_bits(first.to_bits() / 2 + last.to_bits() / 2);
        assert!(!allowed4(middle), "{middle} must be blocked");
        if first.to_bits() > 0 {
            let before = Ipv4Addr::from_bits(first.to_bits() - 1);
            assert!(allowed4(before), "{before} before {first} must be allowed");
        }
        if last.to_bits() < u32::MAX {
            let after = Ipv4Addr::from_bits(last.to_bits() + 1);
            assert!(allowed4(after), "{after} after {last} must be allowed");
        }
    }
}

#[test]
fn ipv4_public_addresses_are_allowed() {
    for text in [
        "1.1.1.1",
        "8.8.8.8",
        "93.184.216.34",
        "100.63.255.255",
        "100.128.0.0",
        "172.15.255.255",
        "172.32.0.0",
        "192.0.1.1",
        "192.169.0.1",
        "198.20.0.1",
        "223.255.255.255",
    ] {
        assert!(allowed4(v4(text)), "{text} must be allowed");
    }
}

/// Blocked IPv6 ranges as first and last address. `neighbours` says whether
/// the addresses just outside are inside `2000::/3`, and so must be allowed.
const V6_RANGES: [(&str, &str, bool); 11] = [
    ("::", "::ffff:ffff", false),
    ("::ffff:0:0", "::ffff:ffff:ffff", false),
    ("64:ff9b::", "64:ff9b::ffff:ffff", false),
    ("64:ff9b:1::", "64:ff9b:1:ffff:ffff:ffff:ffff:ffff", false),
    ("2001::", "2001:1ff:ffff:ffff:ffff:ffff:ffff:ffff", true),
    ("2001:db8::", "2001:db8:ffff:ffff:ffff:ffff:ffff:ffff", true),
    ("2002::", "2002:ffff:ffff:ffff:ffff:ffff:ffff:ffff", true),
    ("fc00::", "fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", false),
    ("fe80::", "febf:ffff:ffff:ffff:ffff:ffff:ffff:ffff", false),
    ("fec0::", "feff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", false),
    ("ff00::", "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", false),
];

#[test]
fn ipv6_blocked_ranges_hold_their_edges() {
    for (first, last, neighbours_allowed) in V6_RANGES {
        let (first, last) = (v6(first), v6(last));
        assert!(!allowed6(first), "{first} must be blocked");
        assert!(!allowed6(last), "{last} must be blocked");
        if neighbours_allowed {
            let before = Ipv6Addr::from_bits(first.to_bits() - 1);
            let after = Ipv6Addr::from_bits(last.to_bits() + 1);
            assert!(allowed6(before), "{before} must be allowed");
            assert!(allowed6(after), "{after} must be allowed");
        }
    }
}

#[test]
fn ipv6_is_allowed_only_inside_the_global_unicast_block() {
    assert!(!allowed6(v6("1fff:ffff:ffff:ffff:ffff:ffff:ffff:ffff")));
    assert!(allowed6(v6("2000::1")));
    assert!(allowed6(v6("3ffe:ffff:ffff:ffff:ffff:ffff:ffff:ffff")));
    assert!(!allowed6(v6("3fff::")));
    assert!(!allowed6(v6("3fff:fff:ffff:ffff:ffff:ffff:ffff:ffff")));
    assert!(allowed6(v6("3fff:1000::")));
    assert!(!allowed6(v6("4000::")));
    assert!(!allowed6(v6("5f00::1")));
    for text in [
        "2606:4700:4700::1111",
        "2a00:1450:4001:81b::200e",
        "2001:4860:4860::8888",
        "2001:200::1",
        "2001:db9::1",
        "2003::1",
    ] {
        assert!(allowed6(v6(text)), "{text} must be allowed");
    }
}

#[test]
fn ipv6_forms_that_embed_an_ipv4_address_are_refused() {
    for text in [
        "::ffff:8.8.8.8",
        "::ffff:127.0.0.1",
        "::8.8.8.8",
        "::1",
        "::",
        "64:ff9b::808:808",
        "64:ff9b:1::808:808",
        "2002:808:808::1",
        "2001:0:4136:e378:8000:63bf:3fff:fdd2",
    ] {
        assert!(!allowed6(v6(text)), "{text} must be refused");
    }
}

fn socket(ip: &str) -> SocketAddr {
    SocketAddr::new(ip.parse().expect("IP literal"), 443)
}

#[test]
fn an_address_set_is_refused_when_any_member_is_refused() {
    assert_eq!(check_addresses(&[]), Err(AddressRefusal::Empty));
    assert_eq!(check_addresses(&[socket("8.8.8.8")]), Ok(()));
    assert_eq!(
        check_addresses(&[socket("8.8.8.8"), socket("2606:4700::1111")]),
        Ok(())
    );
    assert_eq!(
        check_addresses(&[socket("8.8.8.8"), socket("10.0.0.1")]),
        Err(AddressRefusal::NotGlobalUnicast)
    );
    assert_eq!(
        check_addresses(&[socket("10.0.0.1"), socket("8.8.8.8")]),
        Err(AddressRefusal::NotGlobalUnicast)
    );
    assert_eq!(
        check_addresses(&[socket("2606:4700::1111"), socket("::1")]),
        Err(AddressRefusal::NotGlobalUnicast)
    );
}

fn block_on<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}

#[test]
fn pinning_returns_only_a_validated_answer() {
    let public = vec![socket("8.8.8.8"), socket("2606:4700::1111")];
    let answer = block_on(pin_addresses(async { Ok(public.clone()) }));
    assert_eq!(answer, Ok(public));
}

#[test]
fn pinning_refuses_a_mixed_answer_as_blocked() {
    let mixed = vec![socket("8.8.8.8"), socket("192.168.1.1")];
    assert_eq!(
        block_on(pin_addresses(async { Ok(mixed) })),
        Err(ResolveError::Blocked)
    );
}

#[test]
fn pinning_reports_an_empty_answer_and_a_failed_lookup_as_lookup_failures() {
    assert_eq!(
        block_on(pin_addresses(async { Ok(Vec::new()) })),
        Err(ResolveError::Lookup)
    );
    assert_eq!(
        block_on(pin_addresses(async {
            Err(io::Error::other("no such host"))
        })),
        Err(ResolveError::Lookup)
    );
}

#[test]
fn the_resolver_refuses_localhost_and_records_the_failure() {
    let state = Arc::new(ResolveState::default());
    let resolver = PinningResolver::new(Arc::clone(&state));
    let name: Name = "localhost".parse().expect("name");
    let result = block_on(resolver.resolve(name));
    let error = match result {
        Ok(_) => panic!("localhost must not resolve to an allowed address"),
        Err(error) => error,
    };
    assert_eq!(
        error.downcast_ref::<ResolveError>(),
        Some(&ResolveError::Blocked)
    );
    assert_eq!(state.failure(), Some(ResolveError::Blocked));
}

#[test]
fn the_first_recorded_failure_is_kept() {
    let state = ResolveState::default();
    assert_eq!(state.failure(), None);
    state.record(ResolveError::Blocked);
    state.record(ResolveError::Lookup);
    assert_eq!(state.failure(), Some(ResolveError::Blocked));
}
