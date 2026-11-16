//! The real `omgcode-desktop` binary refuses `--endpoint` beside
//! `--provider-profile` before any window or service exists (ADR-0048).
//!
//! Both cases fail during argument handling or the profile check, so no window
//! opens and the process exits at once.

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_omgcode-desktop"))
        .args(args)
        .output()
        .expect("run omgcode-desktop")
}

#[test]
fn an_endpoint_beside_a_profile_prints_the_usage_and_exits_with_two() {
    let root = std::env::temp_dir().join(format!("omgcode-desktop-flags-{}", std::process::id()));
    let data = root.to_string_lossy().into_owned();
    for args in [
        &[
            "--data-dir",
            data.as_str(),
            "--endpoint",
            "http://127.0.0.1:9/v1",
            "--provider-profile",
            "lms",
        ][..],
        &[
            "--provider-profile",
            "lms",
            "--endpoint",
            "http://127.0.0.1:1234/v1",
        ],
    ] {
        let output = run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.starts_with("usage: omgcode-desktop"), "{stderr}");
        assert!(stderr.contains("cannot be combined with --endpoint"));
    }
    assert!(!root.exists(), "a usage error created the data root");
}

#[test]
fn a_profile_with_no_settings_store_exits_with_one_and_creates_nothing() {
    let root = std::env::temp_dir().join(format!(
        "omgcode-desktop-flags-store-{}",
        std::process::id()
    ));
    let data = root.to_string_lossy().into_owned();
    let output = run(&["--data-dir", data.as_str(), "--provider-profile", "lms"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("omgcode-desktop: credential: no settings store exists"),
        "{stderr}"
    );
    assert!(!root.exists(), "the launch check created the data root");
}
