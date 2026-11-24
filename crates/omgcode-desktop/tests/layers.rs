//! Agreement between the layers that gate a command: the Rust policy, the
//! capability files, the build manifest, the frontend's calls, and the Tauri
//! configuration. None of these tests starts a webview.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use omgcode_app::desktop::{DesktopConfig, DesktopService, SessionRequest};
use omgcode_desktop_lib::error::CommandError;
use omgcode_desktop_lib::policy::{Command, WindowLabel};
use serde_json::Value;

const COMMAND_NAMES_SOURCE: &str = include_str!("../src/command_names.rs");

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn json(path: &Path) -> Value {
    serde_json::from_str(&read(path))
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

/// The permission identifier Tauri generates for an application command.
fn permission_for(command: Command) -> String {
    format!("allow-{}", command.name().replace('_', "-"))
}

fn commands_for(window: WindowLabel) -> BTreeSet<&'static str> {
    Command::ALL
        .into_iter()
        .filter(|command| command.window() == window)
        .map(Command::name)
        .collect()
}

fn capability_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(crate_dir().join("capabilities"))
        .expect("capabilities directory")
        .map(|entry| entry.expect("entry").path())
        .collect();
    files.sort();
    files
}

fn capability_for(window: WindowLabel) -> Value {
    let matching: Vec<Value> = capability_files()
        .iter()
        .map(|path| json(path))
        .filter(|capability| capability["windows"] == serde_json::json!([window.as_str()]))
        .collect();
    assert_eq!(matching.len(), 1, "one capability per window");
    matching.into_iter().next().expect("one capability")
}

fn string_array(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|item| item.as_str().expect("plain string entry").to_owned())
        .collect()
}

#[test]
fn capability_grants_equal_the_policy_for_each_window() {
    for window in WindowLabel::ALL {
        // Given the capability that names this window.
        let capability = capability_for(window);

        // When its permissions are read.
        let granted = string_array(&capability["permissions"]);
        let granted_set: BTreeSet<&str> = granted.iter().map(String::as_str).collect();

        // Then they are exactly the policy's commands for the window, once each.
        let expected: Vec<String> = Command::ALL
            .into_iter()
            .filter(|command| command.window() == window)
            .map(permission_for)
            .collect();
        let expected_set: BTreeSet<&str> = expected.iter().map(String::as_str).collect();
        assert_eq!(granted_set, expected_set, "{window:?} grants");
        assert_eq!(granted.len(), expected.len(), "no duplicate grants");
        assert!(
            granted.iter().all(|p| !p.contains(':')),
            "no core or plugin permission"
        );
    }
}

#[test]
fn each_capability_names_one_local_window_and_no_remote_url() {
    let files = capability_files();
    assert_eq!(files.len(), 3, "exactly three capability files");
    for path in files {
        let capability = json(&path);
        let windows = string_array(&capability["windows"]);
        assert_eq!(windows.len(), 1, "{}", path.display());
        assert!(
            !windows[0].contains(['*', '?', '[']),
            "no glob in {}",
            path.display()
        );
        assert_eq!(capability["local"], Value::Bool(true));
        assert!(capability.get("remote").is_none(), "no remote access");
        assert!(capability.get("webviews").is_none(), "windows only");
        assert!(capability.get("platforms").is_none(), "all platforms");
    }
    let labels: BTreeSet<String> = capability_files()
        .iter()
        .flat_map(|path| string_array(&json(path)["windows"]))
        .collect();
    assert_eq!(
        labels,
        BTreeSet::from([
            "main".to_owned(),
            "approvals".to_owned(),
            "settings".to_owned()
        ])
    );
}

#[test]
fn decision_commands_are_granted_to_no_other_window() {
    let main = string_array(&capability_for(WindowLabel::Main)["permissions"]);
    for command in [
        Command::ApprovalsList,
        Command::ApprovalDetail,
        Command::ApprovalDecide,
    ] {
        assert!(!main.contains(&permission_for(command)));
    }
    let approvals = string_array(&capability_for(WindowLabel::Approvals)["permissions"]);
    assert_eq!(approvals.len(), 3);
    let settings = string_array(&capability_for(WindowLabel::Settings)["permissions"]);
    for command in [
        Command::ApprovalsList,
        Command::ApprovalDetail,
        Command::ApprovalDecide,
    ] {
        assert!(!settings.contains(&permission_for(command)));
    }
}

