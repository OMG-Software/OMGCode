import type { SessionSummary, TranscriptEntry } from "../ipc/types";

/** Present in every mock module so a production bundle can be checked for leaked fixtures. */
export const OMGCODE_MOCK_FIXTURE_MARKER = "OMGCODE_MOCK_FIXTURE_MARKER";

/** Strings that must render as inert text. */
export const HOSTILE = {
  img: "<img src=x onerror=alert(1)>",
  script: "</script>",
  rtl: "\u202Eexe.txt",
  long: "A".repeat(5000),
  link: "javascript:alert(1)",
} as const;

/** A reply that exercises the Markdown renderer (ADR-0067), with hostile text in each part. */
const MARKDOWN_REPLY = [
  "To run this web server, you can execute the following command in your terminal " +
    "(assuming you are in the same directory as `server.py`):",
  "",
  "```bash",
  "python3 server.py",
  "```",
  "",
  "## Notes",
  "",
  "1. The server listens on **port 8000** by default.",
  "2. Stop it with *Ctrl-C*.",
  "   - Logs go to `stderr`.",
  "",
  `> See [the docs](${HOSTILE.link}) and ${HOSTILE.img}`,
  "",
  "| Flag | Meaning |",
  "| :-- | :-- |",
  "| `--port` | Listening port |",
  "| `--bind` | Address to bind |",
].join("\n");

export interface FixtureGroup {
  group_id: string;
  folder_display: string;
  session_ids: string[];
}

export interface FixtureSession {
  summary: SessionSummary;
  entries: TranscriptEntry[];
  incomplete_tail: boolean;
}

