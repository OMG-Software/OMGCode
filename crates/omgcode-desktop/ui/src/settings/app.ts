import { button, type Child, el, replaceChildren } from "../dom";
import { CANCELLED_CODE, toIpcError } from "../ipc/errors";
import { settingsApi } from "../ipc/settings_client";
import type {
  ConnectionView,
  PermissionFlags,
  SettingsOverview,
  SkillSettingView,
  WorkspaceSettingsView,
} from "../ipc/types";

export interface SettingsApp {
  /** Reads the configuration again. */
  refresh(): Promise<void>;
  dispose(): void;
}

/**
 * What this window changes, and what still needs the trusted CLI (ADR-0059 phase S2). Folders
 * are chosen in a system dialog, and every change that allows more access is confirmed there.
 */
export const INTRO =
  "Choose folders in a system dialog. Changes that allow more access ask you to confirm in a " +
  "system dialog first. Language-model connections, toolchain profiles, container grants, and " +
  "skills are still changed with the trusted CLI: omgcode workspace, omgcode skills, and " +
  "--provider-profile at launch.";

/** Shown when the operator closed a dialog or declined. */
export const NOTHING_CHANGED = "No change was made.";

/** The preferences in the order they are shown, with their labels. */
export const PERMISSION_LABELS: ReadonlyArray<[keyof PermissionFlags, string]> = [
  ["model_tools", "model tools"],
  ["read", "read"],
  ["write", "write"],
  ["execute", "execute"],
  ["task_network", "task network"],
  ["host_services", "host services"],
];

/** What one control asks the core to do, given the revision the window last read. */
type Change = (revision: number) => Promise<SettingsOverview>;

function facts(pairs: Array<[string, string]>): HTMLElement {
  const list = el("dl", { class: "facts" });
  for (const [term, value] of pairs) {
    list.append(el("dt", { text: term }), el("dd", { text: value }));
  }
  return list;
}

function cards(items: HTMLElement[], emptyText: string): HTMLElement {
  if (items.length === 0) {
    return el("p", { class: "empty", text: emptyText });
  }
  return el("ul", { class: "card-list" }, ...items);
}

function card(title: string, ...children: Child[]): HTMLElement {
  return el("li", { class: "card" }, el("div", { class: "card-title", text: title }), ...children);
}

function connectionSource(connection: ConnectionView): string {
  switch (connection.source) {
    case "profile":
      return `Provider profile ${connection.profile ?? "(unnamed)"}, set at launch`;
    case "environment":
      return "Launch endpoint, token from OMGCODE_LM_TOKEN";
    case "injected":
      return "Supplied by the embedding application";
  }
}

function flagsOf(workspace: WorkspaceSettingsView): PermissionFlags {
  return {
    model_tools: workspace.model_tools,
    read: workspace.read,
    write: workspace.write,
    execute: workspace.execute,
    task_network: workspace.task_network,
    host_services: workspace.host_services,
  };
}

/** One preference as a checkbox. Changing it asks the core for the whole set with it flipped. */
function permissionToggle(
  workspace: WorkspaceSettingsView,
  key: keyof PermissionFlags,
  label: string,
  submit: (change: Change) => void,
): HTMLElement {
  const box = el("input", { attrs: { type: "checkbox", "aria-label": label } });
  box.checked = workspace[key];
  box.addEventListener("change", () => {
    const wanted = { ...flagsOf(workspace), [key]: box.checked };
    submit((revision) => settingsApi.setPermissions(workspace.workspace_id, revision, wanted));
  });
  return el("label", { class: box.checked ? "flag on" : "flag" }, box, ` ${label}`);
}

