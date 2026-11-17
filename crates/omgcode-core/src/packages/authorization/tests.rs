use super::*;

fn encoded(input: &[u8]) -> String {
    let mut output = Vec::new();
    encode_base64(input, &mut output);
    String::from_utf8(output).expect("base64 is ASCII")
}

fn secret(text: &str) -> Secret {
    Secret::new(text.as_bytes().to_vec()).expect("secret")
}

/// The encoder matches every RFC 4648 section 10 test vector.
#[test]
fn base64_matches_the_rfc_4648_vectors() {
    for (input, expected) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(encoded(input.as_bytes()), expected, "input {input:?}");
    }
}

/// Every byte value encodes to the standard alphabet, including `+` and `/`.
#[test]
fn base64_uses_the_standard_alphabet_for_all_bytes() {
    assert_eq!(encoded(&[0xfb, 0xff, 0xbf]), "+/+/");
    assert_eq!(encoded(&[0x00, 0x10, 0x83]), "ABCD");
}

#[test]
fn bearer_and_basic_values_are_sensitive_and_exact() {
    let bearer = header_value(PackageAuth::Bearer, &secret("tok_123.abc")).expect("bearer");
    assert_eq!(bearer.as_bytes(), b"Bearer tok_123.abc");
    assert!(bearer.is_sensitive());
    assert!(!format!("{bearer:?}").contains("tok_123"));

    let basic = header_value(PackageAuth::Basic, &secret("alice:s3cret")).expect("basic");
    assert_eq!(basic.as_bytes(), b"Basic YWxpY2U6czNjcmV0");
    assert!(basic.is_sensitive());
}
