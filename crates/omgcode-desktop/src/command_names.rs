// Single source of truth for the command names. It is `include!`d by `build.rs`
// (to declare the permissions) and by `policy.rs` (to check the `Command` enum
// against it in tests). Keep it a plain constant: no `use`, no items besides it.

/// Every application command, in `snake_case`, exactly as the frontend invokes it.
pub const COMMAND_NAMES: &[&str] = &[
    "app_info",
    "session_tree",
    "session_search",
    "session_transcript",
    "session_archive",
    "session_restore",
    "session_interrupt",
    "workspace_list",
    "session_create",
    "chat_defaults",
    "chat_send",
    "chat_status",
    "chat_cancel",
    "chat_subscribe",
    "approvals_summary",
    "approvals_show_window",
    "approvals_list",
    "approval_detail",
    "approval_decide",
];