function workspaceCard(
  workspace: WorkspaceSettingsView,
  submit: (change: Change) => void,
): HTMLElement {
  const id = workspace.workspace_id;
  const cache = [
    button("Choose…", () => submit((revision) => settingsApi.choosePackageCache(id, revision))),
  ];
  if (workspace.package_cache_root !== null) {
    cache.push(
      button("Clear", () => submit((revision) => settingsApi.clearPackageCache(id, revision))),
    );
  }
  return card(
    workspace.folder_display,
    el(
      "div",
      { class: "flags" },
      ...PERMISSION_LABELS.map(([key, label]) => permissionToggle(workspace, key, label, submit)),
    ),
    facts([["Identifier", workspace.workspace_id]]),
    el(
      "div",
      { class: "row-actions" },
      el("span", { class: "muted", text: "Package cache" }),
      el("span", { class: "root", text: workspace.package_cache_root ?? "not set" }),
      ...cache,
    ),
  );
}

function skillSetting(skill: SkillSettingView, folders: Map<string, string>): HTMLElement {
  const application =
    skill.application === null ? "not set (disabled)" : skill.application ? "enabled" : "disabled";
  const pairs: Array<[string, string]> = [["Application", application]];
  for (const entry of skill.workspace_overrides) {
    const where = folders.get(entry.workspace_id) ?? entry.workspace_id;
    pairs.push([where, entry.enabled ? "enabled" : "disabled"]);
  }
  return card(skill.name, facts(pairs));
}

/**
 * Renders the whole overview with its controls. Every value is written as a text node. A control
 * only names what to change; folders come from a system dialog that the core opens.
 */
export function renderOverview(
  overview: SettingsOverview,
  submit: (change: Change) => void,
): HTMLElement[] {
  const folders = new Map(overview.workspaces.map((w) => [w.workspace_id, w.folder_display]));
  const rootSet = overview.development_root !== null;
  const register = button("Register workspace…", () =>
    submit((revision) => settingsApi.registerWorkspace(revision)),
  );
  register.disabled = !rootSet;
  return [
    el("h2", { text: "Model connection" }),
    facts([
      ["Endpoint", overview.connection.endpoint],
      ["Source", connectionSource(overview.connection)],
    ]),
    el("h2", { text: "Development root" }),
    el(
      "div",
      { class: "row-actions" },
      el("span", { class: "root", text: overview.development_root ?? "not set" }),
      button(rootSet ? "Change…" : "Choose folder…", () =>
        submit((revision) => settingsApi.chooseRoot(revision)),
      ),
    ),
    el("p", {
      class: "empty",
      text: "Workspaces must be folders inside the development root. Tools reach only registered workspaces.",
    }),
    el("h2", { text: "Workspaces" }),
    el(
      "div",
      { class: "row-actions" },
      register,
      rootSet ? null : el("span", { class: "muted", text: "Choose a development root first." }),
    ),
    cards(
      overview.workspaces.map((workspace) => workspaceCard(workspace, submit)),
      "No workspace is registered.",
    ),
    el("h2", { text: "Language-model connections" }),
    cards(
      overview.provider_profiles.map((profile) =>
        card(
          profile.name,
          facts([
            ["Provider", profile.kind],
            ["Authentication", profile.auth],
            ["Endpoint", profile.endpoint ?? "none recorded"],
            ["Credential", profile.credential_bound ? "stored" : "none"],
          ]),
        ),
      ),
      "No provider profile is recorded.",
    ),
    el("h2", { text: "Toolchain profiles" }),
    cards(
      overview.toolchain_profiles.map((profile) =>
        card(
          profile.name,
          facts([
            ["Platform", profile.platform],
            ["Read-only roots", profile.read_only_roots.join("\n") || "none"],
            ["PATH entries", profile.path_entries.join("\n") || "none"],
            ["Variables", profile.environment_names.join(", ") || "none"],
          ]),
        ),
      ),
      "No toolchain profile is recorded.",
    ),
    el("h2", { text: "Package-network profiles" }),
    cards(
      overview.package_profiles.map((profile) =>
        card(
          profile.name,
          facts([
            ["Origins", profile.origins.join("\n")],
            ["Credentials", String(profile.credentials_bound)],
          ]),
        ),
      ),
      "No package-network profile is recorded.",
    ),
    el("h2", { text: "Container grants" }),
    cards(
      overview.container_grants.map((grant) =>
        card(
          grant.directory,
          facts([
            ["Workspace", folders.get(grant.workspace_id) ?? grant.workspace_id],
            ["Access", grant.access === "read_write" ? "read and write" : "read and execute"],
            ["State", grant.state],
            ["Grant", grant.grant_id],
          ]),
        ),
      ),
      "No container grant is recorded.",
    ),
    el("h2", { text: "Skills" }),
    cards(
      overview.skills.map((skill) => skillSetting(skill, folders)),
      "No skill is imported.",
    ),
  ];
}

