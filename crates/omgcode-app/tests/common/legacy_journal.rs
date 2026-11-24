//! Rewrites a settings journal into the shape an older version wrote, for
//! tests of states that version could record. Shared by the `omgcode-app`
//! tests (as `common::legacy_journal`) and the `omgcode-cli` tests (through a
//! `#[path]` include), so one copy follows core's event serialization.

use std::fs;
use std::path::Path;

/// Remove the endpoint core now records for a hosted `api_key` profile added
/// without one, so the settings journal holds the bytes 0.58.0 or earlier
/// wrote for it (ADR-0092). Replay still accepts such a profile, and the
/// refusals for one are what these tests check.
pub fn forget_recorded_endpoint(data: &Path, name: &str) {
    use std::fmt::Write as _;

    const FIELD: &str = ",\"endpoint\":\"";
    let path = data.join("control-v1.jsonl");
    let text = fs::read_to_string(&path).expect("settings journal");
    let marker = format!("\"name\":\"{name}\"");
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        match line.find(FIELD).filter(|_| line.contains(&marker)) {
            Some(start) => {
                let value = start + FIELD.len();
                let end = value + line[value..].find('"').expect("endpoint end") + 1;
                writeln!(out, "{}{}", &line[..start], &line[end..]).expect("line");
            }
            None => writeln!(out, "{line}").expect("line"),
        }
    }
    fs::write(&path, out).expect("settings journal");
}
