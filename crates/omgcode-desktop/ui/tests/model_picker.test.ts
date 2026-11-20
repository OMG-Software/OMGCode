import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setInvoker } from "../src/ipc/client";
import type { ChatModels } from "../src/ipc/types";
import { type MainApp, startMain } from "../src/main/app";
import { CLOUD_SUFFIX, CLOUD_WARNING, EMPTY_NOTE, UNLISTED_SUFFIX } from "../src/main/model_picker";
import {
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

// The composer chooses the model from the connection's list (ADR-0066).

let app: MainApp | null = null;
let root: HTMLElement;
let ipc: FakeIpc;

const s = session({ session_id: "s-pick", title: "Pick" });
const running = { session_id: "s-pick", state: "running", outcome: null, error: null };

function listing(...models: Array<[string, boolean]>): Handler {
  return (args) =>
    ({
      session_id: args.session_id as string,
      endpoint: SETTINGS.endpoint,
      kind: null,
      models: models.map(([id, cloud_tag]) => ({ id, cloud_tag })),
    }) satisfies ChatModels;
}

async function start(handlers: Record<string, Handler> = {}): Promise<void> {
  ipc = installFakeIpc(
    mainHandlers({
      session_tree: () => tree([{ id: "g1", folder: "C:\\work", sessions: [s] }]),
      session_transcript: () => page(s, [userMessage(1, "hi")]),
      chat_send: () => running,
      chat_status: () => running,
      ...handlers,
    }),
  );
  root = mountRoot();
  app = startMain(root);
  await settle();
  root.querySelector<HTMLElement>('[data-session-id="s-pick"]')?.click();
  await settle();
}

function labels(): string[] {
  return Array.from(modelSelect(root).options).map((option) => option.textContent ?? "");
}

function note(): string {
  return root.querySelector(".model-note")?.textContent ?? "";
}

function warning(): string {
  const node = root.querySelector<HTMLElement>(".model-warning");
  return node === null || node.hidden ? "" : (node.textContent ?? "");
}

function send(): void {
  const box = root.querySelector(".message-box") as HTMLTextAreaElement;
  box.value = "hello";
  box.dispatchEvent(new Event("input", { bubbles: true }));
  buttonByText(root, "Send").click();
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

describe("model choice", () => {
  it("offers the listed models and sends the chosen one", async () => {
    await start({ chat_models: listing(["a-model", false], [SETTINGS.model, false]) });
    expect(ipc.callsTo("chat_models")).toHaveLength(1);
    expect(ipc.callsTo("chat_models")[0]?.args).toEqual({ session_id: "s-pick" });
    expect(labels()).toEqual(["Choose a model", "a-model", SETTINGS.model]);
    // The saved default is chosen, and the model is no longer a text box.
    expect(modelSelect(root).value).toBe(SETTINGS.model);
    expect(root.querySelector('.settings input[type="text"]')).toBeNull();

    choose(modelSelect(root), "a-model");
    send();
    await settle();
    expect(ipc.callsTo("chat_send")[0]?.args).toMatchObject({ settings: { model: "a-model" } });
  });

  it("keeps a saved model the server does not report, marked", async () => {
    await start({ chat_models: listing(["other", false]) });
    expect(labels()).toEqual(["Choose a model", `${SETTINGS.model}${UNLISTED_SUFFIX}`, "other"]);
    expect(modelSelect(root).value).toBe(SETTINGS.model);
  });

  it("marks a cloud-tagged Ollama model and warns while it is chosen", async () => {
    await start({ chat_models: listing(["kimi-k3:cloud", true], [SETTINGS.model, false]) });
    expect(labels()).toContain(`kimi-k3:cloud${CLOUD_SUFFIX}`);
    expect(warning()).toBe("");
    choose(modelSelect(root), "kimi-k3:cloud");
    expect(warning()).toBe(CLOUD_WARNING);
    choose(modelSelect(root), SETTINGS.model);
    expect(warning()).toBe("");
  });

  it("warns about a saved cloud model while listing and after listing fails", async () => {
    let fail: (reason: unknown) => void = () => undefined;
    await start({
      connection_status: () => connectionStatus({ model: "gpt-oss:120b-cloud" }),
      chat_models: () =>
        new Promise((_resolve, reject) => {
          fail = reject;
        }),
    });
    // Still listing: the status says so and the warning shows beside it.
    expect(note()).toBe("Listing models…");
    expect(warning()).toBe(CLOUD_WARNING);
    expect(labels()).toContain(`gpt-oss:120b-cloud${CLOUD_SUFFIX}${UNLISTED_SUFFIX}`);
    fail({ code: "provider", message: "local model transport failed" });
    await settle();
    expect(note()).toContain("Could not list models");
    expect(warning()).toBe(CLOUD_WARNING);
  });

  it("offers no model of the previous server while the new list loads", async () => {
    let current = SETTINGS.endpoint;
    const ollama = "http://127.0.0.1:11434/v1";
    let answer: (value: ChatModels) => void = () => undefined;
    await start({
      chat_models: listing(["lm-only", false], [SETTINGS.model, false]),
      connection_status: () =>
        connectionStatus({
          connection: {
            endpoint: current,
            source: "active",
            profile: null,
            kind: null,
            selectable: true,
          },
        }),
      chat_send: (args) =>
        (args.settings as { endpoint: string }).endpoint === current
          ? running
          : rejection("connection_changed", "The model connection changed in settings."),
    });
    expect(labels()).toContain("lm-only");
    ipc.on(
      "chat_models",
      () =>
        new Promise<ChatModels>((resolve) => {
          answer = resolve;
        }),
    );
    // The settings window switched to Ollama; the refused send reads the new connection.
    current = ollama;
    send();
    await settle();
    expect(labels()).not.toContain("lm-only");
    expect(labels()).toContain(`${SETTINGS.model}${UNLISTED_SUFFIX}`);
    answer({
      session_id: "s-pick",
      endpoint: ollama,
      kind: "ollama_local",
      models: [{ id: "lfm2.5:latest", cloud_tag: false }],
    });
    await settle();
    expect(labels()).toContain("lfm2.5:latest");
  });

  it("sends a listed id exactly as the server reported it", async () => {
    await start({ chat_models: listing([" spaced ", false], [SETTINGS.model, false]) });
    choose(modelSelect(root), " spaced ");
    send();
    await settle();
    expect(ipc.callsTo("chat_send")[0]?.args).toMatchObject({ settings: { model: " spaced " } });
  });

  it("cannot send without a chosen model", async () => {
    await start({ connection_status: () => connectionStatus({ model: "" }) });
    expect(modelSelect(root).value).toBe("");
    send();
    await settle();
    expect(buttonByText(root, "Send").disabled).toBe(true);
    expect(ipc.callsTo("chat_send")).toHaveLength(0);
    choose(modelSelect(root), SETTINGS.model);
    expect(buttonByText(root, "Send").disabled).toBe(false);
  });

  it("shows why the list could not be read and keeps the saved model", async () => {
    await start({
      chat_models: () => rejection("provider", "local model transport failed"),
    });
    expect(note()).toBe("Could not list models: local model transport failed (provider)");
    expect(labels()).toEqual(["No model listed", `${SETTINGS.model}${UNLISTED_SUFFIX}`]);
    expect(modelSelect(root).value).toBe(SETTINGS.model);
  });

  it("says when the server reports no models", async () => {
    await start({ chat_models: listing() });
    expect(note()).toBe(EMPTY_NOTE);
  });

  // Regression (ADR-0069 decision 10): the list used to be dropped whenever the endpoint the
  // answer named differed from the endpoint the composer had cached from `chat_defaults`,
  // which happens after the connection changes outside this window's footer.
  it("keeps the list when the answer names another endpoint than the cached one", async () => {
    await start({
      chat_models: (args) => ({
        session_id: args.session_id as string,
        endpoint: "http://127.0.0.1:11434/v1",
        kind: "ollama_local",
        models: [{ id: "elsewhere", cloud_tag: false }],
      }),
    });
    expect(labels()).toContain("elsewhere");
    expect(note()).toBe("");
  });

  it("lists again on Refresh", async () => {
    const answers: Array<(value: ChatModels) => void> = [];
    await start({ chat_models: listing([SETTINGS.model, false]) });
    ipc.on(
      "chat_models",
      () =>
        new Promise<ChatModels>((resolve) => {
          answers.push(resolve);
        }),
    );
    const refresh = root.querySelector<HTMLButtonElement>(
      'button[aria-label="Refresh the model list"]',
    ) as HTMLButtonElement;
    refresh.click();
    await settle();
    // Disabled while listing, so a second click cannot start another request.
    expect(refresh.disabled).toBe(true);
    expect(note()).toBe("Listing models…");
    expect(answers).toHaveLength(1);
    answers[0]?.({
      session_id: "s-pick",
      endpoint: SETTINGS.endpoint,
      kind: null,
      models: [{ id: "fresh", cloud_tag: false }],
    });
    await settle();
    expect(labels()).toContain("fresh");
    expect(refresh.disabled).toBe(false);
    expect(ipc.callsTo("chat_models")).toHaveLength(2);
  });

  it("keeps its options while the message is typed", async () => {
    await start({ chat_models: listing(["a-model", false], [SETTINGS.model, false]) });
    const first = modelSelect(root).options[1];
    const box = root.querySelector(".message-box") as HTMLTextAreaElement;
    box.value = "typing";
    box.dispatchEvent(new Event("input", { bubbles: true }));
    expect(modelSelect(root).options[1]).toBe(first);
  });

  it("shows a hostile id as text only", async () => {
    const hostile = '<img src=x onerror="alert(1)">';
    await start({ chat_models: listing([hostile, false], [SETTINGS.model, false]) });
    expect(labels()).toContain(hostile);
    expect(root.querySelector(".settings img")).toBeNull();
  });
});
