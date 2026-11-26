import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setInvoker } from "../src/ipc/client";
import type { ConnectionStatus, ReasoningEffort } from "../src/ipc/types";
import { type MainApp, startMain } from "../src/main/app";
import { EFFORT_NOT_TAKEN } from "../src/main/effort_select";
import {
  ALL_EFFORTS,
  buttonByText,
  choose,
  connectionStatus,
  type FakeIpc,
  type Handler,
  installFakeIpc,
  mainHandlers,
  modelSelect,
  mountRoot,
  page,
  rejection,
  SETTINGS,
  session,
  settle,
  tree,
  userMessage,
} from "./helpers";

// The model box's Effort dropdown chooses one session's reasoning effort (ADR-0102).

const LMS = "http://127.0.0.1:1234/v1";
const ANTHROPIC = "https://api.anthropic.com/v1";

const a = session({ session_id: "s-a", title: "Alpha" });
const b = session({ session_id: "s-b", title: "Beta" });

let app: MainApp | null = null;
let root: HTMLElement;
let ipc: FakeIpc;

/** What each session has chosen, as Rust records it. */
interface Choice {
  profile: string | null;
  effort: ReasoningEffort | null;
  /** A record that still follows the default connection, as an effort alone creates. */
  follow?: boolean;
}

const PROFILES = [
  { name: "lms", kind: "lm_studio", endpoint: LMS },
  { name: "claude", kind: "anthropic", endpoint: ANTHROPIC },
];

/** A stateful fake of the per-session commands; the Anthropic connection takes no effort. */
function backend(initial: Record<string, Choice> = {}, selectable = true): Record<string, Handler> {
  const choices = new Map<string, Choice>(Object.entries(initial));
  const find = (id: string): Choice => choices.get(id) ?? { profile: null, effort: null };
  const status = (id: string): ConnectionStatus => {
    const choice = find(id);
    const profile = PROFILES.find((each) => each.name === choice.profile);
    return connectionStatus({
      connection: {
        endpoint: profile?.endpoint ?? LMS,
        source: "active",
        profile: profile?.name ?? null,
        kind: profile?.kind ?? null,
        selectable,
      },
      // Like Rust, a record that follows the default reports the default origin.
      origin: !selectable ? "launch" : choices.has(id) && !choice.follow ? "session" : "default",
      reasoning_effort: choice.effort,
      reasoning_efforts: profile?.kind === "anthropic" ? [] : [...ALL_EFFORTS],
      choices: selectable
        ? PROFILES.map((each) => ({ ...each, active: each.name === choice.profile }))
        : [],
    });
  };
  return {
    session_tree: () => tree([{ id: "g1", folder: "C:\\work", sessions: [a, b] }]),
    session_transcript: (args) => page(args.session_id === "s-b" ? b : a, [userMessage(1, "hi")]),
    connection_status: (args) => status((args.session_id as string | null) ?? ""),
    // Like Rust, a provider choice clears the effort with the model.
    connection_select: (args) => {
      const id = args.session_id as string;
      choices.set(id, { profile: args.profile as string | null, effort: null });
      return status(id);
    },
    // Like Rust, an effort alone creates a record that follows the default connection.
    session_effort_select: (args) => {
      const id = args.session_id as string;
      const existing = choices.get(id) ?? { profile: null, effort: null, follow: true };
      choices.set(id, { ...existing, effort: args.reasoning_effort as ReasoningEffort | null });
      return status(id);
    },
    chat_send: () => ({ session_id: "s-a", state: "running", outcome: null, error: null }),
    chat_status: () => ({ session_id: "s-a", state: "running", outcome: null, error: null }),
  };
}

async function start(handlers: Record<string, Handler>): Promise<void> {
  ipc = installFakeIpc(mainHandlers(handlers));
  root = mountRoot();
  app = startMain(root);
  await settle();
}

async function open(id: string): Promise<void> {
  const node = Array.from(root.querySelectorAll<HTMLElement>("[data-session-id]")).find(
    (item) => item.dataset.sessionId === id,
  );
  node?.click();
  await settle();
}

function effortSelect(): HTMLSelectElement {
  return root.querySelector('.model-box select[aria-label="Effort"]') as HTMLSelectElement;
}

function boxProvider(): HTMLSelectElement {
  return root.querySelector('.model-box select[aria-label="Provider"]') as HTMLSelectElement;
}

function effortLabels(): string[] {
  return Array.from(effortSelect().options).map((option) => option.textContent ?? "");
}

function note(): HTMLElement | undefined {
  return Array.from(root.querySelectorAll<HTMLElement>(".model-box-status [role=status]")).find(
    (node) => node.textContent === EFFORT_NOT_TAKEN,
  );
}

async function send(): Promise<void> {
  const box = root.querySelector(".message-box") as HTMLTextAreaElement;
  box.value = "hello";
  box.dispatchEvent(new Event("input", { bubbles: true }));
  buttonByText(root, "Send").click();
  await settle();
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  app?.dispose();
  app = null;
  setInvoker(null);
  vi.useRealTimers();
});

