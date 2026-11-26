import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setInvoker } from "../src/ipc/client";
import type { SettingsOverview, ToolchainDiscovery, ToolchainProposalView } from "../src/ipc/types";
import { createMockBackend } from "../src/mock/backend";
import { HOSTILE } from "../src/mock/fixtures";
import { type SettingsApp, startSettings } from "../src/settings/app";
import {
  ADD_SELECTED_LABEL,
  DISCOVER_LABEL,
  DISCOVERY_HELP,
  GRANT_SELECTED_LABEL,
  NOTHING_FOUND,
  SELECTION_LIMIT,
  WORKSPACE_GRANT_MISSING,
} from "../src/settings/discovery_panel";
import {
  buttonByText,
  chatDefaultsView,
  choose,
  type FakeIpc,
  installFakeIpc,
  mountRoot,
  rejection,
  settle,
} from "./helpers";

const A = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

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
        execute: "allow",
        task_network: "deny",
        host_services: "deny",
        web_fetch: "deny",
        web_fetch_hosts: [],
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
    package_profiles: [],
    container_grants: [],
    container_grants_supported: true,
    skills: [],
    chat: chatDefaultsView(),
    scope: null,
    ...overrides,
  };
}

function digest(n: number): string {
  return n.toString(16).padStart(2, "0").repeat(32);
}

function proposal(overrides: Partial<ToolchainProposalView> = {}): ToolchainProposalView {
  return {
    digest: digest(1),
    family: "go",
    family_label: "Go",
    name: "go",
    version: "1.23.4",
    read_only_roots: [
      { folder: "C:\\Program Files\\Go", grant: "not_applicable" },
      { folder: "C:\\Windows\\System32", grant: "not_applicable" },
    ],
    path_entries: ["C:\\Program Files\\Go\\bin", "C:\\Windows\\System32"],
    environment: [
      { name: "GOROOT", value: "C:\\Program Files\\Go" },
      { name: "GOTOOLCHAIN", value: "local" },
    ],
    notes: ["Runs are offline."],
    refusal: null,
    recorded: false,
    ...overrides,
  };
}

const RUST = proposal({
  digest: digest(2),
  family: "rust",
  family_label: "Rust",
  name: "rust",
  version: "stable-x86_64-pc-windows-msvc",
  read_only_roots: [
    { folder: "C:\\Users\\op\\.rustup\\toolchains\\stable", grant: "not_applicable" },
  ],
  path_entries: ["C:\\Users\\op\\.rustup\\toolchains\\stable\\bin"],
  environment: [],
  notes: [],
});