/**
 * Builds the settings window. It renders only operator-recorded configuration that the core
 * has made display-safe; never transcript, tool, or skill text. One change runs at a time; while
 * it waits for the operator's answer in a system dialog, every control is disabled.
 */
export function startSettings(root: HTMLElement): SettingsApp {
  const revisionText = el("span", { class: "revision" });
  const problem = el("p", { class: "error", attrs: { role: "alert" } });
  problem.hidden = true;
  const notice = el("p", { class: "notice", attrs: { role: "status" } });
  notice.hidden = true;
  // A disabled fieldset disables every control inside it without touching their own state.
  const body = el("fieldset", { class: "settings-body" });
  const refreshButton = button("Refresh", () => void refresh());
  let current: SettingsOverview | null = null;
  let working = false;

  function setWorking(value: boolean): void {
    working = value;
    body.disabled = value;
    refreshButton.disabled = value;
  }

  function show(overview: SettingsOverview): void {
    current = overview;
    replaceChildren(body, ...renderOverview(overview, submit));
    replaceChildren(revisionText, `Settings revision ${overview.revision}`);
  }

  function report(reason: unknown): void {
    const error = toIpcError(reason);
    if (error.code === CANCELLED_CODE) {
      replaceChildren(notice, NOTHING_CHANGED);
      notice.hidden = false;
      return;
    }
    replaceChildren(problem, `${error.message} (${error.code})`);
    problem.hidden = false;
  }

  function clearMessages(): void {
    problem.hidden = true;
    notice.hidden = true;
  }

  async function refresh(): Promise<void> {
    if (working) {
      return;
    }
    setWorking(true);
    try {
      show(await settingsApi.overview());
      clearMessages();
    } catch (reason) {
      report(reason);
    } finally {
      setWorking(false);
    }
  }

  /** Runs one change. After any failure the window reads the configuration again, so every
   *  control shows what is recorded rather than what was asked for. If that read fails too, it
   *  shows the last overview it read, which undoes a checkbox the operator just changed. */
  function submit(change: Change): void {
    if (working || current === null) {
      return;
    }
    const recorded = current;
    const revision = recorded.revision;
    setWorking(true);
    clearMessages();
    void (async () => {
      try {
        show(await change(revision));
      } catch (reason) {
        report(reason);
        try {
          show(await settingsApi.overview());
        } catch (again) {
          show(recorded);
          report(again);
        }
      } finally {
        setWorking(false);
      }
    })();
  }

  const onVisible = (): void => {
    if (document.visibilityState === "visible") {
      void refresh();
    }
  };
  document.addEventListener("visibilitychange", onVisible);

  replaceChildren(
    root,
    el(
      "main",
      { class: "settings-window" },
      el("h1", { text: "Settings" }),
      el("p", { class: "intro", text: INTRO }),
      el("div", { class: "toolbar" }, refreshButton, revisionText),
      problem,
      notice,
      body,
    ),
  );
  void refresh();

  return {
    refresh,
    dispose() {
      document.removeEventListener("visibilitychange", onVisible);
    },
  };
}
