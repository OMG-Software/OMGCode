import type { Invoker } from "../ipc/client";
import { CANCELLED_CODE } from "../ipc/errors";
import type {
  ApprovalDetail,
  ApprovalView,
  ChatDefaults,
  ChatEvent,
  ChatOutcome,
  ChatPreferences,
  ChatSettings,
  ChatStatus,
  PermissionFlags,
  SearchHit,
  SessionSummary,
  SettingsOverview,
  SnippetPart,
  TreeSort,
  TreeView,
  WorkspaceSettingsView,
} from "../ipc/types";
import {
  buildFixtures,
  type FixtureSession,
  HOSTILE,
  OMGCODE_MOCK_FIXTURE_MARKER,
} from "./fixtures";

/** In-memory stand-in for the Rust core. Used only by the development server. */
export const MOCK_BACKEND_NAME = OMGCODE_MOCK_FIXTURE_MARKER;

const CHAT_DURATION_MS = 2000;
const MOCK_REPLY_PIECES = ["This ", "is a ", "mock ", "reply."];
const TOKEN_LIFETIME_MS = 120_000;
const MOCK_ENDPOINT = "http://127.0.0.1:1234/v1";

interface Fail {
  code: string;
  message: string;
}

function fail(code: string, message: string): never {
  const error: Fail = { code, message };
  throw error;
}

function str(args: Record<string, unknown>, key: string): string {
  const value = args[key];
  return typeof value === "string" ? value : fail("invalid_argument", `Missing ${key}.`);
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

function snippet(text: string, query: string): SnippetPart[] | null {
  const index = text.toLowerCase().indexOf(query.toLowerCase());
  if (index < 0) {
    return null;
  }
  const start = Math.max(0, index - 40);
  const end = Math.min(text.length, index + query.length + 40);
  const parts: SnippetPart[] = [];
  const before = text.slice(start, index);
  const after = text.slice(index + query.length, end);
  if (before !== "") {
    parts.push({ text: (start > 0 ? "\u2026" : "") + before, matched: false });
  }
  parts.push({ text: text.slice(index, index + query.length), matched: true });
  if (after !== "") {
    parts.push({ text: after + (end < text.length ? "\u2026" : ""), matched: false });
  }
  return parts;
}

interface ChatRecord {
  status: ChatStatus;
  timer: ReturnType<typeof setTimeout> | null;
  deltaTimers: Array<ReturnType<typeof setTimeout>>;
  preview: string;
}

/** The object a caller passes as `on_event`: a plain stand-in for a Tauri channel. */
interface Subscriber {
  onmessage: (value: unknown) => void;
}

function isSubscriber(value: unknown): value is Subscriber {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { onmessage?: unknown }).onmessage === "function"
  );
}