function sid(n: number): string {
  return `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
}

function iso(base: number, minutesAgo: number): string {
  return new Date(base - minutesAgo * 60_000).toISOString();
}

function summary(
  n: number,
  title: string,
  base: number,
  minutesAgo: number,
  overrides: Partial<SessionSummary> = {},
): SessionSummary {
  return {
    session_id: sid(n),
    title,
    created_at: iso(base, minutesAgo + 600),
    updated_at: iso(base, minutesAgo),
    status: "idle",
    archived: false,
    event_count: 0,
    invalid_code: null,
    ...overrides,
  };
}

function fullTranscript(base: number): TranscriptEntry[] {
  let seq = 0;
  const at = (): { seq: number; timestamp: string } => {
    seq += 1;
    return { seq, timestamp: iso(base, 900 - seq) };
  };
  // Core records a request manifest only inside its turn, so it follows the turn start.
  const entries: TranscriptEntry[] = [
    { ...at(), kind: "turn_started", request_id: "r1" },
    {
      ...at(),
      kind: "request_manifest",
      request_id: "r1",
      model: "qwen2.5-coder",
      prompt_version: "v3",
      tools: ["workspace_read_file", "workspace_list_dir"],
    },
    {
      ...at(),
      kind: "message",
      role: "user",
      source: "chat",
      text: `Please look at this: ${HOSTILE.img} and ${HOSTILE.script}\nLine two ${HOSTILE.rtl}`,
      call_id: null,
    },
    {
      ...at(),
      kind: "tool_requested",
      request_id: "r1",
      call_id: "c1",
      tool: "workspace_read_file",
      arguments: '{"path":"src/lib.rs","note":"</script><img src=x onerror=alert(1)>"}',
    },
    {
      ...at(),
      kind: "tool_completed",
      request_id: "r1",
      call_id: "c1",
      outcome: "succeeded",
      code: null,
      output_bytes: 2048,
    },
    {
      ...at(),
      kind: "message",
      role: "tool",
      source: "tool",
      text: "fn main() {}\n",
      call_id: "c1",
    },
    {
      ...at(),
      kind: "tool_requested",
      request_id: "r1",
      call_id: "c2",
      tool: "workspace_run_command",
      arguments: '{"program":"cargo","args":["test"]}',
    },
    {
      ...at(),
      kind: "tool_completed",
      request_id: "r1",
      call_id: "c2",
      outcome: "denied",
      code: "approval_required",
      output_bytes: 0,
    },
    {
      ...at(),
      kind: "message",
      role: "assistant",
      source: "chat",
      text: `Reply with a link ${HOSTILE.link} and a token ${HOSTILE.long}`,
      call_id: null,
    },
    {
      ...at(),
      kind: "message",
      role: "assistant",
      source: "chat",
      text: MARKDOWN_REPLY,
      call_id: null,
    },
    { ...at(), kind: "turn_completed", request_id: "r1" },
    {
      ...at(),
      kind: "context_compacted",
      summary: `The user asked about ${HOSTILE.img}. ${HOSTILE.link}`,
      compacted_messages: 14,
      model: "qwen2.5-coder",
    },
    { ...at(), kind: "turn_started", request_id: "r2" },
    { ...at(), kind: "turn_failed", request_id: "r2", code: "provider_unreachable" },
    { ...at(), kind: "session_archived" },
    { ...at(), kind: "session_restored" },
  ];
  for (let i = 0; i < 120; i += 1) {
    entries.push(
      {
        ...at(),
        kind: "message",
        role: "user",
        source: "chat",
        text: `Filler question ${i}: how does paging behave?`,
        call_id: null,
      },
      {
        ...at(),
        kind: "message",
        role: "assistant",
        source: "chat",
        text: `Filler answer ${i}: the transcript loads 200 entries per page.`,
        call_id: null,
      },
    );
  }
  return entries;
}

function shortTranscript(base: number, text: string, open: boolean): TranscriptEntry[] {
  const entries: TranscriptEntry[] = [
    { seq: 1, timestamp: iso(base, 50), kind: "turn_started", request_id: "r1" },
    {
      seq: 2,
      timestamp: iso(base, 49),
      kind: "message",
      role: "user",
      source: "chat",
      text,
      call_id: null,
    },
  ];
  if (!open) {
    entries.push(
      {
        seq: 3,
        timestamp: iso(base, 48),
        kind: "message",
        role: "assistant",
        source: "chat",
        text: "Understood.",
        call_id: null,
      },
      { seq: 4, timestamp: iso(base, 47), kind: "turn_completed", request_id: "r1" },
    );
  }
  return entries;
}

/** Builds the mock sessions and groups relative to `base` (a millisecond timestamp). */
export function buildFixtures(base: number): {
  groups: FixtureGroup[];
  sessions: FixtureSession[];
} {
  const groups: FixtureGroup[] = [
    {
      group_id: "g1",
      folder_display: "C:\\work\\omgcode",
      session_ids: [sid(1), sid(2), sid(3), sid(4)],
    },
    {
      group_id: "g2",
      folder_display: `~/src/${HOSTILE.img}`,
      session_ids: [sid(5), sid(6), sid(7), sid(8)],
    },
    {
      group_id: "g3",
      folder_display: `/home/dev/${"x".repeat(200)}`,
      session_ids: [sid(9), sid(10), sid(11), sid(12)],
    },
  ];
  const full = fullTranscript(base);
  const make = (
    n: number,
    title: string,
    minutes: number,
    overrides: Partial<SessionSummary>,
    entries: TranscriptEntry[],
    incomplete = false,
  ): FixtureSession => ({
    summary: summary(n, title, base, minutes, { event_count: entries.length, ...overrides }),
    entries,
    incomplete_tail: incomplete,
  });
  const small = (text: string, open = false): TranscriptEntry[] =>
    shortTranscript(base, text, open);
  const sessions: FixtureSession[] = [
    make(1, "Refactor the session index", 3, {}, full),
    make(2, "Fix a flaky CI test", 1, { status: "running" }, small("Why does the test flake?")),
    make(
      3,
      "Add a frozen index",
      20,
      { status: "turn_open" },
      small("Start the migration", true),
      true,
    ),
    make(4, "Old spike", 3000, { archived: true }, small("Spike notes")),
    make(5, `${HOSTILE.script} cleanup`, 90, {}, small(`Remove ${HOSTILE.img}`)),
    make(6, "Migration notes", 400, { status: "busy" }, []),
    make(
      7,
      "Unreadable journal",
      800,
      { status: "invalid", invalid_code: "journal_hash_mismatch" },
      [],
    ),
    make(8, "Archived notes", 5000, { archived: true }, small("Archived content")),
    make(9, `${HOSTILE.rtl} review`, 30, {}, small("Review the token please")),
    make(10, HOSTILE.link, 200, {}, small(`See ${HOSTILE.link}`)),
    make(11, `Long token ${HOSTILE.long}`, 1500, {}, small("Layout check")),
    make(12, "Archived review", 7000, { archived: true }, small("Done")),
  ];
  return { groups, sessions };
}
