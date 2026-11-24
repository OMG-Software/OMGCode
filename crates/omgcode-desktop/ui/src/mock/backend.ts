import type { Invoker } from "../ipc/client";
import { CANCELLED_CODE } from "../ipc/errors";
import type {
  ApprovalDetail,
  ApprovalView,
  ChatBudgets,
  ChatDefaults,
  ChatDefaultsView,
  ChatEvent,
  ChatModels,
  ChatOutcome,
  ChatSettings,
  ChatStatus,
  ComposerPreferences,
  ConnectionStatus,
  ConnectionView,
  PermissionFlags,
  SearchHit,
  SessionSummary,
  SettingsOverview,
  SnippetPart,
  TreeSort,
  TreeView,
  WorkspaceListing,
  WorkspaceSettingsView,
} from "../ipc/types";
import {
  buildFixtures,
  type FixtureSession,
  HOSTILE,
  MOCK_WORKSPACE_TREE,
  OMGCODE_MOCK_FIXTURE_MARKER,
} from "./fixtures";

/** In-memory stand-in for the Rust core. Used only by the development server. */
export const MOCK_BACKEND_NAME = OMGCODE_MOCK_FIXTURE_MARKER;

const CHAT_DURATION_MS = 2000;
const MOCK_REPLY_PIECES = ["This ", "is a ", "mock ", "reply."];
const TOKEN_LIFETIME_MS = 120_000;
const MOCK_ENDPOINT = "http://127.0.0.1:1234/v1";
/** A mock session's record that follows the global default (a model chosen, no provider). */
const FOLLOW = Symbol("follow");

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

/** Rust's bounds on a `workspace_entries` path (ADR-0083). */
const MOCK_PATH_DEPTH_MAX = 32;
const MOCK_COMPONENT_CHARS_MAX = 255;

