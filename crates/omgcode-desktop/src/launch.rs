//! Launch configuration, read from the process command line only.
//!
//! ```text
//! omgcode-desktop [--data-dir <path>] [--endpoint <url>] [--provider-profile <name>]
//!                 [--stream <on|off>]
//! ```
//!
//! The command line belongs to the operator who starts the process. The
//! webviews cannot influence it, and neither can workspace content. This module
//! reads no environment variable and no configuration file. The local server
//! token is a separate matter and this module never touches it. Without
//! `--provider-profile` the service reads `OMGCODE_LM_TOKEN` for each chat
//! turn. With it, the service reads the credential bound to that recorded
//! provider profile instead and never reads the variable (ADR-0047). The flag
//! carries only a profile name, not a secret. A selected profile also supplies
//! the endpoint: `--endpoint` and `--provider-profile` together are a
//! [`UsageError`], and with a profile [`LaunchArgs::endpoint`] holds only the
//! default and is not used (ADR-0048).
//!
//! `--stream` chooses whether chat turns send streamed model requests and push
//! their text to the window (`on`, the default) or send non-streaming requests
//! (`off`). Only the exact values `on` and `off` are accepted (ADR-0049).
//!
//! Any unknown argument, a flag without a value, a value that looks like
//! another flag, an empty value, a non-UTF-8 endpoint, a repeated flag, or
//! both `--endpoint` and `--provider-profile` is a [`UsageError`]. The caller
//! prints [`USAGE`] and exits before a window opens.

use std::ffi::OsString;
use std::path::PathBuf;

/// The endpoint used when `--endpoint` is absent. It is the CLI's default.
pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:1234/v1";

/// The fixed message printed to stderr for a usage error.
pub const USAGE: &str = "usage: omgcode-desktop [--data-dir <path>] [--endpoint <url>] [--provider-profile <name>] [--stream <on|off>]\n  \
--data-dir <path>           application data root (default: the per-user OMGCode data directory)\n  \
--endpoint <url>            local model endpoint, a loopback http URL (default: http://127.0.0.1:1234/v1)\n  \
--provider-profile <name>   use this lm_studio provider profile's recorded endpoint and stored credential\n                              (default: --endpoint and the OMGCODE_LM_TOKEN environment variable)\n                              cannot be combined with --endpoint\n  \
--stream <on|off>           stream model replies into the window as they arrive (default: on); off sends non-streaming requests";

/// What the command line asked for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchArgs {
    /// The data root override, or `None` for the per-user default.
    pub data_dir: Option<PathBuf>,
    /// The chat endpoint for the whole process. With a provider profile this is
    /// the default and is not used: the profile's recorded endpoint is.
    pub endpoint: String,
    /// The recorded provider profile whose stored credential is the server
    /// token, or `None` to use the `OMGCODE_LM_TOKEN` variable.
    pub provider_profile: Option<String>,
    /// Whether chat turns send streamed requests and push their text to the
    /// window. `--stream off` sets it to `false` (ADR-0049).
    pub stream: bool,
}

/// The command line was not understood. It carries no detail on purpose: the
/// usage text is the only message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsageError;

