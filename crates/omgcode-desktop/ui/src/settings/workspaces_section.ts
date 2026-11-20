import { button, el } from "../dom";
import type {
  Access,
  PermissionFlags,
  SettingsOverview,
  WorkspaceSettingsView,
  WriteToolAccess,
} from "../ipc/types";
import { segmented } from "../ui/segmented";
import { type SectionContext, scopedWorkspace } from "./context";
import { emptyLine, facts, sectionHead } from "./parts";

export const WORKSPACES_HELP =
  "Workspaces are folders inside the development root. Tools reach only registered workspaces.";

/** The three-state preferences, keyed by their wire names. */
type AccessKey = Exclude<keyof PermissionFlags, "model_tools" | "write_tools">;

/** The three-state preferences in the order they are shown, with their labels. */
export const PERMISSION_LABELS: ReadonlyArray<[AccessKey, string]> = [
  ["read", "Read"],
  ["write", "Write"],
  ["execute", "Execute"],
  ["task_network", "Task network"],
  ["host_services", "Host services"],
];

const PERMISSION_HELP: Readonly<Record<AccessKey, string>> = {
  read: "Read and list files in the workspace.",
  write: "Change files in the workspace.",
  execute: "Run commands through the trusted runner.",
  task_network: "Network access for a trusted run started with --network. No model tool uses it.",
  host_services: "Host services a trusted run selects. No model tool uses it.",
};

/** The per-tool write limits in the order they are shown, with their labels. */
export const WRITE_TOOL_LABELS: ReadonlyArray<[keyof WriteToolAccess, string]> = [
  ["edit", "Edit tool"],
  ["multi_edit", "Multi-edit tool"],
  ["create", "Create tool"],
  ["rename", "Rename tool"],
  ["delete", "Delete tool"],
];

const TOOL_HELP: Readonly<Record<keyof WriteToolAccess, string>> = {
  edit: "Replace one exact text in a file.",
  multi_edit: "Replace several exact texts in one file.",
  create: "Make one new file.",
  rename: "Move a file to a name that does not exist.",
  delete: "Remove a file after saving a checkpoint.",
};

/** The states in the order a control offers them, with their labels. */
export const ACCESS_LABELS: ReadonlyArray<[Access, string]> = [
  ["deny", "Deny"],
  ["prompt", "Prompt"],
  ["allow", "Allow"],
];

/** What the three states mean, shown once at the top of the section. */
export const ACCESS_HELP =
  "Deny stops the action. Prompt asks you to approve each model call, and the turn waits for " +
  "your decision. Allow lets model calls run without asking. A tool limit can only narrow " +
  "write. Every model action is recorded in the action journal.";

const WIDTH: Readonly<Record<Access, number>> = { deny: 0, prompt: 1, allow: 2 };

function accessLabel(state: Access): string {
  return ACCESS_LABELS.find(([value]) => value === state)?.[1] ?? state;
}

function flagsOf(workspace: WorkspaceSettingsView): PermissionFlags {
  return {
    model_tools: workspace.model_tools,
    read: workspace.read,
    write: workspace.write,
    execute: workspace.execute,
    task_network: workspace.task_network,
    host_services: workspace.host_services,
    write_tools: { ...workspace.write_tools },
  };
}

/** Ask the core for the workspace's whole preference set with one change applied. */
function submitWith(
  workspace: WorkspaceSettingsView,
  { submit, actions }: SectionContext,
  change: (flags: PermissionFlags) => void,
): void {
  const wanted = flagsOf(workspace);
  change(wanted);
  submit(actions.setPermissions(workspace.workspace_id, wanted));
}

/** One labelled row: its name and description, then a Deny, Prompt, or Allow control. */
function accessRow(
  label: string,
  help: string,
  current: Access,
  choose: (state: Access) => void,
  note: string | null,
): HTMLElement {
  return el(
    "div",
    { class: "perm-row" },
    el(
      "div",
      { class: "perm-text" },
      el("span", { class: "perm-label", text: label }),
      el("span", { class: "perm-help", text: help }),
      note === null ? null : el("span", { class: "perm-note", text: note }),
    ),
    segmented<Access>({
      label,
      choices: ACCESS_LABELS,
      value: current,
      class: "access",
      onChoose: choose,
    }),
  );
}

function permissionRow(
  workspace: WorkspaceSettingsView,
  key: AccessKey,
  label: string,
  ctx: SectionContext,
): HTMLElement {
  return accessRow(
    label,
    PERMISSION_HELP[key],
    workspace[key],
    (state) =>
      submitWith(workspace, ctx, (flags) => {
        flags[key] = state;
      }),
    null,
  );
}

function toolRow(
  workspace: WorkspaceSettingsView,
  key: keyof WriteToolAccess,
  label: string,
  ctx: SectionContext,
): HTMLElement {
  const own = workspace.write_tools[key];
  // A tool limit can only narrow Write, so a wider one is not what the tool gets.
  const note =
    WIDTH[own] > WIDTH[workspace.write]
      ? `Limited by Write: ${accessLabel(workspace.write)}`
      : null;
  return accessRow(
    label,
    TOOL_HELP[key],
    own,
    (state) =>
      submitWith(workspace, ctx, (flags) => {
        flags.write_tools[key] = state;
      }),
    note,
  );
}

function toolLimits(workspace: WorkspaceSettingsView, ctx: SectionContext): HTMLElement {
  const details = el(
    "details",
    { class: "disclosure tool-limits" },
    el("summary", { text: "File-write tool limits" }),
    ...WRITE_TOOL_LABELS.map(([key, label]) => toolRow(workspace, key, label, ctx)),
  );
  details.open = ctx.view.toolLimitsOpen;
  details.addEventListener("toggle", () => {
    ctx.view.toolLimitsOpen = details.open;
  });
  return details;
}