/** Lists one directory of the fake workspace, refusing a path as Rust does. */
function mockWorkspaceEntries(path: unknown): WorkspaceListing {
  if (!Array.isArray(path) || path.length > MOCK_PATH_DEPTH_MAX) {
    fail("invalid_request", "The folder path is not acceptable.");
  }
  let directory: Record<string, unknown> = MOCK_WORKSPACE_TREE;
  for (const part of path) {
    if (
      typeof part !== "string" ||
      part === "" ||
      part === "." ||
      part === ".." ||
      part.toLowerCase() === ".omgcode" ||
      /[/\\0]/.test(part) ||
      part.length > MOCK_COMPONENT_CHARS_MAX
    ) {
      fail("invalid_request", "The folder path is not acceptable.");
    }
    const next = Object.hasOwn(directory, part) ? directory[part] : undefined;
    if (typeof next !== "object" || next === null) {
      fail("invalid_request", "The folder could not be listed.");
    }
    directory = next as Record<string, unknown>;
  }
  const entries = Object.entries(directory).map(([name, value]) => ({
    name,
    kind: value === null ? ("file" as const) : ("directory" as const),
    lossy: false,
  }));
  entries.sort(
    (a, b) =>
      Number(a.kind !== "directory") - Number(b.kind !== "directory") ||
      a.name.toLowerCase().localeCompare(b.name.toLowerCase()),
  );
  return { entries, truncated: false };
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

/** The composer's compaction slider range (ADR-0080), as Rust reports it. */
const COMPACT_MIN = 128_000;
const COMPACT_MAX = 1_000_000;
/** The bound Rust puts on each saved custom instruction text, in UTF-8 bytes. */
const INSTRUCTIONS_MAX_BYTES = 16 * 1024;
/** The largest value of Rust's `u32`, the type of the output reserve. */
const U32_MAX = 4_294_967_295;
/** Bytes of Rust's `Workspace instructions:` heading line. */
const WORKSPACE_HEADING_BYTES = 24;
/** Bytes both headings and the blank line between the texts add. */
const BOTH_HEADINGS_BYTES = 47;

/** The one saved chat defaults file: the settings window's fields and the composer's. */
type MockPreferences = ChatBudgets & ComposerPreferences & { model: string };

const BUILT_IN_PREFERENCES: MockPreferences = {
  model: "qwen2.5-coder",
  context_tokens: 8192,
  max_output_tokens: 1024,
  max_tool_calls: 128,
  turn_timeout_seconds: 120,
  instructions: "",
  compact_threshold: 200_000,
  auto_compact: true,
  enter_to_send: true,
};

/** Creates a fresh mock core. `now` supplies milliseconds since the epoch. */
export function createMockBackend(now: () => number = Date.now): { invoke: Invoker } {
  const { groups, sessions } = buildFixtures(now());
  const byId = new Map<string, FixtureSession>(sessions.map((s) => [s.summary.session_id, s]));
  const chats = new Map<string, ChatRecord>();
  const tokens = new Map<string, { id: string; expires: number }>();
  let subscriber: Subscriber | null = null;
  /** The saved chat defaults, as Rust keeps them in one file; `null` until something is saved. */
  let savedPreferences: MockPreferences | null = null;
  let nextSkillFolder = 0;
  /**
   * Each session's own provider (a profile name, `null` for the default), the endpoint confirmed
   * for it, as Rust records it, and its model.
   */
  const sessionChoices = new Map<
    string,
    { profile: string | null | typeof FOLLOW; endpoint: string | null; model: string }
  >();
  /** The workspace each session created here belongs to, as core records it. */
  const sessionWorkspaces = new Map<string, string>();
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

  /** Records the request manifest, so a reply is labelled with the model as in a real journal. */
  function appendManifest(item: FixtureSession, model: string): void {
    const last = item.entries[item.entries.length - 1];
    item.entries.push({
      seq: (last?.seq ?? 0) + 1,
      timestamp: new Date(now()).toISOString(),
      kind: "request_manifest",
      request_id: `mock-${item.entries.length}`,
      model,
      prompt_version: "mock",
      tools: [],
    });
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
    if (staleChoice(id)) {
      fail(
        "session_connection_stale",
        "The connection chosen for this session changed or was removed. Choose the provider again, then send.",
      );
    }
    if (settings.endpoint !== effectiveConnection(id).endpoint) {
      fail(
        "connection_changed",
        "The model connection changed in settings. Check the model name, then send again.",
      );
    }
    if (message.trim() === "" || settings.model.trim() === "") {
      fail("invalid_argument", "A message and a model are required.");
    }
    appendManifest(item, settings.model);
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
      waiting: !stale && view.operation === "command",
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
        sessionWorkspaces.set(id, str(args, "workspace_id"));
        groups[0]?.session_ids.unshift(id);
        return clone(created.summary);
      }
      case "chat_defaults":
        return chatDefaults();
      // A fixed list per provider kind, like the servers' `/v1/models` (ADR-0066).
      case "chat_models": {
        const id = str(args, "session_id");
        session(id);
        if (staleChoice(id)) {
          fail(
            "session_connection_stale",
            "The connection chosen for this session changed or was removed. Choose the provider again, then send.",
          );
        }
        const view = effectiveConnection(id);
        return {
          session_id: id,
          endpoint: view.endpoint,
          kind: view.kind,
          models:
            view.kind === "ollama_cloud"
              ? [
                  { id: "gpt-oss:120b", cloud_tag: false },
                  { id: "qwen3-coder:480b", cloud_tag: false },
                ]
              : view.kind === "ollama_local"
                ? [
                    { id: "gpt-oss:120b-cloud", cloud_tag: true },
                    { id: "lfm2.5:latest", cloud_tag: false },
                    { id: "qwen2.5-coder:7b", cloud_tag: false },
                  ]
                : [
                    { id: "google/gemma-4-e4b", cloud_tag: false },
                    { id: "qwen2.5-coder", cloud_tag: false },
                  ],
        } satisfies ChatModels;
      }
      // Each window saves only its own fields; the rest keep their values (ADR-0080).
      case "chat_preferences_save": {
        const saved = clone(args.preferences as ComposerPreferences);
        if (saved.compact_threshold < COMPACT_MIN || saved.compact_threshold > COMPACT_MAX) {
          return fail(
            "invalid_request",
            `The compaction threshold must be from ${COMPACT_MIN} to ${COMPACT_MAX} tokens.`,
          );
        }
        savedPreferences = { ...preferences(), ...saved };
        settings.chat = chatView();
        return chatDefaults();
      }
      case "settings_save_chat_defaults": {
        const saved = clone(args.defaults as ChatBudgets);
        if (saved.context_tokens < 1 || saved.max_output_tokens < 1) {
          return fail("invalid_request", "Set the context size to a positive number of tokens.");
        }
        // Rust's request type holds a 32-bit output reserve; a larger one never reaches the save.
        if (saved.max_output_tokens > U32_MAX) {
          return fail("invalid_request", "invalid value: integer, expected u32");
        }
        const globalRoom = overviewView().chat.instructions_max_bytes ?? 0;
        if (new TextEncoder().encode(saved.instructions).length > globalRoom) {
          return fail(
            "invalid_request",
            `The global instructions can be at most ${globalRoom} bytes beside the longest workspace instructions.`,
          );
        }
        savedPreferences = { ...preferences(), ...saved };
        settings.chat = chatView();
        return overviewView();
      }
      case "settings_set_workspace_instructions": {
        const workspace = mockWorkspace(args);
        const instructions = str(args, "instructions");
        // Rust's bound: UTF-8 bytes beside the global text, and a refusal keeps the saved text.
        const room =
          overviewView().workspaces.find((w) => w.workspace_id === workspace.workspace_id)
            ?.instructions_max_bytes ?? 0;
        if (new TextEncoder().encode(instructions).length > room) {
          return fail(
            "invalid_request",
            `The workspace instructions can be at most ${room} bytes beside the global instructions.`,
          );
        }
        workspace.instructions = instructions;
        return overviewView();
      }
      case "session_skills":
        str(args, "session_id");
        return { workspace_registered: true, skills: ["review-checklist", "release-notes"] };
      case "settings_show_window":
        settings.scope = null;
        return null;
      case "settings_show_workspace": {
        const id = str(args, "session_id");
        session(id);
        // A session created here names its workspace; a fixture session uses the first one.
        const wanted = sessionWorkspaces.get(id) ?? settings.workspaces[0]?.workspace_id;
        const found = settings.workspaces.find((w) => w.workspace_id === wanted);
        if (found === undefined) {
          return fail("unknown_workspace", "The workspace is not registered.");
        }
        settings.scope = { workspace_id: found.workspace_id };
        return null;
      }
      case "settings_clear_scope":
        settings.scope = null;
        return overviewView();
      case "settings_overview":
        return overviewView();
      // The mock's data folder grants no extra account, so, like Rust for a private folder,
      // nothing is asked and nothing changes (ADR-0073).
      case "settings_repair_data_folder":
        return overviewView();
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
            read: "allow",
            write: "prompt",
            execute: "deny",
            task_network: "deny",
            host_services: "deny",
            write_tools: {
              edit: "allow",
              multi_edit: "allow",
              create: "allow",
              rename: "allow",
              delete: "allow",
              create_directory: "allow",
            },
            package_cache_root: null,
            instructions: "",
            instructions_max_bytes: 0,
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
      // Like core: the registration goes; the folder and the sessions recorded in it stay (ADR-0081).
      case "settings_unregister_workspace":
        return changeSettings(args, () => {
          const removed = mockWorkspace(args);
          settings.workspaces = settings.workspaces.filter((w) => w !== removed);
        });
      // Like core: only a local profile with an endpoint can be chosen, and the confirmation
      // (a native dialog in the application) is accepted here.
      case "settings_select_connection":
        return changeSettings(args, () => selectConnection(args.profile));
      // The Files tab (ADR-0083): one directory of a small fake workspace, validated like Rust.
      case "workspace_entries": {
        session(str(args, "session_id"));
        return mockWorkspaceEntries(args.path);
      }
      // The main window's status bar (ADR-0064). The mock's workspace is on a fixed branch.
      case "session_context":
        str(args, "session_id");
        return {
          workspace_registered: true,
          branch: { state: "branch", name: "main", note: null },
        };
      case "connection_status": {
        const id = args.session_id;
        if (id !== null && typeof id !== "string") {
          fail("invalid_argument", "Bad session.");
        }
        if (id !== null) {
          session(id);
        }
        return connectionStatus(id);
      }
      // One session's provider (ADR-0071). The native confirmation is accepted here; a model
      // belongs to one server, so the choice clears it.
      case "connection_select": {
        const id = str(args, "session_id");
        session(id);
        if (!settings.connection.selectable) {
          fail("connection_fixed", "The connection was set when OMGCode started.");
        }
        const wanted = args.profile ?? null;
        if (wanted !== null && typeof wanted !== "string") {
          fail("invalid_argument", "Bad profile.");
        }
        const profile =
          wanted === null ? null : settings.provider_profiles.find((p) => p.name === wanted);
        if (profile === undefined || (profile !== null && !profile.selectable)) {
          fail("control", "only a profile with a recorded endpoint can be chosen");
        }
        // A stale choice is no current choice, as in Rust, so the same name can be chosen again.
        if (!staleChoice(id) && (effectiveConnection(id).profile ?? null) === wanted) {
          fail("control", "this is already the connection of this session");
        }
        sessionChoices.set(id, {
          profile: wanted,
          endpoint: profile === null ? MOCK_ENDPOINT : profile.endpoint,
          model: "",
        });
        return connectionStatus(id);
      }
      case "session_model_select": {
        const id = str(args, "session_id");
        session(id);
        const model = str(args, "model");
        // The service's rule: at most 256 UTF-8 bytes and no control character.
        if (new TextEncoder().encode(model).length > 256 || /\p{Cc}/u.test(model)) {
          fail("invalid_request", "The model name is not valid.");
        }
        const existing = sessionChoices.get(id);
        sessionChoices.set(id, {
          profile: existing === undefined ? FOLLOW : existing.profile,
          endpoint: existing === undefined ? null : existing.endpoint,
          model,
        });
        return connectionStatus(id);
      }
      case "settings_add_connection":
        return changeSettings(args, () => {
          const name = str(args, "name");
          const kind = str(args, "kind");
          const endpoint = args.endpoint ?? null;
          const apiKey = args.api_key ?? null;
          if (kind !== "lm_studio" && kind !== "ollama_local" && kind !== "ollama_cloud") {
            fail("invalid_argument", "Bad kind.");
          }
          if (kind === "ollama_cloud") {
            if (endpoint !== null || typeof apiKey !== "string") {
              fail("invalid_request", "Ollama Cloud takes an API key and no endpoint.");
            }
            checkApiKey(apiKey);
          } else if (typeof endpoint !== "string" || apiKey !== null) {
            fail("invalid_request", "A local connection takes an endpoint and no API key.");
          } else if (!isLocalEndpoint(endpoint)) {
            fail("control", "a local provider endpoint must be http://<loopback IP>:<port>/v1");
          }
          if (settings.provider_profiles.some((p) => p.name === name)) {
            fail("control", "a provider profile with this name already exists; remove it first");
          }
          settings.provider_profiles.push({
            name,
            kind,
            auth: kind === "ollama_cloud" ? "api_key" : "none",
            endpoint: kind === "ollama_cloud" ? "https://ollama.com/v1" : (endpoint as string),
            credential_bound: kind === "ollama_cloud",
            selectable: true,
            active: false,
          });
        });
      case "settings_set_api_key":
        return changeSettings(args, () => {
          const name = str(args, "name");
          const profile = settings.provider_profiles.find((p) => p.name === name);
          if (profile === undefined) {
            fail("control", "unknown provider profile");
          }
          if (profile.kind !== "ollama_cloud") {
            fail("invalid_request", "Only an Ollama Cloud connection has an API key set here.");
          }
          checkApiKey(args.api_key);
          profile.credential_bound = true;
        });
      // The mock stands in for the native folder dialog: each import "chooses" the next sample
      // folder, and the last one is already registered, so the refusal shows (ADR-0082).
      case "settings_import_skill":
        return changeSettings(args, () => {
          const name = SAMPLE_SKILL_FOLDERS[nextSkillFolder % SAMPLE_SKILL_FOLDERS.length] ?? "";
          nextSkillFolder += 1;
          if (settings.skills.some((skill) => skill.name === name)) {
            fail(
              "skill_name_taken",
              "a skill with this name is already registered; remove it first",
            );
          }
          if (settings.skills.length >= MAX_SKILLS) {
            fail("too_many_skills", `at most ${MAX_SKILLS} skills may be registered`);
          }
          settings.skills.push({ name, application: null, workspace_overrides: [] });
        });
      // Rust confirms in a native dialog before a change that enables the skill; the mock accepts.
      case "settings_set_skill_state":
        return changeSettings(args, () => {
          const skill = settings.skills.find((entry) => entry.name === str(args, "name"));
          const state = str(args, "skill_state");
          const workspaceId = args.workspace_id ?? null;
          if (state !== "enabled" && state !== "disabled" && state !== "inherit") {
            fail("invalid_argument", "Bad skill state.");
          }
          if (skill === undefined) {
            fail("skill_not_found", "no registered skill has this name or identifier");
          }
          if (workspaceId === null) {
            if (state === "inherit") {
              fail("skill_inherit_needs_workspace", "inherit applies only to a workspace scope");
            }
            skill.application = state === "enabled";
            return;
          }
          if (typeof workspaceId !== "string") {
            fail("invalid_argument", "Bad workspace_id.");
          }
          mockWorkspace({ workspace_id: workspaceId });
          skill.workspace_overrides = skill.workspace_overrides.filter(
            (entry) => entry.workspace_id !== workspaceId,
          );
          if (state !== "inherit") {
            skill.workspace_overrides.push({
              workspace_id: workspaceId,
              enabled: state === "enabled",
            });
          }
        });
      case "settings_remove_skill":
        return changeSettings(args, () => {
          const index = settings.skills.findIndex((entry) => entry.name === str(args, "name"));
          if (index < 0) {
            fail("skill_not_found", "no registered skill has this name or identifier");
          }
          settings.skills.splice(index, 1);
        });
      case "settings_remove_connection":
        return changeSettings(args, () => {
          const name = str(args, "name");
          const index = settings.provider_profiles.findIndex((p) => p.name === name);
          const profile = settings.provider_profiles[index];
          if (profile === undefined) {
            fail("control", "unknown provider profile");
          }
          // Rust asks a native Yes/No first when a key is stored, then deletes both.
          settings.provider_profiles.splice(index, 1);
          if (profile.active) {
            settings.connection = {
              endpoint: MOCK_ENDPOINT,
              source: "active",
              profile: null,
              kind: null,
              selectable: true,
            };
          }
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

  /** The saved chat defaults, or the mock's built-in ones. */
  function preferences(): MockPreferences {
    return clone(savedPreferences ?? BUILT_IN_PREFERENCES);
  }

  function chatDefaults(): ChatDefaults {
    const { enter_to_send, ...turnSettings } = preferences();
    return {
      settings: { endpoint: currentEndpoint(), ...turnSettings } satisfies ChatSettings,
      tool_call_limit_max: 256,
      turn_timeout_seconds_max: 3600,
      preferences: savedPreferences === null ? "defaults" : "saved",
      enter_to_send,
      compact_threshold_min: COMPACT_MIN,
      compact_threshold_max: COMPACT_MAX,
    };
  }

  /**
   * The overview as Rust returns it, with each instruction room worked out as Rust does
   * (ADR-0080): what is left of 16 KiB beside the other text and the headings a turn adds.
   */
  function overviewView(): SettingsOverview {
    const view = clone(settings);
    const bytes = (text: string): number => new TextEncoder().encode(text).length;
    const global = bytes(preferences().instructions);
    for (const workspace of view.workspaces) {
      const used = global === 0 ? WORKSPACE_HEADING_BYTES : BOTH_HEADINGS_BYTES + global;
      workspace.instructions_max_bytes = Math.max(0, INSTRUCTIONS_MAX_BYTES - used);
    }
    const longest = Math.max(0, ...view.workspaces.map((w) => bytes(w.instructions ?? "")));
    view.chat.instructions_max_bytes =
      longest === 0
        ? INSTRUCTIONS_MAX_BYTES
        : Math.max(0, INSTRUCTIONS_MAX_BYTES - BOTH_HEADINGS_BYTES - longest);
    return view;
  }

  /** The chat defaults the settings window shows (ADR-0080). */
  function chatView(): ChatDefaultsView {
    const saved = preferences();
    return {
      defaults: {
        context_tokens: saved.context_tokens,
        max_output_tokens: saved.max_output_tokens,
        max_tool_calls: saved.max_tool_calls,
        turn_timeout_seconds: saved.turn_timeout_seconds,
        instructions: saved.instructions,
      },
      source: savedPreferences === null ? "defaults" : "saved",
      max_output_tokens_max: U32_MAX,
      tool_call_limit_max: 256,
      turn_timeout_seconds_max: 3600,
      instructions_max_bytes: INSTRUCTIONS_MAX_BYTES,
    };
  }

  /** The endpoint the next mock turn uses: the active connection's, or the default. */
  function currentEndpoint(): string {
    return settings.connection.endpoint;
  }

  /** Makes `wanted` (a profile name, or `null` for the default) the active connection. */
  function selectConnection(wanted: unknown): void {
    if (wanted !== null && typeof wanted !== "string") {
      fail("invalid_argument", "Bad profile.");
    }
    const profile =
      wanted === null ? null : settings.provider_profiles.find((p) => p.name === wanted);
    if (profile === undefined || (profile !== null && !profile.selectable)) {
      fail("control", "only a local profile with a recorded endpoint can be chosen");
    }
    for (const each of settings.provider_profiles) {
      each.active = each === profile;
    }
    settings.connection = {
      endpoint: profile?.endpoint ?? MOCK_ENDPOINT,
      source: "active",
      profile: profile?.name ?? null,
      kind: profile?.kind ?? null,
      selectable: true,
    };
  }

  /**
   * Whether a session's own choice can no longer be used as recorded: its profile is gone, cannot
   * be chosen, or has another endpoint (ADR-0071 decision 7). Rust then refuses its turns.
   */
  function staleChoice(sessionId: string | null): boolean {
    const choice = sessionId === null ? undefined : sessionChoices.get(sessionId);
    if (!settings.connection.selectable || choice === undefined || choice.profile === FOLLOW) {
      return false;
    }
    if (choice.profile === null) {
      return choice.endpoint !== MOCK_ENDPOINT;
    }
    const profile = settings.provider_profiles.find((each) => each.name === choice.profile);
    return profile === undefined || !profile.selectable || profile.endpoint !== choice.endpoint;
  }

  /** The connection a session's next turn uses: its own choice, or the global default. */
  function effectiveConnection(sessionId: string | null): ConnectionView {
    const choice = sessionId === null ? undefined : sessionChoices.get(sessionId);
    if (!settings.connection.selectable || choice === undefined || choice.profile === FOLLOW) {
      return settings.connection;
    }
    const profile = settings.provider_profiles.find((each) => each.name === choice.profile);
    return {
      // A stale choice shows what was recorded, which the next turn refuses.
      endpoint: choice.endpoint ?? profile?.endpoint ?? MOCK_ENDPOINT,
      source: "active",
      profile: choice.profile,
      kind: profile?.kind ?? null,
      selectable: true,
    };
  }

  /** What the composer reads for a session (ADR-0071). */
  function connectionStatus(sessionId: string | null): ConnectionStatus {
    const choice = sessionId === null ? undefined : sessionChoices.get(sessionId);
    const view = effectiveConnection(sessionId);
    const own = choice !== undefined && choice.profile !== FOLLOW;
    const stale = staleChoice(sessionId);
    const choices = settings.connection.selectable
      ? settings.provider_profiles
          .filter((profile) => profile.selectable && profile.endpoint !== null)
          .map((profile) => ({
            name: profile.name,
            kind: profile.kind,
            endpoint: profile.endpoint ?? "",
            // A replacement under the same name is offered, but active only once chosen again.
            active: !stale && profile.name === view.profile,
          }))
      : [];
    return clone({
      connection: view,
      origin: !settings.connection.selectable ? "launch" : own ? "session" : "default",
      model: choice === undefined ? chatDefaults().settings.model : choice.model,
      stale,
      session_choices_unreadable: false,
      choices,
    } satisfies ConnectionStatus);
  }

  /** Applies one mock settings change after the same revision check as core. */
  function changeSettings(args: Record<string, unknown>, change: () => void): SettingsOverview {
    if (args.revision !== settings.revision) {
      fail("stale_revision", "The settings changed since this window read them.");
    }
    change();
    settings.revision += 1;
    return overviewView();
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
          read: "allow",
          write: "prompt",
          execute: "deny",
          task_network: "deny",
          host_services: "deny",
          write_tools: {
            edit: "allow",
            multi_edit: "allow",
            create: "allow",
            rename: "allow",
            delete: "prompt",
            create_directory: "allow",
          },
          package_cache_root: null,
          instructions: "Run the tests before answering.",
          instructions_max_bytes: 0,
        },
        {
          workspace_id: "88888888-8888-4888-8888-888888888888",
          folder_display: HOSTILE.rtl,
          model_tools: false,
          read: "deny",
          write: "deny",
          execute: "deny",
          task_network: "deny",
          host_services: "deny",
          write_tools: {
            edit: "allow",
            multi_edit: "allow",
            create: "allow",
            rename: "allow",
            delete: "allow",
            create_directory: "allow",
          },
          package_cache_root: null,
          instructions: "",
          instructions_max_bytes: 0,
        },
      ],
      connection: {
        endpoint: MOCK_ENDPOINT,
        source: "active",
        profile: null,
        kind: null,
        selectable: true,
      },
      provider_profiles: [
        {
          name: "lms",
          kind: "lm_studio",
          auth: "api_key",
          endpoint: MOCK_ENDPOINT,
          credential_bound: true,
          selectable: true,
          active: false,
        },
        {
          name: "ollama",
          kind: "ollama_local",
          auth: "none",
          endpoint: "http://127.0.0.1:11434/v1",
          credential_bound: false,
          selectable: true,
          active: false,
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
      chat: chatView(),
      scope: null,
    };
  }

  return { invoke };
}

export const mockInvoke: Invoker = createMockBackend().invoke;

/** The sample folders the mock's skill import cycles through; the last is already registered. */
const SAMPLE_SKILL_FOLDERS = ["release-notes", "api-docs", "review-checklist"];

/** The most skills core registers. */
const MAX_SKILLS = 64;

/** The most bytes a recorded provider endpoint may have, as in core. */
const MAX_ENDPOINT_BYTES = 256;

/**
 * The mock's copy of core's local endpoint rule (`validate_base_url` with the profile length bound):
 * `http://<loopback IP>:<port>/v1`, with an optional trailing slash, a port from 1 to 65535, and no
 * user information, query, or fragment. Core remains the authority; this keeps the development mock
 * from accepting or refusing what the application would not.
 */
export function isLocalEndpoint(endpoint: string): boolean {
  if (new TextEncoder().encode(endpoint).length > MAX_ENDPOINT_BYTES) {
    return false;
  }
  if (Array.from(endpoint).some((c) => /\p{Cc}/u.test(c))) {
    return false;
  }
  const match = /^http:\/\/(?:\[([0-9A-Fa-f:]+)\]|([0-9.]+)):(\d+)\/v1\/?$/.exec(endpoint);
  if (match === null) {
    return false;
  }
  const [, v6, v4, port] = match;
  const portNumber = Number(port);
  if (!Number.isInteger(portNumber) || portNumber < 1 || portNumber > 65535) {
    return false;
  }
  return v6 === undefined ? isIpv4Loopback(v4 ?? "") : isIpv6Loopback(v6);
}

/** The bound core's `Secret` applies to a key, in UTF-8 bytes. */
const MAX_API_KEY_BYTES = 16 * 1024;

/**
 * Refuses an API key as Rust's `ApiKeyText::into_secret` does: not text, empty, over the bound,
 * or holding a control character. One message for all, as in Rust.
 */
function checkApiKey(key: unknown): void {
  if (
    typeof key !== "string" ||
    key === "" ||
    new TextEncoder().encode(key).length > MAX_API_KEY_BYTES ||
    /\p{Cc}/u.test(key)
  ) {
    fail(
      "invalid_request",
      "The API key is empty, longer than 16 KiB, or contains a control character.",
    );
  }
}

/** `127.0.0.0/8` in strict dotted decimal: four parts, no leading zero. */
function isIpv4Loopback(host: string): boolean {
  const parts = host.split(".");
  return (
    parts.length === 4 &&
    parts.every((part) => /^(0|[1-9]\d{0,2})$/.test(part) && Number(part) <= 255) &&
    parts[0] === "127"
  );
}

/** Only `::1`, in any spelling of eight groups; no embedded IPv4 form. */
function isIpv6Loopback(host: string): boolean {
  const halves = host.split("::");
  if (halves.length > 2) {
    return false;
  }
  const groups = (half: string | undefined): string[] =>
    half === undefined || half === "" ? [] : half.split(":");
  const head = groups(halves[0]);
  const tail = groups(halves[1]);
  const missing = 8 - head.length - tail.length;
  if (halves.length === 1 ? missing !== 0 : missing < 1) {
    return false;
  }
  const all = [...head, ...Array<string>(halves.length === 1 ? 0 : missing).fill("0"), ...tail];
  if (!all.every((group) => /^[0-9A-Fa-f]{1,4}$/.test(group))) {
    return false;
  }
  const values = all.map((group) => Number.parseInt(group, 16));
  return values.slice(0, 7).every((value) => value === 0) && values[7] === 1;
}
