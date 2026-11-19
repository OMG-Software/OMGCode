import { el } from "../dom";
import type { SettingsOverview, SkillSettingView } from "../ipc/types";
import { emptyLine, facts, sectionHead } from "./parts";

export const ADVANCED_HELP =
  "These change only through the trusted CLI (omgcode workspace, omgcode skills). This window shows them and changes none of them.";

function row(title: string, ...children: HTMLElement[]): HTMLElement {
  return el(
    "li",
    { class: "row" },
    el(
      "div",
      { class: "row-body" },
      el("div", { class: "row-head" }, el("span", { class: "row-title", text: title })),
      ...children,
    ),
  );
}

function group(title: string, rows: HTMLElement[], emptyText: string): HTMLElement {
  return el(
    "section",
    { class: "block" },
    el("h3", { text: title }),
    rows.length === 0 ? emptyLine(emptyText) : el("ul", { class: "row-list" }, ...rows),
  );
}

function skillRow(skill: SkillSettingView, folders: Map<string, string>): HTMLElement {
  const application =
    skill.application === null ? "not set (disabled)" : skill.application ? "enabled" : "disabled";
  const pairs: Array<[string, string]> = [["Application", application]];
  for (const entry of skill.workspace_overrides) {
    const where = folders.get(entry.workspace_id) ?? entry.workspace_id;
    pairs.push([where, entry.enabled ? "enabled" : "disabled"]);
  }
  return row(skill.name, facts(pairs));
}

/** Read-only configuration. Nothing here is a control. */
export function renderAdvanced(overview: SettingsOverview): HTMLElement[] {
  const folders = new Map(overview.workspaces.map((w) => [w.workspace_id, w.folder_display]));
  return [
    ...sectionHead("Advanced", ADVANCED_HELP),
    group(
      "Toolchain profiles",
      overview.toolchain_profiles.map((profile) =>
        row(
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
    group(
      "Package-network profiles",
      overview.package_profiles.map((profile) =>
        row(
          profile.name,
          facts([
            ["Origins", profile.origins.join("\n")],
            ["Credentials", String(profile.credentials_bound)],
          ]),
        ),
      ),
      "No package-network profile is recorded.",
    ),
    group(
      "Container grants",
      overview.container_grants.map((grant) =>
        row(
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
    group(
      "Skills",
      overview.skills.map((skill) => skillRow(skill, folders)),
      "No skill is imported.",
    ),
  ];
}
