use super::*;
use std::fs;
use std::path::PathBuf;

use crate::control::FILE_NAME;
use crate::policy::{Access, PermissionSet};

fn refusal(text: &str) -> WebHostRefusal {
    WebHostPattern::parse(text).expect_err(text)
}

fn pattern(text: &str) -> WebHostPattern {
    WebHostPattern::parse(text).expect(text)
}

/// Accepted forms are normalised to one canonical text.
#[test]
fn accepted_patterns_are_canonical() {
    for (typed, canonical, wildcard) in [
        ("example.com", "example.com", false),
        ("Docs.Example.COM", "docs.example.com", false),
        ("example.com.", "example.com", false),
        ("*.example.com", "*.example.com", true),
        ("*.Example.com.", "*.example.com", true),
        ("xn--bcher-kva.example", "xn--bcher-kva.example", false),
        ("a-b.c-d.example.org", "a-b.c-d.example.org", false),
        ("1a.example.com", "1a.example.com", false),
        ("*.co.uk", "*.co.uk", true),
    ] {
        let parsed = pattern(typed);
        assert_eq!(parsed.as_str(), canonical, "{typed}");
        assert_eq!(parsed.is_wildcard(), wildcard, "{typed}");
    }
}

/// Every refusal has its own stable code, and the table covers each class.
#[test]
fn invalid_patterns_are_refused_with_stable_codes() {
    let long_label = "a".repeat(64);
    let long_name = format!("{}.com", vec!["a".repeat(60); 5].join("."));
    let cases: Vec<(String, WebHostRefusal)> = vec![
        (String::new(), WebHostRefusal::Empty),
        (".".into(), WebHostRefusal::Empty),
        ("*.".into(), WebHostRefusal::BadCharacter),
        ("b\u{fc}cher.example".into(), WebHostRefusal::NotAscii),
        ("\u{ff4}.example.com".into(), WebHostRefusal::NotAscii),
        (long_name, WebHostRefusal::TooLong),
        (format!("{long_label}.com"), WebHostRefusal::BadLabel),
        ("-a.example.com".into(), WebHostRefusal::BadLabel),
        ("a-.example.com".into(), WebHostRefusal::BadLabel),
        ("a..example.com".into(), WebHostRefusal::BadLabel),
        (".example.com".into(), WebHostRefusal::BadLabel),
        ("example.com..".into(), WebHostRefusal::BadLabel),
        ("exa mple.com".into(), WebHostRefusal::BadCharacter),
        ("example.com/path".into(), WebHostRefusal::BadCharacter),
        ("example.com:443".into(), WebHostRefusal::BadCharacter),
        ("user@example.com".into(), WebHostRefusal::BadCharacter),
        ("exa_mple.com".into(), WebHostRefusal::BadCharacter),
        ("https://example.com".into(), WebHostRefusal::BadCharacter),
        ("a.*.example.com".into(), WebHostRefusal::BadCharacter),
        ("*example.com".into(), WebHostRefusal::BadCharacter),
        ("**.example.com".into(), WebHostRefusal::BadCharacter),
        ("*.*.example.com".into(), WebHostRefusal::BadCharacter),
        ("ex*.example.com".into(), WebHostRefusal::BadCharacter),
        ("[::1]".into(), WebHostRefusal::BadCharacter),
        ("::1".into(), WebHostRefusal::BadCharacter),
        ("127.0.0.1".into(), WebHostRefusal::IpLiteral),
        ("8.8.8.8.".into(), WebHostRefusal::IpLiteral),
        ("*.10.0.0.1".into(), WebHostRefusal::IpLiteral),
        ("127.1".into(), WebHostRefusal::IpLiteral),
        ("0x7f.0x1".into(), WebHostRefusal::IpLiteral),
        ("1.0x7f000001".into(), WebHostRefusal::IpLiteral),
        ("example.123".into(), WebHostRefusal::IpLiteral),
        ("2130706433".into(), WebHostRefusal::IpLiteral),
        ("localhost".into(), WebHostRefusal::Reserved),
        ("LOCALHOST.".into(), WebHostRefusal::Reserved),
        ("app.localhost".into(), WebHostRefusal::Reserved),
        ("*.localhost".into(), WebHostRefusal::Reserved),
        ("printer.local".into(), WebHostRefusal::Reserved),
        ("db.internal".into(), WebHostRefusal::Reserved),
        ("*.corp.internal".into(), WebHostRefusal::Reserved),
        ("router.home.arpa".into(), WebHostRefusal::Reserved),
        ("home.arpa".into(), WebHostRefusal::Reserved),
        ("nas.lan".into(), WebHostRefusal::Reserved),
        ("host.localdomain".into(), WebHostRefusal::Reserved),
        ("abc.onion".into(), WebHostRefusal::Reserved),
        ("com".into(), WebHostRefusal::TooFewLabels),
        ("example".into(), WebHostRefusal::TooFewLabels),
        ("*.com".into(), WebHostRefusal::TooFewLabels),
        ("*.example".into(), WebHostRefusal::TooFewLabels),
    ];
    for (text, expected) in cases {
        assert_eq!(refusal(&text), expected, "{text:?}");
    }
}

