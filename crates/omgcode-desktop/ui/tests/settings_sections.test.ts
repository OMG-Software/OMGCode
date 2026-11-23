import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setInvoker } from "../src/ipc/client";
import type {
  ProviderProfileView,
  SettingsOverview,
  WorkspaceSettingsView,
  WriteToolAccess,
} from "../src/ipc/types";
import { type SettingsApp, startSettings } from "../src/settings/app";
import { workspaceTooLong } from "../src/settings/chat_section";
import {
  buttonByText,
  chatDefaultsView,
  type FakeIpc,
  installFakeIpc,
  mountRoot,
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
    package_profiles: [],
    container_grants: [],
    skills: [],
    chat: chatDefaultsView(),
    scope: null,
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
  app?.dispose();
  ipc = installFakeIpc({ settings_overview: reply, ...more });
  root = mountRoot();
  app = startSettings(root);
  await settle();
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

function chooser(): HTMLSelectElement {
  return root.querySelector('select[aria-label="Workspace"]') as HTMLSelectElement;
}

function pick(select: HTMLSelectElement, value: string): void {
  select.value = value;
  select.dispatchEvent(new Event("change"));
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
    await start(() => overview());
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

describe("workspace chooser", () => {
  it("is absent for one workspace and chooses between several", async () => {
    await start(() => overview({ workspaces: [workspace(A, "/home/dev/a")] }));
    expect(chooser()).toBeNull();
    expect(heading()).toBe("/home/dev/a");

    await start(() => overview());
    expect(chooser().labels?.[0]?.textContent).toBe("Workspace");
    expect(Array.from(chooser().options).map((o) => [o.value, o.textContent])).toEqual([
      [A, "/home/dev/a"],
      [B, "/home/dev/b"],
    ]);
    expect(heading()).toBe("/home/dev/a");
  });

  it("shows the chosen workspace, sends its identifier, and remembers it by identifier", async () => {
    await start(() => overview(), {
      settings_set_permissions: () =>
        overview({
          revision: 4,
          workspaces: [
            workspace(B, "/home/dev/b", { write: "allow", execute: "prompt" }),
            workspace(A, "/home/dev/a"),
          ],
        }),
    });
    pick(chooser(), B);
    expect(heading()).toBe("/home/dev/b");
    expect(chooser().value).toBe(B);
    // Choosing a workspace to look at sends nothing.
    expect(ipc.calls.map((call) => call.command)).toEqual(["settings_overview"]);

    choose("Execute", "prompt");
    await settle();
    expect(ipc.callsTo("settings_set_permissions")[0]?.args).toMatchObject({
      workspace_id: B,
      revision: 3,
    });
    // The order changed, but the same workspace is still shown.
    expect(heading()).toBe("/home/dev/b");
    expect(chooser().value).toBe(B);

    go("Model");
    go("Workspaces");
    expect(heading()).toBe("/home/dev/b");
  });

  it("keeps keyboard focus on the chooser after switching workspace", async () => {
    await start(() => overview());
    chooser().focus();
    pick(chooser(), B);
    expect(heading()).toBe("/home/dev/b");
    expect(document.activeElement).toBe(chooser());
  });

  it("falls back to the first workspace when the shown one disappears", async () => {
    await start(() => overview());
    pick(chooser(), B);
    expect(heading()).toBe("/home/dev/b");

    ipc.on("settings_overview", () =>
      overview({ revision: 4, workspaces: [workspace(A, "/home/dev/a")] }),
    );
    buttonByText(root, "Refresh").click();
    await settle();
    expect(heading()).toBe("/home/dev/a");
    expect(chooser()).toBeNull();
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
    expect(notes).toEqual(Array(5).fill("Limited by Write: Prompt"));

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
    // Only the tools wider than Write carry the note: multi-edit, rename, delete.
    const rows = Array.from(root.querySelectorAll(".tool-limits .perm-row"));
    const noted = rows
      .filter((row) => row.querySelector(".perm-note") !== null)
      .map((row) => row.querySelector(".perm-label")?.textContent);
    expect(noted).toEqual(["Multi-edit tool", "Rename tool", "Delete tool"]);

    await start(() => overview({ workspaces: [workspace(A, "/home/dev/a", { write: "allow" })] }));
    expect(root.querySelectorAll(".perm-note")).toHaveLength(0);
  });

  it("hold the tool limits in a disclosure under Write that stays open", async () => {
    await start(() => overview(), {
      settings_set_permissions: () => overview({ revision: 4 }),
    });
    const details = root.querySelector("details.tool-limits") as HTMLDetailsElement;
    expect(details.querySelector("summary")?.textContent).toBe("File-write tool limits");
    expect(details.querySelectorAll("[role=radiogroup]")).toHaveLength(5);
    expect(details.open).toBe(false);
    details.open = true;
    details.dispatchEvent(new Event("toggle"));
    choose("Delete tool", "prompt");
    await settle();
    expect((root.querySelector("details.tool-limits") as HTMLDetailsElement).open).toBe(true);
  });
});

describe("advanced section", () => {
  it("is read-only: no button, field, or checkbox changes anything", async () => {
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
    expect(root.querySelector(".settings-body button")).toBeNull();
    expect(root.querySelector(".settings-body input, .settings-body select")).toBeNull();
    expect(root.querySelector(".settings-body textarea")).toBeNull();
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
    expect(text).toContain("not set (disabled)");
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

  it("shows only the scoped workspace without sections, chooser, root, or registration", async () => {
    await start(() => scoped(B));

    expect(heading()).toBe("/home/dev/b");
    expect(root.textContent).toContain("Workspace settings");
    expect(root.textContent).not.toContain("/home/dev/a");
    expect(root.querySelector(".settings-window.scoped")).not.toBeNull();
    expect((root.querySelector(".settings-nav") as HTMLElement).hidden).toBe(true);
    expect(chooser()).toBeNull();
    expect(root.textContent).not.toContain("Development root");
    expect(root.textContent).not.toContain("Register workspace");
    expect(root.querySelector(".tool-limits")).not.toBeNull();
    expect(root.textContent).toContain("Package cache");
    expect((root.querySelector(".scope-bar") as HTMLElement).hidden).toBe(false);
  });

  it("restores the sections and the chooser when All settings clears the scope", async () => {
    await start(() => scoped(B), { settings_clear_scope: () => overview({ revision: 4 }) });
    buttonByText(root, "All settings").click();
    await settle();

    expect(ipc.callsTo("settings_clear_scope")).toEqual([
      { command: "settings_clear_scope", args: {} },
    ]);
    expect((root.querySelector(".settings-nav") as HTMLElement).hidden).toBe(false);
    expect((root.querySelector(".scope-bar") as HTMLElement).hidden).toBe(true);
    expect(root.querySelector(".settings-window.scoped")).toBeNull();
    expect(chooser()).not.toBeNull();
    expect(current()).toBe("Workspaces");
  });

  it("shows a note and the full view when the scoped workspace is gone", async () => {
    await start(() => scoped("cccccccc-cccc-4ccc-8ccc-cccccccccccc"));

    expect(root.textContent).toContain("no longer registered");
    expect((root.querySelector(".settings-nav") as HTMLElement).hidden).toBe(false);
    expect(chooser()).not.toBeNull();
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
    pick(chooser(), B);
    expect(field<HTMLTextAreaElement>(`workspace-instructions-${B}`).value).toBe("");
  });

  it("keeps a typed workspace draft when the chooser moves away and back", async () => {
    await start(() => overview());
    type(field(`workspace-instructions-${A}`), "Unsaved");
    pick(chooser(), B);
    pick(chooser(), A);
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
