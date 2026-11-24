import { button, el } from "../dom";
import type { ContainerAccess, SectionContext } from "./context";
import { emptyLine, facts, row } from "./parts";

/** The buttons that ask Rust to open its folder dialogs and confirmations (ADR-0088). */
export const ADD_TOOLCHAIN_LABEL = "Add profile…";
export const ADD_PACKAGE_LABEL = "Add package profile…";
export const GRANT_FOLDER_LABEL = "Grant folder…";

export const TOOLCHAIN_HELP =
  "A toolchain profile lists the directories a run may read and execute, the directories searched for programs, and extra environment variables. Rust opens a system folder dialog for each root and each PATH entry, then asks you to confirm the whole profile. A profile grants nothing by itself, and it cannot be edited: remove it and add it again.";
export const ENVIRONMENT_WARNING =
  "Variable values are stored in plain text in the settings journal, so never put a secret in one. Only the names are shown here and in the confirmation.";
export const PACKAGE_HELP =
  "A package-network profile records registry origins as configuration only. No run uses it and it grants no network access. Registry credentials are bound with the trusted CLI, not here. Removing a profile also unbinds and deletes the credentials bound to its origins. A profile cannot be edited: remove it and add it again.";
export const GRANT_HELP =
  "A container grant changes a folder's access-control list on disk for one workspace's AppContainer, and stays until you revoke it. Rust opens the folder dialog and asks you to confirm. A grant that shows pending or revoking did not finish: Revoke tries again.";
export const GRANTS_UNSUPPORTED =
  "Container grants are Windows-only. This computer cannot record or apply them, so there is nothing to change here.";

function labelled(label: string, id: string, control: HTMLElement): HTMLElement {
  return el("div", { class: "field" }, el("label", { text: label, attrs: { for: id } }), control);
}

/** A disclosure that remembers whether it is open, so a redraw keeps it. */
function disclosure(
  summary: string,
  isOpen: boolean,
  remember: (open: boolean) => void,
): HTMLDetailsElement {
  const details = el("details", { class: "disclosure" }, el("summary", { text: summary }));
  details.open = isOpen;
  details.addEventListener("toggle", () => remember(details.open));
  return details;
}

function localProblem(): HTMLElement {
  const problem = el("p", { class: "error", attrs: { role: "alert" } });
  problem.hidden = true;
  return problem;
}

function show(problem: HTMLElement, text: string): void {
  problem.textContent = text;
  problem.hidden = false;
}

type Variables = Array<{ name: string; value: string }>;

/** Parses `NAME=VALUE` lines. Blank lines are skipped; the first `=` splits. */
export function parseEnvironment(
  text: string,
): { ok: true; entries: Variables } | { ok: false; line: number } {
  const entries: Variables = [];
  for (const [index, line] of text.split(/\r?\n/).entries()) {
    if (line.trim() === "") {
      continue;
    }
    const at = line.indexOf("=");
    if (at <= 0) {
      return { ok: false, line: index + 1 };
    }
    entries.push({ name: line.slice(0, at).trim(), value: line.slice(at + 1) });
  }
  return { ok: true, entries };
}