#[test]
fn settings_window_is_granted_only_its_own_commands() {
    // ADR-0059 decision 2: the settings window reads and changes the trusted
    // configuration and nothing else; no chat, transcript, session, or
    // approval command.
    let settings = string_array(&capability_for(WindowLabel::Settings)["permissions"]);
    let expected: Vec<String> = commands_for(WindowLabel::Settings)
        .into_iter()
        .map(|name| format!("allow-{}", name.replace('_', "-")))
        .collect();
    assert_eq!(settings, expected);
    assert_eq!(
        settings,
        [
            "allow-settings-add-connection",
            "allow-settings-add-package-profile",
            "allow-settings-add-toolchain-profile",
            "allow-settings-choose-package-cache",
            "allow-settings-choose-root",
            "allow-settings-clear-package-cache",
            "allow-settings-clear-scope",
            "allow-settings-grant-container-folder",
            "allow-settings-import-skill",
            "allow-settings-overview",
            "allow-settings-register-workspace",
            "allow-settings-remove-connection",
            "allow-settings-remove-package-profile",
            "allow-settings-remove-skill",
            "allow-settings-remove-toolchain-profile",
            "allow-settings-repair-data-folder",
            "allow-settings-revoke-container-grant",
            "allow-settings-save-chat-defaults",
            "allow-settings-select-connection",
            "allow-settings-set-api-key",
            "allow-settings-set-permissions",
            "allow-settings-set-skill-state",
            "allow-settings-set-workspace-instructions",
            "allow-settings-unregister-workspace",
        ]
    );
    for window in [WindowLabel::Main, WindowLabel::Approvals] {
        let granted = string_array(&capability_for(window)["permissions"]);
        for command in commands_for(WindowLabel::Settings) {
            let permission = format!("allow-{}", command.replace('_', "-"));
            assert!(
                !granted.contains(&permission),
                "{window:?} must not be granted {command}"
            );
        }
    }
}

#[test]
fn no_dialog_plugin_or_other_tauri_plugin_is_a_dependency() {
    // ADR-0060: native dialogs are opened from Rust through `rfd`, so no
    // plugin registers a dialog command that a webview could call.
    let manifest = read(&crate_dir().join("Cargo.toml"));
    assert!(!manifest.contains("tauri-plugin"), "no Tauri plugin");
    assert!(manifest.contains("rfd = {"), "rfd opens the dialogs");
    let lib = read(&crate_dir().join("src/lib.rs"));
    assert!(!lib.contains(".plugin("), "no plugin is registered");
    // Only the settings handlers open dialogs, and in the main window's
    // handlers only `connection_select`, whose native Yes/No is the
    // safeguard for the model box's provider choice (ADR-0071, ADR-0084,
    // ADR-0089).
    for (path, text) in handler_sources() {
        let opens_dialogs = text.contains("NativePrompt");
        let main_handlers = path.ends_with("chat.rs");
        assert_eq!(
            opens_dialogs,
            path.ends_with("settings.rs") || main_handlers,
            "{}",
            path.display()
        );
        if main_handlers {
            let start = text
                .find("pub async fn connection_select(")
                .expect("connection_select handler");
            let end = text[start + 1..]
                .find("pub async fn ")
                .map_or(text.len(), |offset| start + 1 + offset);
            assert_eq!(text.matches("NativePrompt::new(").count(), 1);
            assert!(text[start..end].contains("NativePrompt::new(webview.window())"));
        }
    }
}

