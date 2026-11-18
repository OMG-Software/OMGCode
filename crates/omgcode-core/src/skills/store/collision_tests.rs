//! Exclusive package creation never claims an entry this import did not make.

use std::fs;

use super::*;
use crate::test_support::private_data_root;

#[test]
fn given_an_existing_package_id_when_importing_then_the_existing_package_is_kept() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let data = directory.path().join("data");
    private_data_root(&data);
    let source = directory.path().join("demo");
    fs::create_dir(&source).expect("source");
    fs::write(
        source.join("SKILL.md"),
        "---\nname: demo\ndescription: Demonstrates a skill.\n---\nFollow the steps.\n",
    )
    .expect("SKILL.md");
    let root = DirectoryRecord::capture(&data).expect("data root");
    let first = import_package_in(&root, &source).expect("first import");

    // Force the second import to generate the identifier the first one owns.
    let mut faults = Faults {
        skill_id: Some(first.skill_id),
        ..Faults::default()
    };
    let error = import_with(&root, &source, &mut faults).expect_err("identifier collision");

    assert_eq!(error.code(), "store_io");
    assert!(
        !matches!(error, SkillImportError::Orphaned { .. }),
        "a definite collision is not reported as a leftover: {error}"
    );
    let loaded = load_skill_body_in(
        &root,
        first.skill_id,
        &first.skill_md_sha256,
        first.skill_md_bytes,
    )
    .expect("the existing package is untouched");
    assert_eq!(loaded.skill_md_sha256, first.skill_md_sha256);
}
