//! Native DPAPI store tests on a temporary NTFS private root.

use super::*;
use crate::credentials::CredentialStore;
use crate::credentials::tests::{exercise_contract, fresh_id, sentinel_secret};
use crate::private_store::windows::tests::private_root;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// A trusted private data root inside a temporary directory.
struct Root {
    path: PathBuf,
    _temporary: tempfile::TempDir,
}

fn root() -> Root {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let path = private_root(temporary.path(), "data");
    Root {
        path,
        _temporary: temporary,
    }
}

fn open(root: &Root) -> CredentialStore {
    CredentialStore::open(&root.path).expect("DPAPI credential store")
}

fn credential_path(root: &Root, id: &CredentialId) -> PathBuf {
    root.path
        .join(DIRECTORY)
        .join(format!("{}{SUFFIX}", id.as_str()))
}

#[test]
fn dpapi_store_meets_the_native_contract() {
    let root = root();
    let store = open(&root);
    assert_eq!(
        store.backend(),
        crate::credentials::CredentialBackend::WindowsDpapi
    );
    exercise_contract(&store);
}

#[test]
fn stored_files_hold_no_plaintext_and_opening_creates_nothing() {
    let root = root();
    let store = open(&root);
    assert!(!root.path.join(DIRECTORY).exists(), "open must not create");
    let id = fresh_id();
    let secret = sentinel_secret();
    store.store(&id, &secret).expect("store");

    let bytes = fs::read(credential_path(&root, &id)).expect("credential file");
    assert!(
        !bytes
            .windows(secret.expose().len())
            .any(|window| window == secret.expose()),
        "the file must not contain the plaintext secret"
    );
}

#[test]
fn a_missing_root_is_unavailable() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let missing = temporary.path().join("missing");
    assert!(matches!(
        CredentialStore::open(&missing),
        Err(CredentialError::Unavailable(_))
    ));
    assert!(!missing.exists());
}

#[test]
fn an_edited_or_truncated_blob_is_corrupt_not_absent() {
    let root = root();
    let store = open(&root);
    let id = fresh_id();
    store.store(&id, &sentinel_secret()).expect("store");
    let path = credential_path(&root, &id);
    let original = fs::read(&path).expect("blob");

    // Flip one byte near the end, inside the ciphertext or its MAC.
    let mut edited = original.clone();
    let last = edited.len() - 8;
    edited[last] ^= 0x5a;
    fs::write(&path, &edited).expect("edit blob in place");
    assert!(
        matches!(store.load(&id), Err(CredentialError::Corrupt(_))),
        "an edited blob must be corrupt"
    );

    fs::write(&path, &original[..original.len() / 2]).expect("truncate blob");
    assert!(
        matches!(store.load(&id), Err(CredentialError::Corrupt(_))),
        "a truncated blob must be corrupt"
    );

    fs::write(&path, b"").expect("empty blob");
    assert!(matches!(store.load(&id), Err(CredentialError::Corrupt(_))));
}

#[test]
fn a_blob_copied_to_another_identifier_fails_its_entropy_binding() {
    let root = root();
    let store = open(&root);
    let source = fresh_id();
    store.store(&source, &sentinel_secret()).expect("store");
    let blob = fs::read(credential_path(&root, &source)).expect("blob");

    // Create the target through the private store so its DACL is exact; only
    // the identifier binding can then refuse it.
    let target = fresh_id();
    let directory = store.backend.existing_directory().expect("directory");
    let mut file = directory
        .create_file(&file_name(&target))
        .expect("private target file");
    file.write_all(&blob).expect("copy blob");
    file.sync_all().expect("sync copy");
    drop(file);

    assert!(
        matches!(store.load(&target), Err(CredentialError::Corrupt(_))),
        "a blob bound to another identifier must not decrypt"
    );
    assert!(store.load(&source).is_ok(), "the source still decrypts");
}

#[test]
fn a_credential_file_with_a_broadened_dacl_is_refused() {
    let root = root();
    let store = open(&root);
    let id = fresh_id();
    store.store(&id, &sentinel_secret()).expect("store");
    let path = credential_path(&root, &id);

    // Add an explicit read grant for Everyone (S-1-1-0) with the native tool.
    let status = Command::new("icacls")
        .arg(&path)
        .args(["/grant", "*S-1-1-0:(R)"])
        .output()
        .expect("run icacls");
    assert!(
        status.status.success(),
        "icacls failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    assert!(
        matches!(store.load(&id), Err(CredentialError::Corrupt(_))),
        "a file with a non-private DACL must be refused before decryption"
    );
}

#[test]
fn an_unprotected_credential_file_is_refused() {
    let root = root();
    let store = open(&root);
    let id = fresh_id();
    store.store(&id, &sentinel_secret()).expect("store");
    let path = credential_path(&root, &id);

    // Re-enable inheritance so the DACL is no longer protected.
    let status = Command::new("icacls")
        .arg(&path)
        .arg("/inheritance:e")
        .output()
        .expect("run icacls");
    assert!(
        status.status.success(),
        "icacls failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert!(matches!(store.load(&id), Err(CredentialError::Corrupt(_))));
}