#[test]
fn build_script_declares_the_shared_command_list() {
    // Given the shared list and the build script.
    let build = read(&crate_dir().join("build.rs"));

    // Then the script includes that file, so the manifest is the policy's list.
    assert!(build.contains("include!(\"src/command_names.rs\")"));
    assert!(build.contains(".commands(COMMAND_NAMES)"));

    // And the shared list holds the 56 unique policy names.
    let listed: Vec<&str> = COMMAND_NAMES_SOURCE
        .lines()
        .filter_map(|line| line.trim().strip_prefix('"'))
        .filter_map(|rest| rest.strip_suffix("\","))
        .collect();
    let unique: BTreeSet<&str> = listed.iter().copied().collect();
    assert_eq!(listed.len(), 56);
    assert_eq!(unique.len(), 56);
    let policy: Vec<&str> = Command::ALL.into_iter().map(Command::name).collect();
    assert_eq!(listed, policy);
}

/// Command names passed to `invokeCommand<T>("name", ...)` in a TypeScript file.
fn invoked_commands(source: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = source;
    while let Some(at) = rest.find("invokeCommand<") {
        rest = &rest[at + "invokeCommand<".len()..];
        let Some(open) = rest.find(">(\"") else { break };
        // The generic argument and `(` must belong to one expression.
        if rest[..open].contains([';', '\n']) {
            continue;
        }
        let name_start = open + 3;
        if let Some(len) = rest[name_start..].find('"') {
            found.insert(rest[name_start..name_start + len].to_owned());
        }
    }
    found
}

/// Command names passed to `invokeWithChannel("name", ...)`, the push-channel
/// form of an invocation (`ui/src/ipc/chat_events.ts`).
fn channel_commands(source: &str) -> BTreeSet<String> {
    const CALL: &str = "invokeWithChannel(\"";
    let mut found = BTreeSet::new();
    let mut rest = source;
    while let Some(at) = rest.find(CALL) {
        rest = &rest[at + CALL.len()..];
        if let Some(len) = rest.find('"') {
            found.insert(rest[..len].to_owned());
        }
    }
    found
}

/// The argument keys `invokeWithChannel` in `client.ts` passes to the command it
/// runs, once per transport form (the real `Channel` and the plain object).
fn channel_argument_keys(client: &str) -> Vec<BTreeSet<String>> {
    const CALL: &str = "invokeCommand<null>(command, ";
    let start = client
        .find("export async function invokeWithChannel(")
        .expect("invokeWithChannel is defined");
    let body = &client[start..];
    // The function ends at the first closing brace in column zero.
    let end = body.find("\n}").expect("end of invokeWithChannel");
    let body = &body[..end];
    let mut found = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find(CALL) {
        rest = &rest[at + CALL.len()..];
        assert!(rest.starts_with('{'), "an argument object follows the name");
        let close = matching(rest, 0);
        found.push(
            split_top_level(&rest[1..close])
                .into_iter()
                .map(|entry| {
                    entry
                        .split_once(':')
                        .map_or(entry.as_str(), |(key, _)| key)
                        .trim()
                        .to_owned()
                })
                .collect(),
        );
    }
    found
}

#[test]
fn frontend_clients_call_only_commands_the_policy_gives_their_window() {
    // Given the three IPC client modules.
    let ui = crate_dir().join("ui/src/ipc");
    let main_calls = invoked_commands(&read(&ui.join("client.ts")));
    let approval_calls = invoked_commands(&read(&ui.join("approvals_client.ts")));
    let settings_calls = invoked_commands(&read(&ui.join("settings_client.ts")));
    // The one push-channel command is invoked from `chat_events.ts`.
    let channel_calls = channel_commands(&read(&ui.join("chat_events.ts")));
    assert_eq!(
        channel_calls,
        BTreeSet::from(["chat_subscribe".to_owned()]),
        "the push channel opens exactly one command"
    );
    let main_calls: BTreeSet<String> = main_calls.union(&channel_calls).cloned().collect();

    // Then the main side uses a subset of the main commands (all 29 today)...
    let main_commands: BTreeSet<String> = commands_for(WindowLabel::Main)
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert!(main_calls.is_subset(&main_commands), "{main_calls:?}");
    assert_eq!(main_calls.len(), 29);
    assert_eq!(main_calls, main_commands);

    // ...the approvals client uses exactly the three approval commands...
    let approval_commands: BTreeSet<String> = commands_for(WindowLabel::Approvals)
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(approval_calls, approval_commands);

    // ...the settings client uses exactly the settings commands...
    let settings_commands: BTreeSet<String> = commands_for(WindowLabel::Settings)
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(settings_calls, settings_commands);

    // ...and together they cover all 56 policy commands.
    let union: BTreeSet<String> = main_calls
        .union(&approval_calls)
        .cloned()
        .collect::<BTreeSet<String>>()
        .union(&settings_calls)
        .cloned()
        .collect();
    let all: BTreeSet<String> = Command::ALL
        .into_iter()
        .map(|c| c.name().to_owned())
        .collect();
    assert_eq!(union, all);
}

