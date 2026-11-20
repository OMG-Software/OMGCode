import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { neutralise } from "../src/dom";
import { setInvoker } from "../src/ipc/client";
import type {
  ProviderProfileView,
  SettingsOverview,
  WorkspaceSettingsView,
  WriteToolAccess,
} from "../src/ipc/types";
import { HOSTILE } from "../src/mock/fixtures";
import {
  ACCESS_HELP,
  ADVANCED_HELP,
  DEFAULT_CHOICE,
  DEFAULT_ENDPOINTS,
  MODEL_HELP,
  NOTHING_CHANGED,
  PERMISSION_LABELS,
  type SettingsApp,
  startSettings,
  WORKSPACES_HELP,
  WRITE_TOOL_LABELS,
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
const ALL_TOOLS: WriteToolAccess = {
  edit: "allow",
  multi_edit: "allow",
  create: "allow",
  rename: "allow",
  delete: "allow",
};

function workspace(overrides: Partial<WorkspaceSettingsView> = {}): WorkspaceSettingsView {
  return {
    workspace_id: WORKSPACE,
    folder_display: "/home/dev/project",
    model_tools: true,
    read: "allow",
    write: "prompt",
    execute: "deny",
    task_network: "deny",
    host_services: "deny",
    write_tools: { ...ALL_TOOLS },
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

/** Show a section as the operator would, by pressing its navigation button. */
function go(section: string): void {
  const found = Array.from(root.querySelectorAll<HTMLButtonElement>(".nav-item")).find(
    (item) => item.textContent === section,
  );
  if (found === undefined) {
    throw new Error(`No navigation button ${section}`);
  }
  found.click();
}

function group(label: string): HTMLElement {
  const found = root.querySelector(`[role=radiogroup][aria-label="${label}"]`);
  if (!(found instanceof HTMLElement)) {
    throw new Error(`No permission control ${label}`);
  }
  return found;
}

/** The state a Deny, Prompt, Allow control shows as chosen. */
function stateOf(label: string): string | null | undefined {
  return group(label).querySelector("[aria-checked=true]")?.getAttribute("data-value");
}

/** Choose a state in a Deny, Prompt, Allow control as the operator would. */
function choose(label: string, value: string): void {
  const choice = group(label).querySelector<HTMLButtonElement>(`[data-value="${value}"]`);
  if (choice === null) {
    throw new Error(`No ${value} choice in ${label}`);
  }
  choice.click();
}

function workspaceCard(): HTMLElement {
  const found = root.querySelector(".workspace-block");
  if (!(found instanceof HTMLElement)) {
    throw new Error("No workspace block");
  }
  return found;
}

function rowOf(name: string): HTMLElement {
  const found = Array.from(root.querySelectorAll<HTMLElement>(".row")).find(
    (row) => row.querySelector(".row-title")?.textContent === name,
  );
  if (found === undefined) {
    throw new Error(`No row ${name}`);
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

  it("gives each section one short line of help and says what still needs the CLI", async () => {
    await start(() => overview());
    // Workspaces is shown first.
    expect(root.textContent).toContain(WORKSPACES_HELP);
    go("Model");
    expect(root.textContent).toContain(MODEL_HELP);
    go("Advanced");
    expect(root.textContent).toContain(ADVANCED_HELP);
    expect(ADVANCED_HELP).toContain("omgcode workspace");
    expect(ADVANCED_HELP).toContain("omgcode skills");
  });

  it("types no path and offers exactly three access states everywhere", async () => {
    await start(() => overview());
    // No control takes a typed path: folders come only from the system dialog. The only typed
    // values are a new connection name and loopback URL (ADR-0064).
    const typed: Array<string | null> = [];
    for (const section of ["Workspaces", "Model", "Advanced"]) {
      go(section);
      expect(root.querySelectorAll("textarea")).toHaveLength(0);
      for (const input of Array.from(root.querySelectorAll("input"))) {
        if (input.type !== "checkbox") {
          typed.push(input.getAttribute("aria-label"));
        }
      }
    }
    expect(typed).toEqual(["New connection name", "New connection endpoint"]);
    // Two dropdowns choose a provider. Every permission and tool limit is a Deny, Prompt, Allow
    // group, and the three labels are the same everywhere.
    go("Model");
    const providerLabels = ["Model provider", "New connection provider"];
    const selects = Array.from(root.querySelectorAll("select"));
    expect(selects.map((s) => s.ariaLabel)).toEqual(providerLabels);
    go("Workspaces");
    expect(root.querySelectorAll("select")).toHaveLength(0);
    const groups = Array.from(root.querySelectorAll("[role=radiogroup]"));
    expect(groups).toHaveLength(PERMISSION_LABELS.length + WRITE_TOOL_LABELS.length);
    for (const one of groups) {
      expect(one.classList.contains("access")).toBe(true);
      const buttons = Array.from(one.querySelectorAll("button"));
      expect(buttons.map((b) => b.textContent)).toEqual(["Deny", "Prompt", "Allow"]);
      expect(buttons.map((b) => b.getAttribute("data-value"))).toEqual(["deny", "prompt", "allow"]);
    }
    expect(root.textContent).toContain(ACCESS_HELP);
  });

  it("shows each group of the configuration", async () => {
    await start(() => overview());
    expect(root.textContent).toContain("Revision 7");
    expect(root.textContent).toContain("/home/dev/project");
    // Model tools is a checkbox; each other preference is a control showing the recorded state.
    expect(checkbox("model tools").checked).toBe(true);
    expect(PERMISSION_LABELS.map(([, label]) => stateOf(label))).toEqual([
      "allow",
      "prompt",
      "deny",
      "deny",
      "deny",
    ]);
    expect(WRITE_TOOL_LABELS.map(([, label]) => stateOf(label))).toEqual([
      "allow",
      "allow",
      "allow",
      "allow",
      "allow",
    ]);

    go("Model");
    let text = root.textContent ?? "";
    expect(text).toContain("Provider profile lms, set at launch");
    expect(text).toContain("LM Studio");
    expect(text).toContain("stored");

    go("Advanced");
    text = root.textContent ?? "";
    expect(text).toContain("/opt/rust");
    expect(text).toContain("RUSTUP_HOME");
    expect(text).toContain("No package-network profile is recorded.");
    expect(text).toContain("No container grant is recorded.");
    // A skill override names the workspace by its folder.
    const skill = rowOf("review");
    expect(skill.textContent).toContain("/home/dev/project");
    expect(skill.textContent).toContain("disabled");
  });

  it("asks the core to choose the root with the revision it read and shows the reply", async () => {
    await start(() => overview({ development_root: null, workspaces: [] }), {
      settings_choose_root: () => overview({ revision: 8, development_root: "C:\\code" }),
    });
    expect(buttonByText(root, "Register workspace").disabled).toBe(true);
    expect(root.textContent).toContain("Choose a development root first.");
    expect(root.textContent).toContain("No workspace is registered.");

    buttonByText(root, "Choose folder").click();
    await settle();

    // The window names no folder: Rust opens the dialog.
    expect(ipc.callsTo("settings_choose_root")[0]?.args).toEqual({ revision: 7 });
    expect(root.textContent).toContain("C:\\code");
    expect(root.textContent).toContain("Revision 8");
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

  it("sends the complete preference set with one state changed", async () => {
    await start(() => overview(), {
      settings_set_permissions: () =>
        overview({ revision: 8, workspaces: [workspace({ execute: "prompt" })] }),
    });
    choose("Execute", "prompt");
    await settle();

    expect(ipc.callsTo("settings_set_permissions")[0]?.args).toEqual({
      workspace_id: WORKSPACE,
      revision: 7,
      permissions: {
        model_tools: true,
        read: "allow",
        write: "prompt",
        execute: "prompt",
        task_network: "deny",
        host_services: "deny",
        write_tools: ALL_TOOLS,
      },
    });
    expect(stateOf("Execute")).toBe("prompt");
  });

  it("sends a tool limit inside the complete preference set", async () => {
    await start(() => overview(), {
      settings_set_permissions: () =>
        overview({
          revision: 8,
          workspaces: [workspace({ write_tools: { ...ALL_TOOLS, delete: "deny" } })],
        }),
    });
    choose("Delete tool", "deny");
    await settle();

    expect(ipc.callsTo("settings_set_permissions")[0]?.args).toEqual({
      workspace_id: WORKSPACE,
      revision: 7,
      permissions: {
        model_tools: true,
        read: "allow",
        write: "prompt",
        execute: "deny",
        task_network: "deny",
        host_services: "deny",
        write_tools: { ...ALL_TOOLS, delete: "deny" },
      },
    });
    expect(stateOf("Delete tool")).toBe("deny");
    expect(stateOf("Edit tool")).toBe("allow");
  });

  it("sends the model tools checkbox with every state unchanged", async () => {
    await start(() => overview(), {
      settings_set_permissions: () =>
        overview({ revision: 8, workspaces: [workspace({ model_tools: false })] }),
    });
    const tools = checkbox("model tools");
    tools.checked = false;
    tools.dispatchEvent(new Event("change"));
    await settle();
    expect(ipc.callsTo("settings_set_permissions")[0]?.args).toMatchObject({
      permissions: { model_tools: false, read: "allow", write: "prompt", write_tools: ALL_TOOLS },
    });
  });

  it("shows a declined dialog as no change and restores the recorded value", async () => {
    await start(() => overview(), {
      settings_set_permissions: () => rejection("cancelled", "No change was made."),
    });
    choose("Write", "allow");
    await settle();

    const notice = root.querySelector("[role=status]") as HTMLElement;
    expect(notice.hidden).toBe(false);
    expect(notice.textContent).toBe(NOTHING_CHANGED);
    expect((root.querySelector("[role=alert]") as HTMLElement).hidden).toBe(true);
    // The window read the configuration again, so the selector shows what is recorded.
    expect(ipc.callsTo("settings_overview")).toHaveLength(2);
    expect(stateOf("Write")).toBe("prompt");
  });

  it("restores the last read value when reading again after a declined dialog fails", async () => {
    await start(() => overview(), {
      settings_set_permissions: () => rejection("cancelled", "No change was made."),
    });
    ipc.on("settings_overview", () => rejection("io", "The settings could not be read."));
    choose("Write", "allow");
    await settle();

    expect(ipc.callsTo("settings_overview")).toHaveLength(2);
    // The control shows the recorded state again, not what the operator asked for.
    expect(stateOf("Write")).toBe("prompt");
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
    expect(root.textContent).toContain("Revision 9");
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

    // Every control is disabled, the navigation included.
    const items = Array.from(root.querySelectorAll<HTMLButtonElement>(".nav-item"));
    expect(items).toHaveLength(3);
    expect(items.every((item) => item.disabled)).toBe(true);

    // A second click while the first waits sends nothing.
    choose("Write", "allow");
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
            read: "deny",
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
    go("Model");
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
    expect(rowOf("ollama").textContent).toContain("Active");
    expect(rowOf("lms").textContent).not.toContain("Active");
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
    go("Model");
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
    go("Model");
    const provider = picker("Model provider");
    provider.value = "ollama";
    provider.dispatchEvent(new Event("change"));
    await settle();
    expect((root.querySelector("[role=status]") as HTMLElement).textContent).toBe(NOTHING_CHANGED);
    expect(picker("Model provider").value).toBe(DEFAULT_CHOICE);
  });

  it("disables the dropdown when a launch argument fixed the connection", async () => {
    await start(() => overview());
    go("Model");
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
    go("Model");
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
    expect(root.textContent).toContain("Revision 8");
  });

  it("keeps a typed connection across redraws, after a refusal, and clears it after an add", async () => {
    let refuse = true;
    await start(() => choosable({ provider_profiles: [] }), {
      settings_add_local_connection: () =>
        refuse ? rejection("invalid_endpoint", "Bad.") : choosable({ revision: 8 }),
    });
    go("Model");
    const type = (label: string, value: string): void => {
      field(label).value = value;
      field(label).dispatchEvent(new Event("input"));
    };
    type("New connection name", "box");
    type("New connection endpoint", "http://127.0.0.1:5000/v1");
    go("Advanced");
    go("Model");
    buttonByText(root, "Refresh").click();
    await settle();
    expect(field("New connection name").value).toBe("box");
    expect(field("New connection endpoint").value).toBe("http://127.0.0.1:5000/v1");

    buttonByText(root, "Add connection").click();
    await settle();
    expect(field("New connection name").value).toBe("box");

    refuse = false;
    buttonByText(root, "Add connection").click();
    await settle();
    expect(field("New connection name").value).toBe("ollama");
    expect(field("New connection endpoint").value).toBe(DEFAULT_ENDPOINTS.ollama_local);
  });

  it("removes a connection without a credential and leaves a stored one to the CLI", async () => {
    await start(() => choosable(), {
      settings_remove_connection: () => choosable({ revision: 8, provider_profiles: [lms()] }),
    });
    go("Model");
    expect(buttonByText(rowOf("lms"), "Remove").disabled).toBe(true);
    expect(rowOf("lms").textContent).toContain("remove with the trusted CLI");
    buttonByText(rowOf("ollama"), "Remove").click();
    await settle();
    expect(ipc.callsTo("settings_remove_connection")[0]?.args).toEqual({
      revision: 7,
      name: "ollama",
    });
    expect(() => rowOf("ollama")).toThrow();
  });

  it("keeps the active connection when its removal is declined", async () => {
    await start(() => choosable(), {
      settings_remove_connection: () => rejection("cancelled", "The change was not confirmed."),
    });
    go("Model");
    buttonByText(rowOf("ollama"), "Remove").click();
    await settle();
    expect(root.textContent).toContain("No change was made.");
    // The configuration is read again, so the row is still shown.
    expect(ipc.callsTo("settings_overview").length).toBeGreaterThan(1);
    expect(root.textContent).toContain("ollama");
  });

  it("reads again when the window becomes visible", async () => {
    await start(() => overview());
    document.dispatchEvent(new Event("visibilitychange"));
    await settle();
    expect(ipc.callsTo("settings_overview")).toHaveLength(2);
  });
});
