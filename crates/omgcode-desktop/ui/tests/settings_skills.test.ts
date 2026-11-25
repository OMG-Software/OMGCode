import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { neutralise } from "../src/dom";
import { setInvoker } from "../src/ipc/client";
import type { SettingsOverview, SkillSettingView } from "../src/ipc/types";
import { createMockBackend } from "../src/mock/backend";
import { HOSTILE } from "../src/mock/fixtures";
import { IMPORT_SKILL_LABEL } from "../src/settings/advanced_section";
import { type SettingsApp, startSettings } from "../src/settings/app";
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
const B = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

function workspace(id: string, folder: string): SettingsOverview["workspaces"][number] {
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
  };
}

function overview(
  skills: SkillSettingView[],
  overrides: Partial<SettingsOverview> = {},
): SettingsOverview {
  return {
    revision: 5,
    development_root: "/home/dev",
    workspaces: [workspace(A, "/home/dev/a"), workspace(B, "/home/dev/b")],
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
    skills,
    chat: chatDefaultsView(),
    scope: null,
    ...overrides,
  };
}

const REVIEW: SkillSettingView = {
  name: "review",
  application: true,
  workspace_overrides: [{ workspace_id: A, enabled: false }],
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
  const advanced = Array.from(root.querySelectorAll<HTMLButtonElement>(".nav-item")).find(
    (item) => item.textContent === "Advanced",
  );
  advanced?.click();
}

function skillRow(name: string): HTMLElement {
  const found = Array.from(root.querySelectorAll<HTMLElement>(".row")).find(
    (row) => row.querySelector(".row-title")?.textContent === name,
  );
  if (found === undefined) {
    throw new Error(`No skill ${name}`);
  }
  return found;
}

function select(label: string): HTMLSelectElement {
  const found = root.querySelector(`select[aria-label="${label}"]`);
  if (!(found instanceof HTMLSelectElement)) {
    throw new Error(`No select ${label}`);
  }
  return found;
}