fn csp_directives(csp: &str) -> Vec<(String, Vec<String>)> {
    csp.split(';')
        .map(str::trim)
        .filter(|directive| !directive.is_empty())
        .map(|directive| {
            let mut parts = directive.split_whitespace().map(str::to_owned);
            let name = parts.next().expect("directive name");
            (name, parts.collect())
        })
        .collect()
}

#[test]
fn tauri_config_is_locked_down() {
    let config = json(&crate_dir().join("tauri.conf.json"));

    // Globals, bundling, windows, and assets.
    assert_eq!(config["app"]["withGlobalTauri"], Value::Bool(false));
    assert_eq!(config["bundle"]["active"], Value::Bool(false));
    assert_eq!(config["app"]["windows"], serde_json::json!([]));
    assert_eq!(
        config["app"]["security"]["freezePrototype"],
        Value::Bool(true)
    );
    assert_eq!(
        config["app"]["security"]["assetProtocol"]["enable"],
        Value::Bool(false)
    );
    assert!(
        config["app"]["security"]
            .as_object()
            .expect("security")
            .keys()
            .all(|key| !key.starts_with("dangerous")),
        "no dangerous flag"
    );

    // The capability list is exactly the three files' identifiers.
    let listed = string_array(&config["app"]["security"]["capabilities"]);
    let from_files: Vec<String> = capability_files()
        .iter()
        .map(|path| {
            json(path)["identifier"]
                .as_str()
                .expect("identifier")
                .to_owned()
        })
        .collect();
    let listed_set: BTreeSet<&String> = listed.iter().collect();
    let files_set: BTreeSet<&String> = from_files.iter().collect();
    assert_eq!(listed.len(), 3);
    assert_eq!(listed_set, files_set);

    // The CSP carries each required directive and no loose source.
    let csp = config["app"]["security"]["csp"]
        .as_str()
        .expect("csp string");
    let directives = csp_directives(csp);
    let get = |name: &str| -> Vec<String> {
        let matching: Vec<_> = directives.iter().filter(|(n, _)| n == name).collect();
        assert_eq!(matching.len(), 1, "directive {name} appears once");
        matching[0].1.clone()
    };
    assert_eq!(get("default-src"), ["'none'"]);
    assert_eq!(get("script-src"), ["'self'"]);
    assert_eq!(get("style-src"), ["'self'"]);
    assert_eq!(get("img-src"), ["'self'"]);
    assert_eq!(get("font-src"), ["'self'"]);
    assert_eq!(get("connect-src"), ["ipc:", "http://ipc.localhost"]);
    assert_eq!(get("base-uri"), ["'none'"]);
    assert_eq!(get("form-action"), ["'none'"]);
    assert_eq!(get("frame-ancestors"), ["'none'"]);
    for (_, sources) in &directives {
        for source in sources {
            assert!(
                !matches!(source.as_str(), "'unsafe-inline'" | "'unsafe-eval'" | "*"),
                "loose source {source}"
            );
            assert!(!source.starts_with("data:") && !source.starts_with("blob:"));
            if source.starts_with("http://") || source.starts_with("https://") {
                assert_eq!(source, "http://ipc.localhost");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers: registration, window policy call, and argument names.
// ---------------------------------------------------------------------------

/// Every source file that may hold a `#[tauri::command]`.
fn handler_sources() -> Vec<(PathBuf, String)> {
    let mut paths = vec![crate_dir().join("src/commands.rs")];
    let mut nested: Vec<PathBuf> = fs::read_dir(crate_dir().join("src/commands"))
        .expect("commands directory")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    nested.sort();
    paths.extend(nested);
    paths
        .into_iter()
        .map(|path| {
            let text = read(&path);
            (path, text)
        })
        .collect()
}

/// One `#[tauri::command]` function as found in source text.
struct Handler {
    name: String,
    is_async: bool,
    /// The parameter list as `(name, type)` pairs.
    params: Vec<(String, String)>,
    /// The text from the opening brace of the body to the end of the handler.
    body: String,
}

/// Splits `text` at commas that are not nested in `<>`, `()`, `[]`, or `{}`.
fn split_top_level(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0_i32;
    let mut current = String::new();
    let mut previous = ' ';
    for ch in text.chars() {
        match ch {
            '<' | '(' | '[' | '{' => depth += 1,
            // `->` is not a closing angle bracket.
            '>' if previous == '-' => {}
            '>' | ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(std::mem::take(&mut current));
                previous = ch;
                continue;
            }
            _ => {}
        }
        current.push(ch);
        previous = ch;
    }
    parts.push(current);
    parts
        .into_iter()
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect()
}

/// Index of the delimiter closing the one opened at `open` in `text`.
fn matching(text: &str, open: usize) -> usize {
    let bytes = text.as_bytes();
    let (opening, closing) = match bytes[open] {
        b'(' => (b'(', b')'),
        b'{' => (b'{', b'}'),
        other => panic!("not a delimiter: {}", other as char),
    };
    let mut depth = 0_i32;
    for (offset, byte) in bytes[open..].iter().enumerate() {
        if *byte == opening {
            depth += 1;
        } else if *byte == closing {
            depth -= 1;
            if depth == 0 {
                return open + offset;
            }
        }
    }
    panic!("unbalanced delimiter");
}

fn parse_handlers(source: &str) -> Vec<Handler> {
    let test_module = source.find("#[cfg(test)]").unwrap_or(source.len());
    let source = &source[..test_module];
    let mut handlers = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find(
        "
#[tauri::command",
    ) {
        rest = &rest[at
            + "
#[tauri::command"
                .len()..];
        let end_of_handler = rest
            .find(
                "
#[tauri::command",
            )
            .unwrap_or(rest.len());
        let chunk = &rest[..end_of_handler];
        let fn_at = chunk.find("fn ").expect("a function follows the attribute");
        let is_async = chunk[..fn_at].trim_end().ends_with("async");
        let after_fn = &chunk[fn_at + 3..];
        let paren = after_fn.find('(').expect("parameter list");
        let name = after_fn[..paren].trim().to_owned();
        let close = matching(after_fn, paren);
        let params = split_top_level(&after_fn[paren + 1..close])
            .into_iter()
            .map(|param| {
                let (n, t) = param.split_once(':').expect("typed parameter");
                (n.trim().to_owned(), t.trim().to_owned())
            })
            .collect();
        let brace = close + after_fn[close..].find('{').expect("body");
        handlers.push(Handler {
            name,
            is_async,
            params,
            body: after_fn[brace..].to_owned(),
        });
    }
    handlers
}

fn all_handlers() -> Vec<Handler> {
    handler_sources()
        .iter()
        .flat_map(|(_, text)| parse_handlers(text))
        .collect()
}

fn injected(param: &(String, String)) -> bool {
    let ty = param.1.as_str();
    ty.starts_with("Webview") || ty.starts_with("Service<") || ty.starts_with("State<")
}

#[test]
fn generate_handler_registers_exactly_the_policy_commands() {
    // Given the invoke handler in lib.rs.
    let lib = read(&crate_dir().join("src/lib.rs"));
    let start =
        lib.find("generate_handler![").expect("generate_handler") + "generate_handler![".len();
    let end = start + lib[start..].find(']').expect("closing bracket");

    // When the registered function names (last path segment) are collected.
    let registered: Vec<&str> = lib[start..end]
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| entry.rsplit("::").next().expect("segment"))
        .collect();

    // Then they are the 56 shared names, once each.
    let unique: BTreeSet<&str> = registered.iter().copied().collect();
    assert_eq!(registered.len(), 56);
    assert_eq!(unique.len(), 56);
    let shared: BTreeSet<&str> = COMMAND_NAMES_SOURCE
        .lines()
        .filter_map(|line| line.trim().strip_prefix('"'))
        .filter_map(|rest| rest.strip_suffix("\","))
        .collect();
    assert_eq!(unique, shared);

    // And every registered name is a handler in the commands sources.
    let defined: BTreeSet<String> = all_handlers().into_iter().map(|h| h.name).collect();
    let defined: BTreeSet<&str> = defined.iter().map(String::as_str).collect();
    assert_eq!(defined, shared);
}

#[test]
fn every_handler_authorizes_its_own_command_before_touching_the_service() {
    let handlers = all_handlers();
    assert_eq!(handlers.len(), 56);
    for handler in handlers {
        // Given the command the function name denotes.
        let command = Command::ALL
            .into_iter()
            .find(|command| command.name() == handler.name)
            .unwrap_or_else(|| panic!("{} is not a policy command", handler.name));
        assert!(
            handler
                .params
                .first()
                .is_some_and(|p| p.1.starts_with("Webview")),
            "{} must take the calling Webview first",
            handler.name
        );

        // When the body is searched for its policy call.
        let call = format!("authorize(Command::{command:?}, webview.label())?;");
        let authorize_at = handler
            .body
            .find(&call)
            .unwrap_or_else(|| panic!("{} lacks `{call}`", handler.name));
        assert_eq!(
            handler.body.matches("authorize(").count(),
            1,
            "{} calls authorize exactly once",
            handler.name
        );

        // Then nothing but the opening brace precedes it, and any service use follows it.
        assert!(
            handler.body[1..authorize_at].trim().is_empty(),
            "{}: authorize must be the first statement",
            handler.name
        );
        if handler
            .params
            .iter()
            .any(|p| p.1.starts_with("Service<") || p.1.starts_with("State<"))
        {
            assert!(
                handler.is_async,
                "{} uses the service, so it is async",
                handler.name
            );
            let state_at = handler.body.find("&state").expect("state is used");
            assert!(
                authorize_at < state_at,
                "{}: state used before authorize",
                handler.name
            );
            assert!(
                handler.body.contains("blocking(&state"),
                "{} must run the service through the blocking helper",
                handler.name
            );
            assert!(
                !handler.body.contains("state.inner()"),
                "{} must not reach the service except through the helper",
                handler.name
            );
        }
    }
}

#[test]
fn the_blocking_helper_uses_the_blocking_pool_and_maps_failures() {
    let commands = read(&crate_dir().join("src/commands.rs"));
    assert!(commands.contains("tauri::async_runtime::spawn_blocking"));
    assert!(commands.contains("CommandError::from"));
    assert!(commands.contains("CommandError::internal()"));
}

/// The keys of every argument object the frontend passes to each command.
fn frontend_arguments(source: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut found = BTreeMap::new();
    let mut rest = source;
    while let Some(at) = rest.find("invokeCommand<") {
        rest = &rest[at + "invokeCommand<".len()..];
        let Some(open) = rest.find(">(\"") else { break };
        if rest[..open].contains([';', '\n']) {
            continue;
        }
        let name_start = open + 3;
        let Some(len) = rest[name_start..].find('"') else {
            break;
        };
        let name = rest[name_start..name_start + len].to_owned();
        let after_name = &rest[name_start + len + 1..];
        let brace = after_name.find('{').expect("argument object");
        assert!(
            after_name[..brace].trim() == ",",
            "{name}: argument object must follow the name directly"
        );
        let close = matching(after_name, brace);
        let keys = split_top_level(&after_name[brace + 1..close])
            .into_iter()
            .map(|entry| {
                entry
                    .split_once(':')
                    .map_or(entry.as_str(), |(key, _)| key)
                    .trim()
                    .to_owned()
            })
            .collect();
        found.insert(name, keys);
    }
    found
}

#[test]
fn handler_parameters_match_the_keys_the_frontend_passes() {
    // Given the argument keys each frontend call passes.
    let ui = crate_dir().join("ui/src/ipc");
    let mut expected = frontend_arguments(&read(&ui.join("client.ts")));
    expected.extend(frontend_arguments(&read(&ui.join("approvals_client.ts"))));
    expected.extend(frontend_arguments(&read(&ui.join("settings_client.ts"))));
    // The push-channel command passes one argument, `on_event`, in both of the
    // transport forms of `invokeWithChannel`.
    let channel_keys = channel_argument_keys(&read(&ui.join("client.ts")));
    assert_eq!(channel_keys.len(), 2, "one call per transport form");
    for keys in &channel_keys {
        assert_eq!(keys, &BTreeSet::from(["on_event".to_owned()]));
    }
    for name in channel_commands(&read(&ui.join("chat_events.ts"))) {
        assert!(
            expected
                .insert(name.clone(), channel_keys[0].clone())
                .is_none(),
            "{name} is also a plain invocation"
        );
    }
    assert_eq!(
        expected.len(),
        56,
        "all commands are called by the frontend"
    );

    // Then each handler's non-injected parameters are exactly those keys.
    for handler in all_handlers() {
        let actual: BTreeSet<String> = handler
            .params
            .iter()
            .filter(|param| !injected(param))
            .map(|param| param.0.clone())
            .collect();
        let wanted = expected
            .get(&handler.name)
            .unwrap_or_else(|| panic!("{} is never called by the frontend", handler.name));
        assert_eq!(&actual, wanted, "{} parameters", handler.name);
    }
}

#[test]
fn every_handler_uses_snake_case_argument_matching() {
    for (path, text) in handler_sources() {
        let attributes = text
            .matches(
                "
#[tauri::command",
            )
            .count();
        let renamed = text
            .matches(
                "
#[tauri::command(rename_all = \"snake_case\")]",
            )
            .count();
        assert_eq!(attributes, renamed, "{}", path.display());
    }
}

#[test]
fn command_error_keeps_the_service_code_and_message() {
    // Given a real service failure: archiving a session that does not exist.
    let root = std::env::temp_dir().join(format!(
        "omgcode-desktop-layers-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let service = DesktopService::open(DesktopConfig {
        data_root: root.clone(),
        endpoint: "http://127.0.0.1:1234/v1".to_owned(),
        token_source: omgcode_app::TokenSource::Environment,
        stream: true,
    })
    .expect("open service");
    let failure = service
        .session_archive(SessionRequest {
            session_id: uuid::Uuid::from_u128(7),
        })
        .expect_err("no such session");

    // When it is converted for the webview.
    let converted = CommandError::from(failure.clone());

    // Then the serialized forms are identical and the accessors agree.
    assert_eq!(converted.code(), failure.code());
    assert_eq!(converted.message(), failure.message());
    assert_eq!(
        serde_json::to_value(&converted).expect("command error"),
        serde_json::to_value(&failure).expect("desktop error")
    );
    drop(service);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn internal_error_is_fixed_and_carries_no_detail() {
    let value = serde_json::to_value(CommandError::internal()).expect("serialize");
    assert_eq!(value["code"], "internal");
    assert_eq!(value["message"], "The command could not be completed.");
}

/// The settings window names every provider kind core records and shows each
/// named hosted provider's fixed endpoint exactly as core enforces it
/// (ADR-0091). Rust supplies the endpoint; a drift would only mislead the
/// operator, so this keeps the display honest.
#[test]
fn the_frontend_names_every_provider_and_shows_core_endpoints() {
    let source = read(&crate_dir().join("ui/src/providers.ts"));
    let flat = source.split_whitespace().collect::<Vec<_>>().join(" ");
    for kind in omgcode_app::desktop::NewConnectionKind::ALL {
        let name = kind.as_str();
        assert!(flat.contains(&format!("{name}: \"")), "{name} is not named");
        assert!(
            flat.contains(&format!("\"{name}\",")),
            "{name} is not listed"
        );
        if let Some(endpoint) = kind.fixed_endpoint() {
            assert!(
                flat.contains(&format!("{name}: \"{endpoint}\"")),
                "{name} shows another endpoint than {endpoint}"
            );
        }
    }
}
