//! Where a Claude Code connection added in the desktop finds `claude`, keeps
//! its sign-in, and how Claude's own sign-in starts (ADR-0111).
//!
//! Add creates the connection's sign-in folder itself, empty, so nothing in
//! it was written by the connection's sandbox. On Windows it then starts the
//! operator's `claude auth login` in a console window of its own, with
//! `CLAUDE_CONFIG_DIR` set to that folder and an environment built from
//! nothing. That program runs outside the sandbox, as it would in a
//! terminal: the operator finishes Claude's sign-in in the browser, and
//! OMGCode waits for nothing, reads nothing in the folder, and never sees
//! the credential.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use omgcode_core::turn::display_safe_line;
use omgcode_platform::process::{ProcessSpec, start_in_new_console};
use uuid::Uuid;

/// The folder under the per-user local data folder that holds each
/// connection's own sign-in folder. It is not OMGCode's application storage,
/// which a sign-in folder must not overlap.
const SIGN_IN_PARENT: &str = "omgcode-claude-code";

/// Whether Add starts Claude's sign-in in a window of its own. Elsewhere the
/// confirmation names the command to run in a terminal.
pub(super) const SIGN_IN_WINDOW: bool = cfg!(windows);

/// The operator variables a Windows sign-in keeps, when set: the system and
/// profile folders a program and its browser hand-off need. Every other
/// variable, including any Anthropic key or token, is left out.
const WINDOWS_KEPT: [&str; 8] = [
    "SystemRoot",
    "windir",
    "SystemDrive",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "TEMP",
    "TMP",
];

/// What the desktop needs for Claude Code connections (ADR-0111). Every
/// field comes from the shell, never from the interface.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClaudeCodeHost {
    /// The `omgcode` executable a turn runs as its MCP relay. The shell
    /// passes the one in its own executable's folder; on Windows the
    /// connection's container needs a grant for that folder, so Add is
    /// refused there without one. `None` fails every Claude Code turn with
    /// `claude_code_unusable` before any process starts.
    pub relay: Option<PathBuf>,
    /// The operator's `PATH`, searched for `claude` when a connection is
    /// added. Nothing found opens a file dialog.
    pub search_path: Option<OsString>,
    /// The operator's home folder, whose native-installer folders are
    /// searched after `PATH`.
    pub home: Option<PathBuf>,
    /// The folder under which Add creates each connection's own empty
    /// sign-in folder. `None` refuses Add.
    pub sign_in_root: Option<PathBuf>,
}

impl ClaudeCodeHost {
    /// The host for this operator: `relay`, this process's `PATH`, and the
    /// per-user home and local data folders.
    #[must_use]
    pub fn for_operator(relay: Option<PathBuf>) -> Self {
        let folders = directories::BaseDirs::new();
        Self {
            relay,
            search_path: std::env::var_os("PATH"),
            home: folders
                .as_ref()
                .map(|folders| folders.home_dir().to_owned()),
            sign_in_root: folders
                .as_ref()
                .map(|folders| folders.data_local_dir().join(SIGN_IN_PARENT)),
        }
    }
}

/// How Claude's own sign-in starts for a new connection: the program, its
/// arguments, the complete environment, and the working folder, which is the
/// sign-in folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeCodeSignIn {
    /// The connection's recorded `claude` program.
    pub program: PathBuf,
    /// `auth login`.
    pub arguments: Vec<OsString>,
    /// The complete environment; nothing else is inherited.
    pub environment: Vec<(OsString, OsString)>,
    /// The sign-in folder, also `CLAUDE_CONFIG_DIR`.
    pub folder: PathBuf,
}

