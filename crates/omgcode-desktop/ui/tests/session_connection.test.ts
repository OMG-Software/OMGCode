import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setInvoker } from "../src/ipc/client";
import type { ChatModels, ConnectionStatus } from "../src/ipc/types";
import { type MainApp, startMain } from "../src/main/app";
import { SESSION_CHOICES_UNREADABLE } from "../src/main/composer";
import { CONNECTION_UNCHANGED } from "../src/main/connection_state";
import { CLOUD_WARNING, HOSTED_SUFFIX, HOSTED_WARNING } from "../src/main/model_picker";
import {
  buttonByLabel,
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

// The provider and the model belong to the open session (ADR-0071 decisions 8 to 11).

const LMS = "http://127.0.0.1:1234/v1";
const OLLAMA = "http://127.0.0.1:11434/v1";
const CLOUD = "https://ollama.com/v1";

const a = session({ session_id: "s-a", title: "Alpha" });
const b = session({ session_id: "s-b", title: "Beta" });

let app: MainApp | null = null;
let root: HTMLElement;
let ipc: FakeIpc;

/** What each session has chosen, as Rust records it. */
interface Choice {
  profile: string | null;
  model: string;
}

const PROFILES = [
  { name: "lms", kind: "lm_studio", endpoint: LMS },
  { name: "ollama", kind: "ollama_local", endpoint: OLLAMA },
  { name: "cloud", kind: "ollama_cloud", endpoint: CLOUD },
];

/** A stateful fake of the per-session commands. */
function backend(initial: Record<string, Choice> = {}, selectable = true): Record<string, Handler> {
  const choices = new Map<string, Choice>(Object.entries(initial));
  const find = (id: string): Choice => choices.get(id) ?? { profile: null, model: SETTINGS.model };
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
      origin: !selectable ? "launch" : choices.has(id) ? "session" : "default",
      model: choice.model,
      choices: selectable
        ? PROFILES.map((each) => ({ ...each, active: each.name === choice.profile }))
        : [],
    });
  };
  return {
    session_tree: () => tree([{ id: "g1", folder: "C:\\work", sessions: [a, b] }]),
    session_transcript: (args) => page(args.session_id === "s-b" ? b : a, [userMessage(1, "hi")]),
    connection_status: (args) => status((args.session_id as string | null) ?? ""),
    connection_select: (args) => {
      const id = args.session_id as string;
      choices.set(id, { profile: args.profile as string | null, model: "" });
      return status(id);
    },
    session_model_select: (args) => {
      const id = args.session_id as string;
      choices.set(id, { ...find(id), model: args.model as string });
      return status(id);
    },
    chat_models: (args) => {
      const id = args.session_id as string;
      const profile = PROFILES.find((each) => each.name === find(id).profile);
      return {
        session_id: id,
        endpoint: profile?.endpoint ?? LMS,
        kind: profile?.kind ?? null,
        models: [
          { id: `${profile?.name ?? "default"}-model`, cloud_tag: false },
          { id: "tagged:cloud", cloud_tag: true },
        ],
      } satisfies ChatModels;
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

function composerProvider(): HTMLSelectElement {
  return root.querySelector('.composer select[aria-label="Provider"]') as HTMLSelectElement;
}

function modelLabels(): string[] {
  return Array.from(modelSelect(root).options).map((option) => option.textContent ?? "");
}

function warning(): string {
  const node = root.querySelector<HTMLElement>(".model-warning");
  return node === null || node.hidden ? "" : (node.textContent ?? "");
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

describe("per-session provider and model", () => {
  it("reads the session's connection and models when it opens, and shows both", async () => {
    await start(backend({ "s-a": { profile: "ollama", model: "ollama-model" } }));
    await open("s-a");
    expect(ipc.callsTo("connection_status").map((call) => call.args)).toContainEqual({
      session_id: "s-a",
    });
    expect(ipc.callsTo("chat_models").map((call) => call.args)).toEqual([{ session_id: "s-a" }]);
    expect(composerProvider().value).toBe("ollama");
    expect(modelSelect(root).value).toBe("ollama-model");
    expect(modelLabels()).toContain("ollama-model");
  });

  it("switches provider and model with the session, without a dialog", async () => {
    await start(
      backend({
        "s-a": { profile: "ollama", model: "ollama-model" },
        "s-b": { profile: "lms", model: "lms-model" },
      }),
    );
    await open("s-a");
    expect(composerProvider().value).toBe("ollama");
    await open("s-b");
    expect(composerProvider().value).toBe("lms");
    expect(modelSelect(root).value).toBe("lms-model");
    expect(modelLabels()).not.toContain("ollama-model");
    await open("s-a");
    expect(composerProvider().value).toBe("ollama");
    expect(modelSelect(root).value).toBe("ollama-model");
    // Switching is a read; only a change of provider asks Rust to confirm.
    expect(ipc.callsTo("connection_select")).toHaveLength(0);
    expect(ipc.callsTo("session_model_select")).toHaveLength(0);
  });

  it("chooses a provider for the open session, then clears and refills the model", async () => {
    let finish: (value: unknown) => void = () => undefined;
    const handlers = backend();
    await start(handlers);
    await open("s-a");
    expect(modelLabels()).toContain("default-model");
    ipc.on(
      "chat_models",
      (args) =>
        new Promise((resolve) => {
          finish = () =>
            resolve({
              session_id: args.session_id,
              endpoint: OLLAMA,
              kind: "ollama_local",
              models: [{ id: "ollama-model", cloud_tag: false }],
            });
        }),
    );
    choose(composerProvider(), "ollama");
    await settle();
    expect(ipc.callsTo("connection_select").map((call) => call.args)).toEqual([
      { session_id: "s-a", profile: "ollama" },
    ]);
    // Cleared and disabled until the new list arrives.
    expect(modelSelect(root).value).toBe("");
    expect(modelSelect(root).disabled).toBe(true);
    expect(modelLabels()).not.toContain("default-model");
    finish(null);
    await settle();
    expect(modelSelect(root).disabled).toBe(false);
    expect(modelLabels()).toContain("ollama-model");
    // The status bar has no provider dropdown (ADR-0084).
    expect(root.querySelector(".status-footer select")).toBeNull();
    expect(ipc.callsTo("session_model_select")).toHaveLength(0);
  });

  it("changes the same session's provider from the composer", async () => {
    await start(backend());
    await open("s-a");
    choose(composerProvider(), "lms");
    await settle();
    expect(ipc.callsTo("connection_select")[0]?.args).toEqual({
      session_id: "s-a",
      profile: "lms",
    });
    expect(composerProvider().value).toBe("lms");
    expect(modelLabels()).toContain("lms-model");
  });

  it("records a chosen model for the session", async () => {
    await start(backend());
    await open("s-a");
    choose(modelSelect(root), "default-model");
    await settle();
    expect(ipc.callsTo("session_model_select")[0]?.args).toEqual({
      session_id: "s-a",
      model: "default-model",
    });
    expect(modelSelect(root).value).toBe("default-model");
  });

  it("leaves everything as it was when the operator answers No", async () => {
    await start({
      ...backend({ "s-a": { profile: "lms", model: "lms-model" } }),
      connection_select: () => rejection("cancelled", "The change was not confirmed."),
    });
    await open("s-a");
    const lists = ipc.callsTo("chat_models").length;
    choose(composerProvider(), "ollama");
    await settle();
    expect(root.querySelector(".composer-model-status")?.textContent).toContain(
      CONNECTION_UNCHANGED,
    );
    expect(composerProvider().value).toBe("lms");
    expect(modelSelect(root).value).toBe("lms-model");
    expect(ipc.callsTo("chat_models")).toHaveLength(lists);
  });

  it("ignores a status or model list that arrives for a session that was left", async () => {
    let lateStatus: (value: unknown) => void = () => undefined;
    let lateList: (value: unknown) => void = () => undefined;
    const handlers = backend({
      "s-a": { profile: "ollama", model: "ollama-model" },
      "s-b": { profile: "lms", model: "lms-model" },
    });
    await start({
      ...handlers,
      connection_status: (args) =>
        args.session_id === "s-a"
          ? new Promise((resolve) => {
              lateStatus = resolve;
            })
          : handlers.connection_status?.(args),
      chat_models: (args) =>
        args.session_id === "s-a"
          ? new Promise((resolve) => {
              lateList = resolve;
            })
          : handlers.chat_models?.(args),
    });
    await open("s-a");
    await open("s-b");
    expect(composerProvider().value).toBe("lms");
    lateStatus(
      connectionStatus({
        origin: "session",
        model: "ollama-model",
        connection: {
          endpoint: OLLAMA,
          source: "active",
          profile: "ollama",
          kind: "ollama_local",
          selectable: true,
        },
        choices: [{ name: "ollama", kind: "ollama_local", endpoint: OLLAMA, active: true }],
      }),
    );
    lateList({
      session_id: "s-a",
      endpoint: OLLAMA,
      kind: "ollama_local",
      models: [{ id: "ollama-model", cloud_tag: false }],
    });
    await settle();
    expect(composerProvider().value).toBe("lms");
    expect(modelSelect(root).value).toBe("lms-model");
    expect(modelLabels()).not.toContain("ollama-model");
  });

  it("keeps the model dropdown empty and disabled until the list arrives", async () => {
    let finish: (value: unknown) => void = () => undefined;
    const handlers = backend();
    await start({
      ...handlers,
      chat_models: (args) =>
        new Promise((resolve) => {
          finish = () => resolve(handlers.chat_models?.(args));
        }),
    });
    await open("s-a");
    expect(modelSelect(root).disabled).toBe(true);
    expect(modelLabels()).toEqual([
      "No model listed",
      `${SETTINGS.model} (not reported by the server)`,
    ]);
    finish(null);
    await settle();
    expect(modelSelect(root).disabled).toBe(false);
    expect(buttonByText(root, "Refresh").disabled).toBe(false);
  });

  it("warns that Ollama Cloud models leave this computer", async () => {
    await start(backend({ "s-a": { profile: "cloud", model: "" } }));
    await open("s-a");
    expect(warning()).toBe("");
    expect(modelLabels()).toContain(`cloud-model${HOSTED_SUFFIX}`);
    choose(modelSelect(root), "cloud-model");
    await settle();
    expect(warning()).toBe(HOSTED_WARNING);
    await open("s-b");
    expect(warning()).toBe("");
    choose(modelSelect(root), "tagged:cloud");
    await settle();
    expect(warning()).toBe(CLOUD_WARNING);
  });

  it("disables the provider dropdown when a launch argument fixed the connection", async () => {
    await start(backend({}, false));
    await open("s-a");
    expect(composerProvider().disabled).toBe(true);
    expect(composerProvider().textContent).toContain("Set at launch");
    // The model is still chosen from the fixed connection's list.
    expect(modelSelect(root).disabled).toBe(false);
  });

  it("asks for another choice when the session's recorded connection went stale", async () => {
    const handlers = backend({ "s-a": { profile: "ollama", model: "ollama-model" } });
    let stale = false;
    await start({
      ...handlers,
      connection_status: (args) => {
        const status = handlers.connection_status?.(args) as ConnectionStatus;
        return { ...status, stale };
      },
      chat_send: () =>
        rejection("session_connection_stale", "The connection chosen for this session changed."),
    });
    await open("s-a");
    stale = true;
    const box = root.querySelector(".message-box") as HTMLTextAreaElement;
    box.value = "hello";
    box.dispatchEvent(new Event("input", { bubbles: true }));
    buttonByText(root, "Send").click();
    await settle();
    expect(root.querySelector(".notice")?.textContent).toContain("session_connection_stale");
    expect(composerProvider().selectedOptions[0]?.textContent).toContain("Choose again");
    expect(buttonByText(root, "Send").disabled).toBe(true);
    // Choosing the provider again works from the stale entry.
    stale = false;
    choose(composerProvider(), "ollama");
    await settle();
    expect(ipc.callsTo("connection_select")).toHaveLength(1);
  });

  it("warns while the saved per-session choices are unreadable", async () => {
    const handlers = backend();
    let unreadable = true;
    await start({
      ...handlers,
      connection_status: (args) => {
        const status = handlers.connection_status?.(args) as ConnectionStatus;
        return { ...status, session_choices_unreadable: unreadable };
      },
    });
    await open("s-a");
    const warning = (): HTMLElement | undefined =>
      Array.from(root.querySelectorAll<HTMLElement>(".model-warning")).find(
        (node) => node.textContent === SESSION_CHOICES_UNREADABLE,
      );
    expect(warning()?.hidden).toBe(false);

    // A later read that finds the file readable again hides it.
    unreadable = false;
    await open("s-b");
    expect(warning()?.hidden).toBe(true);
  });

  it("sends the newly opened session to Workspace settings before its transcript loads", async () => {
    const handlers = backend();
    await start({
      ...handlers,
      // Session B's transcript never answers in this test.
      session_transcript: (args) =>
        args.session_id === "s-b"
          ? new Promise(() => undefined)
          : handlers.session_transcript?.(args),
    });
    await open("s-a");
    await open("s-b");

    buttonByLabel(root, "Workspace settings").click();
    await settle();
    expect(ipc.callsTo("settings_show_workspace")).toEqual([
      { command: "settings_show_workspace", args: { session_id: "s-b" } },
    ]);
  });

  it("does not show a Workspace settings failure of the session that was left", async () => {
    let fail: (reason: unknown) => void = () => undefined;
    await start({
      ...backend(),
      settings_show_workspace: () =>
        new Promise((_resolve, reject) => {
          fail = reject;
        }),
    });
    await open("s-a");
    buttonByLabel(root, "Workspace settings").click();
    await settle();
    await open("s-b");
    fail({ code: "unknown_workspace", message: "The workspace is not registered." });
    await settle();
    const problem = root.querySelector(".composer-error") as HTMLElement;
    expect(problem.hidden).toBe(true);
    expect(problem.textContent).not.toContain("The workspace is not registered.");
  });
});
