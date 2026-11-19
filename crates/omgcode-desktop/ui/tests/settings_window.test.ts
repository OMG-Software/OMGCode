import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { neutralise } from "../src/dom";
import { setInvoker } from "../src/ipc/client";
import type { SettingsOverview, WorkspaceSettingsView } from "../src/ipc/types";
import { HOSTILE } from "../src/mock/fixtures";
import {
  INTRO,
  NOTHING_CHANGED,
  PERMISSION_LABELS,
  type SettingsApp,
  startSettings,
} from "../src/settings/app";
import {
  buttonByText,
  type FakeIpc,
  installFakeIpc,
  mountRoot,
  rejection,
  settle,
} from "./helpers";

const WORKSPACE = "77777777-7777-4777-8777-777777777777";

function workspace(overrides: Partial<WorkspaceSettingsView> = {}): WorkspaceSettingsView {
  return {
    workspace_id: WORKSPACE,
    folder_display: "/home/dev/project",
    model_tools: true,
    read: true,
    write: false,
    execute: false,
    task_network: false,
    host_services: false,
    package_cache_root: null,
    ...overrides,
  };
}

function overview(overrides: Partial<SettingsOverview> = {}): SettingsOverview {
  return {
    revision: 7,
    development_root: "/home/dev",
    workspaces: [workspace()],
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

async function start(
  reply: () => unknown,
  more: Record<string, (args: Record<string, unknown>) => unknown> = {},
): Promise<void> {
  ipc = installFakeIpc({ settings_overview: reply, ...more });
  root = mountRoot();
  app = startSettings(root);
  await settle();
}

function checkbox(label: string): HTMLInputElement {
  const found = root.querySelector(`input[aria-label="${label}"]`);
  if (!(found instanceof HTMLInputElement)) {
    throw new Error(`No checkbox ${label}`);
  }
  return found;
}

function workspaceCard(): HTMLElement {
  const found = root.querySelector(".card");
  if (!(found instanceof HTMLElement)) {
    throw new Error("No workspace card");
  }
  return found;
}

function body(): HTMLFieldSetElement {
  return root.querySelector("fieldset.settings-body") as HTMLFieldSetElement;
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

  it("explains the system dialogs and what still needs the CLI", async () => {
    await start(() => overview());
    expect(root.textContent).toContain(INTRO);
    // No control takes typed text: folders come only from the system dialog.
    expect(root.querySelectorAll("textarea, select")).toHaveLength(0);
    const inputs = Array.from(root.querySelectorAll("input"));
    expect(inputs.every((input) => input.type === "checkbox")).toBe(true);
  });

  it("shows each group of the configuration", async () => {
    await start(() => overview());
    const text = root.textContent ?? "";
    expect(text).toContain("Settings revision 7");
    expect(text).toContain("/home/dev/project");
    expect(text).toContain("Provider profile lms, set at launch");
    expect(text).toContain("lm_studio");
    expect(text).toContain("stored");
    expect(text).toContain("/opt/rust");
    expect(text).toContain("RUSTUP_HOME");
    expect(text).toContain("No package-network profile is recorded.");
    expect(text).toContain("No container grant is recorded.");
    // Each preference is a checkbox showing the recorded value.
    expect(PERMISSION_LABELS.map(([, label]) => checkbox(label).checked)).toEqual([
      true,
      true,
      false,
      false,
      false,
      false,
    ]);
    // A skill override names the workspace by its folder.
    const skill = Array.from(root.querySelectorAll(".card")).find((card) =>
      card.textContent?.startsWith("review"),
    );
    expect(skill?.textContent).toContain("/home/dev/project");
    expect(skill?.textContent).toContain("disabled");
  });

  it("asks the core to choose the root with the revision it read and shows the reply", async () => {
    await start(() => overview({ development_root: null, workspaces: [] }), {
      settings_choose_root: () => overview({ revision: 8, development_root: "C:\\code" }),
    });
    expect(buttonByText(root, "Register workspace").disabled).toBe(true);
    expect(root.textContent).toContain("Choose a development root first.");

    buttonByText(root, "Choose folder").click();
    await settle();

    // The window names no folder: Rust opens the dialog.
    expect(ipc.callsTo("settings_choose_root")[0]?.args).toEqual({ revision: 7 });
    expect(root.textContent).toContain("C:\\code");
    expect(root.textContent).toContain("Settings revision 8");
    expect(buttonByText(root, "Register workspace").disabled).toBe(false);
  });

  it("registers a workspace with only the revision", async () => {
    await start(() => overview(), {
      settings_register_workspace: () =>
        overview({
          revision: 8,
          workspaces: [
            workspace(),
            workspace({ workspace_id: "w2", folder_display: "/home/dev/b" }),
          ],
        }),
    });
    buttonByText(root, "Register workspace").click();
    await settle();
    expect(ipc.callsTo("settings_register_workspace")[0]?.args).toEqual({ revision: 7 });
    expect(root.textContent).toContain("/home/dev/b");
  });

  it("sends the complete preference set with one flag changed", async () => {
    await start(() => overview(), {
      settings_set_permissions: () =>
        overview({ revision: 8, workspaces: [workspace({ execute: true })] }),
    });
    const execute = checkbox("execute");
    execute.checked = true;
    execute.dispatchEvent(new Event("change"));
    await settle();

    expect(ipc.callsTo("settings_set_permissions")[0]?.args).toEqual({
      workspace_id: WORKSPACE,
      revision: 7,
      permissions: {
        model_tools: true,
        read: true,
        write: false,
        execute: true,
        task_network: false,
        host_services: false,
      },
    });
    expect(checkbox("execute").checked).toBe(true);
  });

  it("shows a declined dialog as no change and restores the recorded value", async () => {
    await start(() => overview(), {
      settings_set_permissions: () => rejection("cancelled", "No change was made."),
    });
    const write = checkbox("write");
    write.checked = true;
    write.dispatchEvent(new Event("change"));
    await settle();

    const notice = root.querySelector("[role=status]") as HTMLElement;
    expect(notice.hidden).toBe(false);
    expect(notice.textContent).toBe(NOTHING_CHANGED);
    expect((root.querySelector("[role=alert]") as HTMLElement).hidden).toBe(true);
    // The window read the configuration again, so the box shows what is recorded.
    expect(ipc.callsTo("settings_overview")).toHaveLength(2);
    expect(checkbox("write").checked).toBe(false);
  });

  it("shows a refusal and reads the configuration again", async () => {
    await start(() => overview(), {
      settings_choose_root: () =>
        rejection("stale_revision", "The settings changed since this window read them."),
    });
    ipc.on("settings_overview", () => overview({ revision: 9 }));
    buttonByText(root, "Change").click();
    await settle();

    const alert = root.querySelector("[role=alert]") as HTMLElement;
    expect(alert.hidden).toBe(false);
    expect(alert.textContent).toBe(
      "The settings changed since this window read them. (stale_revision)",
    );
    expect(root.textContent).toContain("Settings revision 9");
  });

  it("disables every control while a change waits for the operator", async () => {
    let answer: (value: SettingsOverview) => void = () => undefined;
    await start(() => overview(), {
      settings_choose_root: () =>
        new Promise<SettingsOverview>((resolve) => {
          answer = resolve;
        }),
    });
    buttonByText(root, "Change").click();
    await settle();
    expect(body().disabled).toBe(true);
    expect(buttonByText(root, "Refresh").disabled).toBe(true);

    // A second click while the first waits sends nothing.
    checkbox("write").dispatchEvent(new Event("change"));
    await settle();
    expect(ipc.callsTo("settings_set_permissions")).toHaveLength(0);

    answer(overview({ revision: 8 }));
    await settle();
    expect(body().disabled).toBe(false);
    expect(buttonByText(root, "Refresh").disabled).toBe(false);
  });

  it("offers Clear only when a package cache is set", async () => {
    await start(() => overview(), {
      settings_choose_package_cache: () =>
        overview({
          revision: 8,
          workspaces: [workspace({ package_cache_root: "/home/dev/.cache/pkgs" })],
        }),
      settings_clear_package_cache: () => overview({ revision: 9 }),
    });
    expect(() => buttonByText(workspaceCard(), "Clear")).toThrow();

    buttonByText(workspaceCard(), "Choose…").click();
    await settle();
    expect(ipc.callsTo("settings_choose_package_cache")[0]?.args).toEqual({
      workspace_id: WORKSPACE,
      revision: 7,
    });
    expect(workspaceCard().textContent).toContain("/home/dev/.cache/pkgs");

    buttonByText(workspaceCard(), "Clear").click();
    await settle();
    expect(ipc.callsTo("settings_clear_package_cache")[0]?.args).toEqual({
      workspace_id: WORKSPACE,
      revision: 8,
    });
    expect(() => buttonByText(workspaceCard(), "Clear")).toThrow();
  });

  it("renders hostile recorded text as text", async () => {
    await start(() =>
      overview({
        development_root: HOSTILE.img,
        workspaces: [
          workspace({
            folder_display: HOSTILE.rtl,
            model_tools: false,
            read: false,
            package_cache_root: HOSTILE.script,
          }),
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