impl ClaudeCodeSignIn {
    /// The sign-in for `program` and `folder`, keeping only the listed
    /// operator variables that `operator` reports.
    pub(super) fn new(
        program: &Path,
        folder: &Path,
        operator: &dyn Fn(&str) -> Option<OsString>,
    ) -> Self {
        let mut environment: Vec<(OsString, OsString)> = Vec::new();
        if cfg!(windows) {
            for name in WINDOWS_KEPT {
                if let Some(value) = operator(name).filter(|value| !value.is_empty()) {
                    environment.push((name.into(), value));
                }
            }
            let root = operator("SystemRoot")
                .filter(|value| !value.is_empty())
                .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
            let system32 = root.join("System32");
            let mut search = OsString::from(system32.as_os_str());
            for folder in [
                root.clone(),
                system32.join("WindowsPowerShell").join("v1.0"),
            ] {
                search.push(";");
                search.push(folder.as_os_str());
            }
            environment.push(("PATH".into(), search));
            environment.push(("ComSpec".into(), system32.join("cmd.exe").into_os_string()));
            environment.push(("PATHEXT".into(), ".COM;.EXE;.BAT;.CMD".into()));
        } else {
            for name in ["HOME", "DISPLAY", "WAYLAND_DISPLAY", "XDG_RUNTIME_DIR"] {
                if let Some(value) = operator(name).filter(|value| !value.is_empty()) {
                    environment.push((name.into(), value));
                }
            }
            environment.push(("PATH".into(), "/usr/bin:/bin".into()));
            environment.push(("LANG".into(), "C.UTF-8".into()));
        }
        environment.extend([
            ("CLAUDE_CONFIG_DIR".into(), folder.as_os_str().to_owned()),
            // Defence in depth: the folder is new and empty, so it holds no
            // settings, hooks, or plugins for safe mode to switch off.
            ("CLAUDE_CODE_SAFE_MODE".into(), "1".into()),
            ("DISABLE_AUTOUPDATER".into(), "1".into()),
            ("DISABLE_TELEMETRY".into(), "1".into()),
        ]);
        Self {
            program: program.to_owned(),
            arguments: vec!["auth".into(), "login".into()],
            environment,
            folder: folder.to_owned(),
        }
    }
}

/// The command an operator runs in a terminal to sign `program` in to
/// `folder`, quoted for PowerShell on Windows and for a POSIX shell
/// elsewhere. Display-safe.
pub(super) fn terminal_command(program: &Path, folder: &Path) -> String {
    let program = program.to_string_lossy();
    let folder = folder.to_string_lossy();
    let command = if cfg!(windows) {
        let quoted = |text: &str| format!("'{}'", text.replace('\'', "''"));
        format!(
            "$env:CLAUDE_CONFIG_DIR = {}; & {} auth login",
            quoted(&folder),
            quoted(program.trim_start_matches(r"\\?\")),
        )
    } else {
        let quoted = |text: &str| format!("'{}'", text.replace('\'', r"'\''"));
        format!(
            "CLAUDE_CONFIG_DIR={} {} auth login",
            quoted(&folder),
            quoted(&program),
        )
    };
    display_safe_line(&command)
}

/// Start `sign_in` in a console window of its own and return without
/// waiting for it (ADR-0111). Windows only; elsewhere the reason says so.
///
/// # Errors
///
/// A display-safe reason when the program could not be started.
pub fn start_claude_code_sign_in(sign_in: &ClaudeCodeSignIn) -> Result<(), String> {
    start_in_new_console(&ProcessSpec {
        program: sign_in.program.clone(),
        arguments: sign_in.arguments.clone(),
        working_directory: sign_in.folder.clone(),
        environment: sign_in.environment.clone(),
    })
    .map_err(|error| display_safe_line(&error.to_string()))
}

/// A sign-in folder this change created. It is new and empty, and it is
/// removed again unless [`keep`](Self::keep) is called, so a cancelled or
/// refused change leaves none behind.
pub(super) struct NewSignInFolder {
    path: PathBuf,
    kept: bool,
}

impl NewSignInFolder {
    /// Create a new folder with a random name under `root`, creating `root`
    /// first if needed. On Unix both are private to the owner.
    ///
    /// # Errors
    ///
    /// `root` is not absolute, or a folder could not be created. A folder of
    /// the same name is never reused.
    pub(super) fn create(root: &Path) -> io::Result<Self> {
        if !root.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the sign-in parent is not absolute",
            ));
        }
        private_builder(true).create(root)?;
        let path = root.join(Uuid::new_v4().simple().to_string());
        // Not recursive, so an existing folder of that name is an error.
        private_builder(false).create(&path)?;
        Ok(Self { path, kept: false })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    /// Keep the folder: a connection records it.
    pub(super) fn keep(mut self) -> PathBuf {
        self.kept = true;
        std::mem::take(&mut self.path)
    }
}

