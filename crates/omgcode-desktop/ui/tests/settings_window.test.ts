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
  FOLDER_REPAIRED,
  MODEL_HELP,
  NOTHING_CHANGED,
  PERMISSION_LABELS,
  REPAIR_FOLDER_LABEL,
  RETRY_AFTER_REPAIR,
  type SettingsApp,
  startSettings,
  WORKSPACES_HELP,
  WRITE_TOOL_LABELS,
} from "../src/settings/app";
import {
  buttonByText,
  chatDefaultsView,
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
  create_directory: "allow",
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
    instructions: "",
    instructions_max_bytes: 16_360,
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
    container_grants_supported: true,
    skills: [
      {
        name: "review",
        application: true,
        workspace_overrides: [{ workspace_id: WORKSPACE, enabled: false }],
      },
    ],
    chat: chatDefaultsView(),
    scope: null,
    ...overrides,
  };
}

let app: SettingsApp | null = null;
let root: HTMLElement;
let ipc: FakeIpc;

/** Opens the window on the Workspaces overview, as the operator first sees it. */
async function startOverview(
  reply: () => unknown,
  more: Record<string, (args: Record<string, unknown>) => unknown> = {},
): Promise<void> {
  ipc = installFakeIpc({ settings_overview: reply, ...more });
  root = mountRoot();
  app = startSettings(root);
  await settle();
}

/** Opens the window and then the first workspace listed under Workspaces, when there is one. */
async function start(
  reply: () => unknown,
  more: Record<string, (args: Record<string, unknown>) => unknown> = {},
): Promise<void> {
  await startOverview(reply, more);
  openWorkspace();
}

