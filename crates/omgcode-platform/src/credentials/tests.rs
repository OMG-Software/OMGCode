//! Platform-neutral checks that run before any storage I/O, plus the native
//! store contract exercised against the real backend on Windows and Linux.

use super::*;

/// A random-UUID identifier in the fixed lowercase simple form.
pub(crate) fn fresh_id() -> CredentialId {
    CredentialId::parse(&uuid::Uuid::new_v4().simple().to_string())
        .expect("generated identifier is valid")
}

/// A distinctive sentinel secret that tests can search for in outputs.
pub(crate) fn sentinel_secret() -> Secret {
    Secret::new(b"omgcode-test-sentinel-secret-7f3a9c".to_vec()).expect("sentinel")
}

#[test]
fn identifiers_accept_only_the_random_uuid_simple_form() {
    let valid = "0123456789ab4def8123456789abcdef";
    assert_eq!(CredentialId::parse(valid).expect("valid").as_str(), valid);
    for invalid in [
        "",
        "0123456789ab4def8123456789abcde",
        "0123456789ab4def8123456789abcdef0",
        "0123456789AB4DEF8123456789ABCDEF",
        "0123456789ab1def8123456789abcdef",
        "0123456789ab4def7123456789abcdef",
        "0123456789ab4def8123456789abcdeg",
        "01234567-89ab-4def-8123-456789abcdef",
        "../123456789ab4def8123456789abcd",
        "0123456789ab4def8123456789abc\\ef",
    ] {
        assert!(
            matches!(
                CredentialId::parse(invalid),
                Err(CredentialError::Invalid(_))
            ),
            "{invalid:?} must be refused"
        );
    }
}

#[test]
fn secrets_are_bounded_non_empty_and_redacted() {
    assert!(matches!(
        Secret::new(Vec::new()),
        Err(CredentialError::Invalid(_))
    ));
    assert!(matches!(
        Secret::new(vec![b'k'; MAX_SECRET_BYTES + 1]),
        Err(CredentialError::TooLarge)
    ));
    let largest = Secret::new(vec![b'k'; MAX_SECRET_BYTES]).expect("largest secret");
    assert_eq!(largest.expose().len(), MAX_SECRET_BYTES);

    let secret = sentinel_secret();
    let debug = format!("{secret:?}");
    assert_eq!(debug, "Secret(<redacted>)");
    assert!(!debug.contains("sentinel"));
}

#[test]
fn reading_strips_exactly_one_line_ending_and_refuses_empty_input() {
    let read = |input: &[u8]| Secret::read_from(input);
    assert_eq!(read(b"key\n").expect("lf").expose(), b"key");
    assert_eq!(read(b"key\r\n").expect("crlf").expose(), b"key");
    assert_eq!(read(b"key").expect("no ending").expose(), b"key");
    assert_eq!(read(b"key\n\n").expect("two lf").expose(), b"key\n");
    assert_eq!(read(b"key\r\n\r\n").expect("two crlf").expose(), b"key\r\n");
    assert_eq!(read(b" key \r").expect("lone cr").expose(), b" key \r");
    for empty in [&b""[..], b"\n", b"\r\n"] {
        assert!(
            matches!(read(empty), Err(CredentialError::Invalid(_))),
            "{empty:?} must be refused"
        );
    }

    let mut largest = vec![b'k'; MAX_SECRET_BYTES];
    largest.extend_from_slice(b"\r\n");
    assert_eq!(
        read(&largest).expect("largest").expose().len(),
        MAX_SECRET_BYTES
    );
    let oversized = vec![b'k'; MAX_SECRET_BYTES + 1];
    assert!(matches!(read(&oversized), Err(CredentialError::TooLarge)));
    let mut long = vec![b'k'; MAX_SECRET_BYTES + 1];
    long.extend_from_slice(b"\n");
    assert!(matches!(read(&long), Err(CredentialError::TooLarge)));
    let huge = vec![b'k'; 4 * MAX_SECRET_BYTES];
    assert!(matches!(read(&huge), Err(CredentialError::TooLarge)));
}

/// A reader that returns its bytes in small pieces and is interrupted once.
struct Trickle<'a> {
    bytes: &'a [u8],
    interrupted: bool,
}

impl Read for Trickle<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if !self.interrupted {
            self.interrupted = true;
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        let count = buffer.len().min(self.bytes.len()).min(3);
        buffer[..count].copy_from_slice(&self.bytes[..count]);
        self.bytes = &self.bytes[count..];
        Ok(count)
    }
}

#[test]
fn reading_assembles_short_reads_and_retries_interruptions() {
    let secret = Secret::read_from(Trickle {
        bytes: b"sk-trickled-value\r\n",
        interrupted: false,
    })
    .expect("trickled secret");
    assert_eq!(secret.expose(), b"sk-trickled-value");
}

#[test]
fn backend_spellings_round_trip() {
    for backend in [
        CredentialBackend::WindowsDpapi,
        CredentialBackend::LinuxSecretService,
    ] {
        assert_eq!(CredentialBackend::parse(backend.as_str()), Some(backend));
    }
    assert_eq!(CredentialBackend::parse("plaintext"), None);
    let expected = if cfg!(windows) {
        Some(CredentialBackend::WindowsDpapi)
    } else if cfg!(target_os = "linux") {
        Some(CredentialBackend::LinuxSecretService)
    } else {
        None
    };
    assert_eq!(CredentialBackend::native(), expected);
}

/// The store contract every native backend must meet. `store` is a real
/// backend; nothing here is mocked.
#[cfg(any(windows, target_os = "linux"))]
pub(crate) fn exercise_contract(store: &CredentialStore) {
    let id = fresh_id();
    let secret = sentinel_secret();

    // Absent before storing, then a round trip returns the exact bytes.
    assert!(matches!(store.load(&id), Err(CredentialError::NotFound)));
    assert!(matches!(store.delete(&id), Err(CredentialError::NotFound)));
    store.store(&id, &secret).expect("store a new credential");
    assert_eq!(store.load(&id).expect("load").expose(), secret.expose());

    // Create-only: an existing identifier is refused and keeps its secret.
    let other = Secret::new(b"a-different-secret".to_vec()).expect("other");
    assert!(matches!(
        store.store(&id, &other),
        Err(CredentialError::AlreadyExists)
    ));
    assert_eq!(
        store.load(&id).expect("unchanged").expose(),
        secret.expose()
    );

    // A second identifier is independent.
    let second = fresh_id();
    store.store(&second, &other).expect("second credential");
    assert_eq!(
        store.load(&second).expect("second").expose(),
        other.expose()
    );

    // Delete, then the identifier is absent again.
    store.delete(&id).expect("delete");
    assert!(matches!(store.load(&id), Err(CredentialError::NotFound)));
    assert!(matches!(store.delete(&id), Err(CredentialError::NotFound)));
    assert_eq!(
        store.load(&second).expect("other survives").expose(),
        other.expose()
    );
    store.delete(&second).expect("delete second");
}