/// A name that merely ends in a reserved word is a different public name.
#[test]
fn reserved_suffixes_match_whole_labels_only() {
    for text in [
        "notlocalhost.com",
        "mylocal.com",
        "internal.example.com",
        "local.example.com",
        "lan.example.com",
        "example.lanes",
        "onion.example.com",
        "home.arpa.example.com",
    ] {
        pattern(text);
    }
}

/// Refusal codes are unique, prefixed, and never echo the input.
#[test]
fn refusal_codes_are_unique_and_messages_are_fixed() {
    let all = [
        WebHostRefusal::Empty,
        WebHostRefusal::NotAscii,
        WebHostRefusal::TooLong,
        WebHostRefusal::BadLabel,
        WebHostRefusal::BadCharacter,
        WebHostRefusal::IpLiteral,
        WebHostRefusal::Reserved,
        WebHostRefusal::TooFewLabels,
        WebHostRefusal::NotCanonical,
        WebHostRefusal::Duplicate,
        WebHostRefusal::NotListed,
        WebHostRefusal::TooMany,
    ];
    let mut codes: Vec<_> = all.iter().map(|each| each.as_str()).collect();
    assert!(codes.iter().all(|code| code.starts_with("web_host_")));
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), all.len());
    assert!(all.iter().all(|each| !each.to_string().is_empty()));
}

/// An exact pattern matches the identical name only.
#[test]
fn exact_pattern_matches_only_the_same_host() {
    let docs = pattern("docs.example.com");
    for host in ["docs.example.com", "DOCS.Example.COM", "docs.example.com."] {
        assert!(docs.matches(host), "{host}");
    }
    for host in [
        "",
        ".",
        "example.com",
        "a.docs.example.com",
        "docs.example.com.evil.net",
        "docs.example.com..",
        "xdocs.example.com",
        "docs.example.co",
        "docs.example.com:443",
        "docs.example.com/",
        " docs.example.com",
        "docs.example.com\0",
        "docs.example.com\u{200b}",
        "docs\u{ff0e}example.com",
    ] {
        assert!(!docs.matches(host), "{host:?}");
    }
}

/// A wildcard matches proper subdomains at any depth and nothing else.
#[test]
fn wildcard_matches_proper_subdomains_only() {
    let wildcard = pattern("*.example.com");
    for host in [
        "docs.example.com",
        "a.b.example.com",
        "A.B.EXAMPLE.COM",
        "docs.example.com.",
        "x-y.example.com",
        "1.example.com",
    ] {
        assert!(wildcard.matches(host), "{host}");
    }
    for host in [
        "example.com",
        "example.com.",
        ".example.com",
        "..example.com",
        "a..example.com",
        "evilexample.com",
        "docs.evilexample.com",
        "example.com.evil.net",
        "docs.example.com.evil.net",
        "docs.example.org",
        "docs.example.com..",
        "docs.example.com:443",
        "a b.example.com",
        "a/b.example.com",
        "user@docs.example.com",
        "docs.example.com\u{200b}",
        "",
        ".",
        "com",
    ] {
        assert!(!wildcard.matches(host), "{host:?}");
    }
}