/// Parses the arguments after the program name.
///
/// A path is kept as an `OsString` so a non-UTF-8 path survives. A value must
/// not begin with `--`, so a forgotten value is not read as the next flag's
/// name. A flag given twice is refused, not merged.
///
/// # Errors
///
/// [`UsageError`] for anything outside the contract above.
pub fn parse_args<I>(args: I) -> Result<LaunchArgs, UsageError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut data_dir: Option<PathBuf> = None;
    let mut endpoint: Option<String> = None;
    let mut provider_profile: Option<String> = None;
    let mut stream: Option<bool> = None;
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        let value = args.next().ok_or(UsageError)?;
        if value.is_empty() || value.as_encoded_bytes().starts_with(b"--") {
            return Err(UsageError);
        }
        match flag.to_str() {
            Some("--data-dir") if data_dir.is_none() => data_dir = Some(PathBuf::from(value)),
            Some("--endpoint") if endpoint.is_none() => {
                endpoint = Some(value.into_string().map_err(|_| UsageError)?);
            }
            Some("--provider-profile") if provider_profile.is_none() => {
                provider_profile = Some(value.into_string().map_err(|_| UsageError)?);
            }
            Some("--stream") if stream.is_none() => {
                stream = Some(match value.to_str() {
                    Some("on") => true,
                    Some("off") => false,
                    _ => return Err(UsageError),
                });
            }
            _ => return Err(UsageError),
        }
    }
    // A selected profile is the only source of its endpoint, so there is no
    // "allowed when equal" case.
    if endpoint.is_some() && provider_profile.is_some() {
        return Err(UsageError);
    }
    Ok(LaunchArgs {
        data_dir,
        endpoint: endpoint.unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned()),
        provider_profile,
        stream: stream.unwrap_or(true),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<LaunchArgs, UsageError> {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn no_arguments_gives_the_defaults() {
        assert_eq!(
            parse(&[]),
            Ok(LaunchArgs {
                data_dir: None,
                endpoint: DEFAULT_ENDPOINT.to_owned(),
                provider_profile: None,
                stream: true,
            })
        );
    }

    #[test]
    fn each_flag_sets_its_value_in_either_order() {
        let both = LaunchArgs {
            data_dir: Some(PathBuf::from("data")),
            endpoint: "http://127.0.0.1:9/v1".to_owned(),
            provider_profile: None,
            stream: true,
        };
        assert_eq!(
            parse(&["--data-dir", "data", "--endpoint", "http://127.0.0.1:9/v1"]),
            Ok(both.clone())
        );
        assert_eq!(
            parse(&["--endpoint", "http://127.0.0.1:9/v1", "--data-dir", "data"]),
            Ok(both)
        );
        assert_eq!(
            parse(&["--data-dir", "C:\\a b\\c"])
                .expect("data dir")
                .data_dir,
            Some(PathBuf::from("C:\\a b\\c"))
        );
        assert_eq!(
            parse(&["--endpoint", "http://127.0.0.1:9/v1"])
                .expect("endpoint")
                .data_dir,
            None
        );
    }

    #[test]
    fn unknown_missing_repeated_and_malformed_arguments_are_refused() {
        for args in [
            &["--bogus"][..],
            &["--bogus", "x"],
            &["positional"],
            &["--data-dir"],
            &["--endpoint"],
            &["--data-dir", "a", "--endpoint"],
            &["--data-dir", "a", "--data-dir", "b"],
            &["--endpoint", "u", "--endpoint", "v"],
            &["--data-dir", "--endpoint", "u"],
            &["--data-dir", ""],
            &["--endpoint", ""],
            &["--data-dir=a"],
            &["--endpoint=http://127.0.0.1:1/v1"],
            &["-h"],
            &["--help"],
            &["--data-dir", "a", "extra"],
            &["--provider-profile"],
            &["--provider-profile", ""],
            &["--provider-profile", "--endpoint", "u"],
            &["--provider-profile", "--data-dir"],
            &["--provider-profile", "a", "--provider-profile", "b"],
            &["--provider-profile=a"],
            &["--endpoint", "u", "--provider-profile"],
            &[
                "--endpoint",
                "http://127.0.0.1:9/v1",
                "--provider-profile",
                "lms",
            ],
            &[
                "--provider-profile",
                "lms",
                "--endpoint",
                "http://127.0.0.1:9/v1",
            ],
            &["--provider-profile", "lms", "--endpoint", DEFAULT_ENDPOINT],
            &[
                "--data-dir",
                "d",
                "--provider-profile",
                "lms",
                "--endpoint",
                "u",
            ],
        ] {
            assert_eq!(parse(args), Err(UsageError), "{args:?}");
        }
    }

    #[test]
    fn the_provider_profile_flag_takes_one_name_in_any_order() {
        // The endpoint is the default and unused: the profile supplies it.
        let expected = LaunchArgs {
            data_dir: Some(PathBuf::from("data")),
            endpoint: DEFAULT_ENDPOINT.to_owned(),
            provider_profile: Some("lms".to_owned()),
            stream: true,
        };
        assert_eq!(
            parse(&["--provider-profile", "lms", "--data-dir", "data"]),
            Ok(expected.clone())
        );
        assert_eq!(
            parse(&["--data-dir", "data", "--provider-profile", "lms"]),
            Ok(expected)
        );
        // The name is kept verbatim; core validates it against recorded profiles.
        assert_eq!(
            parse(&["--provider-profile", "My Profile"])
                .expect("name")
                .provider_profile,
            Some("My Profile".to_owned())
        );
        assert_eq!(parse(&[]).expect("none").provider_profile, None);
    }

    #[test]
    fn streaming_is_on_by_default_and_the_flag_takes_on_or_off() {
        // Given no flag, then streaming is on.
        assert!(parse(&[]).expect("defaults").stream);

        // When the flag says `on` or `off`, then that is the value, in any order.
        assert!(parse(&["--stream", "on"]).expect("on").stream);
        assert!(!parse(&["--stream", "off"]).expect("off").stream);
        let both = parse(&["--stream", "off", "--data-dir", "data"]).expect("both");
        assert!(!both.stream);
        assert_eq!(both.data_dir, Some(PathBuf::from("data")));
        let profile = parse(&["--provider-profile", "lms", "--stream", "off"]).expect("profile");
        assert!(!profile.stream);
        assert_eq!(profile.provider_profile.as_deref(), Some("lms"));
    }

    #[test]
    fn a_bad_missing_or_repeated_stream_value_is_refused() {
        for args in [
            &["--stream", "maybe"][..],
            &["--stream", "ON"],
            &["--stream", "true"],
            &["--stream", "1"],
            &["--stream", " on"],
            &["--stream", ""],
            &["--stream"],
            &["--stream", "--data-dir", "a"],
            &["--stream", "on", "--stream", "on"],
            &["--stream", "on", "--stream", "off"],
            &["--stream=on"],
        ] {
            assert_eq!(parse(args), Err(UsageError), "{args:?}");
        }
    }

    #[test]
    fn usage_text_names_every_flag_and_the_default() {
        assert!(USAGE.contains("--stream <on|off>"));
        assert!(USAGE.contains("--data-dir") && USAGE.contains("--endpoint"));
        assert!(USAGE.contains("--provider-profile"));
        assert!(USAGE.contains(DEFAULT_ENDPOINT));
        assert!(USAGE.contains("cannot be combined with --endpoint"));
    }

    #[cfg(windows)]
    #[test]
    fn a_non_utf8_data_dir_is_kept_and_a_non_utf8_endpoint_is_refused() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};

        // Given a path with an unpaired surrogate, which is valid on Windows only.
        let mut wide: Vec<u16> = OsString::from("C:\\data-").encode_wide().collect();
        wide.push(0xD800);
        let odd = OsString::from_wide(&wide);
        assert!(odd.to_str().is_none());

        // Then the data root keeps it unchanged and the endpoint refuses it.
        let kept = parse_args([OsString::from("--data-dir"), odd.clone()]).expect("path");
        assert_eq!(kept.data_dir, Some(PathBuf::from(&odd)));
        assert_eq!(
            parse_args([OsString::from("--endpoint"), odd]),
            Err(UsageError)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_data_dir_is_kept_and_a_non_utf8_endpoint_is_refused() {
        use std::os::unix::ffi::OsStringExt;

        let odd = OsString::from_vec(b"/data-\xff".to_vec());
        let kept = parse_args([OsString::from("--data-dir"), odd.clone()]).expect("path");
        assert_eq!(kept.data_dir, Some(PathBuf::from(&odd)));
        assert_eq!(
            parse_args([OsString::from("--endpoint"), odd]),
            Err(UsageError)
        );
    }
}
