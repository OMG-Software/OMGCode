import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { neutralise } from "../src/dom";
import { setInvoker } from "../src/ipc/client";
import type { SettingsOverview } from "../src/ipc/types";
import { HOSTILE } from "../src/mock/fixtures";
import { CLI_HINT, type SettingsApp, startSettings } from "../src/settings/app";
import {
  buttonByText,
  type FakeIpc,
  installFakeIpc,
  mountRoot,
  rejection,
  settle,
} from "./helpers";

const WORKSPACE = "77777777-7777-4777-8777-777777777777";

function overview(overrides: Partial<SettingsOverview> = {}): SettingsOverview {
  return {
    store_initialized: true,
    revision: 7,
    development_root: "/home/dev",
    workspaces: [
      {
        workspace_id: WORKSPACE,
        folder_display: "/home/dev/project",
        model_tools: true,
        read: true,
        write: false,
        execute: false,
        package_cache_root: null,
      },
    ],
    connection: { endpoint: "http://127.0.0.1:1234/v1", source: "profile", profile: "lms" },
    provider_profiles: [
      {
        name: "lms",
        kind: "lm_studio",
        auth: "api_key",
        endpoint: "http://127.0.0.1:1234/v1",
        credential_bound: true,
      },
    ],
    toolchain_profiles: [
      {
        name: "rust",
        platform: "linux",
        read_only_roots: ["/opt/rust"],
        path_entries: [],
        environment_names: ["RUSTUP_HOME"],
      },
    ],
    package_profiles: [],
    container_grants: [],
    skills: [
      {
        name: "review",
        application: true,
        workspace_overrides: [{ workspace_id: WORKSPACE, enabled: false }],
      },
    ],
    ...overrides,
  };
}

let app: SettingsApp | null = null;
let root: HTMLElement;
let ipc: FakeIpc;

async function start(reply: () => unknown): Promise<void> {
  ipc = installFakeIpc({ settings_overview: reply });
  root = mountRoot();
  app = startSettings(root);
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

describe("settings window", () => {
  it("reads the overview once on start and calls nothing else", async () => {
    await start(() => overview());
    expect(ipc.calls.map((call) => call.command)).toEqual(["settings_overview"]);
    expect(ipc.callsTo("settings_overview")[0]?.args).toEqual({});
  });

  it("says that it is read-only and names the CLI", async () => {
    await start(() => overview());
    expect(root.textContent).toContain(CLI_HINT);
    expect(root.querySelectorAll("input, textarea, select")).toHaveLength(0);
    expect(Array.from(root.querySelectorAll("button")).map((b) => b.textContent)).toEqual([
      "Refresh",
    ]);
  });

  it("shows each group of the configuration", async () => {
    await start(() => overview());
    const text = root.textContent ?? "";
    expect(text).toContain("Settings revision 7");
    expect(text).toContain("/home/dev/project");
    expect(text).toContain("model tools: on");
    expect(text).toContain("write: off");
    expect(text).toContain("Provider profile lms, set at launch");
    expect(text).toContain("lm_studio");
    expect(text).toContain("stored");
    expect(text).toContain("/opt/rust");
    expect(text).toContain("RUSTUP_HOME");
    expect(text).toContain("No package-network profile is recorded.");
    expect(text).toContain("No container grant is recorded.");
    // A skill override names the workspace by its folder.
    const skill = Array.from(root.querySelectorAll(".card")).find((card) =>
      card.textContent?.startsWith("review"),
    );
    expect(skill?.textContent).toContain("/home/dev/project");
    expect(skill?.textContent).toContain("disabled");
  });

  it("explains an uninitialized store", async () => {
    await start(() => overview({ store_initialized: false, revision: 0, workspaces: [] }));
    expect(root.textContent).toContain("No settings store exists yet.");
    expect(root.textContent).not.toContain("Settings revision");
  });

  it("renders hostile recorded text as text", async () => {
    await start(() =>
      overview({
        development_root: HOSTILE.img,
        workspaces: [
          {
            workspace_id: WORKSPACE,
            folder_display: HOSTILE.rtl,
            model_tools: false,
            read: false,
            write: false,
            execute: false,
            package_cache_root: HOSTILE.script,
          },
        ],
      }),
    );
    expect(root.querySelector("img, script")).toBeNull();
    expect(root.textContent).toContain(HOSTILE.img);
    expect(root.textContent).toContain(neutralise(HOSTILE.rtl));
    expect(root.textContent).not.toContain(String.fromCharCode(0x202e));
  });

  it("shows the core error and keeps the Refresh button", async () => {
    await start(() => rejection("busy", "The settings store is in use."));
    const alert = root.querySelector("[role=alert]") as HTMLElement;
    expect(alert.hidden).toBe(false);
    expect(alert.textContent).toBe("The settings store is in use. (busy)");
    ipc.on("settings_overview", () => overview());
    buttonByText(root, "Refresh").click();
    await settle();
    expect(alert.hidden).toBe(true);
    expect(root.textContent).toContain("/home/dev/project");
  });

  it("reads again when the window becomes visible", async () => {
    await start(() => overview());
    document.dispatchEvent(new Event("visibilitychange"));
    await settle();
    expect(ipc.callsTo("settings_overview")).toHaveLength(2);
  });
});
