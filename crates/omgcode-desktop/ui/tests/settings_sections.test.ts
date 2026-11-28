import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setInvoker } from "../src/ipc/client";
import type {
  ProviderProfileView,
  SettingsOverview,
  ToolchainProfileView,
  WorkspaceSettingsView,
  WriteToolAccess,
} from "../src/ipc/types";
import { NOTHING_CHANGED, type SettingsApp, startSettings } from "../src/settings/app";
import { ROOM_UNKNOWN, workspaceTooLong } from "../src/settings/chat_section";
import { NO_ENDPOINT_NOTE, UNSUPPORTED_AUTH_NOTE } from "../src/settings/model_section";
import {
  ADD_HOST_LABEL,
  HOST_INPUT_LABEL,
  HOSTS_FULL,
  MAX_WEB_HOSTS,
  WEB_HOSTS_EMPTY,
  WEB_HOSTS_HELP,
  webHostMessage,
} from "../src/settings/web_hosts";
import { PROFILES_EMPTY, PROFILES_HELP, useProfileLabel } from "../src/settings/workspace_profiles";
import {
  ALL_TOOLS_LABEL,
  PACKAGE_CACHE_HELP,
  PERMISSION_LABELS,
} from "../src/settings/workspaces_section";
import {
  buttonByLabel,
  buttonByText,
  chatDefaultsView,
  type FakeIpc,
  installFakeIpc,
  mountRoot,
  rejection,
  settle,
} from "./helpers";

const A = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const B = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const ALL_TOOLS: WriteToolAccess = {
  edit: "allow",
  multi_edit: "allow",
  create: "allow",
  rename: "allow",
  delete: "allow",
  create_directory: "allow",
};

function workspace(id: string, folder: string, extra: Partial<WorkspaceSettingsView> = {}) {
  return {
    workspace_id: id,
    folder_display: folder,
    model_tools: true,
    read: "allow",
    write: "prompt",
    execute: "deny",
    task_network: "deny",
    host_services: "deny",
    web_fetch: "deny",
    web_fetch_hosts: [],
    profiles: [],
    write_tools: { ...ALL_TOOLS },
    package_cache_root: null,
    instructions: "",
    instructions_max_bytes: 16_360,
    ...extra,
  } satisfies WorkspaceSettingsView;
}

function profile(overrides: Partial<ProviderProfileView> = {}): ProviderProfileView {
  return {
    name: "ollama",
    kind: "ollama_local",
    auth: "none",
    endpoint: "http://127.0.0.1:11434/v1",
    credential_bound: false,
    selectable: true,
    active: false,
    account_label: null,
    ...overrides,
  };
}

