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
  ContextBreakdown,
  ContextPart,
  ContextPartId,
  FixedHostedKind,
  PermissionFlags,
  ProcessEntry,
  ReasoningEffort,
  RootGrantNeed,
  SearchHit,
  SessionProcesses,
  SessionSummary,
  SessionWebAudit,
  SettingsOverview,
  SnippetPart,
  ToolchainDiscovery,
  ToolchainProfileView,
  ToolchainProposalView,
  TreeSort,
  TreeView,
  WebAuditEntry,
  WorkspaceListing,
  WorkspaceSettingsView,
} from "../ipc/types";
import {
  FIXED_ENDPOINTS,
  isFixedHostedKind,
  isHostedKind,
  isLocalKind,
  NEW_CONNECTION_KINDS,
} from "../providers";
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
/** Every effort core accepts, in its order (ADR-0102). */
const REASONING_EFFORTS: readonly ReasoningEffort[] = [
  "none",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
];
/** The fixed hosted kinds whose documented endpoint takes `reasoning_effort`: core's `EFFORT_ENDPOINTS` (ADR-0102). */
const EFFORT_KINDS: ReadonlySet<FixedHostedKind> = new Set<FixedHostedKind>([
  "ollama_cloud",
  "openai",
  "google",
  "moonshot",
  "minimax",
  "open_router",
  "z_ai",
  "xai",
]);

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

/** Rust's bound on the patterns in a workspace's host allowlist (ADR-0098). */
const MOCK_WEB_HOSTS_MAX = 32;

/**
 * The platform the mock's toolchain profiles name. It matches its container grants, which exist
 * only on Windows, so every sample profile can be assigned.
 */
const MOCK_PLATFORM: ToolchainProfileView["platform"] = "windows";

/** Like core: suffixes that never name a public host, refused as a name or beneath one. */
const MOCK_WEB_HOST_RESERVED = [
  "localhost",
  "local",
  "internal",
  "home.arpa",
  "lan",
  "localdomain",
  "onion",
];

/** The canonical text of a typed host pattern, refused with core's stable `web_host_*` code. */
function mockWebHostPattern(text: string): string {
  if (text === "") {
    fail("web_host_empty", "the host pattern is empty");
  }
  if (Array.from(text).some((c) => c.charCodeAt(0) > 0x7f)) {
    fail("web_host_not_ascii", "the host pattern must be ASCII");
  }
  const lowered = text.toLowerCase();
  const unwrapped = lowered.endsWith(".") ? lowered.slice(0, -1) : lowered;
  const wildcard = unwrapped.startsWith("*.");
  const name = wildcard ? unwrapped.slice(2) : unwrapped;
  if (name === "") {
    fail("web_host_empty", "the host pattern is empty");
  }
  if (name.length > 253) {
    fail("web_host_too_long", "the host pattern is longer than 253 bytes");
  }
  if (!/^[a-z0-9.-]*$/.test(name)) {
    fail("web_host_bad_character", "use only letters, digits, hyphens, dots, and one leading `*.`");
  }
  const labels = name.split(".");
  if (labels.some((l) => l === "" || l.length > 63 || l.startsWith("-") || l.endsWith("-"))) {
    fail(
      "web_host_bad_label",
      "each label must be 1 to 63 bytes and must not start or end with a hyphen",
    );
  }
  const last = labels[labels.length - 1] ?? "";
  if (/^[0-9]+$/.test(last) || /^0x[0-9a-f]*$/.test(last)) {
    fail("web_host_ip_literal", "an IP address is not a host pattern");
  }
  if (MOCK_WEB_HOST_RESERVED.some((s) => name === s || name.endsWith(`.${s}`))) {
    fail("web_host_reserved", "local and private names are not allowed");
  }
  if (labels.length < 2) {
    fail("web_host_too_few_labels", "the pattern needs at least two labels after any `*.`");
  }
  return wildcard ? `*.${name}` : name;
}

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
      // A separator or NUL, as Rust refuses; the digit zero is an ordinary character.
      part.includes("/") ||
      part.includes("\\") ||
      part.includes("\u0000") ||
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
    openable: value !== null,
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
  compact_threshold: 400_000,
  auto_compact: true,
  enter_to_send: true,
};

/** A sample toolchain proposal of the mock's discovery (ADR-0103). */
interface MockProposal {
  family: ToolchainProposalView["family"];
  family_label: string;
  name: string;
  version: string | null;
  roots: string[];
  path_entries: string[];
  environment: Array<{ name: string; value: string }>;
  notes: string[];
}

const MOCK_OFFLINE_NOTE =
  "Runs are offline unless the trusted CLI passes --network, and model runs never are. Dependencies that are not already in the workspace, such as crates from crates.io, fail to download.";