function toolchainForm({ view, submit, actions }: SectionContext): HTMLElement {
  const forms = view.executionForms;
  const draft = forms.toolchain;
  const problem = localProblem();
  const name = el("input", {
    attrs: { id: "new-toolchain-name", type: "text", spellcheck: "false", autocomplete: "off" },
  });
  const roots = el("input", {
    attrs: { id: "new-toolchain-roots", type: "number", min: "1", max: "16", step: "1" },
  });
  const paths = el("input", {
    attrs: { id: "new-toolchain-paths", type: "number", min: "0", max: "16", step: "1" },
  });
  const environment = el("textarea", {
    attrs: {
      id: "new-toolchain-environment",
      rows: "3",
      spellcheck: "false",
      placeholder: "NAME=VALUE, one per line",
    },
  });
  name.value = draft.name;
  roots.value = draft.roots;
  paths.value = draft.pathEntries;
  environment.value = draft.environment;
  name.addEventListener("input", () => {
    draft.name = name.value;
  });
  roots.addEventListener("input", () => {
    draft.roots = roots.value;
  });
  paths.addEventListener("input", () => {
    draft.pathEntries = paths.value;
  });
  environment.addEventListener("input", () => {
    draft.environment = environment.value;
  });
  const add = button(ADD_TOOLCHAIN_LABEL, () => {
    problem.hidden = true;
    const rootCount = Number(roots.value);
    const pathCount = Number(paths.value);
    if (!Number.isInteger(rootCount) || rootCount < 1 || rootCount > 16) {
      show(problem, "Choose 1 to 16 read-only roots.");
      return;
    }
    if (!Number.isInteger(pathCount) || pathCount < 0 || pathCount > 16) {
      show(problem, "Choose 0 to 16 PATH entries.");
      return;
    }
    const parsed = parseEnvironment(environment.value);
    if (!parsed.ok) {
      show(problem, `Variable line ${parsed.line} must be NAME=VALUE.`);
      return;
    }
    const change = actions.addToolchainProfile(
      name.value.trim(),
      parsed.entries,
      rootCount,
      pathCount,
    );
    submit(async (revision) => {
      const recorded = await change(revision);
      // Reached only when the core recorded the profile; a refusal or a cancel keeps the form.
      forms.toolchain = { name: "", roots: "1", pathEntries: "0", environment: "" };
      return recorded;
    });
  });
  return el(
    "div",
    { class: "form" },
    labelled("Name", "new-toolchain-name", name),
    labelled("Read-only roots to choose", "new-toolchain-roots", roots),
    labelled("PATH entries to choose", "new-toolchain-paths", paths),
    labelled("Environment variables", "new-toolchain-environment", environment),
    el("p", { class: "muted", text: ENVIRONMENT_WARNING }),
    problem,
    el("div", { class: "row-actions" }, add),
  );
}

function packageForm({ view, submit, actions }: SectionContext): HTMLElement {
  const forms = view.executionForms;
  const draft = forms.package;
  const name = el("input", {
    attrs: { id: "new-package-name", type: "text", spellcheck: "false", autocomplete: "off" },
  });
  const origins = el("textarea", {
    attrs: {
      id: "new-package-origins",
      rows: "3",
      spellcheck: "false",
      placeholder: "One origin per line, 1 to 8",
    },
  });
  name.value = draft.name;
  origins.value = draft.origins;
  name.addEventListener("input", () => {
    draft.name = name.value;
  });
  origins.addEventListener("input", () => {
    draft.origins = origins.value;
  });
  const add = button(ADD_PACKAGE_LABEL, () => {
    const list = origins.value
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter((line) => line !== "");
    const change = actions.addPackageProfile(name.value.trim(), list);
    submit(async (revision) => {
      const recorded = await change(revision);
      forms.package = { name: "", origins: "" };
      return recorded;
    });
  });
  return el(
    "div",
    { class: "form" },
    labelled("Name", "new-package-name", name),
    labelled("Origins", "new-package-origins", origins),
    el("div", { class: "row-actions" }, add),
  );
}

function toolchainRows(ctx: SectionContext): HTMLElement[] {
  const { overview, submit, actions } = ctx;
  return overview.toolchain_profiles.map((profile) => {
    const item = row(
      profile.name,
      facts([
        ["Platform", profile.platform],
        ["Read-only roots", profile.read_only_roots.join("\n") || "none"],
        ["PATH entries", profile.path_entries.join("\n") || "none"],
        ["Variables", profile.environment_names.join(", ") || "none"],
      ]),
    );
    // Rust names the profile in a system Yes/No first.
    const remove = button("Remove", () => submit(actions.removeToolchainProfile(profile.name)));
    remove.setAttribute("aria-label", `Remove toolchain profile ${profile.name}`);
    item.append(remove);
    return item;
  });
}

function packageRows(ctx: SectionContext): HTMLElement[] {
  const { overview, submit, actions } = ctx;
  return overview.package_profiles.map((profile) => {
    const item = row(
      profile.name,
      facts([
        ["Origins", profile.origins.join("\n")],
        ["Credentials", String(profile.credentials_bound)],
      ]),
    );
    const remove = button("Remove", () => submit(actions.removePackageProfile(profile.name)));
    remove.setAttribute("aria-label", `Remove package-network profile ${profile.name}`);
    item.append(remove);
    return item;
  });
}

