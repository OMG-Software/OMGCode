import { button, el } from "../dom";
import type { SkillSettingView } from "../ipc/types";
import type { SectionContext, SkillState } from "./context";
import { emptyLine, facts, sectionHead } from "./parts";

export const ADVANCED_HELP =
  "Skills are imported, enabled, and removed here. Toolchain profiles, package-network profiles, and container grants change only through the trusted CLI (omgcode workspace); this window shows them and changes none of them.";

/** The button that asks Rust to open its system folder dialog for a skill (ADR-0082). */
export const IMPORT_SKILL_LABEL = "Import skill…";
export const SKILLS_HELP =
  "A skill is a folder with a SKILL.md file. Importing copies it, disabled. Enabling a skill lets you select it for a chat turn, which sends its text to the model as an untrusted message; it grants no permission and runs nothing. A system dialog asks before a skill is enabled or removed.";

const APPLICATION_OPTIONS: ReadonlyArray<[SkillState, string]> = [
  ["enabled", "Enabled"],
  ["disabled", "Disabled"],
];
const WORKSPACE_OPTIONS: ReadonlyArray<[SkillState, string]> = [
  ["inherit", "Inherit the application setting"],
  ["enabled", "Enabled"],
  ["disabled", "Disabled"],
];

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

/**
 * A choice among states. `value` is the recorded state; choosing another asks Rust to change it,
 * and Rust confirms in a system dialog before any change that enables the skill. The control
 * shows the recorded state again after every change, so a declined dialog leaves it unchanged.
 */
function stateSelect(
  label: string,
  options: ReadonlyArray<[SkillState, string]>,
  value: SkillState | null,
  onChoose: (state: SkillState) => void,
): HTMLSelectElement {
  const select = el("select", { attrs: { "aria-label": label } });
  if (value === null) {
    select.append(el("option", { text: "Not set (disabled)", attrs: { value: "" } }));
  }
  for (const [state, text] of options) {
    select.append(el("option", { text, attrs: { value: state } }));
  }
  select.value = value ?? "";
  select.addEventListener("change", () => {
    const chosen = options.find(([state]) => state === select.value);
    if (chosen !== undefined) {
      onChoose(chosen[0]);
    }
  });
  return select;
}

function skillRow(skill: SkillSettingView, ctx: SectionContext): HTMLElement {
  const { submit, actions, overview } = ctx;
  const application: SkillState | null =
    skill.application === null ? null : skill.application ? "enabled" : "disabled";
  const settings = el("dl", { class: "facts" });
  settings.append(
    el("dt", { text: "Application" }),
    el(
      "dd",
      {},
      stateSelect(
        `Application setting of ${skill.name}`,
        APPLICATION_OPTIONS,
        application,
        (state) => submit(actions.setSkillState(skill.name, null, state)),
      ),
    ),
  );
  for (const workspace of overview.workspaces) {
    const override = skill.workspace_overrides.find(
      (entry) => entry.workspace_id === workspace.workspace_id,
    );
    const value: SkillState =
      override === undefined ? "inherit" : override.enabled ? "enabled" : "disabled";
    settings.append(
      el("dt", { text: workspace.folder_display }),
      el(
        "dd",
        {},
        stateSelect(
          `Setting of ${skill.name} in ${workspace.folder_display}`,
          WORKSPACE_OPTIONS,
          value,
          (state) => submit(actions.setSkillState(skill.name, workspace.workspace_id, state)),
        ),
      ),
    );
  }
  // Rust names the skill and says its stored copy is deleted in a system Yes/No first.
  const remove = button("Remove", () => submit(actions.removeSkill(skill.name)));
  remove.setAttribute("aria-label", `Remove ${skill.name}`);
  const item = row(skill.name, settings);
  item.append(remove);
  return item;
}

/** The Skills group: an import button and, for each skill, its settings and a remove button. */
function skillsGroup(ctx: SectionContext): HTMLElement {
  const { overview, submit, actions } = ctx;
  const importButton = button(IMPORT_SKILL_LABEL, () => submit(actions.importSkill()));
  return el(
    "section",
    { class: "block" },
    el("h3", { text: "Skills" }),
    el("p", { class: "section-help", text: SKILLS_HELP }),
    el("div", { class: "row-actions" }, importButton),
    overview.skills.length === 0
      ? emptyLine("No skill is imported.")
      : el("ul", { class: "row-list" }, ...overview.skills.map((skill) => skillRow(skill, ctx))),
  );
}

/**
 * The Advanced section. Toolchain profiles, package-network profiles, and container grants are
 * read-only here; skills have controls (ADR-0082).
 */
export function renderAdvanced(ctx: SectionContext): HTMLElement[] {
  const { overview } = ctx;
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
    skillsGroup(ctx),
  ];
}