function overview(overrides: Partial<SettingsOverview> = {}): SettingsOverview {
  return {
    revision: 3,
    development_root: "/home/dev",
    workspaces: [workspace(A, "/home/dev/a"), workspace(B, "/home/dev/b", { write: "allow" })],
    connection: {
      endpoint: "http://127.0.0.1:1234/v1",
      source: "active",
      profile: null,
      kind: null,
      selectable: true,
    },
    provider_profiles: [profile()],
    toolchain_profiles: [],
    profile_platform: "linux",
    package_profiles: [],
    container_grants: [],
    container_grants_supported: true,
    skills: [],
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
  app?.dispose();
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
  root.querySelector<HTMLButtonElement>(".nav-child")?.click();
}

function navItems(): HTMLButtonElement[] {
  return Array.from(root.querySelectorAll<HTMLButtonElement>(".nav-item"));
}

function go(label: string): void {
  const found = navItems().find((item) => item.textContent === label);
  if (found === undefined) {
    throw new Error(`No navigation button ${label}`);
  }
  found.click();
}

function current(): string | null {
  const marked = navItems().filter((item) => item.getAttribute("aria-current") === "page");
  return marked.length === 1 ? (marked[0]?.textContent ?? null) : null;
}

/** The entries under Workspaces in the navigation, one per registered workspace. */
function children(): HTMLButtonElement[] {
  return Array.from(root.querySelectorAll<HTMLButtonElement>(".nav-child"));
}

function pick(select: HTMLSelectElement, value: string): void {
  select.value = value;
  select.dispatchEvent(new Event("change"));
}

function child(id: string): HTMLButtonElement {
  const found = children().find((item) => item.dataset.workspaceId === id);
  if (found === undefined) {
    throw new Error(`No workspace entry ${id}`);
  }
  return found;
}

/** Shows a workspace as the operator would, by pressing its entry. */
function openChild(id: string): void {
  child(id).click();
}

function toggle(): HTMLButtonElement {
  return root.querySelector(".nav-toggle") as HTMLButtonElement;
}

function shownChild(): string | null {
  const marked = children().filter((item) => item.getAttribute("aria-current") === "page");
  return marked.length === 1 ? (marked[0]?.dataset.workspaceId ?? null) : null;
}

function choose(label: string, value: string): void {
  const group = root.querySelector(`[role=radiogroup][aria-label="${label}"]`);
  const choice = group?.querySelector<HTMLButtonElement>(`[data-value="${value}"]`);
  if (!choice) {
    throw new Error(`No ${value} choice in ${label}`);
  }
  choice.click();
}

function heading(): string | null | undefined {
  return root.querySelector(".workspace-title")?.textContent;
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

describe("settings navigation", () => {
  it("lists three sections, starts on Workspaces, and marks the shown one", async () => {
    await startOverview(() => overview());
    expect(navItems().map((item) => item.textContent)).toEqual(["Model", "Workspaces", "Advanced"]);
    expect(current()).toBe("Workspaces");
    expect(root.querySelector("h2")?.textContent).toBe("Workspaces");

    go("Model");
    expect(current()).toBe("Model");
    expect(root.querySelector("h2")?.textContent).toBe("Model");
    expect(root.querySelector(".workspace-block")).toBeNull();

    go("Advanced");
    expect(current()).toBe("Advanced");
    expect(root.querySelector("h2")?.textContent).toBe("Advanced");
    // Switching sections reads nothing and sends nothing.
    expect(ipc.calls.map((call) => call.command)).toEqual(["settings_overview"]);
  });

  it("puts Refresh and the revision in the navigation", async () => {
    await start(() => overview());
    const nav = root.querySelector("nav") as HTMLElement;
    expect(nav.getAttribute("aria-label")).toBe("Settings sections");
    expect(buttonByText(nav, "Refresh")).toBeDefined();
    expect(nav.textContent).toContain("Revision 3");
  });

  it("keeps the section across a refresh, a visibility read, and a change", async () => {
    await start(() => overview(), {
      settings_select_connection: () => overview({ revision: 4 }),
    });
    go("Model");
    ipc.on("settings_overview", () => overview({ revision: 5 }));
    buttonByText(root, "Refresh").click();
    await settle();
    expect(current()).toBe("Model");
    expect(root.textContent).toContain("Revision 5");

    document.dispatchEvent(new Event("visibilitychange"));
    await settle();
    expect(current()).toBe("Model");

    const provider = root.querySelector("#provider-picker") as HTMLSelectElement;
    pick(provider, "ollama");
    await settle();
    expect(ipc.callsTo("settings_select_connection")).toHaveLength(1);
    expect(current()).toBe("Model");
    expect(root.textContent).toContain("Revision 4");
  });

  it("keeps the section after a refused change that reads the settings again", async () => {
    await start(() => overview(), {
      settings_choose_root: () => {
        throw { code: "stale_revision", message: "The settings changed." };
      },
    });
    go("Advanced");
    go("Workspaces");
    buttonByText(root, "Change").click();
    await settle();
    expect(current()).toBe("Workspaces");
    expect((root.querySelector("[role=alert]") as HTMLElement).hidden).toBe(false);
  });

  it("uses no web storage", async () => {
    const before = [localStorage.length, sessionStorage.length];
    await start(() => overview());
    go("Model");
    expect([localStorage.length, sessionStorage.length]).toEqual(before);
  });
});

describe("workspaces in the navigation", () => {
  it("shows the overview first, with the root and no workspace block", async () => {
    await startOverview(() => overview());
    expect(current()).toBe("Workspaces");
    expect(root.querySelector("h2")?.textContent).toBe("Workspaces");
    expect(root.textContent).toContain("Development root");
    expect(root.textContent).toContain("Register workspace");
    expect(root.querySelector(".workspace-block")).toBeNull();
    expect(root.querySelector("select")).toBeNull();
    expect(shownChild()).toBeNull();
  });

  it("lists one entry per workspace with its full text in a tooltip", async () => {
    await startOverview(() => overview());
    expect(
      children().map((item) => [item.dataset.workspaceId, item.textContent, item.title]),
    ).toEqual([
      [A, "/home/dev/a", "/home/dev/a"],
      [B, "/home/dev/b", "/home/dev/b"],
    ]);
    expect(toggle().getAttribute("aria-expanded")).toBe("true");
    expect(toggle().getAttribute("aria-controls")).toBe(root.querySelector(".nav-children")?.id);
    expect(toggle().querySelector("svg.icon")).not.toBeNull();
  });

  it("lists a single workspace too, and shows an empty line and no group for none", async () => {
    await startOverview(() => overview({ workspaces: [workspace(A, "/home/dev/a")] }));
    expect(children()).toHaveLength(1);
    openChild(A);
    expect(heading()).toBe("/home/dev/a");

    await startOverview(() => overview({ workspaces: [] }));
    expect(children()).toHaveLength(0);
    expect(toggle().hidden).toBe(true);
    expect(root.querySelector(".empty")?.textContent).toBe("No workspace is registered.");
  });

  it("shows the chosen workspace, marks only its entry, and sends nothing", async () => {
    await startOverview(() => overview());
    openChild(B);
    expect(heading()).toBe("/home/dev/b");
    expect(shownChild()).toBe(B);
    expect(navItems().some((item) => item.getAttribute("aria-current") === "page")).toBe(false);
    expect(root.textContent).not.toContain("Development root");
    expect(ipc.calls.map((call) => call.command)).toEqual(["settings_overview"]);

    // The Workspaces entry returns to the overview.
    go("Workspaces");
    expect(current()).toBe("Workspaces");
    expect(root.querySelector(".workspace-block")).toBeNull();
    expect(shownChild()).toBeNull();
  });

  it("sends the shown workspace's identifier and keeps it shown by identifier", async () => {
    await startOverview(() => overview(), {
      settings_set_permissions: () =>
        overview({
          revision: 4,
          workspaces: [
            workspace(B, "/home/dev/b", { write: "allow", execute: "prompt" }),
            workspace(A, "/home/dev/a"),
          ],
        }),
    });
    openChild(B);
    choose("Execute", "prompt");
    await settle();
    expect(ipc.callsTo("settings_set_permissions")[0]?.args).toMatchObject({
      workspace_id: B,
      revision: 3,
    });
    // The order changed, but the same workspace is still shown and marked.
    expect(heading()).toBe("/home/dev/b");
    expect(shownChild()).toBe(B);
    expect(children().map((item) => item.dataset.workspaceId)).toEqual([B, A]);
  });

  it("collapses and expands the group, and remembers that across a refresh", async () => {
    await startOverview(() => overview());
    openChild(B);
    const list = root.querySelector(".nav-children") as HTMLElement;
    expect(list.hidden).toBe(false);

    toggle().click();
    expect(toggle().getAttribute("aria-expanded")).toBe("false");
    expect(list.hidden).toBe(true);
    // Collapsing changes neither the page nor the choice.
    expect(heading()).toBe("/home/dev/b");

    ipc.on("settings_overview", () => overview({ revision: 4 }));
    buttonByText(root, "Refresh").click();
    await settle();
    expect(root.textContent).toContain("Revision 4");
    expect(toggle().getAttribute("aria-expanded")).toBe("false");
    expect(list.hidden).toBe(true);
    expect(heading()).toBe("/home/dev/b");

    toggle().click();
    expect(toggle().getAttribute("aria-expanded")).toBe("true");
    expect(list.hidden).toBe(false);
    expect(shownChild()).toBe(B);
  });

  it("keeps keyboard focus on the entry after choosing it", async () => {
    await startOverview(() => overview());
    child(B).focus();
    openChild(B);
    expect((document.activeElement as HTMLElement).dataset.workspaceId).toBe(B);
  });

  it("uses real buttons in tab order: Model, Workspaces, toggle, entries, Advanced", async () => {
    await startOverview(() => overview());
    const order = Array.from(root.querySelectorAll<HTMLElement>(".settings-nav button")).map(
      (node) => node.getAttribute("aria-label") ?? node.textContent,
    );
    expect(order).toEqual([
      "Model",
      "Workspaces",
      "Workspaces list",
      "/home/dev/a",
      "/home/dev/b",
      "Advanced",
      "Refresh",
    ]);
  });

  it("falls back to the first workspace when the shown one disappears", async () => {
    await startOverview(() => overview());
    openChild(B);
    expect(heading()).toBe("/home/dev/b");

    ipc.on("settings_overview", () =>
      overview({ revision: 4, workspaces: [workspace(A, "/home/dev/a")] }),
    );
    buttonByText(root, "Refresh").click();
    await settle();
    expect(heading()).toBe("/home/dev/a");
    expect(shownChild()).toBe(A);
    expect(children()).toHaveLength(1);
  });

  it("falls back to the overview when the last workspace disappears", async () => {
    await startOverview(() => overview());
    openChild(B);
    ipc.on("settings_overview", () => overview({ revision: 4, workspaces: [] }));
    buttonByText(root, "Refresh").click();
    await settle();
    expect(root.querySelector(".workspace-block")).toBeNull();
    expect(current()).toBe("Workspaces");
    expect(root.querySelector(".empty")?.textContent).toBe("No workspace is registered.");
  });

  it("explains the package cache in one line under its row", async () => {
    await start(() => overview());
    const help = root.querySelector(".package-cache .perm-help");
    expect(help?.textContent).toBe(PACKAGE_CACHE_HELP);
    expect(help?.textContent).toContain("omgcode packages fetch");
    expect(help?.textContent).toContain("read-only");
  });
});

describe("permission controls", () => {
  it("send the whole preference set with one state changed", async () => {
    await start(() => overview(), {
      settings_set_permissions: () => overview({ revision: 4 }),
    });
    choose("Task network", "prompt");
    await settle();
    expect(ipc.callsTo("settings_set_permissions")[0]?.args).toEqual({
      workspace_id: A,
      revision: 3,
      permissions: {
        model_tools: true,
        read: "allow",
        write: "prompt",
        execute: "deny",
        task_network: "prompt",
        host_services: "deny",
        web_fetch: "deny",
        write_tools: ALL_TOOLS,
      },
    });
  });

  it("send a tool limit and the model tools box with every other state", async () => {
    await start(() => overview(), {
      settings_set_permissions: () => overview({ revision: 4 }),
    });
    choose("Create tool", "prompt");
    await settle();
    expect(ipc.callsTo("settings_set_permissions")[0]?.args).toMatchObject({
      permissions: { write: "prompt", write_tools: { ...ALL_TOOLS, create: "prompt" } },
    });
    const box = root.querySelector('input[aria-label="model tools"]') as HTMLInputElement;
    box.checked = false;
    box.dispatchEvent(new Event("change"));
    await settle();
    expect(ipc.callsTo("settings_set_permissions")[1]?.args).toMatchObject({
      permissions: { model_tools: false, read: "allow", write: "prompt", write_tools: ALL_TOOLS },
    });
  });

  it("send the create directory tool limit on its own", async () => {
    await start(() => overview(), {
      settings_set_permissions: () => overview({ revision: 4 }),
    });
    choose("Create directory tool", "deny");
    await settle();
    expect(ipc.callsTo("settings_set_permissions")[0]?.args).toMatchObject({
      permissions: { write_tools: { ...ALL_TOOLS, create_directory: "deny" } },
    });
  });

  it("send nothing for the state already chosen", async () => {
    await start(() => overview());
    choose("Write", "prompt");
    await settle();
    expect(ipc.callsTo("settings_set_permissions")).toHaveLength(0);
  });

  it("name the permission and mark Deny, Prompt, Allow by data-value", async () => {
    await start(() => overview());
    for (const label of ["Read", "Write", "Execute", "Task network", "Host services"]) {
      const group = root.querySelector(`[role=radiogroup][aria-label="${label}"]`);
      expect(group, label).not.toBeNull();
      expect(group?.classList.contains("access")).toBe(true);
    }
  });

  it("show a muted note beside a tool wider than Write", async () => {
    await start(() => overview());
    const notes = Array.from(root.querySelectorAll(".perm-note")).map((n) => n.textContent);
    expect(notes).toEqual(Array(6).fill("Limited by Write: Prompt"));

    await start(() =>
      overview({
        workspaces: [
          workspace(A, "/home/dev/a", {
            write: "prompt",
            write_tools: { ...ALL_TOOLS, edit: "prompt", create: "deny" },
          }),
        ],
      }),
    );
    // Only the tools wider than Write carry the note: multi-edit, rename, delete, create directory.
    const rows = Array.from(root.querySelectorAll(".tool-limits .perm-row"));
    const noted = rows
      .filter((row) => row.querySelector(".perm-note") !== null)
      .map((row) => row.querySelector(".perm-label")?.textContent);
    expect(noted).toEqual([
      "Multi-edit tool",
      "Rename tool",
      "Delete tool",
      "Create directory tool",
    ]);

    await start(() => overview({ workspaces: [workspace(A, "/home/dev/a", { write: "allow" })] }));
    expect(root.querySelectorAll(".perm-note")).toHaveLength(0);
  });

  it("hold the tool limits in a disclosure under Write that stays open", async () => {
    await start(() => overview(), {
      settings_set_permissions: () => overview({ revision: 4 }),
    });
    const details = root.querySelector("details.tool-limits") as HTMLDetailsElement;
    expect(details.querySelector("summary")?.textContent).toBe("File-write tool limits");
    expect(details.querySelectorAll("[role=radiogroup]")).toHaveLength(6);
    expect(details.open).toBe(false);
    details.open = true;
    details.dispatchEvent(new Event("toggle"));
    choose("Delete tool", "prompt");
    await settle();
    expect((root.querySelector("details.tool-limits") as HTMLDetailsElement).open).toBe(true);
  });

  it("set every tool limit at once with one request and leave Write alone", async () => {
    await start(() => overview(), {
      settings_set_permissions: () => overview({ revision: 4 }),
    });
    // Set all sits beside the closed disclosure, not inside it or its summary.
    const details = root.querySelector("details.tool-limits") as HTMLDetailsElement;
    expect(details.open).toBe(false);
    const all = root.querySelector(`[role=radiogroup][aria-label="${ALL_TOOLS_LABEL}"]`);
    expect(all?.closest("details")).toBeNull();
    expect(all?.closest(".tool-limits-group")).toBe(details.parentElement);
    choose(ALL_TOOLS_LABEL, "deny");
    await settle();
    const calls = ipc.callsTo("settings_set_permissions");
    expect(calls).toHaveLength(1);
    expect(calls[0]?.args).toEqual({
      workspace_id: A,
      revision: 3,
      permissions: {
        model_tools: true,
        read: "allow",
        write: "prompt",
        execute: "deny",
        task_network: "deny",
        host_services: "deny",
        web_fetch: "deny",
        write_tools: {
          edit: "deny",
          multi_edit: "deny",
          create: "deny",
          rename: "deny",
          delete: "deny",
          create_directory: "deny",
        },
      },
    });
  });

  it("mark Set all only while every tool limit shares one state", async () => {
    await start(() => overview());
    const checked = () =>
      root
        .querySelector(`[role=radiogroup][aria-label="${ALL_TOOLS_LABEL}"]`)
        ?.querySelector('[aria-checked="true"]')
        ?.getAttribute("data-value") ?? null;
    expect(checked()).toBe("allow");

    await start(() =>
      overview({
        workspaces: [
          workspace(A, "/home/dev/a", { write_tools: { ...ALL_TOOLS, delete: "prompt" } }),
        ],
      }),
    );
    expect(checked()).toBeNull();
    // With none marked, choosing the state most tools already have still sends the change.
    choose(ALL_TOOLS_LABEL, "allow");
    await settle();
    expect(ipc.callsTo("settings_set_permissions")[0]?.args).toMatchObject({
      permissions: { write: "prompt", write_tools: ALL_TOOLS },
    });
  });

  it("send nothing when Set all chooses the state every tool already has", async () => {
    await start(() => overview());
    choose(ALL_TOOLS_LABEL, "allow");
    await settle();
    expect(ipc.callsTo("settings_set_permissions")).toHaveLength(0);
  });
});

describe("advanced section", () => {
  it("lists the toolchain, package-network, and container-grant records with their controls (ADR-0088)", async () => {
    await start(() =>
      overview({
        toolchain_profiles: [
          {
            name: "rust",
            platform: "linux",
            read_only_roots: ["/opt/rust"],
            path_entries: ["/opt/rust/bin"],
            environment_names: ["RUSTUP_HOME"],
          },
        ],
        package_profiles: [
          { name: "crates", origins: ["https://crates.io"], credentials_bound: 0 },
        ],
        container_grants: [
          {
            grant_id: "g1",
            workspace_id: A,
            directory: "C:\\code\\a",
            access: "read_write",
            state: "active",
          },
        ],
        skills: [{ name: "review", application: null, workspace_overrides: [] }],
      }),
    );
    go("Advanced");
    // Every group has controls now; each recorded row has one button and none types a path.
    const blocks = Array.from(root.querySelectorAll<HTMLElement>(".block"));
    expect(blocks).toHaveLength(4);
    const rowButtons = (title: string): string[] =>
      Array.from(
        blocks
          .find((block) => block.querySelector("h3")?.textContent === title)
          ?.querySelectorAll<HTMLButtonElement>(".row > button") ?? [],
      ).map((entry) => entry.getAttribute("aria-label") ?? "");
    expect(rowButtons("Toolchain profiles")).toEqual(["Remove toolchain profile rust"]);
    expect(rowButtons("Package-network profiles")).toEqual([
      "Remove package-network profile crates",
    ]);
    expect(rowButtons("Container grants")).toEqual(["Revoke the grant on C:\\code\\a"]);
    const headings = Array.from(root.querySelectorAll("h3")).map((h) => h.textContent);
    expect(headings).toEqual([
      "Toolchain profiles",
      "Package-network profiles",
      "Container grants",
      "Skills",
    ]);
    const text = root.textContent ?? "";
    expect(text).toContain("https://crates.io");
    expect(text).toContain("read and write");
    expect(text).toContain("/home/dev/a");
    expect(text).toContain("Not set (disabled)");
  });

  it("labels a hosted profile, offers Set key, and keeps Remove for a stored key", async () => {
    await start(() =>
      overview({
        provider_profiles: [
          profile(),
          profile({
            name: "hosted",
            kind: "ollama_cloud",
            auth: "api_key",
            endpoint: "https://ollama.com/v1",
            credential_bound: true,
          }),
        ],
      }),
    );
    go("Model");
    const rows = Array.from(root.querySelectorAll<HTMLElement>(".row"));
    const hosted = rows.find((row) => row.textContent?.includes("hosted"));
    const local = rows.find((row) => row.querySelector(".row-title")?.textContent === "ollama");
    expect(hosted?.textContent).toContain("Hosted");
    expect(hosted?.textContent).toContain("leave this computer");
    expect(hosted?.textContent).toContain("Set key");
    expect(hosted?.querySelector("input[type=password]")).not.toBeNull();
    expect(hosted?.querySelector<HTMLButtonElement>("button:not(summary)")?.disabled).toBe(false);
    // A local profile has neither the hosted label nor a key field.
    expect(local?.textContent).not.toContain("Hosted");
    expect(local?.querySelector("input")).toBeNull();
  });

  it("offers no Set key on a hosted subscription profile, which never holds a key", async () => {
    await start(() =>
      overview({
        provider_profiles: [
          profile({
            name: "plan",
            kind: "anthropic",
            auth: "subscription",
            endpoint: "https://api.anthropic.com/v1",
            credential_bound: false,
          }),
        ],
      }),
    );
    go("Model");
    const row = Array.from(root.querySelectorAll<HTMLElement>(".row")).find(
      (candidate) => candidate.querySelector(".row-title")?.textContent === "plan",
    );
    expect(row?.textContent).toContain("Hosted");
    expect(row?.textContent).not.toContain("Set key");
    expect(row?.querySelector("input[type=password]")).toBeNull();
    // It can never be chosen, so it must not claim that prompts leave for its host.
    expect(row?.textContent).toContain(UNSUPPORTED_AUTH_NOTE);
    expect(row?.textContent).not.toContain("leave this computer");
  });

  it("says a hosted profile without an endpoint cannot be used, and offers no Set key", async () => {
    await start(() =>
      overview({
        provider_profiles: [
          profile({
            name: "bare",
            kind: "openai",
            auth: "api_key",
            endpoint: null,
            credential_bound: false,
          }),
        ],
      }),
    );
    go("Model");
    const row = Array.from(root.querySelectorAll<HTMLElement>(".row")).find(
      (candidate) => candidate.querySelector(".row-title")?.textContent === "bare",
    );
    expect(row?.textContent).toContain(NO_ENDPOINT_NOTE);
    expect(row?.textContent).not.toContain("the address you enter");
    expect(row?.textContent).not.toContain("Set key");
    expect(row?.querySelector("input[type=password]")).toBeNull();
  });

  it("gives each empty group one line", async () => {
    await start(() => overview());
    go("Advanced");
    const lines = Array.from(root.querySelectorAll(".empty")).map((p) => p.textContent);
    expect(lines).toEqual([
      "No toolchain profile is recorded.",
      "No package-network profile is recorded.",
      "No container grant is recorded.",
      "No skill is imported.",
    ]);
  });
});

describe("workspace scope", () => {
  const scoped = (id: string) => overview({ scope: { workspace_id: id } });

  it("shows only the scoped workspace without sections, root, registration, or other workspaces", async () => {
    await start(() => scoped(B));

    expect(heading()).toBe("/home/dev/b");
    expect(root.textContent).toContain("Workspace settings");
    expect(root.textContent).not.toContain("/home/dev/a");
    expect(root.querySelector(".settings-window.scoped")).not.toBeNull();
    expect((root.querySelector(".settings-nav") as HTMLElement).hidden).toBe(true);
    expect(children().map((item) => item.dataset.workspaceId)).toEqual([B]);
    expect(root.textContent).not.toContain("Development root");
    expect(root.textContent).not.toContain("Register workspace");
    expect(root.querySelector(".tool-limits")).not.toBeNull();
    expect(root.textContent).toContain("Package cache");
    expect((root.querySelector(".scope-bar") as HTMLElement).hidden).toBe(false);
  });

  it("restores the sections and every workspace entry when All settings clears the scope", async () => {
    await startOverview(() => scoped(B), { settings_clear_scope: () => overview({ revision: 4 }) });
    buttonByText(root, "All settings").click();
    await settle();

    expect(ipc.callsTo("settings_clear_scope")).toEqual([
      { command: "settings_clear_scope", args: {} },
    ]);
    expect((root.querySelector(".settings-nav") as HTMLElement).hidden).toBe(false);
    expect((root.querySelector(".scope-bar") as HTMLElement).hidden).toBe(true);
    expect(root.querySelector(".settings-window.scoped")).toBeNull();
    expect(children()).toHaveLength(2);
    expect(current()).toBe("Workspaces");
  });

  it("shows a note and the full view when the scoped workspace is gone", async () => {
    await start(() => scoped("cccccccc-cccc-4ccc-8ccc-cccccccccccc"));

    expect(root.textContent).toContain("no longer registered");
    expect((root.querySelector(".settings-nav") as HTMLElement).hidden).toBe(false);
    expect(children()).toHaveLength(2);
  });

  it("reads the scope again when the window gains focus", async () => {
    await start(() => overview());
    expect(root.querySelector(".settings-window.scoped")).toBeNull();

    ipc.on("settings_overview", () => scoped(A));
    window.dispatchEvent(new Event("focus"));
    await settle();

    expect(root.querySelector(".settings-window.scoped")).not.toBeNull();
    expect(heading()).toBe("/home/dev/a");
  });

  it("reads again when the scope changes while an earlier read is still pending", async () => {
    await start(() => overview());

    // A read starts, and its answer will be the old, unscoped view.
    let answerOld: (value: unknown) => void = () => undefined;
    ipc.on(
      "settings_overview",
      () =>
        new Promise((resolve) => {
          answerOld = resolve;
        }),
    );
    window.dispatchEvent(new Event("focus"));
    await settle();

    // The main window scopes the window and focuses it before that read answers.
    ipc.on("settings_overview", () => scoped(A));
    window.dispatchEvent(new Event("focus"));
    await settle();
    answerOld(overview());
    await settle();

    expect(root.querySelector(".settings-window.scoped")).not.toBeNull();
    expect(heading()).toBe("/home/dev/a");
  });

  it("still sends changes for the scoped workspace by its identifier", async () => {
    await start(() => scoped(B), { settings_set_permissions: () => scoped(B) });
    choose("Read", "deny");
    await settle();
    expect(ipc.callsTo("settings_set_permissions")[0]?.args).toMatchObject({ workspace_id: B });
  });
});

describe("hostile configuration text", () => {
  const rtl = String.fromCharCode(0x202e);
  const bidi = String.fromCharCode(0x2066);
  const hostile = `<img src=x onerror=alert(1)>${rtl}exe.txt${bidi}`;

  it("stays text in every section", async () => {
    await start(() =>
      overview({
        development_root: hostile,
        workspaces: [workspace(A, hostile, { package_cache_root: "</script><b>x</b>" })],
        connection: {
          endpoint: hostile,
          source: "profile",
          profile: hostile,
          kind: "lm_studio",
          selectable: false,
        },
        provider_profiles: [
          profile({ name: hostile, endpoint: hostile, auth: hostile, active: true }),
        ],
        toolchain_profiles: [
          {
            name: hostile,
            platform: "windows",
            read_only_roots: [hostile],
            path_entries: [hostile],
            environment_names: [hostile],
          },
        ],
        package_profiles: [{ name: hostile, origins: [hostile], credentials_bound: 1 }],
        container_grants: [
          {
            grant_id: hostile,
            workspace_id: "gone",
            directory: hostile,
            access: "read_execute",
            state: "pending",
          },
        ],
        skills: [
          {
            name: hostile,
            application: true,
            workspace_overrides: [{ workspace_id: A, enabled: true }],
          },
        ],
      }),
    );
    for (const section of ["Workspaces", "Model", "Advanced"]) {
      go(section);
      expect(root.querySelector("img, script, b"), section).toBeNull();
      expect(root.querySelector("[onerror]"), section).toBeNull();
      const text = root.textContent ?? "";
      expect(text, section).toContain("<img src=x onerror=alert(1)>");
      expect(text, section).not.toContain(rtl);
      expect(text, section).not.toContain(bidi);
    }
  });
});

describe("chat defaults and custom instructions (ADR-0080)", () => {
  function field<T extends HTMLElement>(id: string): T {
    const found = root.querySelector<T>(`#${id}`);
    if (found === null) {
      throw new Error(`No field ${id}`);
    }
    return found;
  }

  function type(control: HTMLInputElement | HTMLTextAreaElement, value: string): void {
    control.value = value;
    control.dispatchEvent(new Event("input"));
  }

  it("shows the saved budgets beside the connection and saves them with the global instructions", async () => {
    const saved = chatDefaultsView({
      defaults: {
        context_tokens: 65_536,
        max_output_tokens: 4096,
        max_tool_calls: 64,
        turn_timeout_seconds: 600,
        instructions: "Be brief.",
      },
    });
    await start(() => overview(), {
      settings_save_chat_defaults: () => overview({ chat: saved }),
    });
    go("Model");
    const block = root.querySelector(".chat-defaults") as HTMLElement;
    // After the active connection, before the list of connections.
    const blocks = Array.from(root.querySelectorAll(".block > h3")).map((h) => h.textContent);
    expect(blocks.slice(0, 3)).toEqual(["Active connection", "Chat defaults", "Connections"]);
    expect(field<HTMLInputElement>("chat-context-tokens").value).toBe("8192");
    expect(field<HTMLInputElement>("chat-max-tool-calls").max).toBe("256");
    expect(field<HTMLInputElement>("chat-turn-timeout-seconds").max).toBe("3600");

    type(field("chat-context-tokens"), "65536");
    type(field("chat-max-output-tokens"), "4096");
    type(field("chat-max-tool-calls"), "64");
    type(field("chat-turn-timeout-seconds"), "600");
    type(field("global-instructions"), "Be brief.");
    expect(block.querySelector(".byte-counter")?.textContent).toBe("9 / 16384 bytes");
    buttonByText(root, "Save chat defaults").click();
    await settle();

    expect(ipc.callsTo("settings_save_chat_defaults")).toEqual([
      {
        command: "settings_save_chat_defaults",
        args: {
          defaults: {
            context_tokens: 65_536,
            max_output_tokens: 4096,
            max_tool_calls: 64,
            turn_timeout_seconds: 600,
            instructions: "Be brief.",
          },
        },
      },
    ]);
    // No confirmation and no revision: none of it is a permission.
    expect(field<HTMLTextAreaElement>("global-instructions").value).toBe("Be brief.");
  });

  it("refuses an output reserve larger than Rust's 32-bit field without sending", async () => {
    await start(() => overview());
    go("Model");
    // A safe JavaScript integer, but over u32::MAX.
    type(field("chat-max-output-tokens"), "5000000000");
    buttonByText(root, "Save chat defaults").click();
    await settle();
    expect(ipc.callsTo("settings_save_chat_defaults")).toHaveLength(0);
    expect(root.querySelector(".chat-defaults .error")?.textContent).toBe(
      "Set Max output tokens to a whole number from 1 to 4294967295.",
    );
    expect(field<HTMLInputElement>("chat-max-output-tokens").max).toBe("4294967295");
  });

  it("refuses a value outside its range without sending, and keeps the draft across a redraw", async () => {
    await start(() => overview());
    go("Model");
    type(field("chat-max-tool-calls"), "257");
    type(field("global-instructions"), "Draft text");
    buttonByText(root, "Save chat defaults").click();
    await settle();
    expect(ipc.callsTo("settings_save_chat_defaults")).toHaveLength(0);
    expect(root.querySelector(".chat-defaults .error")?.textContent).toBe(
      "Set Tool calls per turn to a whole number from 1 to 256.",
    );
    type(field("chat-max-tool-calls"), "");
    buttonByText(root, "Save chat defaults").click();
    expect(root.querySelector(".chat-defaults .error")?.textContent).toBe(
      "Set Tool calls per turn to a whole number of at least 1.",
    );

    // Another section and back: what was typed is still there.
    go("Workspaces");
    go("Model");
    expect(field<HTMLTextAreaElement>("global-instructions").value).toBe("Draft text");
    expect(field<HTMLInputElement>("chat-max-tool-calls").value).toBe("");
  });

  it("says when the saved defaults could not be read", async () => {
    await start(() => overview({ chat: chatDefaultsView({ source: "unreadable" }) }));
    go("Model");
    expect(root.querySelector(".chat-defaults")?.textContent).toContain(
      "Saved chat defaults could not be read",
    );
  });

  it("saves each workspace's own instructions, and empty text removes them", async () => {
    await start(() => overview(), {
      settings_set_workspace_instructions: (args) =>
        overview({
          workspaces: [
            workspace(A, "/home/dev/a", { instructions: args.instructions as string }),
            workspace(B, "/home/dev/b"),
          ],
        }),
    });
    const box = field<HTMLTextAreaElement>(`workspace-instructions-${A}`);
    expect(box.value).toBe("");
    type(box, "Run cargo test first.");
    buttonByText(root, "Save instructions").click();
    await settle();
    expect(ipc.callsTo("settings_set_workspace_instructions")).toEqual([
      {
        command: "settings_set_workspace_instructions",
        args: { workspace_id: A, instructions: "Run cargo test first." },
      },
    ]);
    expect(field<HTMLTextAreaElement>(`workspace-instructions-${A}`).value).toBe(
      "Run cargo test first.",
    );

    // The other workspace has its own box, empty.
    openChild(B);
    expect(field<HTMLTextAreaElement>(`workspace-instructions-${B}`).value).toBe("");
  });

  it("keeps a typed workspace draft when another workspace is shown and back", async () => {
    await start(() => overview());
    type(field(`workspace-instructions-${A}`), "Unsaved");
    openChild(B);
    openChild(A);
    expect(field<HTMLTextAreaElement>(`workspace-instructions-${A}`).value).toBe("Unsaved");
  });

  it("counts against the room Rust leaves beside the global text and keeps longer text", async () => {
    await start(() =>
      overview({
        workspaces: [
          workspace(A, "/home/dev/a", { instructions_max_bytes: 10 }),
          workspace(B, "/b"),
        ],
      }),
    );
    const box = field<HTMLTextAreaElement>(`workspace-instructions-${A}`);
    type(box, "eleven byte");
    const counter = root.querySelector(".workspace-instructions .byte-counter");
    expect(counter?.textContent).toBe("11 / 10 bytes");
    expect(counter?.classList.contains("over")).toBe(true);
    buttonByText(root, "Save instructions").click();
    await settle();
    expect(ipc.callsTo("settings_set_workspace_instructions")).toHaveLength(0);
    expect(root.querySelector(".workspace-instructions [role=alert]")?.textContent).toBe(
      workspaceTooLong(10),
    );
    expect(box.value).toBe("eleven byte");
  });

  it("leaves an unknown room to Rust and says why the counter has no limit", async () => {
    await start(() =>
      overview({
        chat: chatDefaultsView({ instructions_max_bytes: null }),
        workspaces: [
          workspace(A, "/home/dev/a", { instructions_max_bytes: null }),
          workspace(B, "/b"),
        ],
      }),
    );
    const box = field<HTMLTextAreaElement>(`workspace-instructions-${A}`);
    type(box, "Use tabs.");
    const counter = root.querySelector(".workspace-instructions .byte-counter");
    expect(counter?.textContent).toBe(`9 bytes; ${ROOM_UNKNOWN}`);
    expect(counter?.classList.contains("over")).toBe(false);
    buttonByText(root, "Save instructions").click();
    await settle();
    // Rust reads the global text again under its lock and decides.
    expect(ipc.callsTo("settings_set_workspace_instructions")).toEqual([
      {
        command: "settings_set_workspace_instructions",
        args: { workspace_id: A, instructions: "Use tabs." },
      },
    ]);

    go("Model");
    type(field("global-instructions"), "Be brief.");
    expect(root.querySelector(".chat-defaults .byte-counter")?.textContent).toBe(
      `9 bytes; ${ROOM_UNKNOWN}`,
    );
    buttonByText(root, "Save chat defaults").click();
    await settle();
    expect(ipc.callsTo("settings_save_chat_defaults")).toHaveLength(1);
  });

  it("says when a workspace's saved instructions could not be read", async () => {
    await start(() =>
      overview({
        workspaces: [workspace(A, "/home/dev/a", { instructions: null }), workspace(B, "/b")],
      }),
    );
    expect(root.querySelector(".workspace-instructions")?.textContent).toContain(
      "could not be read",
    );
  });
});

describe("web fetch", () => {
  const pattern = (value: string) => ({ workspace_id: A, revision: 3, pattern: value });

  function hostInput(): HTMLInputElement {
    const found = root.querySelector<HTMLInputElement>(`input[aria-label="${HOST_INPUT_LABEL}"]`);
    if (found === null) {
      throw new Error("No host field");
    }
    return found;
  }

  function type(value: string): void {
    const input = hostInput();
    input.value = value;
    input.dispatchEvent(new Event("input"));
  }

  function listed(): string[] {
    return Array.from(root.querySelectorAll(".web-host .root")).map((n) => n.textContent ?? "");
  }

  it("shows a Web fetch selector beside the other preferences, with its help text", async () => {
    await start(() =>
      overview({ workspaces: [workspace(A, "/home/dev/a", { web_fetch: "prompt" })] }),
    );
    expect(PERMISSION_LABELS.map(([, label]) => label)).toContain("Web fetch");
    expect(root.querySelector('[role=radiogroup][aria-label="Web fetch"]')).not.toBeNull();
    expect(root.textContent).toContain(
      "Fetch public HTTPS pages for the model. Under Allow, hosts on the list below skip approval; other hosts still ask.",
    );
    expect(root.textContent).not.toContain("No model tool uses it");
    const checked = root.querySelector(
      '[role=radiogroup][aria-label="Web fetch"] [aria-checked="true"]',
    );
    expect(checked?.getAttribute("data-value")).toBe("prompt");
  });

  it("sends web_fetch with the whole preference set", async () => {
    await start(() => overview(), { settings_set_permissions: () => overview({ revision: 4 }) });
    choose("Web fetch", "allow");
    await settle();
    expect(ipc.callsTo("settings_set_permissions")[0]?.args).toEqual({
      workspace_id: A,
      revision: 3,
      permissions: {
        model_tools: true,
        read: "allow",
        write: "prompt",
        execute: "deny",
        task_network: "deny",
        host_services: "deny",
        web_fetch: "allow",
        write_tools: ALL_TOOLS,
      },
    });
  });

  it("lists each host with a remove button named for it and shows the limit", async () => {
    await start(() =>
      overview({
        workspaces: [
          workspace(A, "/home/dev/a", { web_fetch_hosts: ["docs.example.com", "*.example.org"] }),
        ],
      }),
    );
    expect(listed()).toEqual(["docs.example.com", "*.example.org"]);
    expect(root.querySelector(".web-host-count")?.textContent).toBe("2 of 100");
    const remove = buttonByLabel(root, "Remove *.example.org");
    expect(remove.title).toBe("Remove *.example.org");
    expect(root.textContent).toContain(WEB_HOSTS_HELP);
  });

  it("says every fetch asks while no host is listed", async () => {
    await start(() => overview());
    expect(listed()).toEqual([]);
    expect(root.textContent).toContain(WEB_HOSTS_EMPTY);
    expect(root.querySelector(".web-host-count")?.textContent).toBe("0 of 100");
  });

  it("adds the typed pattern with the revision, then clears the field", async () => {
    await start(() => overview(), {
      settings_add_web_fetch_host: () =>
        overview({
          revision: 4,
          workspaces: [workspace(A, "/home/dev/a", { web_fetch_hosts: ["docs.example.com"] })],
        }),
    });
    type("  Docs.Example.com ");
    buttonByText(root, ADD_HOST_LABEL).click();
    await settle();
    expect(ipc.callsTo("settings_add_web_fetch_host")[0]?.args).toEqual(
      pattern("Docs.Example.com"),
    );
    expect(listed()).toEqual(["docs.example.com"]);
    expect(hostInput().value).toBe("");
  });

  it("adds on Enter", async () => {
    await start(() => overview(), { settings_add_web_fetch_host: () => overview({ revision: 4 }) });
    type("a.example.com");
    hostInput().dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", cancelable: true }));
    await settle();
    expect(ipc.callsTo("settings_add_web_fetch_host")).toHaveLength(1);
  });

  it("removes the named pattern", async () => {
    await start(
      () =>
        overview({
          workspaces: [workspace(A, "/home/dev/a", { web_fetch_hosts: ["docs.example.com"] })],
        }),
      { settings_remove_web_fetch_host: () => overview({ revision: 4 }) },
    );
    buttonByLabel(root, "Remove docs.example.com").click();
    await settle();
    expect(ipc.callsTo("settings_remove_web_fetch_host")[0]?.args).toEqual(
      pattern("docs.example.com"),
    );
    expect(listed()).toEqual([]);
  });

  it("shows a refusal code as a readable sentence and keeps what was typed", async () => {
    await start(() => overview(), {
      settings_add_web_fetch_host: () =>
        rejection("web_host_reserved", "local and private names are not allowed"),
    });
    type("localhost");
    buttonByText(root, ADD_HOST_LABEL).click();
    await settle();
    const alert = root.querySelector<HTMLElement>(".change-error");
    expect(alert?.textContent).toBe(
      "Local and private names, such as localhost, cannot be allowed. (web_host_reserved)",
    );
    expect(alert?.hidden).toBe(false);
    expect(hostInput().value).toBe("localhost");
  });

  it("maps every web_host code to a sentence and leaves other codes alone", () => {
    for (const code of [
      "web_host_empty",
      "web_host_not_ascii",
      "web_host_too_long",
      "web_host_bad_label",
      "web_host_bad_character",
      "web_host_ip_literal",
      "web_host_reserved",
      "web_host_too_few_labels",
      "web_host_not_canonical",
      "web_host_duplicate",
      "web_host_not_listed",
      "web_host_too_many",
    ]) {
      expect(webHostMessage(code), code).toMatch(/\.$/);
    }
    expect(webHostMessage("stale_revision")).toBeNull();
    expect(webHostMessage("toString")).toBeNull();
  });

  it("shows a cancelled confirmation as no change", async () => {
    await start(() => overview(), {
      settings_add_web_fetch_host: () => rejection("cancelled", "Cancelled."),
    });
    type("a.example.com");
    buttonByText(root, ADD_HOST_LABEL).click();
    await settle();
    expect(root.querySelector(".notice[role=status]")?.textContent).toBe(NOTHING_CHANGED);
  });

  it("disables adding at 100 hosts and says why", async () => {
    expect(MAX_WEB_HOSTS).toBe(100);
    const full = Array.from({ length: MAX_WEB_HOSTS }, (_, index) => `h${index}.example.com`);
    await start(() =>
      overview({ workspaces: [workspace(A, "/home/dev/a", { web_fetch_hosts: full })] }),
    );
    expect(root.querySelector(".web-host-count")?.textContent).toBe("100 of 100");
    expect(hostInput().disabled).toBe(true);
    expect(buttonByText(root, ADD_HOST_LABEL).disabled).toBe(true);
    expect(root.textContent).toContain(HOSTS_FULL);
  });

  it("renders a hostile pattern as text only", async () => {
    const hostile = "<img src=x onerror=alert(1)>.example.com";
    await start(() =>
      overview({ workspaces: [workspace(A, "/home/dev/a", { web_fetch_hosts: [hostile] })] }),
    );
    expect(root.querySelector(".web-host img")).toBeNull();
    expect(listed()).toEqual([hostile]);
  });
});

describe("toolchain profiles on the workspace page (ADR-0107)", () => {
  function toolchain(
    name: string,
    platform: ToolchainProfileView["platform"],
  ): ToolchainProfileView {
    return {
      name,
      platform,
      read_only_roots: [`/opt/${name}`],
      path_entries: [`/opt/${name}/bin`],
      environment_names: [],
    };
  }

  const RECORDED = [
    toolchain("rust", "linux"),
    toolchain("go", "linux"),
    toolchain("msvc", "windows"),
  ];

  function profileSwitch(name: string): HTMLInputElement | null {
    return root.querySelector<HTMLInputElement>(`input[aria-label="${useProfileLabel(name)}"]`);
  }

  function flip(name: string): void {
    const box = profileSwitch(name);
    if (box === null) {
      throw new Error(`No switch for ${name}`);
    }
    box.checked = !box.checked;
    box.dispatchEvent(new Event("change"));
  }

  it("shows one switch per profile of this platform, on when assigned", async () => {
    await start(() =>
      overview({
        toolchain_profiles: RECORDED,
        // Core compares names case-insensitively, and so does the page.
        workspaces: [workspace(A, "/home/dev/a", { execute: "allow", profiles: ["RUST"] })],
      }),
    );
    expect(root.textContent).toContain(PROFILES_HELP);
    expect(profileSwitch("rust")?.checked).toBe(true);
    expect(profileSwitch("go")?.checked).toBe(false);
    // A Windows profile cannot run here, so it is not offered.
    expect(profileSwitch("msvc")).toBeNull();
    expect(root.querySelector(".workspace-profile")?.textContent).toContain("/opt/rust");
  });

  it("asks Rust to assign or unassign with the revision, the workspace, and the name only", async () => {
    await start(
      () =>
        overview({
          toolchain_profiles: RECORDED,
          workspaces: [workspace(A, "/home/dev/a", { profiles: ["rust"] })],
        }),
      {
        settings_assign_toolchain_profile: () =>
          overview({
            revision: 4,
            toolchain_profiles: RECORDED,
            workspaces: [workspace(A, "/home/dev/a", { profiles: ["rust", "go"] })],
          }),
        settings_unassign_toolchain_profile: () =>
          overview({
            revision: 5,
            toolchain_profiles: RECORDED,
            workspaces: [workspace(A, "/home/dev/a", { profiles: ["go"] })],
          }),
      },
    );
    flip("go");
    await settle();
    expect(ipc.callsTo("settings_assign_toolchain_profile")[0]?.args).toEqual({
      revision: 3,
      workspace_id: A,
      name: "go",
    });
    expect(profileSwitch("go")?.checked).toBe(true);

    flip("rust");
    await settle();
    expect(ipc.callsTo("settings_unassign_toolchain_profile")[0]?.args).toEqual({
      revision: 4,
      workspace_id: A,
      name: "rust",
    });
    expect(profileSwitch("rust")?.checked).toBe(false);
  });

  it("shows the recorded state again when the operator declines", async () => {
    await start(() => overview({ toolchain_profiles: RECORDED }), {
      settings_assign_toolchain_profile: () => rejection("cancelled", "No change was made."),
    });
    flip("go");
    await settle();
    expect(ipc.callsTo("settings_assign_toolchain_profile")).toHaveLength(1);
    expect(profileSwitch("go")?.checked).toBe(false);
  });

  it("says where to add one when no profile of this platform is recorded", async () => {
    await start(() => overview({ toolchain_profiles: [toolchain("msvc", "windows")] }));
    expect(root.textContent).toContain(PROFILES_EMPTY);
    expect(root.querySelector(".workspace-profile")).toBeNull();
  });
});