function choose(control: HTMLSelectElement, value: string): void {
  control.value = value;
  control.dispatchEvent(new Event("change"));
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

describe("skills in the Advanced section", () => {
  it("shows the application setting and each workspace's override as controls", async () => {
    await start(() => overview([REVIEW]));

    expect(select("Application setting of review").value).toBe("enabled");
    expect(select("Setting of review in /home/dev/a").value).toBe("disabled");
    // No override: the workspace follows the application setting.
    expect(select("Setting of review in /home/dev/b").value).toBe("inherit");
    // Only a workspace has the inherit choice.
    const application = Array.from(select("Application setting of review").options);
    expect(application.map((option) => option.value)).toEqual(["enabled", "disabled"]);
    const inWorkspace = Array.from(select("Setting of review in /home/dev/b").options);
    expect(inWorkspace.map((option) => option.value)).toEqual(["inherit", "enabled", "disabled"]);
  });

  it("neutralises the workspace folder in each control's accessible name", async () => {
    await start(() =>
      overview([REVIEW], { workspaces: [workspace(A, HOSTILE.rtl), workspace(B, "/home/dev/b")] }),
    );
    const labels = Array.from(root.querySelectorAll("select")).map(
      (control) => control.getAttribute("aria-label") ?? "",
    );
    expect(labels).toContain(`Setting of review in ${neutralise(HOSTILE.rtl)}`);
    expect(labels.some((label) => label.includes(String.fromCharCode(0x202e)))).toBe(false);
  });

  it("shows an unset application setting as disabled", async () => {
    await start(() => overview([{ name: "review", application: null, workspace_overrides: [] }]));
    const control = select("Application setting of review");
    expect(control.selectedOptions[0]?.textContent).toBe("Not set (disabled)");
  });

  it("imports with only the revision, so no path crosses the window", async () => {
    await start(() => overview([]), {
      settings_import_skill: () => overview([REVIEW], { revision: 6 }),
    });
    expect(root.textContent).toContain("No skill is imported.");

    buttonByText(root, IMPORT_SKILL_LABEL).click();
    await settle();

    expect(ipc.callsTo("settings_import_skill")[0]?.args).toEqual({ revision: 5 });
    expect(root.textContent).toContain("Revision 6");
    expect(skillRow("review")).toBeDefined();
  });

  it("shows a refused import as a controlled error and keeps the list", async () => {
    await start(() => overview([REVIEW]), {
      settings_import_skill: () =>
        rejection(
          "skill_name_directory_mismatch",
          "The skill's name must equal the name of its folder.",
        ),
    });
    buttonByText(root, IMPORT_SKILL_LABEL).click();
    await settle();

    expect(root.querySelector(".change-error")?.textContent).toBe(
      "The skill's name must equal the name of its folder. (skill_name_directory_mismatch)",
    );
    expect(skillRow("review")).toBeDefined();
  });

  it("says nothing changed when the operator closes the folder dialog", async () => {
    await start(() => overview([]), {
      settings_import_skill: () => rejection("cancelled", "No change was made."),
    });
    buttonByText(root, IMPORT_SKILL_LABEL).click();
    await settle();
    expect(root.querySelector(".notice")?.textContent).toBe("No change was made.");
    expect(root.querySelector<HTMLElement>(".change-error")?.hidden).toBe(true);
  });

  it("sets the application setting with the skill name, a null workspace, and the revision", async () => {
    await start(() => overview([REVIEW]), {
      settings_set_skill_state: () =>
        overview([{ ...REVIEW, application: false }], { revision: 6 }),
    });
    choose(select("Application setting of review"), "disabled");
    await settle();

    expect(ipc.callsTo("settings_set_skill_state")[0]?.args).toEqual({
      revision: 5,
      name: "review",
      workspace_id: null,
      skill_state: "disabled",
    });
    expect(select("Application setting of review").value).toBe("disabled");
  });

  it("sets and clears one workspace's override", async () => {
    await start(() => overview([REVIEW]), {
      settings_set_skill_state: () => overview([REVIEW], { revision: 6 }),
    });
    choose(select("Setting of review in /home/dev/b"), "enabled");
    await settle();
    choose(select("Setting of review in /home/dev/a"), "inherit");
    await settle();

    expect(ipc.callsTo("settings_set_skill_state").map((call) => call.args)).toEqual([
      { revision: 5, name: "review", workspace_id: B, skill_state: "enabled" },
      { revision: 6, name: "review", workspace_id: A, skill_state: "inherit" },
    ]);
  });

  it("shows the recorded state again when the operator declines the confirmation", async () => {
    await start(() => overview([{ name: "review", application: null, workspace_overrides: [] }]), {
      settings_set_skill_state: () => rejection("cancelled", "No change was made."),
    });
    choose(select("Application setting of review"), "enabled");
    await settle();

    expect(root.querySelector(".notice")?.textContent).toBe("No change was made.");
    expect(select("Application setting of review").selectedOptions[0]?.textContent).toBe(
      "Not set (disabled)",
    );
  });

  it("removes a skill by name with the revision", async () => {
    await start(() => overview([REVIEW]), {
      settings_remove_skill: () => overview([], { revision: 6 }),
    });
    buttonByText(skillRow("review"), "Remove").click();
    await settle();

    expect(ipc.callsTo("settings_remove_skill")[0]?.args).toEqual({
      revision: 5,
      name: "review",
    });
    expect(root.textContent).toContain("No skill is imported.");
  });

  it("reports a removal whose stored copy was not fully deleted and shows the list without it", async () => {
    let removed = false;
    await start(() => overview(removed ? [] : [REVIEW], { revision: removed ? 6 : 5 }), {
      settings_remove_skill: () => {
        removed = true;
        return rejection(
          "package_orphaned",
          "The skill was removed, but its stored copy was not fully deleted. The copy is inert, and nothing can load it.",
        );
      },
    });
    buttonByText(skillRow("review"), "Remove").click();
    await settle();

    expect(root.querySelector(".change-error")?.textContent).toContain("(package_orphaned)");
    expect(root.textContent).toContain("No skill is imported.");
  });

  it("renders a hostile skill name as text", async () => {
    await start(() =>
      overview([{ name: HOSTILE.img, application: null, workspace_overrides: [] }]),
    );
    expect(root.querySelector(".settings-body img")).toBeNull();
    expect(root.textContent).toContain(HOSTILE.img);
  });

  it("disables every skill control while a change waits for the operator", async () => {
    let finish: (value: SettingsOverview) => void = () => undefined;
    await start(() => overview([REVIEW]), {
      settings_import_skill: () =>
        new Promise<SettingsOverview>((resolve) => {
          finish = resolve;
        }),
    });
    buttonByText(root, IMPORT_SKILL_LABEL).click();
    await settle();
    expect(root.querySelector<HTMLFieldSetElement>("fieldset.settings-body")?.disabled).toBe(true);
    finish(overview([REVIEW], { revision: 6 }));
    await settle();
    expect(root.querySelector<HTMLFieldSetElement>("fieldset.settings-body")?.disabled).toBe(false);
  });
});

describe("mock skill commands", () => {
  async function revision(invoke: (c: string, a: Record<string, unknown>) => Promise<unknown>) {
    return ((await invoke("settings_overview", {})) as SettingsOverview).revision;
  }

  it("imports the next sample folder and refuses a name that is taken", async () => {
    const { invoke } = createMockBackend();
    let next = (await invoke("settings_import_skill", {
      revision: await revision(invoke),
    })) as SettingsOverview;
    expect(next.skills.map((skill) => skill.name)).toEqual(["review-checklist", "release-notes"]);
    expect(next.skills[1]).toEqual({
      name: "release-notes",
      application: null,
      workspace_overrides: [],
    });
    next = (await invoke("settings_import_skill", { revision: next.revision })) as SettingsOverview;
    await expect(
      invoke("settings_import_skill", { revision: next.revision }),
    ).rejects.toMatchObject({ code: "skill_name_taken" });
  });

  it("refuses a stale revision for every skill command", async () => {
    const { invoke } = createMockBackend();
    for (const [command, args] of [
      ["settings_import_skill", {}],
      [
        "settings_set_skill_state",
        { name: "review-checklist", workspace_id: null, skill_state: "enabled" },
      ],
      ["settings_remove_skill", { name: "review-checklist" }],
    ] as const) {
      await expect(invoke(command, { ...args, revision: 1 })).rejects.toMatchObject({
        code: "stale_revision",
      });
    }
  });

  it("sets, overrides, and inherits like the core", async () => {
    const { invoke } = createMockBackend();
    const base = { name: "review-checklist" };
    let current = (await invoke("settings_set_skill_state", {
      ...base,
      revision: await revision(invoke),
      workspace_id: null,
      skill_state: "disabled",
    })) as SettingsOverview;
    expect(current.skills[0]?.application).toBe(false);

    const workspaceId = current.workspaces[0]?.workspace_id as string;
    current = (await invoke("settings_set_skill_state", {
      ...base,
      revision: current.revision,
      workspace_id: workspaceId,
      skill_state: "enabled",
    })) as SettingsOverview;
    expect(current.skills[0]?.workspace_overrides).toEqual([
      { workspace_id: workspaceId, enabled: true },
    ]);

    current = (await invoke("settings_set_skill_state", {
      ...base,
      revision: current.revision,
      workspace_id: workspaceId,
      skill_state: "inherit",
    })) as SettingsOverview;
    expect(current.skills[0]?.workspace_overrides).toEqual([]);

    await expect(
      invoke("settings_set_skill_state", {
        ...base,
        revision: current.revision,
        workspace_id: null,
        skill_state: "inherit",
      }),
    ).rejects.toMatchObject({ code: "skill_inherit_needs_workspace" });
    await expect(
      invoke("settings_set_skill_state", {
        name: "missing",
        revision: current.revision,
        workspace_id: null,
        skill_state: "enabled",
      }),
    ).rejects.toMatchObject({ code: "skill_not_found" });
    await expect(
      invoke("settings_set_skill_state", {
        ...base,
        revision: current.revision,
        workspace_id: "99999999-9999-4999-8999-999999999999",
        skill_state: "enabled",
      }),
    ).rejects.toMatchObject({ code: "unknown_workspace" });
  });

  it("removes a skill and refuses an unknown one", async () => {
    const { invoke } = createMockBackend();
    const removed = (await invoke("settings_remove_skill", {
      revision: await revision(invoke),
      name: "review-checklist",
    })) as SettingsOverview;
    expect(removed.skills).toEqual([]);
    await expect(
      invoke("settings_remove_skill", { revision: removed.revision, name: "review-checklist" }),
    ).rejects.toMatchObject({ code: "skill_not_found" });
  });
});