/// Matching never reads an IP address as a name.
#[test]
fn addresses_match_nothing() {
    for text in ["example.com", "*.example.com"] {
        let parsed = pattern(text);
        for host in [
            "127.0.0.1",
            "[::1]",
            "::1",
            "10.0.0.1.example.com.1",
            "localhost",
        ] {
            assert!(!parsed.matches(host), "{text} vs {host}");
        }
    }
}

/// A pattern serialises as plain text and revalidates on decode.
#[test]
fn pattern_text_round_trips_and_decode_validates() {
    let parsed = pattern("*.Example.com");
    assert_eq!(
        serde_json::to_string(&parsed).expect("encode"),
        "\"*.example.com\""
    );
    let decoded: WebHostPattern = serde_json::from_str("\"*.example.com\"").expect("decode");
    assert_eq!(decoded, parsed);
    for bad in [
        "\"localhost\"",
        "\"127.0.0.1\"",
        "\"\"",
        "\"*.com\"",
        "\"Example.com\"",
        "\"example.com.\"",
        "7",
    ] {
        assert!(
            serde_json::from_str::<WebHostPattern>(bad).is_err(),
            "{bad}"
        );
    }
}

struct Scene {
    store: ControlStore,
    data: PathBuf,
    root: PathBuf,
    _directory: tempfile::TempDir,
}

fn scene() -> Scene {
    let directory = tempfile::tempdir().expect("temporary directory");
    let data = directory.path().join("data");
    let root = directory.path().join("projects");
    fs::create_dir_all(&root).expect("development root");
    let mut store = ControlStore::open(&data).expect("store");
    store.set_development_root(&root, 0).expect("root");
    Scene {
        store,
        data,
        root,
        _directory: directory,
    }
}

impl Scene {
    fn register(&mut self, name: &str) -> WorkspaceId {
        let path = self.root.join(name);
        fs::create_dir(&path).expect("workspace directory");
        let revision = self.store.revision();
        self.store
            .register_workspace(&path, revision)
            .expect("workspace")
            .id()
    }

    fn add(&mut self, id: WorkspaceId, text: &str) -> Result<WebHostPattern, ControlError> {
        let revision = self.store.revision();
        self.store.add_web_fetch_host(id, text, revision)
    }
}

/// Hosts are recorded per workspace, advance the revision, replay, and are
/// removed by canonical text.
#[test]
fn hosts_are_recorded_replayed_and_removed() {
    let mut scene = scene();
    let id = scene.register("app");
    let other = scene.register("other");
    let revision = scene.store.revision();
    assert!(scene.store.web_fetch_hosts(id).is_empty());

    scene.add(id, "Docs.Example.com.").expect("add exact");
    scene.add(id, "*.rust-lang.org").expect("add wildcard");
    assert_eq!(scene.store.revision(), revision + 2);
    let listed: Vec<_> = scene
        .store
        .web_fetch_hosts(id)
        .iter()
        .map(WebHostPattern::as_str)
        .collect();
    assert_eq!(listed, ["docs.example.com", "*.rust-lang.org"]);
    assert!(scene.store.web_fetch_host_allowed(id, "DOCS.example.com"));
    assert!(scene.store.web_fetch_host_allowed(id, "doc.rust-lang.org"));
    assert!(!scene.store.web_fetch_host_allowed(id, "rust-lang.org"));
    assert!(!scene.store.web_fetch_host_allowed(id, "example.com"));
    assert!(scene.store.web_fetch_hosts(other).is_empty());
    assert!(
        !scene
            .store
            .web_fetch_host_allowed(other, "docs.example.com")
    );

    let encoded = serde_json::to_value(scene.store.snapshot()).expect("snapshot JSON");
    assert_eq!(
        encoded["workspaces"][0]["permissions"]["web_fetch_hosts"],
        serde_json::json!(["docs.example.com", "*.rust-lang.org"])
    );
    assert_eq!(
        encoded["workspaces"][1]["permissions"]["web_fetch_hosts"],
        serde_json::json!([])
    );
    assert_eq!(encoded["workspaces"][0]["permissions"]["web_fetch"], "deny");

    let data = scene.data.clone();
    drop(scene.store);
    let mut store = ControlStore::open(&data).expect("replay");
    assert_eq!(store.web_fetch_hosts(id).len(), 2);
    let revision = store.revision();
    let removed = store
        .remove_web_fetch_host(id, "DOCS.example.com", revision)
        .expect("remove");
    assert_eq!(removed.as_str(), "docs.example.com");
    assert_eq!(store.revision(), revision + 1);
    assert_eq!(store.web_fetch_hosts(id).len(), 1);
    drop(store);
    let store = ControlStore::open(&data).expect("replay after removal");
    assert_eq!(store.web_fetch_hosts(id)[0].as_str(), "*.rust-lang.org");
}

