import { el } from "../dom";
import type { ToolchainProfileView, WorkspaceSettingsView } from "../ipc/types";
import type { SectionContext } from "./context";

/** Shown above the switches: what turning a profile on or off changes (ADR-0107). */
export const PROFILES_HELP =
  "A workspace's commands can use only the toolchain profiles turned on here. Turning one on " +
  "lets them read and run its folders; on Windows a system dialog first names each folder the " +
  "workspace's sandbox is granted. Turning one off stops its runs and revokes the grants no " +
  "other profile here needs.";

export const PROFILES_EMPTY =
  "No toolchain profile is recorded for this computer. Add one under Advanced.";

/** The accessible name of one profile's switch. */
export function useProfileLabel(name: string): string {
  return `Use ${name} in this workspace`;
}

/** Whether `name` is assigned, compared as core compares names: case-insensitively. */
function assigned(workspace: WorkspaceSettingsView, name: string): boolean {
  const wanted = name.toLowerCase();
  return workspace.profiles.some((profile) => profile.toLowerCase() === wanted);
}

function profileSwitch(
  workspace: WorkspaceSettingsView,
  profile: ToolchainProfileView,
  { submit, actions }: SectionContext,
): HTMLElement {
  const box = el("input", {
    attrs: { type: "checkbox", "aria-label": useProfileLabel(profile.name) },
  });
  box.checked = assigned(workspace, profile.name);
  box.addEventListener("change", () => {
    const id = workspace.workspace_id;
    submit(
      box.checked
        ? actions.assignToolchainProfile(id, profile.name)
        : actions.unassignToolchainProfile(id, profile.name),
    );
  });
  return el(
    "label",
    { class: "toggle workspace-profile" },
    box,
    el(
      "span",
      { class: "perm-text" },
      el("span", { class: "perm-label", text: profile.name }),
      el("span", { class: "perm-help", text: profile.read_only_roots.join(", ") }),
    ),
  );
}

/**
 * One switch per toolchain profile recorded for this computer's platform. Turning a switch on
 * asks Rust to assign the profile to the workspace, which shows the folders it grants in a system
 * confirmation; turning it off asks Rust to unassign it. The window sends the workspace and the
 * profile name only, and the switch shows the recorded state again after every change.
 */
export function workspaceProfilesBlock(
  workspace: WorkspaceSettingsView,
  ctx: SectionContext,
): HTMLElement {
  // Only a profile of this computer's platform can run, and so be assigned, here.
  const platform = ctx.overview.profile_platform;
  const profiles = ctx.overview.toolchain_profiles.filter(
    (profile) => profile.platform === platform,
  );
  return el(
    "div",
    { class: "workspace-profiles" },
    el("span", { class: "perm-label", text: "Toolchain profiles" }),
    el("p", { class: "perm-help", text: PROFILES_HELP }),
    profiles.length === 0
      ? el("p", { class: "empty", text: PROFILES_EMPTY })
      : el(
          "div",
          { class: "workspace-profile-list" },
          ...profiles.map((profile) => profileSwitch(workspace, profile, ctx)),
        ),
  );
}