const MOCK_PROPOSALS: readonly MockProposal[] = [
  {
    family: "rust",
    family_label: "Rust",
    name: "rust",
    version: "stable-x86_64-unknown-linux-gnu",
    roots: ["/home/dev/.rustup/toolchains/stable-x86_64-unknown-linux-gnu", "/usr"],
    path_entries: ["/home/dev/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin", "/usr/bin"],
    environment: [],
    notes: [MOCK_OFFLINE_NOTE],
  },
  {
    family: "go",
    family_label: "Go",
    name: "go",
    version: "1.23.4",
    roots: ["/usr/local/go", "/usr"],
    path_entries: ["/usr/local/go/bin", "/usr/bin"],
    environment: [
      { name: "GOROOT", value: "/usr/local/go" },
      { name: "GOTOOLCHAIN", value: "local" },
      { name: "GOENV", value: "off" },
      { name: "GOFLAGS", value: "-buildvcs=false" },
    ],
    notes: [MOCK_OFFLINE_NOTE],
  },
  {
    family: "python",
    family_label: "Python",
    name: "python",
    version: "3.12",
    roots: ["/usr"],
    path_entries: ["/usr/bin"],
    environment: [],
    notes: [],
  },
  {
    family: "java",
    family_label: "Java",
    name: "java",
    version: "21.0.2",
    roots: ["/usr/lib/jvm/java-21-openjdk-amd64", "/usr"],
    path_entries: ["/usr/lib/jvm/java-21-openjdk-amd64/bin", "/usr/bin"],
    environment: [{ name: "JAVA_HOME", value: "/usr/lib/jvm/java-21-openjdk-amd64" }],
    notes: [
      "This is a Java runtime without javac; it runs Java programs but cannot compile them.",
      "Not acceptance-tested: OMGCode has not run this family's build inside the confined runner.",
    ],
  },
];