/// Stale revisions, unknown workspaces, duplicates, absent removals, invalid
/// patterns, and the cap are refused without changing the revision.
#[test]
fn refused_changes_leave_the_settings_unchanged() {
    let mut scene = scene();
    let id = scene.register("app");
    scene.add(id, "example.com").expect("first add");
    let revision = scene.store.revision();

    assert!(matches!(
        scene
            .store
            .add_web_fetch_host(id, "other.example.com", revision - 1),
        Err(ControlError::Conflict { .. })
    ));
    assert!(matches!(
        scene
            .store
            .remove_web_fetch_host(id, "example.com", revision - 1),
        Err(ControlError::Conflict { .. })
    ));
    let unknown = WorkspaceId::fresh();
    assert!(matches!(
        scene
            .store
            .add_web_fetch_host(unknown, "example.com", revision),
        Err(ControlError::UnknownWorkspace)
    ));
    assert!(matches!(
        scene
            .store
            .remove_web_fetch_host(unknown, "example.com", revision),
        Err(ControlError::UnknownWorkspace)
    ));
    assert!(matches!(
        scene.add(id, "EXAMPLE.com."),
        Err(ControlError::WebHost(WebHostRefusal::Duplicate))
    ));
    assert!(matches!(
        scene.add(id, "localhost"),
        Err(ControlError::WebHost(WebHostRefusal::Reserved))
    ));
    assert!(matches!(
        scene
            .store
            .remove_web_fetch_host(id, "absent.example.com", revision),
        Err(ControlError::WebHost(WebHostRefusal::NotListed))
    ));
    assert!(matches!(
        scene
            .store
            .remove_web_fetch_host(id, "*.example.com", revision),
        Err(ControlError::WebHost(WebHostRefusal::NotListed))
    ));
    assert!(matches!(
        scene.store.remove_web_fetch_host(id, "127.0.0.1", revision),
        Err(ControlError::WebHost(WebHostRefusal::IpLiteral))
    ));
    assert_eq!(scene.store.revision(), revision);
    assert_eq!(scene.store.web_fetch_hosts(id).len(), 1);
}

/// A workspace holds at most 32 patterns; another workspace has its own cap.
#[test]
fn the_cap_is_per_workspace() {
    let mut scene = scene();
    let id = scene.register("app");
    let other = scene.register("other");
    for index in 1..MAX_WEB_FETCH_HOSTS {
        scene
            .add(id, &format!("h{index}.example.com"))
            .expect("add");
    }
    scene.add(id, "last.example.com").expect("the 32nd");
    assert_eq!(scene.store.web_fetch_hosts(id).len(), MAX_WEB_FETCH_HOSTS);
    let revision = scene.store.revision();
    assert!(matches!(
        scene.add(id, "extra.example.com"),
        Err(ControlError::WebHost(WebHostRefusal::TooMany))
    ));
    assert_eq!(scene.store.revision(), revision);
    scene
        .add(other, "extra.example.com")
        .expect("other workspace");
}

