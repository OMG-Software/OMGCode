import { button, el, neutralise } from "../dom";
import type { SkillSettingView } from "../ipc/types";
import type { SectionContext, SkillState } from "./context";
import { grantsGroup, packageGroup, toolchainGroup } from "./execution_forms";
import { emptyLine, row, sectionHead } from "./parts";

export const ADVANCED_HELP =
  "Toolchain profiles, package-network profiles, container grants (Windows only), and skills are changed here. Rust opens every folder dialog and asks you to confirm in a system dialog before a change that widens access, adds a record, or removes one. Credentials, package fetches, and runs stay with the trusted CLI.";

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
  remove.setAttribute("aria-label", neutralise(`Remove ${skill.name}`));
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
 * The Advanced section: toolchain profiles, package-network profiles, container grants, and
 * skills, each with its own controls (ADR-0082, ADR-0088).
 */
export function renderAdvanced(ctx: SectionContext): HTMLElement[] {
  return [
    ...sectionHead("Advanced", ADVANCED_HELP),
    toolchainGroup(ctx),
    packageGroup(ctx),
    grantsGroup(ctx),
    skillsGroup(ctx),
  ];
}
