import { button, el } from "../dom";
import type {
  Access,
  PermissionFlags,
  SettingsOverview,
  WorkspaceSettingsView,
  WriteToolAccess,
} from "../ipc/types";
import { segmented } from "../ui/segmented";
import { workspaceInstructionsBlock } from "./chat_section";
import { type SectionContext, scopedWorkspace } from "./context";
import { emptyLine, facts, sectionHead } from "./parts";
import { webHostsBlock } from "./web_hosts";

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
  ["web_fetch", "Web fetch"],
];

const PERMISSION_HELP: Readonly<Record<AccessKey, string>> = {
  read: "Read and list files in the workspace.",
  write: "Change files in the workspace.",
  execute: "Run commands through the trusted runner.",
  task_network:
    "Network access for a trusted run started with --network or a trusted package fetch. Web fetch is set separately.",
  host_services: "Host services a trusted run selects. Web fetch is set separately.",
  web_fetch:
    "Fetch public HTTPS pages for the model. Under Allow, hosts on the list below skip approval; other hosts still ask.",
};

/** The per-tool write limits in the order they are shown, with their labels. */
export const WRITE_TOOL_LABELS: ReadonlyArray<[keyof WriteToolAccess, string]> = [
  ["edit", "Edit tool"],
  ["multi_edit", "Multi-edit tool"],
  ["create", "Create tool"],
  ["rename", "Rename tool"],
  ["delete", "Delete tool"],
  ["create_directory", "Create directory tool"],
];