/** The model-tools preference as a checkbox. */
function modelToolsToggle(workspace: WorkspaceSettingsView, ctx: SectionContext): HTMLElement {
  const box = el("input", { attrs: { type: "checkbox", "aria-label": "model tools" } });
  box.checked = workspace.model_tools;
  box.addEventListener("change", () => {
    submitWith(workspace, ctx, (flags) => {
      flags.model_tools = box.checked;
    });
  });
  return el(
    "label",
    { class: "toggle" },
    box,
    el(
      "span",
      { class: "perm-text" },
      el("span", { class: "perm-label", text: "Model tools" }),
      el("span", {
        class: "perm-help",
        text: "Offer tools to the model in chat. Each tool still follows the permissions below.",
      }),
    ),
  );
}

function packageCache(workspace: WorkspaceSettingsView, { submit, actions }: SectionContext) {
  const id = workspace.workspace_id;
  const buttons = [button("Choose…", () => submit(actions.choosePackageCache(id)))];
  if (workspace.package_cache_root !== null) {
    buttons.push(button("Clear", () => submit(actions.clearPackageCache(id))));
  }
  return el(
    "div",
    { class: "row-actions" },
    el("span", { class: "muted", text: "Package cache" }),
    el("span", { class: "root", text: workspace.package_cache_root ?? "not set" }),
    ...buttons,
  );
}

function workspaceBlock(workspace: WorkspaceSettingsView, ctx: SectionContext): HTMLElement {
  return el(
    "section",
    { class: "block workspace-block" },
    el("h3", { class: "workspace-title", text: workspace.folder_display }),
    modelToolsToggle(workspace, ctx),
    ...PERMISSION_LABELS.flatMap(([key, label]) => {
      const row = permissionRow(workspace, key, label, ctx);
      return key === "write" ? [row, toolLimits(workspace, ctx)] : [row];
    }),
    el(
      "div",
      { class: "workspace-details" },
      facts([["Identifier", workspace.workspace_id]]),
      packageCache(workspace, ctx),
    ),
  );
}

function developmentRoot(overview: SettingsOverview, { submit, actions }: SectionContext) {
  const rootSet = overview.development_root !== null;
  const register = button("Register workspace…", () => submit(actions.registerWorkspace()));
  register.disabled = !rootSet;
  return el(
    "section",
    { class: "block" },
    el("h3", { text: "Development root" }),
    el(
      "div",
      { class: "row-actions" },
      el("span", { class: "root", text: overview.development_root ?? "not set" }),
      button(rootSet ? "Change…" : "Choose folder…", () => submit(actions.chooseRoot())),
    ),
    el("p", {
      class: "muted",
      text: "Workspaces must be folders inside the development root.",
    }),
    el(
      "div",
      { class: "row-actions" },
      register,
      rootSet ? null : el("span", { class: "muted", text: "Choose a development root first." }),
    ),
  );
}

/** The workspace to show: the remembered one, or the first when it is gone. */
function shownWorkspace(ctx: SectionContext): WorkspaceSettingsView | null {
  const { workspaces } = ctx.overview;
  const shown = workspaces.find((w) => w.workspace_id === ctx.view.workspaceId) ?? workspaces[0];
  ctx.view.workspaceId = shown?.workspace_id ?? null;
  return shown ?? null;
}

function chooser(shown: WorkspaceSettingsView, ctx: SectionContext): HTMLElement | null {
  const { workspaces } = ctx.overview;
  if (workspaces.length < 2) {
    return null;
  }
  const select = el("select", { attrs: { id: "workspace-chooser", "aria-label": "Workspace" } });
  for (const workspace of workspaces) {
    select.append(
      el("option", { text: workspace.folder_display, attrs: { value: workspace.workspace_id } }),
    );
  }
  select.value = shown.workspace_id;
  select.addEventListener("change", () => {
    ctx.view.workspaceId = select.value;
    ctx.rerender();
  });
  return el(
    "div",
    { class: "row-actions" },
    el("label", { class: "muted", text: "Workspace", attrs: { for: "workspace-chooser" } }),
    select,
  );
}

export const SCOPED_HELP = "Settings for the open session's workspace only.";

/** Shown with the full view when the scoped workspace is no longer registered. */
export const SCOPE_GONE =
  "The workspace you opened is no longer registered. All workspaces are shown.";

export function renderWorkspaces(ctx: SectionContext): HTMLElement[] {
  const scoped = scopedWorkspace(ctx.overview);
  if (scoped !== null) {
    // One workspace: no root, no registration, and no chooser (ADR-0070).
    ctx.view.workspaceId = scoped.workspace_id;
    return [
      ...sectionHead("Workspace settings", SCOPED_HELP),
      el("p", { class: "access-help", text: ACCESS_HELP }),
      workspaceBlock(scoped, ctx),
    ];
  }
  const gone = ctx.overview.scope === null ? null : el("p", { class: "notice", text: SCOPE_GONE });
  const shown = shownWorkspace(ctx);
  if (shown === null) {
    return [
      ...sectionHead("Workspaces", WORKSPACES_HELP),
      gone,
      developmentRoot(ctx.overview, ctx),
      emptyLine("No workspace is registered."),
    ].filter((node): node is HTMLElement => node !== null);
  }
  return [
    ...sectionHead("Workspaces", WORKSPACES_HELP),
    gone,
    el("p", { class: "access-help", text: ACCESS_HELP }),
    developmentRoot(ctx.overview, ctx),
    chooser(shown, ctx),
    workspaceBlock(shown, ctx),
  ].filter((node): node is HTMLElement => node !== null);
}