/** Creates a fresh mock core. `now` supplies milliseconds since the epoch. */
export function createMockBackend(now: () => number = Date.now): { invoke: Invoker } {
  const { groups, sessions } = buildFixtures(now());
  const byId = new Map<string, FixtureSession>(sessions.map((s) => [s.summary.session_id, s]));
  const chats = new Map<string, ChatRecord>();
  const tokens = new Map<string, { id: string; expires: number }>();
  let subscriber: Subscriber | null = null;
  let savedPreferences: ChatPreferences | null = null;
  const settings: SettingsOverview = initialSettings();
  const nowUnix = (): number => Math.floor(now() / 1000);

  const approvals: ApprovalView[] = [
    approval("a1", "command", "cargo test -p omgcode-core", 600, 1, "Fix a flaky CI test"),
    approval(
      "a2",
      "file_write",
      `multi-edit src/lib.rs (2 edits): 120 -> 118 bytes ${HOSTILE.script}`,
      300,
      1,
      "Refactor the session index",
    ),
    approval(
      "a3",
      "file_write",
      "Replace 1 occurrence in src/main.rs",
      900,
      9,
      `${HOSTILE.rtl} review`,
    ),
  ];

  function approval(
    id: string,
    operation: ApprovalView["operation"],
    summaryText: string,
    ttl: number,
    n: number,
    title: string,
  ): ApprovalView {
    return {
      id,
      session_id: `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`,
      session_title: title,
      folder_display: "C:\\work\\omgcode",
      operation,
      summary: summaryText,
      state: "pending",
      created_unix: Math.floor(now() / 1000) - 30,
      expires_unix: Math.floor(now() / 1000) + ttl,
      request_sha256: id.padEnd(64, "0"),
      writers_stopped: false,
    };
  }

  function emit(event: ChatEvent): void {
    try {
      subscriber?.onmessage(event);
    } catch {
      // A failing subscriber must not break the mock turn.
    }
  }

  function clearDeltas(record: ChatRecord): void {
    for (const timer of record.deltaTimers) {
      clearTimeout(timer);
    }
    record.deltaTimers = [];
  }

  function subscribe(value: unknown): null {
    if (!isSubscriber(value)) {
      fail("invalid_argument", "Missing on_event.");
    }
    subscriber = value;
    // Like the core, push after the command has resolved: replay every running turn.
    void Promise.resolve().then(() => {
      for (const [id, record] of chats) {
        if (record.status.state !== "running" && record.status.state !== "cancelling") {
          continue;
        }
        emit({ kind: "request_started", session_id: id, request: 1 });
        if (record.preview !== "") {
          emit({ kind: "delta", session_id: id, request: 1, text: record.preview });
        }
      }
    });
    return null;
  }

  function session(id: string): FixtureSession {
    return byId.get(id) ?? fail("not_found", "No such session.");
  }

  function tree(view: TreeView, sort: TreeSort, descending: boolean): unknown {
    const key = (s: SessionSummary): string =>
      sort === "title"
        ? s.title.toLowerCase()
        : ((sort === "created" ? s.created_at : s.updated_at) ?? "");
    const out = groups
      .map((g) => ({
        group_id: g.group_id,
        folder_display: g.folder_display,
        sessions: g.session_ids
          .map((id) => session(id).summary)
          .filter((s) => (view === "archived") === s.archived)
          .sort((a, b) => key(a).localeCompare(key(b)) * (descending ? -1 : 1)),
      }))
      .filter((g) => g.sessions.length > 0);
    return clone({ groups: out, indexed_sessions: sessions.length, skipped_entries: 1 });
  }

  function search(query: string, view: TreeView, limit: number): unknown {
    const hits: SearchHit[] = [];
    let truncated = false;
    const q = query.trim();
    for (const group of groups) {
      for (const id of group.session_ids) {
        const item = session(id);
        if ((view === "archived") !== item.summary.archived) {
          continue;
        }
        const found: Array<Omit<SearchHit, "session" | "folder_display">> = [];
        const titleHit = snippet(item.summary.title, q);
        if (titleHit !== null) {
          found.push({ seq: null, role: "title", snippet: titleHit });
        }
        for (const entry of item.entries) {
          if (entry.kind === "message" || entry.kind === "context_compacted") {
            const text = entry.kind === "message" ? entry.text : entry.summary;
            const part = snippet(text, q);
            if (part !== null) {
              found.push({
                seq: entry.seq,
                role: entry.kind === "message" ? entry.role : "summary",
                snippet: part,
              });
            }
          }
        }
        for (const hit of found.slice(0, 3)) {
          if (hits.length >= limit) {
            truncated = true;
            break;
          }
          hits.push({ session: clone(item.summary), folder_display: group.folder_display, ...hit });
        }
      }
    }
    return { hits, truncated };
  }

  function transcript(id: string, after: number | null, limit: number): unknown {
    const item = session(id);
    if (item.summary.status === "busy") {
      fail("busy", "Another process holds this journal.");
    }
    if (item.summary.status === "invalid") {
      fail("invalid_journal", "The journal failed validation.");
    }
    const rest = item.entries.filter((e) => e.seq > (after ?? 0));
    const page = rest.slice(0, limit);
    const last = page[page.length - 1];
    const group = groups.find((g) => g.session_ids.includes(id));
    return clone({
      session: item.summary,
      folder_display: group?.folder_display ?? "",
      entries: page,
      next_after_seq: rest.length > page.length && last !== undefined ? last.seq : null,
      incomplete_tail: item.incomplete_tail && rest.length <= limit,
    });
  }

  function append(item: FixtureSession, role: "user" | "assistant", text: string): void {
    const last = item.entries[item.entries.length - 1];
    item.entries.push({
      seq: (last?.seq ?? 0) + 1,
      timestamp: new Date(now()).toISOString(),
      kind: "message",
      role,
      source: "chat",
      text,
      call_id: null,
    });
    item.summary.event_count = item.entries.length;
    item.summary.updated_at = new Date(now()).toISOString();
  }

  function settle(id: string, record: ChatRecord, modelFails: boolean, cancelled: boolean): void {
    finishTurn(id, record, modelFails, cancelled);
    emit({ kind: "finished", session_id: id });
  }

  function finishTurn(
    id: string,
    record: ChatRecord,
    modelFails: boolean,
    cancelled: boolean,
  ): void {
    const item = session(id);
    item.summary.status = "idle";
    record.timer = null;
    clearDeltas(record);
    if (modelFails) {
      record.status = {
        session_id: id,
        state: "failed",
        outcome: null,
        error: {
          code: "provider_unreachable",
          message: "The model endpoint could not be reached.",
        },
      };
      return;
    }
    append(item, "assistant", cancelled ? "(cancelled)" : "This is a mock reply.");
    const outcome: ChatOutcome = {
      finish_reason: cancelled ? "cancelled" : "stop",
      tools_mode: "disabled",
      tools_disabled_reason: "workspace has model tools turned off",
      rounds: 1,
      tool_calls: 0,
      withheld_tool_results: 0,
      context: {
        estimated_tokens: 1200,
        trigger_tokens: 6000,
        context_tokens: 8192,
        compacted: false,
      },
    };
    record.status = { session_id: id, state: "completed", outcome, error: null };
  }

  function chatSend(id: string, message: string, settings: ChatSettings): ChatStatus {
    const item = session(id);
    if (item.summary.status !== "idle" || item.summary.archived) {
      fail("session_not_idle", "The session cannot take a message now.");
    }
    if (settings.endpoint !== MOCK_ENDPOINT) {
      fail("invalid_request", "The endpoint is fixed by the application.");
    }
    if (message.trim() === "" || settings.model.trim() === "") {
      fail("invalid_argument", "A message and a model are required.");
    }
    append(item, "user", message);
    item.summary.status = "running";
    const record: ChatRecord = {
      status: { session_id: id, state: "running", outcome: null, error: null },
      timer: null,
      deltaTimers: [],
      preview: "",
    };
    record.timer = setTimeout(
      () => settle(id, record, settings.model === "fail", false),
      CHAT_DURATION_MS,
    );
    chats.set(id, record);
    emit({ kind: "request_started", session_id: id, request: 1 });
    MOCK_REPLY_PIECES.forEach((piece, index) => {
      const at = (CHAT_DURATION_MS * (index + 1)) / (MOCK_REPLY_PIECES.length + 1);
      record.deltaTimers.push(
        setTimeout(() => {
          record.preview += piece;
          emit({ kind: "delta", session_id: id, request: 1, text: piece });
        }, at),
      );
    });
    return clone(record.status);
  }

  function chatCancel(id: string): ChatStatus {
    const record = chats.get(id);
    if (record === undefined || record.status.state !== "running") {
      fail("not_running", "No turn is running.");
    }
    if (record.timer !== null) {
      clearTimeout(record.timer);
    }
    clearDeltas(record);
    session(id).summary.status = "idle";
    record.timer = null;
    record.status = {
      session_id: id,
      state: "failed",
      outcome: null,
      error: { code: CANCELLED_CODE, message: "turn was cancelled" },
    };
    emit({ kind: "finished", session_id: id });
    return clone(record.status);
  }

  function detail(id: string): ApprovalDetail {
    const view = approvals.find((a) => a.id === id) ?? fail("not_found", "No such approval.");
    const stale = view.id === "a3";
    const pending = view.state === "pending";
    const issueToken = pending && !stale;
    const token = issueToken ? `token-${id}-${now()}` : null;
    if (token !== null) {
      for (const [key, value] of tokens) {
        if (value.id === id) {
          tokens.delete(key);
        }
      }
      tokens.set(token, { id, expires: now() + TOKEN_LIFETIME_MS });
    }
    const fields =
      view.operation === "command"
        ? [
            { label: "Program", value: "cargo", multiline: false },
            {
              label: "Arguments",
              value: `test\n-p\nomgcode-core\n${HOSTILE.img}`,
              multiline: true,
            },
            { label: "Working folder", value: view.folder_display ?? "", multiline: false },
          ]
        : [
            { label: "File", value: "src/lib.rs", multiline: false },
            { label: "Edits", value: "2", multiline: false },
            { label: "Old text 1", value: `fn old() {}\n${HOSTILE.script}`, multiline: true },
            { label: "New text 1", value: "fn new() {}", multiline: true },
            { label: "Old text 2", value: "const A: u8 = 1;", multiline: true },
            { label: "New text 2", value: "const A: u8 = 2;", multiline: true },
          ];
    return clone({
      approval: view,
      binding: stale ? "stale" : "exact",
      tool: stale
        ? null
        : view.operation === "command"
          ? "workspace_run_command"
          : "workspace_multi_edit_file",
      fields: stale ? [] : fields,
      review_token: token,
    });
  }

  function decide(args: Record<string, unknown>): ApprovalView {
    const view =
      approvals.find((a) => a.id === str(args, "id")) ?? fail("not_found", "No such approval.");
    if (view.state !== "pending" || view.expires_unix <= nowUnix()) {
      fail("approval_not_pending", "The approval is not pending.");
    }
    if (args.decision === "deny") {
      view.state = "denied";
      return clone(view);
    }
    const token = typeof args.review_token === "string" ? tokens.get(args.review_token) : undefined;
    if (token === undefined || token.id !== view.id) {
      fail("review_mismatch", "The review token does not match this approval.");
    }
    if (token.expires <= now()) {
      fail("review_expired", "The review token expired.");
    }
    if (view.operation === "file_write" && args.writers_stopped !== true) {
      fail("writers_not_stopped", "Confirm that other writers are stopped.");
    }
    tokens.delete(str(args, "review_token"));
    view.state = "approved";
    view.writers_stopped = args.writers_stopped === true;
    return clone(view);
  }

  const invoke: Invoker = (command, args) => {
    try {
      return Promise.resolve(handle(command, args));
    } catch (reason) {
      return Promise.reject(reason);
    }
  };

  function handle(command: string, args: Record<string, unknown>): unknown {
    switch (command) {
      case "app_info":
        return { version: "0.0.0-mock", platform: "other", model_tools_supported: true };
      case "session_tree":
        return tree(args.view as TreeView, args.sort as TreeSort, args.descending === true);
      case "session_search":
        return search(str(args, "query"), args.view as TreeView, Number(args.limit));
      case "session_transcript":
        return transcript(
          str(args, "session_id"),
          (args.after_seq as number | null) ?? null,
          Number(args.limit),
        );
      case "session_archive": {
        const item = session(str(args, "session_id"));
        item.summary.archived = true;
        return clone(item.summary);
      }
      case "session_restore": {
        const item = session(str(args, "session_id"));
        item.summary.archived = false;
        return clone(item.summary);
      }
      case "session_interrupt": {
        const item = session(str(args, "session_id"));
        item.summary.status = "idle";
        return clone(item.summary);
      }
      // Read from the mock settings, so a workspace registered in the settings window appears
      // in the New session list, as it does with core.
      case "workspace_list":
        return {
          development_root_set: settings.development_root !== null,
          workspaces: settings.workspaces.map((w) => ({
            workspace_id: w.workspace_id,
            folder_display: w.folder_display,
            model_tools: w.model_tools,
            read: w.read,
            write: w.write,
            execute: w.execute,
          })),
        };
      case "session_create": {
        const id = `00000000-0000-4000-8000-${String(100 + sessions.length).padStart(12, "0")}`;
        const created: FixtureSession = {
          summary: {
            session_id: id,
            title: str(args, "title"),
            created_at: new Date(now()).toISOString(),
            updated_at: new Date(now()).toISOString(),
            status: "idle",
            archived: false,
            event_count: 0,
            invalid_code: null,
          },
          entries: [],
          incomplete_tail: false,
        };
        sessions.push(created);
        byId.set(id, created);
        groups[0]?.session_ids.unshift(id);
        return clone(created.summary);
      }
      case "chat_defaults":
        return chatDefaults();
      case "chat_preferences_save":
        savedPreferences = clone(args.preferences as ChatPreferences);
        return chatDefaults();
      case "session_skills":
        str(args, "session_id");
        return { workspace_registered: true, skills: ["review-checklist", "release-notes"] };
      case "settings_show_window":
        return null;
      case "settings_overview":
        return clone(settings);
      // The development mock stands in for the native dialogs: each folder dialog "chooses" a
      // fixed folder and each confirmation is accepted.
      case "settings_choose_root":
        return changeSettings(args, () => {
          settings.development_root = "/home/dev";
        });
      case "settings_register_workspace":
        return changeSettings(args, () => {
          if (settings.development_root === null) {
            fail(
              "no_development_root",
              "Choose a development root before registering a workspace.",
            );
          }
          settings.workspaces.push({
            workspace_id: `99999999-9999-4999-8999-${String(settings.workspaces.length).padStart(12, "0")}`,
            folder_display: `/home/dev/project-${settings.workspaces.length + 1}`,
            model_tools: false,
            read: true,
            write: true,
            execute: false,
            task_network: false,
            host_services: false,
            package_cache_root: null,
          });
        });
      case "settings_set_permissions":
        return changeSettings(args, () => {
          Object.assign(mockWorkspace(args), args.permissions as PermissionFlags);
        });
      case "settings_choose_package_cache":
        return changeSettings(args, () => {
          mockWorkspace(args).package_cache_root = "/home/dev/.cache/omgcode-packages";
        });
      case "settings_clear_package_cache":
        return changeSettings(args, () => {
          mockWorkspace(args).package_cache_root = null;
        });
      case "chat_send":
        // Like the Rust command: `skills` is required, even when empty.
        if (!Array.isArray(args.skills) || !args.skills.every((name) => typeof name === "string")) {
          fail("invalid_argument", "Missing skills.");
        }
        return chatSend(
          str(args, "session_id"),
          str(args, "message"),
          args.settings as ChatSettings,
        );
      case "chat_status": {
        const id = str(args, "session_id");
        return clone(
          chats.get(id)?.status ?? { session_id: id, state: "idle", outcome: null, error: null },
        );
      }
      case "chat_subscribe":
        return subscribe(args.on_event);
      case "chat_cancel":
        return chatCancel(str(args, "session_id"));
      case "approvals_summary":
        return { pending: approvals.filter((a) => a.state === "pending").length };
      case "approvals_show_window":
        return null;
      case "approvals_list":
        return clone({ now_unix: nowUnix(), approvals });
      case "approval_detail":
        return detail(str(args, "id"));
      case "approval_decide":
        return decide(args);
      default:
        return fail("unknown_command", `Unknown command ${command}.`);
    }
  }

  function chatDefaults(): ChatDefaults {
    const preferences = savedPreferences ?? {
      model: "qwen2.5-coder",
      context_tokens: 8192,
      max_output_tokens: 1024,
      instructions: "",
      compact_threshold: 6000,
      auto_compact: true,
    };
    return {
      settings: { endpoint: MOCK_ENDPOINT, ...clone(preferences) } satisfies ChatSettings,
      preferences: savedPreferences === null ? "defaults" : "saved",
    };
  }

  /** Applies one mock settings change after the same revision check as core. */
  function changeSettings(args: Record<string, unknown>, change: () => void): SettingsOverview {
    if (args.revision !== settings.revision) {
      fail("stale_revision", "The settings changed since this window read them.");
    }
    change();
    settings.revision += 1;
    return clone(settings);
  }

  function mockWorkspace(args: Record<string, unknown>): WorkspaceSettingsView {
    const id = str(args, "workspace_id");
    const found = settings.workspaces.find((w) => w.workspace_id === id);
    if (found === undefined) {
      fail("unknown_workspace", "The workspace is not registered.");
    }
    return found;
  }

  function initialSettings(): SettingsOverview {
    const workspace = "77777777-7777-4777-8777-777777777777";
    return {
      revision: 12,
      development_root: "/home/dev",
      workspaces: [
        {
          workspace_id: workspace,
          folder_display: "/home/dev/omgcode",
          model_tools: true,
          read: true,
          write: false,
          execute: false,
          task_network: false,
          host_services: false,
          package_cache_root: null,
        },
        {
          workspace_id: "88888888-8888-4888-8888-888888888888",
          folder_display: HOSTILE.rtl,
          model_tools: false,
          read: false,
          write: false,
          execute: false,
          task_network: false,
          host_services: false,
          package_cache_root: null,
        },
      ],
      connection: { endpoint: MOCK_ENDPOINT, source: "environment", profile: null },
      provider_profiles: [
        {
          name: "lms",
          kind: "lm_studio",
          auth: "api_key",
          endpoint: MOCK_ENDPOINT,
          credential_bound: true,
        },
      ],
      toolchain_profiles: [
        {
          name: "rust",
          platform: "linux",
          read_only_roots: ["/home/dev/.rustup"],
          path_entries: [],
          environment_names: ["RUSTUP_HOME"],
        },
      ],
      package_profiles: [],
      container_grants: [],
      skills: [
        {
          name: "review-checklist",
          application: true,
          workspace_overrides: [{ workspace_id: workspace, enabled: false }],
        },
      ],
    };
  }

  return { invoke };
}

export const mockInvoke: Invoker = createMockBackend().invoke;