/** Shows a workspace as the operator would, by pressing its entry under Workspaces. */
function openWorkspace(index = 0): void {
  root.querySelectorAll<HTMLButtonElement>(".nav-child")[index]?.click();
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
    // Every group changes in this window; credentials, fetches, and runs stay with the CLI.
    expect(ADVANCED_HELP).toContain("are changed here");
    expect(ADVANCED_HELP).toContain("trusted CLI");
    expect(ADVANCED_HELP).not.toContain("only through the trusted CLI");
  });

  it("types no path and offers exactly three access states everywhere", async () => {
    await start(() => overview());
    // No control takes a typed path: folders come only from the system dialog. The only typed
    // values are a new connection name, a loopback URL (ADR-0064), the write-only API key of an
    // Ollama Cloud connection (ADR-0071), the chat budgets as numbers, and the global and
    // workspace custom instructions (ADR-0080), and in Advanced a profile name, root and PATH counts,
    // variables and origins (ADR-0088).
    const typed: Array<string | null> = [];
    for (const section of ["Workspaces", "Model", "Advanced"]) {
      go(section);
      if (section === "Workspaces") {
        openWorkspace();
      }
      for (const input of Array.from(root.querySelectorAll("input, textarea"))) {
        if (input instanceof HTMLInputElement && input.type === "checkbox") {
          continue;
        }
        typed.push(input.getAttribute("aria-label") ?? input.id);
        if (input instanceof HTMLInputElement && input.id.startsWith("chat-")) {
          expect(input.type).toBe("number");
        }
      }
    }
    expect(typed).toEqual([
      `workspace-instructions-${WORKSPACE}`,
      "chat-context-tokens",
      "chat-max-output-tokens",
      "chat-max-tool-calls",
      "chat-turn-timeout-seconds",
      "global-instructions",
      "New connection name",
      "New connection endpoint",
      "API key",
      "new-toolchain-name",
      "new-toolchain-roots",
      "new-toolchain-paths",
      "new-toolchain-environment",
      "new-package-name",
      "new-package-origins",
    ]);
    // Two dropdowns choose a provider. Every permission, tool limit, and the Set all control for
    // the tool limits is a Deny, Prompt, Allow group, and the three labels are the same everywhere.
    go("Model");
    const providerLabels = ["Model provider", "New connection provider"];
    const selects = Array.from(root.querySelectorAll("select"));
    expect(selects.map((s) => s.ariaLabel)).toEqual(providerLabels);
    go("Workspaces");
    openWorkspace();
    expect(root.querySelectorAll("select")).toHaveLength(0);
    const groups = Array.from(root.querySelectorAll("[role=radiogroup]"));
    expect(groups).toHaveLength(PERMISSION_LABELS.length + WRITE_TOOL_LABELS.length + 1);
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
    const [application, inWorkspace] = Array.from(skill.querySelectorAll("select"));
    expect(application?.value).toBe("enabled");
    expect(inWorkspace?.value).toBe("disabled");
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
    await startOverview(() => overview(), {
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
    const alert = root.querySelector(".read-error") as HTMLElement;
    expect(alert.hidden).toBe(false);
    expect(alert.textContent).toBe("The settings could not be read. (io)");
    expect(body().disabled).toBe(false);
  });

  it("keeps a change's error and its failed read apart, and a later read clears only the read's", async () => {
    await startOverview(() => overview(), {
      settings_choose_root: () =>
        rejection("stale_revision", "The settings changed since this window read them."),
    });
    // The store stays busy through the change and the read that follows it.
    ipc.on("settings_overview", () => rejection("busy", "The settings store is in use."));
    buttonByText(root, "Change").click();
    await settle();

    const change = root.querySelector(".change-error") as HTMLElement;
    const read = root.querySelector(".read-error") as HTMLElement;
    expect(change.hidden).toBe(false);
    expect(change.textContent).toContain("(stale_revision)");
    expect(read.hidden).toBe(false);
    expect(read.textContent).toBe("The settings store is in use. (busy)");

    // The store is free again: Refresh reads it, hides the read failure, keeps the change's.
    ipc.on("settings_overview", () => overview({ revision: 9 }));
    buttonByText(root, "Refresh").click();
    await settle();
    expect(read.hidden).toBe(true);
    expect(change.hidden).toBe(false);
    expect(change.textContent).toContain("(stale_revision)");
  });

  it("shows a refusal and reads the configuration again", async () => {
    await startOverview(() => overview(), {
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
    await startOverview(() => overview(), {
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
    // The workspace entries and the group toggle are disabled too.
    const group = Array.from(root.querySelectorAll<HTMLButtonElement>(".nav-child, .nav-toggle"));
    expect(group).toHaveLength(2);
    expect(group.every((item) => item.disabled)).toBe(true);

    // A second click while the first waits sends nothing.
    buttonByText(root, "Register workspace").click();
    await settle();
    expect(ipc.callsTo("settings_register_workspace")).toHaveLength(0);

    answer(overview({ revision: 8 }));
    await settle();
    expect(body().disabled).toBe(false);
    expect(
      Array.from(root.querySelectorAll<HTMLButtonElement>(".nav-child, .nav-toggle")).every(
        (item) => !item.disabled,
      ),
    ).toBe(true);
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

  it("removes a workspace by identifier and revision, and shows none afterwards", async () => {
    await start(() => overview(), {
      settings_unregister_workspace: () => overview({ revision: 8, workspaces: [] }),
    });
    expect(workspaceCard().textContent).toContain("The folder and its files are not touched.");

    buttonByText(workspaceCard(), "Remove workspace…").click();
    await settle();

    // The call carries the identifier and the revision the window read, never a path.
    expect(ipc.callsTo("settings_unregister_workspace")[0]?.args).toEqual({
      workspace_id: WORKSPACE,
      revision: 7,
    });
    expect(root.querySelector(".workspace-block")).toBeNull();
    expect(root.textContent).toContain("No workspace is registered.");
  });

  it("keeps the workspace when the operator declines or a turn is running", async () => {
    await start(() => overview(), {
      settings_unregister_workspace: () => rejection("cancelled", "No change was made."),
    });
    buttonByText(workspaceCard(), "Remove workspace…").click();
    await settle();
    expect(workspaceCard().textContent).toContain("/home/dev/project");

    ipc.on("settings_unregister_workspace", () =>
      rejection("turn_running", "A chat turn is running in a session of this workspace."),
    );
    buttonByText(workspaceCard(), "Remove workspace…").click();
    await settle();
    expect(root.textContent).toContain("A chat turn is running in a session of this workspace.");
    expect(workspaceCard().textContent).toContain("/home/dev/project");
  });

  it("offers no removal in the view scoped to one session's workspace", async () => {
    await start(() => overview({ scope: { workspace_id: WORKSPACE } }));
    expect(workspaceCard().textContent).toContain("/home/dev/project");
    expect(() => buttonByText(root, "Remove workspace…")).toThrow();
  });

  it("renders hostile recorded text as text", async () => {
    await startOverview(() =>
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
    // The navigation's tooltip is an attribute, which `el` does not neutralise for it.
    const navItem = root.querySelector(".nav-child") as HTMLElement;
    expect(navItem.title).not.toContain(String.fromCharCode(0x202e));
    expect(navItem.title).toBe(navItem.textContent);
    openWorkspace();
    expect(root.querySelector("img, script")).toBeNull();
    expect(root.textContent).toContain(HOSTILE.script);
  });

  it("shows the core error and keeps the Refresh button", async () => {
    await start(() => rejection("busy", "The settings store is in use."));
    const alert = root.querySelector(".read-error") as HTMLElement;
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
      settings_add_connection: () => choosable({ revision: 8 }),
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
    expect(ipc.callsTo("settings_add_connection")[0]?.args).toEqual({
      revision: 7,
      name: "local",
      kind: "ollama_local",
      endpoint: "http://127.0.0.1:4000/v1",
      api_key: null,
    });
    expect(root.textContent).toContain("Revision 8");
  });

  it("keeps a typed connection across redraws, after a refusal, and clears it after an add", async () => {
    let refuse = true;
    await start(() => choosable({ provider_profiles: [] }), {
      settings_add_connection: () =>
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

  it("removes a connection, including one with a stored credential", async () => {
    await start(() => choosable(), {
      settings_remove_connection: () => choosable({ revision: 8, provider_profiles: [lms()] }),
    });
    go("Model");
    // A bound profile can be removed: Rust asks a system Yes/No and deletes the key with it.
    expect(buttonByText(rowOf("lms"), "Remove").disabled).toBe(false);
    expect(rowOf("lms").textContent).toContain("Credential stored");
    buttonByText(rowOf("ollama"), "Remove").click();
    await settle();
    expect(ipc.callsTo("settings_remove_connection")[0]?.args).toEqual({
      revision: 7,
      name: "ollama",
    });
    expect(() => rowOf("ollama")).toThrow();
  });

  describe("Ollama Cloud", () => {
    const KEY = "test-key-not-real";

    function cloud(overrides: Partial<ProviderProfileView> = {}): ProviderProfileView {
      return {
        name: "hosted",
        kind: "ollama_cloud",
        auth: "api_key",
        endpoint: "https://ollama.com/v1",
        credential_bound: true,
        selectable: true,
        active: false,
        ...overrides,
      };
    }

    /** Every place the page could hold the key: text, attributes, and each field's value. */
    function pageHolds(text: string): boolean {
      const fields = Array.from(root.querySelectorAll("input, textarea")).map(
        (input) => (input as HTMLInputElement).value,
      );
      return root.innerHTML.includes(text) || fields.some((value) => value.includes(text));
    }

    function chooseCloud(): void {
      const kind = picker("New connection provider");
      kind.value = "ollama_cloud";
      kind.dispatchEvent(new Event("change"));
    }

    function type(label: string, value: string): void {
      field(label).value = value;
      field(label).dispatchEvent(new Event("input"));
    }

    it("hides the endpoint, shows the fixed one, and takes the key in a password field", async () => {
      await start(() => choosable({ provider_profiles: [] }));
      go("Model");
      const endpoint = field("New connection endpoint");
      expect(endpoint.closest(".field")?.hasAttribute("hidden")).toBe(false);
      expect(field("API key").closest(".field")?.hasAttribute("hidden")).toBe(true);
      chooseCloud();
      expect(endpoint.closest(".field")?.hasAttribute("hidden")).toBe(true);
      const key = field("API key");
      expect(key.closest(".field")?.hasAttribute("hidden")).toBe(false);
      expect(key.type).toBe("password");
      expect(key.getAttribute("autocomplete")).toBe("off");
      expect(root.querySelector("#new-connection-fixed-endpoint")?.textContent).toContain(
        "https://ollama.com/v1",
      );
      expect(root.textContent).toContain("leave this computer for ollama.com");
      expect(field("New connection name").value).toBe("ollama-cloud");
      // Switching back shows the endpoint again and hides the key.
      const kind = picker("New connection provider");
      kind.value = "ollama_local";
      kind.dispatchEvent(new Event("change"));
      expect(endpoint.closest(".field")?.hasAttribute("hidden")).toBe(false);
      expect(field("API key").closest(".field")?.hasAttribute("hidden")).toBe(true);
    });

    it("sends the key once, with no endpoint, and clears the field after a success", async () => {
      await start(() => choosable({ provider_profiles: [] }), {
        settings_add_connection: () => choosable({ revision: 8, provider_profiles: [cloud()] }),
      });
      go("Model");
      chooseCloud();
      type("New connection name", "hosted");
      field("API key").value = KEY;
      buttonByText(root, "Add connection").click();
      // The field is empty before the answer arrives.
      expect(field("API key").value).toBe("");
      await settle();
      expect(ipc.callsTo("settings_add_connection")).toHaveLength(1);
      expect(ipc.callsTo("settings_add_connection")[0]?.args).toEqual({
        revision: 7,
        name: "hosted",
        kind: "ollama_cloud",
        endpoint: null,
        api_key: KEY,
      });
      expect(field("API key").value).toBe("");
      expect(pageHolds(KEY)).toBe(false);
      // No other call carried it.
      const carrying = ipc.calls.filter((call) => JSON.stringify(call.args).includes(KEY));
      expect(carrying).toHaveLength(1);
      expect(rowOf("hosted").textContent).toContain("API key stored");
      expect(rowOf("hosted").textContent).toContain("leave this computer for ollama.com");
      expect(root.textContent).toContain("Revision 8");
    });

    it("lists every provider and sends a named hosted one with its key and no endpoint", async () => {
      await start(() => choosable({ provider_profiles: [] }), {
        settings_add_connection: () =>
          choosable({
            revision: 8,
            provider_profiles: [
              cloud({ name: "deep", kind: "deepseek", endpoint: "https://api.deepseek.com" }),
            ],
          }),
      });
      go("Model");
      const kind = picker("New connection provider");
      expect(Array.from(kind.options).map((option) => option.value)).toEqual([
        "ollama_local",
        "lm_studio",
        "ollama_cloud",
        "anthropic",
        "openai",
        "google",
        "meta",
        "moonshot",
        "tencent",
        "thinking_machines",
        "minimax",
        "open_router",
        "z_ai",
        "deepseek",
        "xai",
        "openai_compatible",
      ]);
      kind.value = "deepseek";
      kind.dispatchEvent(new Event("change"));
      expect(field("New connection endpoint").closest(".field")?.hasAttribute("hidden")).toBe(true);
      expect(root.querySelector("#new-connection-fixed-endpoint")?.textContent).toContain(
        "https://api.deepseek.com",
      );
      expect(root.textContent).toContain("leave this computer for api.deepseek.com");
      expect(field("New connection name").value).toBe("deepseek");
      field("API key").value = KEY;
      buttonByText(root, "Add connection").click();
      expect(field("API key").value).toBe("");
      await settle();
      expect(ipc.callsTo("settings_add_connection")[0]?.args).toEqual({
        revision: 7,
        name: "deepseek",
        kind: "deepseek",
        endpoint: null,
        api_key: KEY,
      });
      expect(rowOf("deep").textContent).toContain("DeepSeek");
      expect(rowOf("deep").textContent).toContain("leave this computer for api.deepseek.com");
      expect(pageHolds(KEY)).toBe(false);
    });

    it("sends an OpenAI-compatible service with its typed address and key", async () => {
      await start(() => choosable({ provider_profiles: [] }), {
        settings_add_connection: () => choosable({ revision: 8, provider_profiles: [] }),
      });
      go("Model");
      const kind = picker("New connection provider");
      kind.value = "openai_compatible";
      kind.dispatchEvent(new Event("change"));
      const endpoint = field("New connection endpoint");
      // The local default address is cleared, never sent to a hosted service.
      expect(endpoint.value).toBe("");
      expect(endpoint.closest(".field")?.hasAttribute("hidden")).toBe(false);
      expect(root.querySelector("#new-connection-fixed-endpoint")?.hasAttribute("hidden")).toBe(
        true,
      );
      expect(field("API key").closest(".field")?.hasAttribute("hidden")).toBe(false);
      expect(root.textContent).toContain("leave this computer for the address you enter");
      type("New connection endpoint", " https://provider-api.example.org/v1 ");
      field("API key").value = KEY;
      buttonByText(root, "Add connection").click();
      await settle();
      expect(ipc.callsTo("settings_add_connection")[0]?.args).toEqual({
        revision: 7,
        name: "custom",
        kind: "openai_compatible",
        endpoint: "https://provider-api.example.org/v1",
        api_key: KEY,
      });
      expect(pageHolds(KEY)).toBe(false);
    });

    it("clears the key after a refusal and never shows it in the error", async () => {
      await start(() => choosable({ provider_profiles: [] }), {
        settings_add_connection: () =>
          rejection("credential_locked", "The credential store is locked."),
      });
      go("Model");
      chooseCloud();
      field("API key").value = KEY;
      buttonByText(root, "Add connection").click();
      expect(field("API key").value).toBe("");
      await settle();
      expect(root.textContent).toContain("credential_locked");
      expect(field("API key").value).toBe("");
      expect(pageHolds(KEY)).toBe(false);
      // The form keeps its choice of provider and name, but never the key.
      expect(picker("New connection provider").value).toBe("ollama_cloud");
    });

    it("offers to remove the data folder's extra permissions and sends nothing with it", async () => {
      await start(() => choosable({ provider_profiles: [] }), {
        settings_add_connection: () =>
          rejection(
            "credential_storage_not_private",
            "OMGCode's application data folder grants access to another account.",
          ),
        settings_repair_data_folder: () => choosable({ provider_profiles: [] }),
      });
      go("Model");
      chooseCloud();
      type("New connection name", "hosted");
      field("API key").value = KEY;
      buttonByText(root, "Add connection").click();
      await settle();
      expect(root.textContent).toContain("credential_storage_not_private");
      expect(ipc.callsTo("settings_repair_data_folder")).toHaveLength(0);

      buttonByText(root, REPAIR_FOLDER_LABEL).click();
      await settle();
      // Rust reads the folder and lists the accounts itself; the window names nothing.
      expect(ipc.callsTo("settings_repair_data_folder")).toEqual([
        { command: "settings_repair_data_folder", args: {} },
      ]);
      expect(repairedNoticeShown()).toBe(true);
      expect(root.textContent).toContain(RETRY_AFTER_REPAIR);
      expect(root.querySelector<HTMLElement>(".change-error")?.hidden).toBe(true);
      expect(pageHolds(KEY)).toBe(false);
    });

    /** Fails one add with the shared-folder error, so the repair button is offered. */
    async function offerFolderRepair(repairs: Array<() => unknown>): Promise<void> {
      let attempt = 0;
      await start(() => choosable({ provider_profiles: [] }), {
        settings_add_connection: () =>
          rejection("credential_storage_not_private", "The data folder is not private."),
        settings_repair_data_folder: () => {
          const next = repairs[attempt] ?? repairs[repairs.length - 1];
          attempt += 1;
          return next?.();
        },
      });
      go("Model");
      chooseCloud();
      type("New connection name", "hosted");
      field("API key").value = KEY;
      buttonByText(root, "Add connection").click();
      await settle();
    }

    /** Visible, not only present: `textContent` also counts hidden elements. */
    function repairedNoticeShown(): boolean {
      const notice = root.querySelector<HTMLElement>(".notice");
      return notice?.hidden === false && (notice.textContent ?? "").includes(FOLDER_REPAIRED);
    }

    function repairOffered(): boolean {
      const problem = root.querySelector<HTMLElement>(".change-error");
      return problem?.hidden === false && (problem.textContent ?? "").includes(REPAIR_FOLDER_LABEL);
    }

    it("keeps offering the folder repair after the operator declines it", async () => {
      await offerFolderRepair([
        () => rejection("cancelled", "No change was made."),
        () => choosable({ provider_profiles: [] }),
      ]);
      buttonByText(root, REPAIR_FOLDER_LABEL).click();
      await settle();
      expect(root.textContent).toContain(NOTHING_CHANGED);
      expect(root.textContent).not.toContain(FOLDER_REPAIRED);
      expect(root.textContent).toContain("credential_storage_not_private");
      expect(repairOffered()).toBe(true);

      // The operator can try again without repeating the change that found the problem.
      buttonByText(root, REPAIR_FOLDER_LABEL).click();
      await settle();
      expect(ipc.callsTo("settings_repair_data_folder")).toHaveLength(2);
      expect(repairedNoticeShown()).toBe(true);
      expect(repairOffered()).toBe(false);
    });

    it("keeps offering the folder repair when the folder changed after review", async () => {
      await offerFolderRepair([
        () =>
          rejection(
            "data_folder_changed",
            "The data folder's permissions changed after you reviewed them.",
          ),
      ]);
      buttonByText(root, REPAIR_FOLDER_LABEL).click();
      await settle();
      expect(root.textContent).toContain("data_folder_changed");
      expect(repairOffered()).toBe(true);
    });

    it("keeps offering the folder repair when the folder's permissions could not be checked", async () => {
      await offerFolderRepair([
        () =>
          rejection(
            "data_folder_unreadable",
            "OMGCode could not check the permissions of its application data folder.",
          ),
      ]);
      buttonByText(root, REPAIR_FOLDER_LABEL).click();
      await settle();
      expect(root.textContent).toContain("data_folder_unreadable");
      expect(repairOffered()).toBe(true);
    });

    it("keeps offering the folder repair when another settings change was running", async () => {
      await offerFolderRepair([
        () => rejection("settings_change_running", "Another settings change is running."),
      ]);
      buttonByText(root, REPAIR_FOLDER_LABEL).click();
      await settle();
      expect(root.textContent).toContain("settings_change_running");
      expect(repairOffered()).toBe(true);
    });

    it("reports a completed folder repair even when the settings could not be read again", async () => {
      await offerFolderRepair([
        () =>
          rejection(
            "data_folder_repaired",
            "OMGCode removed the extra permissions but could not read its settings again.",
          ),
      ]);
      buttonByText(root, REPAIR_FOLDER_LABEL).click();
      await settle();
      expect(repairedNoticeShown()).toBe(true);
      expect(root.textContent).toContain("data_folder_repaired");
      // The error says to reopen the window, so the notice gives no other next step.
      expect(root.textContent).not.toContain(RETRY_AFTER_REPAIR);
      // The folder is fixed, so there is nothing left to repair.
      expect(root.textContent).not.toContain(REPAIR_FOLDER_LABEL);
    });

    it("stops offering the folder repair when OMGCode cannot make it", async () => {
      await offerFolderRepair([
        () =>
          rejection(
            "data_folder_not_repairable",
            "OMGCode cannot fix the data folder's permissions.",
          ),
      ]);
      buttonByText(root, REPAIR_FOLDER_LABEL).click();
      await settle();
      expect(root.textContent).toContain("data_folder_not_repairable");
      expect(root.textContent).not.toContain(REPAIR_FOLDER_LABEL);
    });

    it("offers no folder repair for other credential failures", async () => {
      await start(() => choosable({ provider_profiles: [] }), {
        settings_add_connection: () =>
          rejection("credential_unavailable", "No protected credential store is available."),
      });
      go("Model");
      chooseCloud();
      type("New connection name", "hosted");
      field("API key").value = KEY;
      buttonByText(root, "Add connection").click();
      await settle();
      expect(root.textContent).toContain("credential_unavailable");
      expect(root.textContent).not.toContain(REPAIR_FOLDER_LABEL);
    });

    it("keeps an unsent key when focus returns and the configuration has not changed", async () => {
      let revision = 7;
      await start(() => choosable({ revision, provider_profiles: [] }));
      go("Model");
      // The operator opens Add connection, as the form is collapsed until then.
      const form = field("API key").closest("details") as HTMLDetailsElement;
      form.open = true;
      form.dispatchEvent(new Event("toggle"));
      chooseCloud();
      const key = field("API key");
      // The operator pasted a key, then left to find something and came back.
      type("API key", KEY);
      window.dispatchEvent(new Event("focus"));
      await settle();
      expect(ipc.callsTo("settings_overview").length).toBeGreaterThan(1);
      expect(field("API key")).toBe(key);
      expect(key.value).toBe(KEY);

      // A configuration that did change is drawn again, which shows what is recorded; the
      // unsent key moves to the new field of the same form, and nowhere else.
      revision = 8;
      window.dispatchEvent(new Event("focus"));
      await settle();
      expect(root.textContent).toContain("Revision 8");
      expect(field("API key")).not.toBe(key);
      expect(field("API key").value).toBe(KEY);
      expect(key.value).toBe("");
      expect(root.innerHTML.includes(KEY)).toBe(false);
      expect(ipc.calls.some((call) => JSON.stringify(call.args).includes(KEY))).toBe(false);
    });

    it("keeps an unsent Set key entry across a change, and drops it with its connection", async () => {
      let profiles = [cloud()];
      let revision = 7;
      await start(() => choosable({ revision, provider_profiles: profiles }));
      go("Model");
      const disclosure = rowOf("hosted").querySelector("details") as HTMLDetailsElement;
      disclosure.open = true;
      const input = rowOf("hosted").querySelector("input[type=password]") as HTMLInputElement;
      input.value = KEY;

      // Another setting changed (through the CLI): the row is drawn again and keeps the entry.
      revision = 8;
      window.dispatchEvent(new Event("focus"));
      await settle();
      const again = rowOf("hosted").querySelector("input[type=password]") as HTMLInputElement;
      expect(again).not.toBe(input);
      expect(again.value).toBe(KEY);
      expect((rowOf("hosted").querySelector("details") as HTMLDetailsElement).open).toBe(true);

      // Closing the form drops its key at once.
      const open = rowOf("hosted").querySelector("details") as HTMLDetailsElement;
      open.open = false;
      open.dispatchEvent(new Event("toggle"));
      expect(pageHolds(KEY)).toBe(false);

      // A key left in a closed form is not carried into the redraw, and the form stays closed.
      (rowOf("hosted").querySelector("input[type=password]") as HTMLInputElement).value = KEY;
      revision = 9;
      window.dispatchEvent(new Event("focus"));
      await settle();
      expect(pageHolds(KEY)).toBe(false);
      expect((rowOf("hosted").querySelector("details") as HTMLDetailsElement).open).toBe(false);

      // Typed again into the open form, it survives one more change.
      const reopened = rowOf("hosted").querySelector("details") as HTMLDetailsElement;
      reopened.open = true;
      (rowOf("hosted").querySelector("input[type=password]") as HTMLInputElement).value = KEY;
      revision = 10;
      window.dispatchEvent(new Event("focus"));
      await settle();
      expect(
        (rowOf("hosted").querySelector("input[type=password]") as HTMLInputElement).value,
      ).toBe(KEY);

      // The connection was removed: its form is gone, and so is the key.
      revision = 11;
      profiles = [];
      window.dispatchEvent(new Event("focus"));
      await settle();
      expect(pageHolds(KEY)).toBe(false);
    });

    it("keeps a change's error when a read queued during the change follows it", async () => {
      let fail: (reason: unknown) => void = () => undefined;
      await start(() => choosable({ provider_profiles: [cloud()] }), {
        settings_set_api_key: () =>
          new Promise((_, reject) => {
            fail = reject;
          }),
      });
      go("Model");
      const input = rowOf("hosted").querySelector<HTMLInputElement>("input[type=password]");
      if (input === null) {
        throw new Error("No key field");
      }
      input.value = KEY;
      buttonByText(rowOf("hosted"), "Save key").click();
      await settle();
      // Focus returns while the change runs, as it does when a native dialog closes.
      window.dispatchEvent(new Event("focus"));
      await settle();
      const reads = ipc.callsTo("settings_overview").length;
      fail({
        code: "credential_binding_uncertain",
        message: "The API key was stored as 0123456789ab4def8123456789abcdef, but it was kept.",
      });
      await settle();

      // The queued read ran, and the change's error, with the identifier, is still shown.
      expect(ipc.callsTo("settings_overview").length).toBeGreaterThan(reads);
      const problem = Array.from(root.querySelectorAll<HTMLElement>("[role=alert]")).find((node) =>
        node.textContent?.includes("credential_binding_uncertain"),
      );
      expect(problem?.hidden).toBe(false);
      expect(problem?.textContent).toContain("0123456789ab4def8123456789abcdef");
    });

    it("sets a new key from the row, once, and clears the field either way", async () => {
      let refuse = false;
      await start(() => choosable({ provider_profiles: [cloud()] }), {
        settings_set_api_key: () =>
          refuse
            ? rejection("invalid_request", "The API key is empty.")
            : choosable({ revision: 8, provider_profiles: [cloud()] }),
      });
      go("Model");
      const row = rowOf("hosted");
      expect(row.textContent).toContain("Set key\u2026");
      expect(row.textContent).toContain("Hosted");
      const input = row.querySelector<HTMLInputElement>("input[type=password]");
      expect(input?.getAttribute("autocomplete")).toBe("off");
      if (input === null) {
        throw new Error("No key field");
      }
      input.value = KEY;
      buttonByText(row, "Save key").click();
      expect(input.value).toBe("");
      await settle();
      expect(ipc.callsTo("settings_set_api_key")).toHaveLength(1);
      expect(ipc.callsTo("settings_set_api_key")[0]?.args).toEqual({
        revision: 7,
        name: "hosted",
        api_key: KEY,
      });
      expect(pageHolds(KEY)).toBe(false);
      expect(root.textContent).toContain("Revision 8");

      refuse = true;
      const again = rowOf("hosted").querySelector<HTMLInputElement>("input[type=password]");
      if (again === null) {
        throw new Error("No key field");
      }
      again.value = KEY;
      buttonByText(rowOf("hosted"), "Save key").click();
      expect(again.value).toBe("");
      await settle();
      expect(root.textContent).toContain("invalid_request");
      expect(pageHolds(KEY)).toBe(false);
      expect(ipc.callsTo("settings_set_api_key")).toHaveLength(2);
    });

    it("offers Remove for a profile with a stored key and sends the removal", async () => {
      await start(() => choosable({ provider_profiles: [cloud()] }), {
        settings_remove_connection: () => choosable({ revision: 8, provider_profiles: [] }),
      });
      go("Model");
      const remove = buttonByText(rowOf("hosted"), "Remove");
      expect(remove.disabled).toBe(false);
      remove.click();
      await settle();
      expect(ipc.callsTo("settings_remove_connection")[0]?.args).toEqual({
        revision: 7,
        name: "hosted",
      });
      expect(() => rowOf("hosted")).toThrow();
    });

    it("has no Set key control on a local profile", async () => {
      await start(() => choosable());
      go("Model");
      expect(rowOf("lms").textContent).not.toContain("Set key");
      expect(rowOf("lms").querySelector("input")).toBeNull();
    });
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
