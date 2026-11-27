import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setInvoker } from "../src/ipc/client";
import type { SettingsOverview, ToolchainProfileView } from "../src/ipc/types";
import { createMockBackend } from "../src/mock/backend";
import { HOSTILE } from "../src/mock/fixtures";
import { type SettingsApp, startSettings } from "../src/settings/app";
import {
  ADD_PACKAGE_LABEL,
  ADD_TOOLCHAIN_LABEL,
  ENVIRONMENT_WARNING,
  GRANT_FOLDER_LABEL,
  GRANT_HELP,
  GRANT_READ_EXECUTE_HINT,
  GRANT_READ_WRITE_HINT,
  GRANT_WORKSPACE_FOLDER_LABEL,
  GRANTS_UNSUPPORTED,
  PACKAGE_HELP,
  parseEnvironment,
} from "../src/settings/execution_forms";
import {
  buttonByText,
  chatDefaultsView,
  type FakeIpc,
  installFakeIpc,
  mountRoot,
  rejection,
  settle,
} from "./helpers";

const A = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const GRANT = "11111111-1111-4111-8111-111111111111";

function overview(overrides: Partial<SettingsOverview> = {}): SettingsOverview {
  return {
    revision: 5,
    development_root: "C:\\dev",
    workspaces: [
      {
        workspace_id: A,
        folder_display: "C:\\dev\\a",
        model_tools: true,
        read: "allow",
        write: "prompt",
        execute: "deny",
        task_network: "deny",
        host_services: "deny",
        web_fetch: "deny",
        web_fetch_hosts: [],
        profiles: [],
        write_tools: {
          edit: "allow",
          multi_edit: "allow",
          create: "allow",
          rename: "allow",
          delete: "allow",
          create_directory: "allow",
        },
        package_cache_root: null,
        instructions: "",
        instructions_max_bytes: 16_360,
      },
    ],
    connection: {
      endpoint: "http://127.0.0.1:1234/v1",
      source: "active",
      profile: null,
      kind: null,
      selectable: true,
    },
    provider_profiles: [],
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

const RUST: ToolchainProfileView = {
  name: "rust",
  platform: "windows",
  read_only_roots: ["C:\\tools\\rust"],
  path_entries: ["C:\\tools\\rust\\bin"],
  environment_names: ["RUSTUP_HOME"],
};

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
  Array.from(root.querySelectorAll<HTMLButtonElement>(".nav-item"))
    .find((item) => item.textContent === "Advanced")
    ?.click();
}

function field(id: string): HTMLInputElement | HTMLTextAreaElement {
  const found = root.querySelector(`#${id}`);
  if (!(found instanceof HTMLInputElement || found instanceof HTMLTextAreaElement)) {
    throw new Error(`No field ${id}`);
  }
  return found;
}

function type(id: string, value: string): void {
  const control = field(id);
  control.value = value;
  control.dispatchEvent(new Event("input"));
}

function block(title: string): HTMLElement {
  const found = Array.from(root.querySelectorAll<HTMLElement>(".block")).find(
    (candidate) => candidate.querySelector("h3")?.textContent === title,
  );
  if (found === undefined) {
    throw new Error(`No group ${title}`);
  }
  return found;
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

describe("parsing variables", () => {
  it("splits at the first equals sign and skips blank lines", () => {
    expect(parseEnvironment("A=1\n\nB=x=y\r\nC=")).toEqual({
      ok: true,
      entries: [
        { name: "A", value: "1" },
        { name: "B", value: "x=y" },
        { name: "C", value: "" },
      ],
    });
  });

  it("names the first line without an equals sign or a name", () => {
    expect(parseEnvironment("A=1\nB")).toEqual({ ok: false, line: 2 });
    expect(parseEnvironment("=1")).toEqual({ ok: false, line: 1 });
    // A name of only spaces is no name, so it is refused before any dialog.
    expect(parseEnvironment("A=1\n  =x")).toEqual({ ok: false, line: 2 });
  });
});

describe("toolchain profiles", () => {
  it("sends the name, variables, and counts, and never a path", async () => {
    await start(() => overview(), {
      settings_add_toolchain_profile: () =>
        overview({ revision: 6, toolchain_profiles: [{ ...RUST }] }),
    });
    expect(block("Toolchain profiles").textContent).toContain(ENVIRONMENT_WARNING);
    type("new-toolchain-name", " rust ");
    type("new-toolchain-roots", "2");
    type("new-toolchain-paths", "1");
    type("new-toolchain-environment", "RUSTUP_HOME=C:\\tools\\rustup\n\nFLAG=a=b");
    buttonByText(root, ADD_TOOLCHAIN_LABEL).click();
    await settle();

    expect(ipc.callsTo("settings_add_toolchain_profile")[0]?.args).toEqual({
      revision: 5,
      name: "rust",
      environment: [
        { name: "RUSTUP_HOME", value: "C:\\tools\\rustup" },
        { name: "FLAG", value: "a=b" },
      ],
      root_count: 2,
      path_entry_count: 1,
    });
    expect(block("Toolchain profiles").textContent).toContain("RUSTUP_HOME");
    // The form is emptied after the core recorded the profile.
    expect(field("new-toolchain-name").value).toBe("");
  });

  it("keeps the typed form when the operator cancels a dialog", async () => {
    await start(() => overview(), {
      settings_add_toolchain_profile: () => rejection("cancelled", "No change was made."),
    });
    type("new-toolchain-name", "rust");
    buttonByText(root, ADD_TOOLCHAIN_LABEL).click();
    await settle();
    expect(root.querySelector(".notice")?.textContent).toBe("No change was made.");
    expect(field("new-toolchain-name").value).toBe("rust");
  });

  it("shows core's refusal as text", async () => {
    await start(() => overview(), {
      settings_add_toolchain_profile: () =>
        rejection("control", "a profile with this name already exists; remove it first"),
    });
    type("new-toolchain-name", "rust");
    buttonByText(root, ADD_TOOLCHAIN_LABEL).click();
    await settle();
    expect(root.querySelector(".change-error")?.textContent).toBe(
      "a profile with this name already exists; remove it first (control)",
    );
  });

  it("refuses a bad count or variable line before asking Rust anything", async () => {
    await start(() => overview());
    type("new-toolchain-roots", "0");
    buttonByText(root, ADD_TOOLCHAIN_LABEL).click();
    await settle();
    expect(block("Toolchain profiles").querySelector(".error")?.textContent).toContain("1 to 16");
    type("new-toolchain-roots", "1");
    type("new-toolchain-environment", "NOEQUALS");
    buttonByText(root, ADD_TOOLCHAIN_LABEL).click();
    await settle();
    expect(block("Toolchain profiles").querySelector(".error")?.textContent).toContain("line 1");
    expect(ipc.callsTo("settings_add_toolchain_profile")).toHaveLength(0);
  });

  it("removes a profile by name with the revision", async () => {
    await start(() => overview({ toolchain_profiles: [{ ...RUST }] }), {
      settings_remove_toolchain_profile: () => overview({ revision: 6 }),
    });
    buttonByText(block("Toolchain profiles"), "Remove").click();
    await settle();
    expect(ipc.callsTo("settings_remove_toolchain_profile")[0]?.args).toEqual({
      revision: 5,
      name: "rust",
    });
    expect(root.textContent).toContain("No toolchain profile is recorded.");
  });

  it("renders a hostile name as text", async () => {
    await start(() =>
      overview({ toolchain_profiles: [{ ...RUST, name: HOSTILE.img, environment_names: [] }] }),
    );
    expect(root.querySelector(".settings-body img")).toBeNull();
    expect(root.textContent).toContain(HOSTILE.img);
  });
});

describe("package-network profiles", () => {
  it("states that a profile is configuration only and sends trimmed origin lines", async () => {
    await start(() => overview(), {
      settings_add_package_profile: () =>
        overview({
          revision: 6,
          package_profiles: [
            { name: "crates", origins: ["https://crates.io:443/"], credentials_bound: 0 },
          ],
        }),
    });
    expect(block("Package-network profiles").textContent).toContain(PACKAGE_HELP);
    expect(PACKAGE_HELP).toContain("configuration only");
    expect(PACKAGE_HELP).toContain("no network access");
    type("new-package-name", "crates");
    type("new-package-origins", " https://crates.io \n\nhttps://index.crates.io/ ");
    buttonByText(root, ADD_PACKAGE_LABEL).click();
    await settle();
    expect(ipc.callsTo("settings_add_package_profile")[0]?.args).toEqual({
      revision: 5,
      name: "crates",
      origins: ["https://crates.io", "https://index.crates.io/"],
    });
    expect(block("Package-network profiles").textContent).toContain("https://crates.io:443/");
  });

  it("shows core's controlled origin error", async () => {
    await start(() => overview(), {
      settings_add_package_profile: () =>
        rejection("control", "an origin must not carry credentials"),
    });
    type("new-package-name", "x");
    type("new-package-origins", "https://u:p@host/");
    buttonByText(root, ADD_PACKAGE_LABEL).click();
    await settle();
    expect(root.querySelector(".change-error")?.textContent).toContain(
      "an origin must not carry credentials",
    );
  });

  it("removes a profile by name", async () => {
    await start(
      () =>
        overview({
          package_profiles: [
            { name: "crates", origins: ["https://crates.io"], credentials_bound: 1 },
          ],
        }),
      { settings_remove_package_profile: () => overview({ revision: 6 }) },
    );
    buttonByText(block("Package-network profiles"), "Remove").click();
    await settle();
    expect(ipc.callsTo("settings_remove_package_profile")[0]?.args).toEqual({
      revision: 5,
      name: "crates",
    });
  });
});

describe("container grants", () => {
  const GRANT_VIEW = {
    grant_id: GRANT,
    workspace_id: A,
    directory: "C:\\dev\\a\\vendor",
    access: "read_execute",
    state: "pending",
  } as const;

  it("grants with the workspace and access only, never a path", async () => {
    await start(() => overview(), {
      settings_grant_container_folder: () =>
        overview({ revision: 6, container_grants: [{ ...GRANT_VIEW, state: "active" }] }),
    });
    const access = root.querySelector<HTMLSelectElement>("#new-grant-access");
    if (access === null) {
      throw new Error("no access select");
    }
    access.value = "read_write";
    access.dispatchEvent(new Event("change"));
    buttonByText(root, GRANT_WORKSPACE_FOLDER_LABEL).click();
    await settle();
    expect(ipc.callsTo("settings_grant_container_folder")[0]?.args).toEqual({
      revision: 5,
      workspace_id: A,
      access: "read_write",
    });
    expect(block("Container grants").textContent).toContain("C:\\dev\\a\\vendor");
  });

  it("says the sandbox gets the access and that read and write opens no folder dialog", async () => {
    await start(() => overview());
    const group = block("Container grants");
    expect(group.textContent).toContain(GRANT_HELP);
    expect(group.textContent).toContain("Workspace whose commands get access");
    const hint = group.querySelector("#new-grant-hint");
    const access = group.querySelector<HTMLSelectElement>("#new-grant-access");
    if (hint === null || access === null) {
      throw new Error("no hint or access select");
    }
    // Read and execute is the default: a folder dialog follows the button.
    expect(hint.textContent).toBe(GRANT_READ_EXECUTE_HINT);
    expect(buttonByText(group, GRANT_FOLDER_LABEL)).toBeTruthy();

    access.value = "read_write";
    access.dispatchEvent(new Event("change"));
    expect(hint.textContent).toBe(GRANT_READ_WRITE_HINT);
    expect(buttonByText(group, GRANT_WORKSPACE_FOLDER_LABEL)).toBeTruthy();
    expect(group.textContent).not.toContain(GRANT_FOLDER_LABEL);

    access.value = "read_execute";
    access.dispatchEvent(new Event("change"));
    expect(hint.textContent).toBe(GRANT_READ_EXECUTE_HINT);
    expect(buttonByText(group, GRANT_FOLDER_LABEL)).toBeTruthy();
  });

  it("shows a pending grant with its state and a Revoke button", async () => {
    await start(() => overview({ container_grants: [{ ...GRANT_VIEW }] }), {
      settings_revoke_container_grant: () => overview({ revision: 6 }),
    });
    expect(block("Container grants").textContent).toContain("pending");
    buttonByText(block("Container grants"), "Revoke").click();
    await settle();
    expect(ipc.callsTo("settings_revoke_container_grant")[0]?.args).toEqual({
      revision: 5,
      grant_id: GRANT,
    });
  });

  it("asks to register a workspace first when there is none", async () => {
    await start(() => overview({ workspaces: [] }));
    expect(root.querySelector("#new-grant-workspace")).toBeNull();
    expect(block("Container grants").textContent).toContain("Register a workspace");
  });

  it("offers no control where grants are unsupported", async () => {
    await start(() => overview({ container_grants_supported: false }));
    const group = block("Container grants");
    expect(group.textContent).toContain(GRANTS_UNSUPPORTED);
    expect(group.querySelector("button, input, select, textarea, details")).toBeNull();
  });
});

describe("mock execution commands", () => {
  async function overviewOf(invoke: (c: string, a: Record<string, unknown>) => Promise<unknown>) {
    return (await invoke("settings_overview", {})) as SettingsOverview;
  }

  it("adds and removes a toolchain profile and refuses a duplicate or a stale revision", async () => {
    const { invoke } = createMockBackend();
    const before = await overviewOf(invoke);
    const added = (await invoke("settings_add_toolchain_profile", {
      revision: before.revision,
      name: "node",
      environment: [{ name: "NODE_ENV", value: "x" }],
      root_count: 1,
      path_entry_count: 1,
    })) as SettingsOverview;
    const profile = added.toolchain_profiles.find((entry) => entry.name === "node");
    expect(profile?.environment_names).toEqual(["NODE_ENV"]);
    expect(profile?.path_entries).toHaveLength(1);
    await expect(
      invoke("settings_add_toolchain_profile", {
        revision: before.revision,
        name: "other",
        environment: [],
        root_count: 1,
        path_entry_count: 0,
      }),
    ).rejects.toMatchObject({ code: "stale_revision" });
    await expect(
      invoke("settings_add_toolchain_profile", {
        revision: added.revision,
        name: "NODE",
        environment: [],
        root_count: 1,
        path_entry_count: 0,
      }),
    ).rejects.toMatchObject({ code: "control" });
    const removed = (await invoke("settings_remove_toolchain_profile", {
      revision: added.revision,
      name: "node",
    })) as SettingsOverview;
    expect(removed.toolchain_profiles.some((entry) => entry.name === "node")).toBe(false);
  });

  it("assigns a profile, grants its home-folder root, and revokes it on unassign (ADR-0107)", async () => {
    const { invoke } = createMockBackend();
    const before = await overviewOf(invoke);
    const workspace = before.workspaces[0]?.workspace_id ?? "";
    const change = (command: string, revision: number) =>
      invoke(command, { revision, workspace_id: workspace, name: "rust" });

    const assigned = (await change(
      "settings_assign_toolchain_profile",
      before.revision,
    )) as SettingsOverview;
    expect(assigned.workspaces[0]?.profiles).toEqual(["rust"]);
    expect(assigned.container_grants).toEqual([
      expect.objectContaining({
        workspace_id: workspace,
        directory: "/home/dev/.rustup",
        access: "read_execute",
        state: "active",
      }),
    ]);
    await expect(
      change("settings_assign_toolchain_profile", assigned.revision),
    ).rejects.toMatchObject({ code: "control" });

    const unassigned = (await change(
      "settings_unassign_toolchain_profile",
      assigned.revision,
    )) as SettingsOverview;
    expect(unassigned.workspaces[0]?.profiles).toEqual([]);
    expect(unassigned.container_grants).toEqual([]);
    await expect(
      change("settings_unassign_toolchain_profile", unassigned.revision),
    ).rejects.toMatchObject({ code: "control" });
  });

  it("ends the assignments and their grants when an assigned profile is removed", async () => {
    const { invoke } = createMockBackend();
    const before = await overviewOf(invoke);
    const workspace = before.workspaces[0]?.workspace_id ?? "";
    const assigned = (await invoke("settings_assign_toolchain_profile", {
      revision: before.revision,
      workspace_id: workspace,
      name: "rust",
    })) as SettingsOverview;
    const removed = (await invoke("settings_remove_toolchain_profile", {
      revision: assigned.revision,
      name: "rust",
    })) as SettingsOverview;
    expect(removed.workspaces.every((entry) => entry.profiles.length === 0)).toBe(true);
    expect(removed.container_grants).toEqual([]);
  });

  it("adds and removes a package profile and refuses a bad origin", async () => {
    const { invoke } = createMockBackend();
    const before = await overviewOf(invoke);
    await expect(
      invoke("settings_add_package_profile", {
        revision: before.revision,
        name: "x",
        origins: ["ftp://host"],
      }),
    ).rejects.toMatchObject({ code: "control" });
    const added = (await invoke("settings_add_package_profile", {
      revision: before.revision,
      name: "crates",
      origins: ["https://crates.io/"],
    })) as SettingsOverview;
    expect(added.package_profiles).toHaveLength(1);
    const removed = (await invoke("settings_remove_package_profile", {
      revision: added.revision,
      name: "crates",
    })) as SettingsOverview;
    expect(removed.package_profiles).toEqual([]);
  });

  it("grants and revokes a container grant", async () => {
    const { invoke } = createMockBackend();
    const before = await overviewOf(invoke);
    const workspace = before.workspaces[0]?.workspace_id ?? "";
    const granted = (await invoke("settings_grant_container_folder", {
      revision: before.revision,
      workspace_id: workspace,
      access: "read_execute",
    })) as SettingsOverview;
    expect(granted.container_grants).toHaveLength(1);
    await expect(
      invoke("settings_grant_container_folder", {
        revision: granted.revision,
        workspace_id: "99999999-9999-4999-8999-999999999999",
        access: "read_execute",
      }),
    ).rejects.toMatchObject({ code: "unknown_workspace" });
    const revoked = (await invoke("settings_revoke_container_grant", {
      revision: granted.revision,
      grant_id: granted.container_grants[0]?.grant_id,
    })) as SettingsOverview;
    expect(revoked.container_grants).toEqual([]);
  });

  it("grants read and write on the workspace's own folder, as Rust does", async () => {
    const { invoke } = createMockBackend();
    const before = await overviewOf(invoke);
    const workspace = before.workspaces[0];
    if (workspace === undefined) {
      throw new Error("the mock has a workspace");
    }
    const granted = (await invoke("settings_grant_container_folder", {
      revision: before.revision,
      workspace_id: workspace.workspace_id,
      access: "read_write",
    })) as SettingsOverview;
    expect(granted.container_grants[0]?.directory).toBe(workspace.folder_display);
    expect(granted.container_grants[0]?.access).toBe("read_write");
  });
});