describe("effort dropdown", () => {
  it("sits under Model and offers Default and every effort the connection takes", async () => {
    await start(backend());
    await open("s-a");
    const box = root.querySelector(".model-box") as HTMLElement;
    const labels = Array.from(box.querySelectorAll(".model-box-field > span")).map(
      (node) => node.textContent,
    );
    expect(labels).toEqual(["Provider", "Model", "Effort"]);
    expect(effortSelect().closest(".model-box")).toBe(box);
    expect(effortLabels()).toEqual([
      "Default",
      "None",
      "Minimal",
      "Low",
      "Medium",
      "High",
      "Extra high",
      "Max",
    ]);
    expect(Array.from(effortSelect().options).map((option) => option.value)).toEqual([
      "",
      ...ALL_EFFORTS,
    ]);
    expect(effortSelect().value).toBe("");
    expect(effortSelect().disabled).toBe(false);
    expect(note()?.hidden).toBe(true);
  });

  it("records a chosen effort for the session, and Default records none", async () => {
    await start(backend());
    await open("s-a");
    choose(effortSelect(), "xhigh");
    await settle();
    expect(ipc.callsTo("session_effort_select").map((call) => call.args)).toEqual([
      { session_id: "s-a", reasoning_effort: "xhigh" },
    ]);
    expect(effortSelect().value).toBe("xhigh");
    expect(effortSelect().selectedOptions[0]?.textContent).toBe("Extra high");

    choose(effortSelect(), "");
    await settle();
    expect(ipc.callsTo("session_effort_select")[1]?.args).toEqual({
      session_id: "s-a",
      reasoning_effort: null,
    });
    expect(effortSelect().value).toBe("");
    // An effort choice is not a model choice: the model list is not read again.
    expect(ipc.callsTo("chat_models")).toHaveLength(1);
    expect(ipc.callsTo("session_model_select")).toHaveLength(0);
  });

  it("shows each session's recorded effort when it opens", async () => {
    await start(
      backend({
        "s-a": { profile: "lms", effort: "low" },
        "s-b": { profile: "lms", effort: "max" },
      }),
    );
    await open("s-a");
    expect(effortSelect().value).toBe("low");
    await open("s-b");
    expect(effortSelect().value).toBe("max");
    await open("s-a");
    expect(effortSelect().value).toBe("low");
    expect(ipc.callsTo("session_effort_select")).toHaveLength(0);
  });

  it("sends the effort the box shows, and none for Default", async () => {
    await start(backend({ "s-a": { profile: "lms", effort: "medium" } }));
    await open("s-a");
    await send();
    expect(ipc.callsTo("chat_send")[0]?.args).toMatchObject({
      settings: { endpoint: LMS, model: SETTINGS.model, reasoning_effort: "medium" },
    });
  });

  it("sends no effort by default", async () => {
    await start(backend());
    await open("s-a");
    await send();
    const settings = ipc.callsTo("chat_send")[0]?.args.settings as Record<string, unknown>;
    expect(settings.reasoning_effort).toBeNull();
  });

  it("is disabled with a note when the connection takes no effort, and sends none", async () => {
    await start(backend({ "s-a": { profile: "claude", effort: null } }));
    await open("s-a");
    expect(effortSelect().disabled).toBe(true);
    expect(effortLabels()).toEqual(["Default"]);
    expect(note()?.hidden).toBe(false);
    // The model is still chosen as before.
    expect(modelSelect(root).disabled).toBe(false);
    await send();
    const settings = ipc.callsTo("chat_send")[0]?.args.settings as Record<string, unknown>;
    expect(settings.reasoning_effort).toBeNull();
  });

  it("clears the effort when the operator chooses another provider", async () => {
    await start(backend({ "s-a": { profile: "lms", effort: "high" } }));
    await open("s-a");
    expect(effortSelect().value).toBe("high");
    choose(boxProvider(), "claude");
    await settle();
    expect(effortSelect().value).toBe("");
    expect(effortSelect().disabled).toBe(true);
    expect(note()?.hidden).toBe(false);
  });

  it("is disabled while no session is open and while a turn runs", async () => {
    await start(backend());
    expect(effortSelect().disabled).toBe(true);
    // No session is open, so the note about the connection is not shown either.
    expect(note()?.hidden).toBe(true);
    await open("s-a");
    expect(effortSelect().disabled).toBe(false);
    await send();
    expect(effortSelect().disabled).toBe(true);
  });

  it("stays enabled when a launch argument fixed the connection", async () => {
    await start(backend({}, false));
    await open("s-a");
    expect(boxProvider().disabled).toBe(true);
    expect(effortSelect().disabled).toBe(false);
  });

  // Reopening resets the model list, and the reopened session's status read is dropped while the
  // save is pending; the save's own answer must then load the list.
  it("loads the model list when an effort save settles after the session was reopened", async () => {
    const handlers = backend({ "s-a": { profile: "lms", effort: null } });
    const record = handlers.session_effort_select;
    // A no-op until the held save replaces it, so its type stays a function for the checker.
    let answer: () => void = () => undefined;
    await start({
      ...handlers,
      session_effort_select: (args) =>
        new Promise((resolve) => {
          answer = () => resolve(record?.(args));
        }),
    });
    await open("s-a");
    choose(effortSelect(), "high");
    await settle();
    await open("s-b");
    await open("s-a");
    const loads = (): number =>
      ipc.callsTo("chat_models").filter((call) => call.args.session_id === "s-a").length;
    const before = loads();
    expect(ipc.callsTo("session_effort_select")).toHaveLength(1);

    answer();
    await settle();

    expect(loads()).toBe(before + 1);
    expect(Array.from(modelSelect(root).options).map((option) => option.value)).toContain(
      SETTINGS.model,
    );
    expect(effortSelect().value).toBe("high");
  });

  it("shows a refused save and returns to the recorded effort", async () => {
    const handlers = backend({ "s-a": { profile: "lms", effort: "low" } });
    await start({
      ...handlers,
      session_effort_select: () =>
        rejection("invalid_request", "This connection does not take a reasoning effort."),
    });
    await open("s-a");
    choose(effortSelect(), "max");
    await settle();
    expect(root.querySelector(".model-box-status")?.textContent).toContain("invalid_request");
    expect(effortSelect().value).toBe("low");
  });
});
