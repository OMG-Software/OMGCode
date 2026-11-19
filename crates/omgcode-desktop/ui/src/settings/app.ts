import { button, type Child, el, replaceChildren } from "../dom";
import { toIpcError } from "../ipc/errors";
import { settingsApi } from "../ipc/settings_client";
import type {
  ConnectionView,
  SettingsOverview,
  SkillSettingView,
  WorkspaceSettingsView,
} from "../ipc/types";

export interface SettingsApp {
  /** Reads the configuration again. */
  refresh(): Promise<void>;
  dispose(): void;
}

/** The CLI commands that change each group in this version (ADR-0058 phase S1). */
export const CLI_HINT =
  "Read-only in this version. Change settings with the trusted CLI: " +
  "omgcode workspace, omgcode skills, and chat --provider-profile at launch.";

function facts(pairs: Array<[string, string]>): HTMLElement {
  const list = el("dl", { class: "facts" });
  for (const [term, value] of pairs) {
    list.append(el("dt", { text: term }), el("dd", { text: value }));
  }
  return list;
}

function flag(label: string, on: boolean): HTMLElement {
  return el("span", { class: on ? "flag on" : "flag", text: `${label}: ${on ? "on" : "off"}` });
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

function workspaceCard(workspace: WorkspaceSettingsView): HTMLElement {
  return card(
    workspace.folder_display,
    el(
      "div",
      { class: "flags" },
      flag("model tools", workspace.model_tools),
      flag("read", workspace.read),
      flag("write", workspace.write),
      flag("execute", workspace.execute),
    ),
    facts([
      ["Identifier", workspace.workspace_id],
      ["Package cache", workspace.package_cache_root ?? "not set"],
    ]),
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

/** Renders the whole overview. Every value is written as a text node. */
export function renderOverview(overview: SettingsOverview): HTMLElement[] {
  const folders = new Map(overview.workspaces.map((w) => [w.workspace_id, w.folder_display]));
  const sections: HTMLElement[] = [
    el("h2", { text: "Model connection" }),
    facts([
      ["Endpoint", overview.connection.endpoint],
      ["Source", connectionSource(overview.connection)],
    ]),
  ];
  if (!overview.store_initialized) {
    sections.push(
      el("h2", { text: "Workspace settings" }),
      el("p", {
        class: "empty",
        text: "No settings store exists yet. Run omgcode workspace status to create it.",
      }),
    );
    return sections;
  }
  sections.push(
    el("h2", { text: "Development root" }),
    el("p", { class: "empty", text: overview.development_root ?? "not set" }),
    el("h2", { text: "Workspaces" }),
    cards(overview.workspaces.map(workspaceCard), "No workspace is registered."),
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
  );
  return sections;
}

/**
 * Builds the settings window. It renders only operator-recorded configuration that the core
 * has made display-safe; never transcript, tool, or skill text.
 */
export function startSettings(root: HTMLElement): SettingsApp {
  const revision = el("span", { class: "revision" });
  const problem = el("p", { class: "error", attrs: { role: "alert" } });
  problem.hidden = true;
  const body = el("div", { class: "settings-body" });
  let loading = false;

  async function refresh(): Promise<void> {
    if (loading) {
      return;
    }
    loading = true;
    try {
      const overview = await settingsApi.overview();
      replaceChildren(body, ...renderOverview(overview));
      replaceChildren(
        revision,
        overview.store_initialized ? `Settings revision ${overview.revision}` : "",
      );
      problem.hidden = true;
    } catch (reason) {
      const error = toIpcError(reason);
      replaceChildren(problem, `${error.message} (${error.code})`);
      problem.hidden = false;
    } finally {
      loading = false;
    }
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
      el("p", { class: "intro", text: CLI_HINT }),
      el(
        "div",
        { class: "toolbar" },
        button("Refresh", () => void refresh()),
        revision,
      ),
      problem,
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