impl Drop for NewSignInFolder {
    fn drop(&mut self) {
        if !self.kept {
            // Only an empty folder is removed. One that cannot be removed is
            // left empty, and nothing records it.
            let _ = fs::remove_dir(&self.path);
        }
    }
}

fn private_builder(recursive: bool) -> fs::DirBuilder {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(recursive);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operator(name: &str) -> Option<OsString> {
        match name {
            "SystemRoot" => Some(r"D:\Windows".into()),
            "USERPROFILE" | "HOME" => Some("/home/operator".into()),
            "ANTHROPIC_API_KEY" | "CLAUDE_CODE_OAUTH_TOKEN" => Some("secret".into()),
            "TEMP" => Some("temporary".into()),
            "APPDATA" => Some(OsString::new()),
            _ => None,
        }
    }

    fn value<'a>(sign_in: &'a ClaudeCodeSignIn, name: &str) -> Option<&'a OsString> {
        sign_in
            .environment
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }

    #[test]
    fn the_sign_in_environment_is_built_from_nothing() {
        let folder = std::env::temp_dir().join("sign-in");
        let program = std::env::temp_dir().join("claude");
        let sign_in = ClaudeCodeSignIn::new(&program, &folder, &operator);
        assert_eq!(
            sign_in.arguments,
            vec![OsString::from("auth"), "login".into()]
        );
        assert_eq!(sign_in.folder, folder);
        assert_eq!(
            value(&sign_in, "CLAUDE_CONFIG_DIR"),
            Some(&folder.clone().into_os_string())
        );
        assert_eq!(value(&sign_in, "CLAUDE_CODE_SAFE_MODE"), Some(&"1".into()));
        for withheld in ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN", "APPDATA"] {
            assert_eq!(value(&sign_in, withheld), None, "{withheld}");
        }
        let names: Vec<_> = sign_in.environment.iter().map(|(name, _)| name).collect();
        for (index, name) in names.iter().enumerate() {
            assert!(!names[..index].contains(name), "{name:?} twice");
        }
        if cfg!(windows) {
            assert_eq!(
                value(&sign_in, "PATH"),
                Some(&OsString::from(
                    r"D:\Windows\System32;D:\Windows;D:\Windows\System32\WindowsPowerShell\v1.0"
                ))
            );
            assert_eq!(
                value(&sign_in, "USERPROFILE"),
                Some(&"/home/operator".into())
            );
            assert_eq!(value(&sign_in, "TEMP"), Some(&"temporary".into()));
        } else {
            assert_eq!(value(&sign_in, "PATH"), Some(&"/usr/bin:/bin".into()));
            assert_eq!(value(&sign_in, "HOME"), Some(&"/home/operator".into()));
            assert_eq!(value(&sign_in, "TEMP"), None);
        }
    }

    #[test]
    fn the_terminal_command_quotes_both_paths() {
        let command = terminal_command(Path::new("/opt/it's/claude"), Path::new("/data/sign in"));
        if cfg!(windows) {
            assert_eq!(
                command,
                "$env:CLAUDE_CONFIG_DIR = '/data/sign in'; & '/opt/it''s/claude' auth login"
            );
            assert_eq!(
                terminal_command(Path::new(r"\\?\C:\bin\claude.exe"), Path::new(r"C:\s")),
                r"$env:CLAUDE_CONFIG_DIR = 'C:\s'; & 'C:\bin\claude.exe' auth login"
            );
        } else {
            assert_eq!(
                command,
                r"CLAUDE_CONFIG_DIR='/data/sign in' '/opt/it'\''s/claude' auth login"
            );
        }
    }

    #[test]
    fn a_new_sign_in_folder_is_removed_unless_kept() {
        let root = tempfile::tempdir().expect("root");
        let parent = root.path().join("sign-ins");
        let dropped = NewSignInFolder::create(&parent).expect("created");
        let path = dropped.path().to_owned();
        assert!(path.is_dir());
        assert_eq!(fs::read_dir(&path).expect("listed").count(), 0);
        drop(dropped);
        assert!(!path.exists());
        let kept = NewSignInFolder::create(&parent).expect("created").keep();
        assert!(kept.is_dir());
        assert_ne!(kept, path);
        assert!(NewSignInFolder::create(Path::new("relative")).is_err());
    }
}