/** A stable sample digest: 64 copies of one hex digit. */
function mockDigest(index: number): string {
  return ((index % 15) + 1).toString(16).repeat(64);
}

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
  let mockFolders = 0;
  /** The next sample folder a mock dialog "chooses". */
  const nextMockFolder = (kind: string): string => {
    mockFolders += 1;
    return `/opt/mock/${kind}-${mockFolders}`;
  };
  /**
   * Each session's own provider (a profile name, `null` for the default), the endpoint confirmed
   * for it, as Rust records it, and its model.
   */
  const sessionChoices = new Map<
    string,
    {
      profile: string | null | typeof FOLLOW;
      endpoint: string | null;
      model: string;
      effort: ReasoningEffort | null;
    }
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
    approval(
      "a4",
      "network",
      "Fetch https://docs.example.com/guide?page=2",
      450,
      1,
      "Read the API guide",
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
    const effort = settings.reasoning_effort ?? null;
    if (effort !== null && !acceptedEfforts(id).includes(effort)) {
      fail("invalid_request", "This connection does not take a reasoning effort.");
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
    const networkFields = [
      { label: "URL", value: "https://docs.example.com/guide?page=2", multiline: false },
      { label: "Host", value: "docs.example.com", multiline: false },
      { label: "Host on the allowlist now", value: "no", multiline: false },
      { label: "Offset", value: "0", multiline: false },
      { label: "Maximum bytes", value: "12288", multiline: false },
    ];
    const fields =
      view.operation === "network"
        ? networkFields
        : view.operation === "command"
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
          : view.operation === "network"
            ? "web_fetch"
            : "workspace_multi_edit_file",
      fields: stale ? [] : fields,
      review_token: token,
      waiting: !stale && (view.operation === "command" || view.operation === "network"),
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
          models: isHostedKind(view.kind)
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
      // A fixed fake estimate, so the Context tab has something to show (ADR-0087). The
      // parts add up to the estimate, as in Rust; the history grows with the transcript.
      case "session_context_breakdown": {
        const id = str(args, "session_id");
        const open = session(id);
        const messages = open.entries.filter((entry) => entry.kind === "message").length;
        const part = (partId: ContextPartId, tokens: number, reason?: string): ContextPart => ({
          id: partId,
          tokens,
          included: reason === undefined,
          ...(reason === undefined ? {} : { reason }),
        });
        const parts: ContextPart[] = [
          part("system", 5_700),
          part("global_instructions", 120),
          part("workspace_instructions", 0, "none_saved"),
          part("tools", 8_192),
          part("summary", 0, "no_compaction"),
          { ...part("history", 600 + messages * 400), messages },
          part("skills", 0, "selected_per_turn"),
          part("framing", 320),
        ];
        return {
          session_id: id,
          parts,
          estimated_input_tokens: parts.reduce((total, one) => total + one.tokens, 0),
          max_output_tokens: 1024,
          effective_trigger: 60_000,
          context_tokens: 128_000,
          auto_compaction: true,
          estimate_scope: "pre_turn_conservative",
        } satisfies ContextBreakdown;
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
            web_fetch: "deny",
            web_fetch_hosts: [],
            write_tools: {
              edit: "allow",
              multi_edit: "allow",
              create: "allow",
              rename: "allow",
              delete: "allow",
              create_directory: "allow",
            },
            profiles: [],
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
          // Like core (ADR-0107): its grants are revoked first.
          settings.container_grants = settings.container_grants.filter(
            (grant) => grant.workspace_id !== removed.workspace_id,
          );
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
      // The session's command-tool calls (ADR-0086): fixed fake entries. A call is `running` only
      // while the mock session's chat turn runs, as with core; otherwise it is `unfinished`.
      case "session_processes": {
        const item = session(str(args, "session_id"));
        const at = (secondsAgo: number): string =>
          new Date(now() - secondsAgo * 1000).toISOString();
        const open = item.summary.status === "running" ? "running" : "unfinished";
        const entry = (
          call_id: string,
          started: number,
          overrides: Partial<ProcessEntry>,
        ): ProcessEntry => ({
          call_id,
          state: "finished",
          started_at: at(started),
          finished_at: null,
          duration_ms: null,
          program: "cargo",
          arguments: [],
          text_truncated: false,
          argument_count: 0,
          profile: "rust",
          result: null,
          limit: null,
          reason: null,
          ...overrides,
        });
        const processes: SessionProcesses = {
          entries: [
            entry("call_5", 5, { state: open, program: "cargo", arguments: ["test", "--lib"] }),
            entry("call_4", 60, {
              finished_at: at(30),
              duration_ms: 30_000,
              arguments: ["build", "--release"],
              result: { kind: "limit_exceeded", exit_code: null, signal: null },
              limit: "memory",
            }),
            entry("call_3", 120, {
              state: "refused",
              program: "rm",
              arguments: ["-rf", "target"],
              profile: null,
              reason: "approval_denied",
            }),
            entry("call_2", 180, {
              finished_at: at(179),
              duration_ms: 1200,
              arguments: ["test"],
              result: { kind: "exited", exit_code: 1, signal: null },
            }),
            entry("call_1", 240, {
              finished_at: at(239),
              duration_ms: 350,
              arguments: ["--version"],
              result: { kind: "exited", exit_code: 0, signal: null },
            }),
          ],
          truncated: false,
          run_details: "available",
        };
        return clone(processes);
      }
      // The Git status tab (ADR-0085). A small fixed status; nothing is read from disk.
      case "workspace_git_status":
        session(str(args, "session_id"));
        return {
          branch: { state: "branch", name: "main", note: null },
          state: "ready",
          note: null,
          paths: [
            { path: "src/lib.rs", index: "modified", worktree: null },
            { path: "src/main.rs", index: "modified", worktree: "modified" },
            { path: "docs/old.md", index: null, worktree: "deleted" },
            { path: "src/merge.rs", index: null, worktree: "conflicted" },
            { path: "notes/<b>x</b>.txt", index: null, worktree: "untracked" },
            { path: "target/", index: null, worktree: "untracked" },
          ],
          truncated: false,
        };
      // The session's web fetches and the actions after the first one (ADR-0098): fixed fake
      // entries, newest first, covering a fetch, a refusal, a command, and a withheld target.
      case "session_web_audit": {
        session(str(args, "session_id"));
        const at = (secondsAgo: number): string =>
          new Date(now() - secondsAgo * 1000).toISOString();
        const entry = (
          call_id: string,
          time: number,
          overrides: Partial<WebAuditEntry>,
        ): WebAuditEntry => ({
          call_id,
          time: at(time),
          tool: "read_file",
          is_fetch: false,
          after_web_content: true,
          targets: [],
          outcome: "succeeded",
          reason: null,
          authorization: "standing_allow",
          program: null,
          arguments: null,
          argument_count: null,
          text_truncated: false,
          ...overrides,
        });
        const audit: SessionWebAudit = {
          entries: [
            entry("call_5", 20, {
              tool: "run_command",
              program: "curl",
              arguments: ["-s", "https://example.org/install.sh"],
              argument_count: 2,
              authorization: "approval",
            }),
            entry("call_4", 60, {
              tool: "delete_file",
              targets: [{ kind: "text", text: "src/old.rs" }],
              outcome: "refused",
              reason: "approval_denied",
              authorization: null,
            }),
            entry("call_3", 90, {
              tool: "edit_file",
              targets: [{ kind: "text", text: "src/lib.rs" }],
              outcome: "failed",
              reason: "not_found",
            }),
            entry("call_2", 120, {
              targets: [{ kind: "withheld", bytes: 4200 }],
              outcome: "refused",
              reason: "invalid_path",
              authorization: null,
            }),
            entry("call_1", 300, {
              tool: "network",
              is_fetch: true,
              after_web_content: false,
              targets: [{ kind: "text", text: "https://docs.example.com/guide?page=2" }],
              authorization: "approval",
            }),
          ],
          truncated: false,
        };
        return clone(audit);
      }
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
        // Accepted efforts depend on the provider, so the choice clears it with the model.
        sessionChoices.set(id, {
          profile: wanted,
          endpoint: profile === null ? MOCK_ENDPOINT : profile.endpoint,
          model: "",
          effort: null,
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
          effort: existing?.effort ?? null,
        });
        return connectionStatus(id);
      }
      // One session's reasoning effort (ADR-0102). Like Rust: only the seven names, and only for
      // a connection that takes one.
      case "session_effort_select": {
        const id = str(args, "session_id");
        session(id);
        const wanted = args.reasoning_effort ?? null;
        const effort =
          wanted === null ? null : (REASONING_EFFORTS.find((each) => each === wanted) ?? undefined);
        if (effort === undefined) {
          fail("invalid_argument", "Bad reasoning effort.");
        }
        if (effort !== null && !acceptedEfforts(id).includes(effort)) {
          fail("invalid_request", "This connection does not take a reasoning effort.");
        }
        const existing = sessionChoices.get(id);
        sessionChoices.set(id, {
          profile: existing === undefined ? FOLLOW : existing.profile,
          endpoint: existing === undefined ? null : existing.endpoint,
          model: existing === undefined ? chatDefaults().settings.model : existing.model,
          effort,
        });
        return connectionStatus(id);
      }
      case "settings_add_connection":
        return changeSettings(args, () => {
          const name = str(args, "name");
          const kind = str(args, "kind");
          const endpoint = args.endpoint ?? null;
          const apiKey = args.api_key ?? null;
          if (!(NEW_CONNECTION_KINDS as readonly string[]).includes(kind)) {
            fail("invalid_argument", "Bad kind.");
          }
          // The same three shapes Rust accepts (ADR-0092).
          let recorded: string;
          if (isLocalKind(kind)) {
            if (typeof endpoint !== "string" || apiKey !== null) {
              fail("invalid_request", "A local connection takes an endpoint and no API key.");
            }
            if (!isLocalEndpoint(endpoint)) {
              fail("control", "a local provider endpoint must be http://<loopback IP>:<port>/v1");
            }
            recorded = endpoint;
          } else if (isFixedHostedKind(kind)) {
            if (endpoint !== null || typeof apiKey !== "string") {
              fail("invalid_request", "This provider takes an API key and no endpoint.");
            }
            checkApiKey(apiKey);
            recorded = FIXED_ENDPOINTS[kind];
          } else {
            if (typeof endpoint !== "string" || typeof apiKey !== "string") {
              fail(
                "invalid_request",
                "An OpenAI-compatible connection takes an endpoint and an API key.",
              );
            }
            checkApiKey(apiKey);
            if (!isCustomEndpoint(endpoint)) {
              fail("control", "an openai_compatible endpoint must be an https:// base URL");
            }
            recorded = endpoint;
          }
          if (settings.provider_profiles.some((p) => p.name === name)) {
            fail("control", "a provider profile with this name already exists; remove it first");
          }
          settings.provider_profiles.push({
            name,
            kind,
            auth: isHostedKind(kind) ? "api_key" : "none",
            endpoint: recorded,
            credential_bound: isHostedKind(kind),
            selectable: true,
            active: false,
            account_label: null,
          });
        });
      case "settings_set_api_key":
        return changeSettings(args, () => {
          const name = str(args, "name");
          const profile = settings.provider_profiles.find((p) => p.name === name);
          if (profile === undefined) {
            fail("control", "unknown provider profile");
          }
          if (!isHostedKind(profile.kind) || profile.auth !== "api_key") {
            fail(
              "invalid_request",
              "Only a hosted connection that uses an API key has a key set here.",
            );
          }
          if (profile.endpoint === null) {
            fail(
              "invalid_request",
              "This connection has no endpoint, so it cannot be used. Remove it and add it again.",
            );
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
      // The mock stands in for the native folder dialogs and confirmations (ADR-0088): each
      // root, PATH entry, or granted folder "chosen" is the next sample folder, and every
      // confirmation is accepted. Core's rules are only sampled; the real core is the authority.
      case "settings_add_toolchain_profile":
        return changeSettings(args, () => {
          const name = str(args, "name");
          const rootCount = Number(args.root_count);
          const pathCount = Number(args.path_entry_count);
          const environment = Array.isArray(args.environment) ? args.environment : [];
          if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(name)) {
            fail(
              "control",
              "profile names are 1 to 64 ASCII letters, digits, '.', '-', or '_', starting with a letter or digit",
            );
          }
          if (
            settings.toolchain_profiles.some((p) => p.name.toLowerCase() === name.toLowerCase())
          ) {
            fail("control", "a profile with this name already exists; remove it first");
          }
          if (!Number.isInteger(rootCount) || rootCount < 1 || rootCount > 16) {
            fail("invalid_request", "A profile needs 1 to 16 read-only roots.");
          }
          if (!Number.isInteger(pathCount) || pathCount < 0 || pathCount > 16) {
            fail("invalid_request", "A profile has at most 16 PATH entries.");
          }
          const roots = Array.from({ length: rootCount }, () => nextMockFolder("root"));
          settings.toolchain_profiles.push({
            name,
            platform: MOCK_PLATFORM,
            read_only_roots: roots,
            path_entries: Array.from(
              { length: pathCount },
              (_, index) => `${roots[0] ?? "/opt/mock"}/bin${index + 1}`,
            ),
            environment_names: environment.map((entry) =>
              typeof entry === "object" && entry !== null && "name" in entry
                ? String(entry.name)
                : "",
            ),
          });
        });
      case "settings_remove_toolchain_profile":
        return changeSettings(args, () => {
          const index = settings.toolchain_profiles.findIndex((p) => p.name === str(args, "name"));
          if (index < 0) {
            fail("control", "unknown profile");
          }
          const [removed] = settings.toolchain_profiles.splice(index, 1);
          // Like core (ADR-0107): every assignment ends and the grants it alone needed go.
          for (const workspace of settings.workspaces) {
            if (removed !== undefined && mockAssigned(workspace, removed.name)) {
              mockUnassign(workspace, removed);
            }
          }
        });
      // Like core (ADR-0107): assigning grants the workspace's container each root it cannot read
      // yet; the system confirmation that names them is accepted here.
      case "settings_assign_toolchain_profile":
        return changeSettings(args, () => {
          const workspace = mockWorkspace(args);
          const profile = mockProfile(str(args, "name"));
          if (mockAssigned(workspace, profile.name)) {
            fail("control", "the profile is already assigned to this workspace");
          }
          for (const root of profile.read_only_roots) {
            if (mockGrantNeed(workspace.workspace_id, root) === "needs_grant") {
              mockGrant(workspace.workspace_id, root);
            }
          }
          workspace.profiles.push(profile.name);
        });
      case "settings_unassign_toolchain_profile":
        return changeSettings(args, () => {
          const workspace = mockWorkspace(args);
          const profile = mockProfile(str(args, "name"));
          if (!mockAssigned(workspace, profile.name)) {
            fail("control", "the profile is not assigned to this workspace");
          }
          mockUnassign(workspace, profile);
        });
      case "settings_add_package_profile":
        return changeSettings(args, () => {
          const name = str(args, "name");
          const origins = Array.isArray(args.origins) ? args.origins.map(String) : [];
          if (origins.length < 1 || origins.length > 8) {
            fail("invalid_request", "A profile needs 1 to 8 origins.");
          }
          if (settings.package_profiles.some((p) => p.name.toLowerCase() === name.toLowerCase())) {
            fail(
              "control",
              "a package-network profile with this name already exists; remove it first",
            );
          }
          for (const origin of origins) {
            if (!/^https?:\/\/[^/\s]+(\/\S*)?$/.test(origin)) {
              fail("control", "an origin must be an http or https URL without credentials");
            }
          }
          settings.package_profiles.push({ name, origins, credentials_bound: 0 });
        });
      case "settings_remove_package_profile":
        return changeSettings(args, () => {
          const index = settings.package_profiles.findIndex((p) => p.name === str(args, "name"));
          if (index < 0) {
            fail("control", "unknown package-network profile");
          }
          settings.package_profiles.splice(index, 1);
        });
      // Like core: the pattern is parsed first and a refusal carries its stable code; the system
      // confirmation is accepted here. A list holds at most 32 patterns and no duplicate.
      case "settings_add_web_fetch_host":
        return changeSettings(args, () => {
          const workspace = mockWorkspace(args);
          const pattern = mockWebHostPattern(str(args, "pattern"));
          if (workspace.web_fetch_hosts.includes(pattern)) {
            fail("web_host_duplicate", "the workspace already lists this host pattern");
          }
          if (workspace.web_fetch_hosts.length >= MOCK_WEB_HOSTS_MAX) {
            fail("web_host_too_many", "a workspace may list at most 32 host patterns");
          }
          workspace.web_fetch_hosts.push(pattern);
        });
      case "settings_remove_web_fetch_host":
        return changeSettings(args, () => {
          const workspace = mockWorkspace(args);
          const pattern = mockWebHostPattern(str(args, "pattern"));
          const index = workspace.web_fetch_hosts.indexOf(pattern);
          if (index < 0) {
            fail("web_host_not_listed", "the workspace does not list this host pattern");
          }
          workspace.web_fetch_hosts.splice(index, 1);
        });
      case "settings_grant_container_folder":
        return changeSettings(args, () => {
          const workspaceId = str(args, "workspace_id");
          const workspace = mockWorkspace({ workspace_id: workspaceId });
          const access = str(args, "access");
          if (access !== "read_execute" && access !== "read_write") {
            fail("invalid_argument", "Bad access.");
          }
          settings.container_grants.push({
            grant_id: mockGrantId(),
            workspace_id: workspaceId,
            // As in Rust (ADR-0104): read and write goes on the workspace's own folder, with no
            // folder dialog; read and execute goes on a folder the dialog would choose.
            directory: access === "read_write" ? workspace.folder_display : nextMockFolder("grant"),
            access,
            state: "active",
          });
        });
      case "settings_revoke_container_grant":
        return changeSettings(args, () => {
          const index = settings.container_grants.findIndex(
            (grant) => grant.grant_id === str(args, "grant_id"),
          );
          if (index < 0) {
            fail("control", "unknown container grant");
          }
          settings.container_grants.splice(index, 1);
        });
      // The mock stands in for toolchain discovery (ADR-0103): a fixed list of sample proposals,
      // checked against the recorded names. Every confirmation is accepted.
      case "settings_discover_toolchains":
        return mockDiscovery(optionalWorkspace(args));
      case "settings_add_discovered_profiles": {
        const chosen = mockSelection(args);
        const workspaceId = optionalWorkspace(args);
        if (args.revision !== settings.revision) {
          fail("stale_revision", "The settings changed since this window read them.");
        }
        for (const proposal of chosen) {
          if (mockRecorded(proposal.name)) {
            fail("control", `${proposal.name}: a profile with this name already exists`);
          }
        }
        for (const proposal of chosen) {
          settings.toolchain_profiles.push({
            name: proposal.name,
            platform: MOCK_PLATFORM,
            read_only_roots: [...proposal.roots],
            path_entries: [...proposal.path_entries],
            environment_names: proposal.environment.map((variable) => variable.name),
          });
          settings.revision += 1;
        }
        // Like core (ADR-0107): with a workspace, the same confirmation assigns them and grants
        // the roots its container cannot read yet.
        if (workspaceId !== null) {
          const workspace = mockWorkspace({ workspace_id: workspaceId });
          for (const proposal of chosen) {
            for (const root of proposal.roots) {
              if (mockGrantNeed(workspaceId, root) === "needs_grant") {
                mockGrant(workspaceId, root);
                settings.revision += 1;
              }
            }
            workspace.profiles.push(proposal.name);
            settings.revision += 1;
          }
        }
        return mockDiscovery(workspaceId);
      }
      // The mock stands in for the browser step: a sign-in succeeds at once (ADR-0093).
      case "settings_sign_in_chatgpt":
        return changeSettings(args, () => {
          const name = str(args, "name");
          const known = settings.provider_profiles.find((p) => p.name === name);
          if (known !== undefined && (known.kind !== "openai" || known.auth !== "subscription")) {
            fail("control", "only an openai subscription profile can sign in with ChatGPT");
          }
          if (known?.credential_bound === true) {
            fail("control", "this profile is already signed in; sign out first");
          }
          const profile = known ?? {
            name,
            kind: "openai",
            auth: "subscription",
            endpoint: "https://api.openai.com/v1",
            credential_bound: false,
            selectable: false,
            active: false,
            account_label: null,
          };
          profile.credential_bound = true;
          profile.selectable = true;
          profile.account_label = "person@example.test";
          if (known === undefined) {
            settings.provider_profiles.push(profile);
          }
        });
      case "settings_cancel_chatgpt_sign_in":
        return null;
      case "settings_sign_out_chatgpt": {
        const overview = changeSettings(args, () => {
          const name = str(args, "name");
          const profile = settings.provider_profiles.find((p) => p.name === name);
          if (
            profile === undefined ||
            !profile.credential_bound ||
            profile.auth !== "subscription"
          ) {
            fail("credential", "not_signed_in: this profile is not signed in with ChatGPT");
          }
          profile.credential_bound = false;
          profile.selectable = false;
          profile.account_label = null;
        });
        return { overview, revoked: true };
      }
      case "settings_remove_connection":
        return changeSettings(args, () => {
          const name = str(args, "name");
          const index = settings.provider_profiles.findIndex((p) => p.name === name);
          const profile = settings.provider_profiles[index];
          if (profile === undefined) {
            fail("control", "unknown provider profile");
          }
          if (profile.auth === "subscription" && profile.credential_bound) {
            fail("control", "this profile is signed in with ChatGPT; sign out first");
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
      compact_threshold_step: 8_000,
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

  /** The efforts a session's connection takes: all, or none where core's endpoint rule takes none. */
  function acceptedEfforts(sessionId: string | null): ReasoningEffort[] {
    return endpointTakesEffort(effectiveConnection(sessionId).endpoint)
      ? [...REASONING_EFFORTS]
      : [];
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
      reasoning_effort: choice?.effort ?? null,
      reasoning_efforts: acceptedEfforts(sessionId),
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

  /** The optional `workspace_id` of a discovery request; a registered one or `null`. */
  function optionalWorkspace(args: Record<string, unknown>): string | null {
    if (args.workspace_id === null || args.workspace_id === undefined) {
      return null;
    }
    return mockWorkspace(args).workspace_id;
  }

  function mockRecorded(name: string): boolean {
    return settings.toolchain_profiles.some((p) => p.name.toLowerCase() === name.toLowerCase());
  }

  function mockProfile(name: string): ToolchainProfileView {
    const found = settings.toolchain_profiles.find(
      (p) => p.name.toLowerCase() === name.toLowerCase(),
    );
    if (found === undefined) {
      fail("control", "unknown profile");
    }
    return found;
  }

  function mockAssigned(workspace: WorkspaceSettingsView, name: string): boolean {
    return workspace.profiles.some((p) => p.toLowerCase() === name.toLowerCase());
  }

  function mockGrantId(): string {
    return `00000000-0000-4000-8000-${String(settings.container_grants.length + 1).padStart(12, "0")}`;
  }

  /** Records one active read-execute grant, as a confirmed grant ends up in core. */
  function mockGrant(workspaceId: string, directory: string): void {
    settings.container_grants.push({
      grant_id: mockGrantId(),
      workspace_id: workspaceId,
      directory,
      access: "read_execute",
      state: "active",
    });
  }

  /**
   * Like core (ADR-0107): the assignment ends, and the workspace's read-execute grants on the
   * profile's roots that no other assigned profile names are revoked.
   */
  function mockUnassign(workspace: WorkspaceSettingsView, profile: ToolchainProfileView): void {
    workspace.profiles = workspace.profiles.filter(
      (name) => name.toLowerCase() !== profile.name.toLowerCase(),
    );
    const kept = new Set(
      settings.toolchain_profiles
        .filter((other) => mockAssigned(workspace, other.name))
        .flatMap((other) => other.read_only_roots),
    );
    settings.container_grants = settings.container_grants.filter(
      (grant) =>
        grant.workspace_id !== workspace.workspace_id ||
        grant.access !== "read_execute" ||
        !profile.read_only_roots.includes(grant.directory) ||
        kept.has(grant.directory),
    );
  }

  /** Home folders need a grant; `/usr/local` needs an administrator; the rest is readable. */
  function mockGrantNeed(workspaceId: string | null, folder: string): RootGrantNeed {
    if (workspaceId === null || !settings.container_grants_supported) {
      return "not_applicable";
    }
    if (
      settings.container_grants.some(
        (grant) => grant.workspace_id === workspaceId && folder.startsWith(grant.directory),
      )
    ) {
      return "readable";
    }
    if (folder.startsWith("/home/")) {
      return "needs_grant";
    }
    return folder.startsWith("/usr/local/") ? "needs_administrator" : "readable";
  }

  function mockDiscovery(workspaceId: string | null): ToolchainDiscovery {
    const proposals: ToolchainProposalView[] = MOCK_PROPOSALS.map((sample, index) => {
      const recorded = mockRecorded(sample.name);
      return {
        digest: mockDigest(index),
        family: sample.family,
        family_label: sample.family_label,
        name: sample.name,
        version: sample.version,
        read_only_roots: sample.roots.map((folder) => ({
          folder,
          grant: mockGrantNeed(workspaceId, folder),
        })),
        path_entries: [...sample.path_entries],
        environment: sample.environment.map((variable) => ({ ...variable })),
        notes: [...sample.notes],
        refusal: recorded ? "a profile with this name already exists; remove it first" : null,
        recorded,
      };
    });
    const active =
      workspaceId === null || !settings.container_grants_supported
        ? null
        : settings.container_grants.some(
            (grant) =>
              grant.workspace_id === workspaceId &&
              grant.access === "read_write" &&
              grant.state === "active",
          );
    return {
      overview: overviewView(),
      workspace_id: workspaceId,
      workspace_grant_active: active,
      proposals,
      omitted: 0,
    };
  }

  /** The sample proposals a request selects, as Rust checks a selection: 1 to 8 known digests. */
  function mockSelection(args: Record<string, unknown>): MockProposal[] {
    const digests = Array.isArray(args.digests) ? args.digests.map(String) : [];
    if (
      digests.length < 1 ||
      digests.length > 8 ||
      new Set(digests).size !== digests.length ||
      !digests.every((digest) => /^[0-9a-f]{64}$/.test(digest))
    ) {
      fail("invalid_request", "Select 1 to 8 proposals at a time.");
    }
    return digests.map((digest) => {
      const found = MOCK_PROPOSALS.find((_, at) => mockDigest(at) === digest);
      if (found === undefined) {
        fail("discovery_changed", "The installed toolchains or the proposals changed.");
      }
      return found;
    });
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
          web_fetch: "prompt",
          web_fetch_hosts: ["docs.example.com", "*.example.org"],
          write_tools: {
            edit: "allow",
            multi_edit: "allow",
            create: "allow",
            rename: "allow",
            delete: "prompt",
            create_directory: "allow",
          },
          profiles: [],
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
          web_fetch: "deny",
          web_fetch_hosts: [],
          write_tools: {
            edit: "allow",
            multi_edit: "allow",
            create: "allow",
            rename: "allow",
            delete: "allow",
            create_directory: "allow",
          },
          profiles: [],
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
          account_label: null,
        },
        {
          name: "ollama",
          kind: "ollama_local",
          auth: "none",
          endpoint: "http://127.0.0.1:11434/v1",
          credential_bound: false,
          selectable: true,
          active: false,
          account_label: null,
        },
      ],
      toolchain_profiles: [
        {
          name: "rust",
          platform: MOCK_PLATFORM,
          read_only_roots: ["/home/dev/.rustup"],
          path_entries: [],
          environment_names: ["RUSTUP_HOME"],
        },
      ],
      profile_platform: MOCK_PLATFORM,
      package_profiles: [],
      container_grants: [],
      container_grants_supported: true,
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
  return isLocalEndpointText(endpoint);
}

/**
 * The mock's copy of core's effort rule (`ModelEndpoint::takes_reasoning_effort`, ADR-0102), in
 * core's parse order. The endpoint decides, not the profile kind: a fixed endpoint, with or without
 * one trailing `/`, takes an effort only where its provider documents one, even when typed as an
 * `openai_compatible` address; any other typed address and a loopback endpoint take one; anything
 * else takes none.
 */
export function endpointTakesEffort(endpoint: string): boolean {
  for (const [kind, fixed] of Object.entries(FIXED_ENDPOINTS) as [FixedHostedKind, string][]) {
    if (endpoint === fixed || endpoint === `${fixed}/`) {
      return EFFORT_KINDS.has(kind);
    }
  }
  return isCustomEndpoint(endpoint) || isLocalEndpoint(endpoint);
}

/**
 * The mock's copy of core's typed-endpoint rule for `openai_compatible` (`validate_custom_url`,
 * ADR-0092), step by step. The browser's `URL` follows the same WHATWG standard as the `url` crate,
 * so "written exactly as the parser writes it" carries over: a `.` or `..` segment, an upper-case
 * host, a default port, or a non-ASCII host is rewritten by the parser and therefore refused.
 */
export function isCustomEndpoint(endpoint: string): boolean {
  if (
    new TextEncoder().encode(endpoint).length > MAX_ENDPOINT_BYTES ||
    !endpoint.startsWith("https://") ||
    Array.from(endpoint).some((c) => /[\p{Cc}\s%@?#\\]/u.test(c))
  ) {
    return false;
  }
  let url: URL;
  try {
    url = new URL(endpoint);
  } catch {
    return false;
  }
  const written = url.href;
  return (
    url.protocol === "https:" &&
    url.hostname !== "" &&
    // Port 0 reaches nothing; core refuses it as the loopback rule does.
    url.port !== "0" &&
    // A server on this computer uses a local kind, as core requires.
    !namesThisComputer(url.hostname) &&
    (written === endpoint || written === `${endpoint}/`) &&
    !url.pathname.includes("//")
  );
}

// Mirrors core's `is_this_computer`: `localhost` names, and loopback or unspecified addresses,
// including IPv4-mapped IPv6. `URL` writes IPv4 in dotted decimal and IPv6 compressed.
function namesThisComputer(hostname: string): boolean {
  // A name with trailing root dots (`localhost.`) resolves the same, so they are dropped first.
  let name = hostname;
  while (name.endsWith(".")) {
    name = name.slice(0, -1);
  }
  if (name === "localhost" || name.endsWith(".localhost")) {
    return true;
  }
  const octets = hostname.split(".");
  if (
    octets.length === 4 &&
    octets.every((octet) => octet !== "" && !Number.isNaN(Number(octet)))
  ) {
    return octets[0] === "127" || hostname === "0.0.0.0";
  }
  if (hostname === "[::1]" || hostname === "[::]" || hostname === "[::ffff:0:0]") {
    return true;
  }
  const mapped = "[::ffff:";
  const group = hostname.startsWith(mapped) ? hostname.slice(mapped.length).split(":")[0] : "";
  return group !== undefined && group.length === 4 && group.startsWith("7f");
}

function isLocalEndpointText(endpoint: string): boolean {
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