function grantRows(ctx: SectionContext): HTMLElement[] {
  const { overview, submit, actions } = ctx;
  const folders = new Map(overview.workspaces.map((w) => [w.workspace_id, w.folder_display]));
  return overview.container_grants.map((grant) => {
    const item = row(
      grant.directory,
      facts([
        ["Workspace", folders.get(grant.workspace_id) ?? grant.workspace_id],
        ["Access", grant.access === "read_write" ? "read and write" : "read and execute"],
        ["State", grant.state],
        ["Grant", grant.grant_id],
      ]),
    );
    if (overview.container_grants_supported) {
      const revoke = button("Revoke", () => submit(actions.revokeContainerGrant(grant.grant_id)));
      revoke.setAttribute("aria-label", `Revoke the grant on ${grant.directory}`);
      item.append(revoke);
    }
    return item;
  });
}

function grantForm(ctx: SectionContext): HTMLElement {
  const { view, overview, submit, actions } = ctx;
  const draft = view.executionForms.grant;
  if (overview.workspaces.length === 0) {
    return emptyLine("Register a workspace before granting it a folder.");
  }
  const workspace = el("select", { attrs: { id: "new-grant-workspace" } });
  for (const entry of overview.workspaces) {
    workspace.append(
      el("option", { text: entry.folder_display, attrs: { value: entry.workspace_id } }),
    );
  }
  const known = overview.workspaces.some((entry) => entry.workspace_id === draft.workspaceId);
  workspace.value = known ? draft.workspaceId : (overview.workspaces[0]?.workspace_id ?? "");
  const access = el("select", { attrs: { id: "new-grant-access" } });
  access.append(
    el("option", { text: "Read and execute", attrs: { value: "read_execute" } }),
    el("option", {
      text: "Read and write (the workspace folder only)",
      attrs: { value: "read_write" },
    }),
  );
  access.value = draft.access;
  workspace.addEventListener("change", () => {
    draft.workspaceId = workspace.value;
  });
  access.addEventListener("change", () => {
    draft.access = access.value as ContainerAccess;
  });
  const grant = button(GRANT_FOLDER_LABEL, () =>
    submit(actions.grantContainerFolder(workspace.value, access.value as ContainerAccess)),
  );
  return el(
    "div",
    { class: "form" },
    labelled("Workspace", "new-grant-workspace", workspace),
    labelled("Access", "new-grant-access", access),
    el("div", { class: "row-actions" }, grant),
  );
}

function block(title: string, ...children: HTMLElement[]): HTMLElement {
  return el("section", { class: "block" }, el("h3", { text: title }), ...children);
}

function list(rows: HTMLElement[], emptyText: string): HTMLElement {
  return rows.length === 0 ? emptyLine(emptyText) : el("ul", { class: "row-list" }, ...rows);
}

/** The Toolchain profiles group: the recorded profiles, an add form, and a remove button each. */
export function toolchainGroup(ctx: SectionContext): HTMLElement {
  const forms = ctx.view.executionForms;
  const form = disclosure("Add toolchain profile", forms.toolchainOpen, (open) => {
    forms.toolchainOpen = open;
  });
  form.append(toolchainForm(ctx));
  return block(
    "Toolchain profiles",
    el("p", { class: "section-help", text: TOOLCHAIN_HELP }),
    form,
    list(toolchainRows(ctx), "No toolchain profile is recorded."),
  );
}

/** The Package-network profiles group. */
export function packageGroup(ctx: SectionContext): HTMLElement {
  const forms = ctx.view.executionForms;
  const form = disclosure("Add package-network profile", forms.packageOpen, (open) => {
    forms.packageOpen = open;
  });
  form.append(packageForm(ctx));
  return block(
    "Package-network profiles",
    el("p", { class: "section-help", text: PACKAGE_HELP }),
    form,
    list(packageRows(ctx), "No package-network profile is recorded."),
  );
}

/** The Container grants group. Without Windows it only says the group is Windows-only. */
export function grantsGroup(ctx: SectionContext): HTMLElement {
  const { overview } = ctx;
  if (!overview.container_grants_supported) {
    return block(
      "Container grants",
      el("p", { class: "section-help", text: GRANTS_UNSUPPORTED }),
      list(grantRows(ctx), "No container grant is recorded."),
    );
  }
  const forms = ctx.view.executionForms;
  const form = disclosure("Grant a folder", forms.grantOpen, (open) => {
    forms.grantOpen = open;
  });
  form.append(grantForm(ctx));
  return block(
    "Container grants",
    el("p", { class: "section-help", text: GRANT_HELP }),
    form,
    list(grantRows(ctx), "No container grant is recorded."),
  );
}