const TOOL_HELP: Readonly<Record<keyof WriteToolAccess, string>> = {
  edit: "Replace one exact text in a file.",
  multi_edit: "Replace several exact texts in one file.",
  create: "Make one new file.",
  rename: "Move a file to a name that does not exist.",
  delete: "Remove a file after saving a checkpoint.",
  create_directory: "Make one new directory in a folder that exists.",
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

/** The accessible name of the control that sets every file-write tool limit at once. */
export const ALL_TOOLS_LABEL = "All file-write tools";

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
    web_fetch: workspace.web_fetch,
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

/** The state every file-write tool limit shares, or `null` when they differ. */
function sharedToolState(tools: WriteToolAccess): Access | null {
  const [first, ...rest] = Object.values(tools);
  return first !== undefined && rest.every((state) => state === first) ? first : null;
}

/**
 * One control that sets every file-write tool limit to the same state. It sends the whole
 * preference set once, so Rust shows one confirmation naming each limit the change widens. It
 * shows no state as chosen while the limits differ. Like each tool's own limit, it only narrows
 * Write and never changes Write itself.
 */
function allToolLimits(workspace: WorkspaceSettingsView, ctx: SectionContext): HTMLElement {
  return el(
    "div",
    { class: "tool-limits-all" },
    el("span", { class: "perm-help", text: "Set all" }),
    segmented<Access>({
      label: ALL_TOOLS_LABEL,
      choices: ACCESS_LABELS,
      value: sharedToolState(workspace.write_tools),
      class: "access",
      onChoose: (state) =>
        submitWith(workspace, ctx, (flags) => {
          // Every key is named, so a new tool cannot be left out without a type error.
          flags.write_tools = {
            edit: state,
            multi_edit: state,
            create: state,
            rename: state,
            delete: state,
            create_directory: state,
          };
        }),
    }),
  );
}

/**
 * The disclosure of per-tool limits, with the Set all control on its summary line. The control
 * sits outside the summary so it works while the disclosure is closed and is not nested inside
 * the summary's own button role.
 */
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
  return el("div", { class: "tool-limits-group" }, allToolLimits(workspace, ctx), details);
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

/** What the package cache root is for; the desktop itself never fetches a package. */
export const PACKAGE_CACHE_HELP =
  "Where the trusted CLI command omgcode packages fetch stores downloads for this workspace. " +
  "A trusted run exposes it read-only. This app never fetches packages.";

function packageCache(workspace: WorkspaceSettingsView, { submit, actions }: SectionContext) {
  const id = workspace.workspace_id;
  const buttons = [button("Choose…", () => submit(actions.choosePackageCache(id)))];
  if (workspace.package_cache_root !== null) {
    buttons.push(button("Clear", () => submit(actions.clearPackageCache(id))));
  }
  return el(
    "div",
    { class: "package-cache" },
    el(
      "div",
      { class: "row-actions" },
      el("span", { class: "muted", text: "Package cache" }),
      el("span", { class: "root", text: workspace.package_cache_root ?? "not set" }),
      ...buttons,
    ),
    el("p", { class: "perm-help", text: PACKAGE_CACHE_HELP }),
  );
}

/**
 * The button that unregisters the workspace (ADR-0081). Rust names the folder and what is
 * removed in a native confirmation; the window sends the identifier and revision only.
 */
function removeWorkspace(workspace: WorkspaceSettingsView, { submit, actions }: SectionContext) {
  return el(
    "div",
    { class: "row-actions" },
    button("Remove workspace…", () => submit(actions.unregisterWorkspace(workspace.workspace_id))),
    el("span", {
      class: "muted",
      text: "Removes this registration and its settings. The folder and its files are not touched.",
    }),
  );
}

/** `removable` adds the removal control, which only the full Workspaces view offers. */
function workspaceBlock(
  workspace: WorkspaceSettingsView,
  ctx: SectionContext,
  removable: boolean,
): HTMLElement {
  return el(
    "section",
    { class: "block workspace-block" },
    el("h3", { class: "workspace-title", text: workspace.folder_display }),
    modelToolsToggle(workspace, ctx),
    ...PERMISSION_LABELS.flatMap(([key, label]) => {
      const row = permissionRow(workspace, key, label, ctx);
      if (key === "write") {
        return [row, toolLimits(workspace, ctx)];
      }
      return key === "web_fetch" ? [row, webHostsBlock(workspace, ctx)] : [row];
    }),
    el(
      "div",
      { class: "workspace-details" },
      facts([["Identifier", workspace.workspace_id]]),
      packageCache(workspace, ctx),
    ),
    workspaceInstructionsBlock(workspace, ctx),
    removable ? removeWorkspace(workspace, ctx) : null,
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

export const SCOPED_HELP = "Settings for the open session's workspace only.";

/** Shown with the full view when the scoped workspace is no longer registered. */
export const SCOPE_GONE =
  "The workspace you opened is no longer registered. All workspaces are shown.";

export function renderWorkspaces(ctx: SectionContext): HTMLElement[] {
  const scoped = scopedWorkspace(ctx.overview);
  if (scoped !== null) {
    // One workspace: no root, no registration, and no navigation group (ADR-0071).
    ctx.view.workspaceId = scoped.workspace_id;
    return [
      ...sectionHead("Workspace settings", SCOPED_HELP),
      el("p", { class: "access-help", text: ACCESS_HELP }),
      workspaceBlock(scoped, ctx, false),
    ];
  }
  const gone = ctx.overview.scope === null ? null : el("p", { class: "notice", text: SCOPE_GONE });
  const shown = ctx.view.workspacePage ? shownWorkspace(ctx) : null;
  ctx.view.workspacePage = shown !== null;
  if (shown !== null) {
    return [
      ...sectionHead("Workspaces", WORKSPACES_HELP),
      gone,
      el("p", { class: "access-help", text: ACCESS_HELP }),
      workspaceBlock(shown, ctx, true),
    ].filter((node): node is HTMLElement => node !== null);
  }
  const empty =
    ctx.overview.workspaces.length === 0 ? emptyLine("No workspace is registered.") : null;
  return [
    ...sectionHead("Workspaces", WORKSPACES_HELP),
    gone,
    developmentRoot(ctx.overview, ctx),
    empty,
  ].filter((node): node is HTMLElement => node !== null);
}
