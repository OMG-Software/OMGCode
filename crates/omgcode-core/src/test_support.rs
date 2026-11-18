//! Helpers shared by tests that need the real native credential store.

use std::fs;
use std::path::Path;

/// Create `data` as a private data root. Windows temporary directories
/// inherit grants the private store refuses, so give the root an owner-only
/// ACL, as an installed profile has.
pub(crate) fn private_data_root(data: &Path) {
    fs::create_dir(data).expect("data root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(data, fs::Permissions::from_mode(0o700)).expect("private mode");
    }
    #[cfg(windows)]
    {
        use std::process::Command;
        let output = Command::new("whoami")
            .args(["/user", "/fo", "csv", "/nh"])
            .output()
            .expect("whoami");
        assert!(output.status.success(), "whoami failed");
        let text = String::from_utf8(output.stdout).expect("UTF-8");
        let sid = text
            .trim()
            .rsplit(',')
            .next()
            .expect("SID column")
            .trim_matches('"')
            .to_owned();
        let status = Command::new("icacls")
            .arg(data)
            .args(["/inheritance:r", "/grant:r", &format!("*{sid}:(OI)(CI)F")])
            .output()
            .expect("icacls");
        assert!(status.status.success(), "icacls failed");
    }
}
