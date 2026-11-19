import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { neutralise } from "../src/dom";
import { setInvoker } from "../src/ipc/client";
import type {
  ProviderProfileView,
  SettingsOverview,
  WorkspaceSettingsView,
} from "../src/ipc/types";
import { HOSTILE } from "../src/mock/fixtures";
import {
  DEFAULT_CHOICE,
  DEFAULT_ENDPOINTS,
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

const OLLAMA_ENDPOINT = "http://127.0.0.1:11434/v1";

function lms(overrides: Partial<ProviderProfileView> = {}): ProviderProfileView {
  return {
    name: "lms",
    kind: "lm_studio",
    auth: "api_key",
    endpoint: "http://127.0.0.1:1234/v1",
    credential_bound: true,
    selectable: true,
    active: false,
    ...overrides,
  };
}

function ollama(overrides: Partial<ProviderProfileView> = {}): ProviderProfileView {
  return {
    name: "ollama",
    kind: "ollama_local",
    auth: "none",
    endpoint: OLLAMA_ENDPOINT,
    credential_bound: false,
    selectable: true,
    active: false,
    ...overrides,
  };
}

function overview(overrides: Partial<SettingsOverview> = {}): SettingsOverview {
  return {
    revision: 7,
    development_root: "/home/dev",
    workspaces: [workspace()],
    connection: {
      endpoint: "http://127.0.0.1:1234/v1",
      source: "profile",
      profile: "lms",
      kind: "lm_studio",
      selectable: false,
    },
    provider_profiles: [lms()],
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

function picker(label: string): HTMLSelectElement {
  const found = root.querySelector(`select[aria-label="${label}"]`);
  if (!(found instanceof HTMLSelectElement)) {
    throw new Error(`No dropdown ${label}`);
  }
  return found;
}

function field(label: string): HTMLInputElement {
  const found = root.querySelector(`input[aria-label="${label}"]`);
  if (!(found instanceof HTMLInputElement)) {
    throw new Error(`No field ${label}`);
  }
  return found;
}

/** A process started without a connection argument, so the window may choose. */
function choosable(overrides: Partial<SettingsOverview> = {}): SettingsOverview {
  return overview({
    connection: {
      endpoint: "http://127.0.0.1:1234/v1",
      source: "active",
      profile: null,
      kind: null,
      selectable: true,
    },
    provider_profiles: [lms(), ollama()],
    ...overrides,
  });
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
    // No control takes a typed path: folders come only from the system dialog. The only typed
    // values are a new connection's name and loopback URL, and the only dropdowns choose a
    // provider (ADR-0063).
    expect(root.querySelectorAll("textarea")).toHaveLength(0);
    const selects = Array.from(root.querySelectorAll("select")).map((s) => s.ariaLabel);
    expect(selects).toEqual(["Model provider", "New connection provider"]);
    const typed = Array.from(root.querySelectorAll("input"))
      .filter((input) => input.type !== "checkbox")
      .map((input) => input.getAttribute("aria-label"));
    expect(typed).toEqual(["New connection name", "New connection endpoint"]);
  });

  it("shows each group of the configuration", async () => {
    await start(() => overview());
    const text = root.textContent ?? "";
    expect(text).toContain("Settings revision 7");
    expect(text).toContain("/home/dev/project");
    expect(text).toContain("Provider profile lms, set at launch");
    expect(text).toContain("LM Studio");
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

  it("restores the last read value when reading again after a declined dialog fails", async () => {
    await start(() => overview(), {
      settings_set_permissions: () => rejection("cancelled", "No change was made."),
    });
    ipc.on("settings_overview", () => rejection("io", "The settings could not be read."));
    const write = checkbox("write");
    write.checked = true;
    write.dispatchEvent(new Event("change"));
    await settle();

    expect(ipc.callsTo("settings_overview")).toHaveLength(2);
    // The box shows the recorded value again, not what the operator asked for.
    expect(checkbox("write").checked).toBe(false);
    expect((root.querySelector("[role=status]") as HTMLElement).textContent).toBe(NOTHING_CHANGED);
    const alert = root.querySelector("[role=alert]") as HTMLElement;
    expect(alert.hidden).toBe(false);
    expect(alert.textContent).toBe("The settings could not be read. (io)");
    expect(body().disabled).toBe(false);
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

  it("lists the default and every selectable connection and shows the active one", async () => {
    await start(() =>
      choosable({
        connection: {
          endpoint: OLLAMA_ENDPOINT,
          source: "active",
          profile: "ollama",
          kind: "ollama_local",
          selectable: true,
        },
        provider_profiles: [
          lms(),
          ollama({ active: true }),
          lms({ name: "cloud", kind: "openai", endpoint: null, selectable: false }),
        ],
      }),
    );
    const provider = picker("Model provider");
    expect(provider.disabled).toBe(false);
    expect(Array.from(provider.options).map((option) => option.value)).toEqual([
      DEFAULT_CHOICE,
      "lms",
      "ollama",
    ]);
    expect(provider.value).toBe("ollama");
    expect(provider.selectedOptions[0]?.textContent).toBe(`Ollama: ollama (${OLLAMA_ENDPOINT})`);
    const text = root.textContent ?? "";
    expect(text).toContain("Provider profile ollama, chosen here");
    expect(text).toContain("ollama (active)");
  });

  it("asks the core to switch with the revision it read when a provider is chosen", async () => {
    await start(() => choosable(), {
      settings_select_connection: (args) =>
        choosable({
          revision: 8,
          connection: {
            endpoint: OLLAMA_ENDPOINT,
            source: "active",
            profile: args.profile as string,
            kind: "ollama_local",
            selectable: true,
          },
          provider_profiles: [lms(), ollama({ active: true })],
        }),
    });
    const provider = picker("Model provider");
    expect(provider.value).toBe(DEFAULT_CHOICE);
    provider.value = "ollama";
    provider.dispatchEvent(new Event("change"));
    await settle();
    // The window names only the profile: Rust asks for the confirmation and resolves it.
    expect(ipc.callsTo("settings_select_connection")[0]?.args).toEqual({
      revision: 7,
      profile: "ollama",
    });
    expect(picker("Model provider").value).toBe("ollama");
    expect(root.textContent).toContain(OLLAMA_ENDPOINT);

    // Choosing the default sends no profile.
    ipc.on("settings_select_connection", () => choosable({ revision: 9 }));
    const again = picker("Model provider");
    again.value = DEFAULT_CHOICE;
    again.dispatchEvent(new Event("change"));
    await settle();
    expect(ipc.callsTo("settings_select_connection")[1]?.args).toEqual({
      revision: 8,
      profile: null,
    });
  });

  it("shows a declined switch as no change and the recorded provider", async () => {
    await start(() => choosable(), {
      settings_select_connection: () => rejection("cancelled", "No change was made."),
    });
    const provider = picker("Model provider");
    provider.value = "ollama";
    provider.dispatchEvent(new Event("change"));
    await settle();
    expect((root.querySelector("[role=status]") as HTMLElement).textContent).toBe(NOTHING_CHANGED);
    expect(picker("Model provider").value).toBe(DEFAULT_CHOICE);
  });

  it("disables the dropdown when a launch argument fixed the connection", async () => {
    await start(() => overview());
    const provider = picker("Model provider");
    expect(provider.disabled).toBe(true);
    expect(provider.selectedOptions[0]?.textContent).toBe(
      "Set at launch (http://127.0.0.1:1234/v1)",
    );
    expect(root.textContent).toContain("Fixed by --endpoint or --provider-profile at launch.");
  });

  it("adds an Ollama connection with its usual address and follows the provider choice", async () => {
    await start(() => choosable({ provider_profiles: [] }), {
      settings_add_local_connection: () => choosable({ revision: 8 }),
    });
    expect(picker("New connection provider").value).toBe("ollama_local");
    expect(field("New connection name").value).toBe("ollama");
    expect(field("New connection endpoint").value).toBe(DEFAULT_ENDPOINTS.ollama_local);

    // Switching the provider moves an unedited address and name with it.
    const kind = picker("New connection provider");
    kind.value = "lm_studio";
    kind.dispatchEvent(new Event("change"));
    expect(field("New connection endpoint").value).toBe(DEFAULT_ENDPOINTS.lm_studio);
    expect(field("New connection name").value).toBe("lmstudio");
    // An edited address is kept.
    field("New connection endpoint").value = "http://127.0.0.1:4000/v1";
    kind.value = "ollama_local";
    kind.dispatchEvent(new Event("change"));
    expect(field("New connection endpoint").value).toBe("http://127.0.0.1:4000/v1");

    field("New connection name").value = "  local  ";
    buttonByText(root, "Add connection").click();
    await settle();
    expect(ipc.callsTo("settings_add_local_connection")[0]?.args).toEqual({
      revision: 7,
      name: "local",
      kind: "ollama_local",
      endpoint: "http://127.0.0.1:4000/v1",
    });
    expect(root.textContent).toContain("Settings revision 8");
  });

  it("removes a connection without a credential and leaves a stored one to the CLI", async () => {
    await start(() => choosable(), {
      settings_remove_connection: () => choosable({ revision: 8, provider_profiles: [lms()] }),
    });
    const cardOf = (name: string): HTMLElement =>
      Array.from(root.querySelectorAll<HTMLElement>(".card")).find(
        (card) => card.querySelector(".card-title")?.textContent === name,
      ) as HTMLElement;
    expect(buttonByText(cardOf("lms"), "Remove").disabled).toBe(true);
    expect(cardOf("lms").textContent).toContain("remove with the trusted CLI");
    buttonByText(cardOf("ollama"), "Remove").click();
    await settle();
    expect(ipc.callsTo("settings_remove_connection")[0]?.args).toEqual({
      revision: 7,
      name: "ollama",
    });
    expect(cardOf("ollama")).toBeUndefined();
  });

  it("reads again when the window becomes visible", async () => {
    await start(() => overview());
    document.dispatchEvent(new Event("visibilitychange"));
    await settle();
    expect(ipc.callsTo("settings_overview")).toHaveLength(2);
  });
});