/// Unregistering drops the allowlist with the registration, as it drops the
/// package cache root, and a re-registered folder starts empty.
#[test]
fn unregister_drops_the_allowlist() {
    let mut scene = scene();
    let id = scene.register("app");
    let keep = scene.register("keep");
    scene.add(id, "example.com").expect("add");
    scene.add(keep, "example.org").expect("add other");
    let revision = scene.store.revision();
    scene
        .store
        .unregister_workspace(id, revision)
        .expect("unregister");
    assert!(scene.store.web_fetch_hosts(id).is_empty());
    assert!(!scene.store.web_fetch_host_allowed(id, "example.com"));
    assert_eq!(scene.store.web_fetch_hosts(keep).len(), 1);
    let path = scene.root.join("app");
    let revision = scene.store.revision();
    let again = scene
        .store
        .register_workspace(&path, revision)
        .expect("register again")
        .id();
    assert_ne!(again, id);
    assert!(scene.store.web_fetch_hosts(again).is_empty());
    let data = scene.data.clone();
    drop(scene.store);
    let store = ControlStore::open(&data).expect("replay");
    assert!(store.web_fetch_hosts(id).is_empty());
    assert!(store.web_fetch_hosts(again).is_empty());
    assert_eq!(store.web_fetch_hosts(keep)[0].as_str(), "example.org");
}

/// Replay re-validates every pattern and refuses a tampered or inconsistent
/// log instead of opening it.
#[test]
fn replay_refuses_invalid_hosts() {
    let mut scene = scene();
    let id = scene.register("app");
    scene.add(id, "example.com").expect("add");
    let data = scene.data.clone();
    drop(scene.store);
    let journal = data.join(FILE_NAME);
    let text = fs::read_to_string(&journal).expect("journal text");
    assert!(text.contains("web_fetch_host_added"), "{text}");

    for replacement in ["127.0.0.1", "localhost", "*.com", "Example.com", ""] {
        let tampered = text.replacen(
            "\"pattern\":\"example.com\"",
            &format!("\"pattern\":\"{replacement}\""),
            1,
        );
        assert_ne!(text, tampered);
        fs::write(&journal, &tampered).expect("tamper journal");
        assert!(
            ControlStore::open(&data).is_err(),
            "{replacement:?} must be refused"
        );
    }

    // A duplicate add and a removal of an absent pattern are not a valid log.
    let line = text
        .lines()
        .find(|line| line.contains("web_fetch_host_added"))
        .expect("added line");
    let duplicated = format!("{text}{line}\n");
    fs::write(&journal, duplicated).expect("duplicate journal");
    assert!(ControlStore::open(&data).is_err());
    fs::write(&journal, &text).expect("restore journal");
    let store = ControlStore::open(&data).expect("untampered log opens");
    assert_eq!(store.web_fetch_hosts(id).len(), 1);
}

/// A permissions change keeps the allowlist, and a stored `web_fetch` state
/// survives replay and is reported by status.
#[test]
fn permissions_changes_keep_the_allowlist() {
    let mut scene = scene();
    let id = scene.register("app");
    scene.add(id, "example.com").expect("add");
    let revision = scene.store.revision();
    scene
        .store
        .set_permissions(
            id,
            PermissionSet {
                model_tools: true,
                web_fetch: Access::Allow,
                ..PermissionSet::default()
            },
            revision,
        )
        .expect("permissions");
    let encoded = serde_json::to_value(scene.store.snapshot()).expect("snapshot JSON");
    assert_eq!(
        encoded["workspaces"][0]["permissions"]["web_fetch"],
        "allow"
    );
    assert_eq!(
        encoded["workspaces"][0]["permissions"]["web_fetch_hosts"][0],
        "example.com"
    );
    let data = scene.data.clone();
    drop(scene.store);
    let store = ControlStore::open(&data).expect("replay");
    let workspace = store.workspace(id).expect("workspace");
    assert_eq!(workspace.permissions().web_fetch, Access::Allow);
    assert_eq!(store.web_fetch_hosts(id).len(), 1);
}