function discovery(
  proposals: ToolchainProposalView[],
  overrides: Partial<ToolchainDiscovery> = {},
): ToolchainDiscovery {
  return {
    overview: overview(),
    workspace_id: null,
    workspace_grant_active: null,
    proposals,
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
  Array.from(root.querySelectorAll<HTMLButtonElement>(".nav-item"))
    .find((item) => item.textContent === "Advanced")
    ?.click();
}

function group(): HTMLElement {
  const found = Array.from(root.querySelectorAll<HTMLElement>(".block")).find(
    (candidate) => candidate.querySelector("h3")?.textContent === "Toolchain profiles",
  );
  if (found === undefined) {
    throw new Error("No Toolchain profiles group");
  }
  return found;
}

function boxes(): HTMLInputElement[] {
  return Array.from(group().querySelectorAll<HTMLInputElement>(".proposal input[type=checkbox]"));
}

function tick(index: number): void {
  const box = boxes()[index];
  if (box === undefined) {
    throw new Error(`No proposal ${index}`);
  }
  box.checked = !box.checked;
  box.dispatchEvent(new Event("change"));
}

async function generate(): Promise<void> {
  buttonByText(group(), DISCOVER_LABEL).click();
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

describe("generating toolchain profiles", () => {
  it("says it runs nothing and asks Rust only for a workspace identifier", async () => {
    await start(() => overview(), {
      settings_discover_toolchains: () => discovery([proposal(), RUST]),
    });
    expect(group().textContent).toContain(DISCOVERY_HELP);
    expect(DISCOVERY_HELP).toContain("runs no program");
    expect(DISCOVERY_HELP).toContain("rust-toolchain.toml");
    // Nothing is listed until the operator asks.
    expect(boxes()).toHaveLength(0);
    expect(ipc.callsTo("settings_discover_toolchains")).toHaveLength(0);

    await generate();

    expect(ipc.callsTo("settings_discover_toolchains")[0]?.args).toEqual({ workspace_id: null });
    expect(boxes()).toHaveLength(2);
  });

  it("shows every proposal's name, family, version, roots, PATH entries, variables, and notes", async () => {
    await start(() => overview(), {
      settings_discover_toolchains: () => discovery([proposal()]),
    });
    await generate();
    const text = group().textContent ?? "";
    for (const expected of [
      "go",
      "Go, 1.23.4",
      "C:\\Program Files\\Go",
      "C:\\Windows\\System32",
      "C:\\Program Files\\Go\\bin",
      "GOROOT=C:\\Program Files\\Go",
      "GOTOOLCHAIN=local",
      "Runs are offline.",
    ]) {
      expect(text).toContain(expected);
    }
  });

  it("says so when nothing is installed", async () => {
    await start(() => overview(), {
      settings_discover_toolchains: () => discovery([]),
    });
    await generate();
    expect(group().textContent).toContain(NOTHING_FOUND);
    expect(() => buttonByText(group(), ADD_SELECTED_LABEL)).toThrow();
  });

  it("renders hostile proposal text as text", async () => {
    await start(() => overview(), {
      settings_discover_toolchains: () =>
        discovery([
          proposal({
            name: HOSTILE.img,
            version: HOSTILE.script,
            environment: [{ name: "X", value: HOSTILE.img }],
            notes: [HOSTILE.rtl],
          }),
        ]),
    });
    await generate();
    expect(root.querySelector(".settings-body img, .settings-body script")).toBeNull();
    expect(group().textContent).toContain(HOSTILE.img);
  });
});

describe("adding proposals", () => {
  it("sends the revision and the selected digests in list order, never a path", async () => {
    await start(() => overview(), {
      settings_discover_toolchains: () => discovery([proposal(), RUST]),
      settings_add_discovered_profiles: () =>
        discovery([
          proposal({ recorded: true, refusal: "a profile with this name already exists" }),
          { ...RUST, recorded: true, refusal: "a profile with this name already exists" },
        ]),
    });
    await generate();
    const add = buttonByText(group(), ADD_SELECTED_LABEL);
    expect(add.disabled).toBe(true);
    tick(1);
    tick(0);
    expect(add.disabled).toBe(false);
    add.click();
    await settle();

    expect(ipc.callsTo("settings_add_discovered_profiles")[0]?.args).toEqual({
      revision: 5,
      digests: [digest(1), digest(2)],
      workspace_id: null,
    });
    // The refreshed list shows both as recorded, and neither can be selected again.
    expect(group().textContent).toContain("A profile with this name is recorded.");
    expect(boxes().every((box) => box.disabled && !box.checked)).toBe(true);
  });

  it("disables a proposal core would refuse and shows the reason", async () => {
    await start(() => overview(), {
      settings_discover_toolchains: () =>
        discovery([proposal({ refusal: "at most 32 profiles can be recorded; remove one first" })]),
    });
    await generate();
    expect(boxes()[0]?.disabled).toBe(true);
    expect(group().textContent).toContain(
      "Cannot be added: at most 32 profiles can be recorded; remove one first",
    );
    expect(buttonByText(group(), ADD_SELECTED_LABEL).disabled).toBe(true);
  });

  it("keeps the list and the selection when the operator declines", async () => {
    await start(() => overview(), {
      settings_discover_toolchains: () => discovery([proposal(), RUST]),
      settings_add_discovered_profiles: () => rejection("cancelled", "No change was made."),
    });
    await generate();
    tick(0);
    buttonByText(group(), ADD_SELECTED_LABEL).click();
    await settle();
    expect(root.querySelector(".notice")?.textContent).toBe("No change was made.");
    expect(boxes()).toHaveLength(2);
    expect(boxes()[0]?.checked).toBe(true);
  });

  it("shows Rust's refusal when the installed toolchains changed", async () => {
    await start(() => overview(), {
      settings_discover_toolchains: () => discovery([proposal()]),
      settings_add_discovered_profiles: () =>
        rejection("discovery_changed", "Generate the list again and select again."),
    });
    await generate();
    tick(0);
    buttonByText(group(), ADD_SELECTED_LABEL).click();
    await settle();
    expect(root.querySelector(".change-error")?.textContent).toBe(
      "Generate the list again and select again. (discovery_changed)",
    );
  });

  it("refuses more than eight proposals before asking Rust", async () => {
    const many = Array.from({ length: 9 }, (_, index) =>
      proposal({ digest: digest(index + 1), name: `go-${index}` }),
    );
    await start(() => overview(), {
      settings_discover_toolchains: () => discovery(many),
    });
    await generate();
    for (let index = 0; index < 9; index += 1) {
      tick(index);
    }
    buttonByText(group(), ADD_SELECTED_LABEL).click();
    await settle();
    expect(group().querySelector(".error")?.textContent).toBe(SELECTION_LIMIT);
    expect(ipc.callsTo("settings_add_discovered_profiles")).toHaveLength(0);
  });
});

describe("granting proposed roots on Windows", () => {
  const NEEDS = proposal({
    read_only_roots: [
      { folder: "C:\\Users\\op\\go", grant: "needs_grant" },
      { folder: "C:\\Program Files\\Go", grant: "readable" },
      { folder: "D:\\locked", grant: "needs_administrator" },
    ],
  });

  it("asks for container access of the chosen workspace and labels each root", async () => {
    await start(() => overview(), {
      settings_discover_toolchains: () =>
        discovery([NEEDS], { workspace_id: A, workspace_grant_active: false }),
    });
    const select = group().querySelector<HTMLSelectElement>("#discovery-workspace");
    if (select === null) {
      throw new Error("No workspace choice");
    }
    choose(select, A);
    await generate();

    expect(ipc.callsTo("settings_discover_toolchains")[0]?.args).toEqual({ workspace_id: A });
    const text = group().textContent ?? "";
    expect(text).toContain("C:\\Users\\op\\go (needs a grant)");
    expect(text).toContain("C:\\Program Files\\Go (the container can read it)");
    expect(text).toContain("D:\\locked (needs an administrator to grant)");
    expect(text).toContain(WORKSPACE_GRANT_MISSING);
  });

  it("sends the revision, the list's workspace, and the selected digests", async () => {
    await start(() => overview(), {
      settings_discover_toolchains: () => discovery([NEEDS], { workspace_id: A }),
      settings_grant_discovered_roots: () =>
        discovery(
          [
            {
              ...NEEDS,
              read_only_roots: [
                { folder: "C:\\Users\\op\\go", grant: "readable" },
                { folder: "C:\\Program Files\\Go", grant: "readable" },
                { folder: "D:\\locked", grant: "needs_administrator" },
              ],
            },
          ],
          { workspace_id: A, overview: overview({ revision: 6 }) },
        ),
    });
    await generate();
    const grant = buttonByText(group(), GRANT_SELECTED_LABEL);
    expect(grant.disabled).toBe(true);
    tick(0);
    expect(grant.disabled).toBe(false);
    grant.click();
    await settle();

    expect(ipc.callsTo("settings_grant_discovered_roots")[0]?.args).toEqual({
      revision: 5,
      workspace_id: A,
      digests: [digest(1)],
    });
    expect(group().textContent).toContain("C:\\Users\\op\\go (the container can read it)");
    // The grant keeps the selection, so the operator can add the profile next.
    expect(boxes()[0]?.checked).toBe(true);
    // No root of the selection needs a grant any more.
    expect(buttonByText(group(), GRANT_SELECTED_LABEL).disabled).toBe(true);
  });

  it("offers no grant without a workspace in the list", async () => {
    await start(() => overview(), {
      settings_discover_toolchains: () => discovery([proposal()]),
    });
    await generate();
    tick(0);
    expect(buttonByText(group(), GRANT_SELECTED_LABEL).disabled).toBe(true);
  });

  it("shows no workspace choice and no grant button off Windows", async () => {
    await start(() => overview({ container_grants_supported: false }), {
      settings_discover_toolchains: () =>
        discovery([proposal()], { overview: overview({ container_grants_supported: false }) }),
    });
    expect(group().querySelector("#discovery-workspace")).toBeNull();
    await generate();
    expect(ipc.callsTo("settings_discover_toolchains")[0]?.args).toEqual({ workspace_id: null });
    expect(() => buttonByText(group(), GRANT_SELECTED_LABEL)).toThrow();
  });
});

describe("mock discovery commands", () => {
  type Invoke = (command: string, args: Record<string, unknown>) => Promise<unknown>;

  it("adds selected samples once and refuses stale, unknown, and repeated selections", async () => {
    const { invoke } = createMockBackend() as { invoke: Invoke };
    const listed = (await invoke("settings_discover_toolchains", {
      workspace_id: null,
    })) as ToolchainDiscovery;
    // The mock starts with a recorded `rust` profile, which its sample proposal names too.
    expect(listed.proposals.find((entry) => entry.name === "rust")?.refusal).not.toBeNull();
    const [first, second] = listed.proposals.filter((entry) => entry.refusal === null);
    if (first === undefined || second === undefined) {
      throw new Error("two addable samples");
    }
    const before = listed.overview.revision;
    const added = (await invoke("settings_add_discovered_profiles", {
      revision: before,
      digests: [first.digest, second.digest],
      workspace_id: null,
    })) as ToolchainDiscovery;
    expect(added.overview.revision).toBe(before + 2);
    expect(added.proposals.find((entry) => entry.digest === first.digest)?.recorded).toBe(true);
    await expect(
      invoke("settings_add_discovered_profiles", {
        revision: before,
        digests: [first.digest],
        workspace_id: null,
      }),
    ).rejects.toMatchObject({ code: "stale_revision" });
    await expect(
      invoke("settings_add_discovered_profiles", {
        revision: added.overview.revision,
        digests: ["f".repeat(64)],
        workspace_id: null,
      }),
    ).rejects.toMatchObject({ code: "discovery_changed" });
    await expect(
      invoke("settings_add_discovered_profiles", {
        revision: added.overview.revision,
        digests: [first.digest, first.digest],
        workspace_id: null,
      }),
    ).rejects.toMatchObject({ code: "invalid_request" });
  });

  it("grants the home-folder roots a workspace needs and then reports them readable", async () => {
    const { invoke } = createMockBackend() as { invoke: Invoke };
    const settings = (await invoke("settings_overview", {})) as SettingsOverview;
    const workspace = settings.workspaces[0]?.workspace_id ?? "";
    const listed = (await invoke("settings_discover_toolchains", {
      workspace_id: workspace,
    })) as ToolchainDiscovery;
    const needing = listed.proposals.find((entry) =>
      entry.read_only_roots.some((rootEntry) => rootEntry.grant === "needs_grant"),
    );
    if (needing === undefined) {
      throw new Error("a sample needs a grant");
    }
    const granted = (await invoke("settings_grant_discovered_roots", {
      revision: listed.overview.revision,
      workspace_id: workspace,
      digests: [needing.digest],
    })) as ToolchainDiscovery;
    const after = granted.proposals.find((entry) => entry.digest === needing.digest);
    expect(after?.read_only_roots.some((rootEntry) => rootEntry.grant === "needs_grant")).toBe(
      false,
    );
    await expect(
      invoke("settings_grant_discovered_roots", {
        revision: granted.overview.revision,
        workspace_id: workspace,
        digests: [needing.digest],
      }),
    ).rejects.toMatchObject({ code: "nothing_to_grant" });
  });
});
