import { button, el, neutralise } from "../dom";
import type { RootGrantNeed, ToolchainProposalView } from "../ipc/types";
import type { DiscoveryChange, DiscoveryState, SectionContext } from "./context";
import { emptyLine, facts, row } from "./parts";

/**
 * Toolchain profiles proposed from the toolchains installed on this computer (ADR-0103). Rust
 * finds them by reading folders and runs nothing; this panel shows each proposal in full and
 * sends back only the digests the operator selects. Each add or grant waits for one system
 * confirmation that lists what it records.
 */
export const DISCOVER_LABEL = "Generate toolchain profiles from local packages";
export const ADD_SELECTED_LABEL = "Add selected profiles…";
export const GRANT_SELECTED_LABEL = "Grant folders for selected…";
/** The most proposals one add or grant sends; Rust refuses more. */
export const MAX_SELECTED = 8;

export const DISCOVERY_HELP =
  "OMGCode can look for installed toolchains (Rust, C and C++, Java, Python, Perl, Go, Node.js, .NET, and Ruby) in their standard install folders, on your PATH, and in the folders named by JAVA_HOME, GOROOT, and RUSTUP_HOME, and propose a profile for each. It only reads folders and small files the installers wrote. It runs no program and never reads a project's configuration, such as rust-toolchain.toml. Nothing is recorded until you select proposals and confirm them in a system dialog.";
export const DISCOVERY_GRANT_HELP =
  "On Windows a run can read a root only when the workspace's container can. Choose a workspace to see which roots need a read-execute grant; a list already shown is generated again for it. A folder only an administrator can change is named, not granted.";
export const NOTHING_FOUND =
  "No toolchain was found in the standard install folders, on PATH, or in the folders named by JAVA_HOME, GOROOT, and RUSTUP_HOME.";
export const SELECTION_LIMIT = "Select at most 8 proposals at a time.";
export const WORKSPACE_GRANT_MISSING =
  "This workspace's sandbox has no active read-write grant on the workspace folder, which every run in it needs. Grant it under Container grants with Read and write the workspace's own folder.";
export const NO_WORKSPACE = "No workspace";

/** Says how many installations the list leaves out. */
export function omittedText(count: number): string {
  const what = count === 1 ? "1 more installation was" : `${count} more installations were`;
  return `${what} found and not listed: a list holds at most 8 installations under one name and 32 in all, each name's preferred installation first.`;
}

const GRANT_TEXT: Record<RootGrantNeed, string> = {
  not_applicable: "",
  readable: "the container can read it",
  needs_grant: "needs a grant",
  needs_administrator: "needs an administrator to grant",
  unknown: "access could not be read",
};

/** Runs a discovery read or change and remembers its proposals for the next draw. */
function run(ctx: SectionContext, change: DiscoveryChange, keepSelection: boolean): void {
  const state = ctx.view.executionForms.discovery;
  ctx.submit(async (revision) => {
    const result = await change(revision);
    // Reached only on success; a refusal or a cancel keeps the list and the selection.
    state.proposals = result.proposals;
    state.workspaceId = result.workspace_id;
    state.workspaceGrantActive = result.workspace_grant_active;
    state.omitted = result.omitted;
    // A kept selection loses proposals that are gone or that core would now refuse.
    const addable = new Set(
      result.proposals
        .filter((proposal) => proposal.refusal === null)
        .map((proposal) => proposal.digest),
    );
    state.selected = keepSelection
      ? new Set([...state.selected].filter((digest) => addable.has(digest)))
      : new Set();
    return result.overview;
  });
}

function rootLines(proposal: ToolchainProposalView): string {
  return proposal.read_only_roots
    .map((root) => {
      const need = GRANT_TEXT[root.grant];
      return need === "" ? root.folder : `${root.folder} (${need})`;
    })
    .join("\n");
}

function proposalRow(
  proposal: ToolchainProposalView,
  index: number,
  state: DiscoveryState,
  onSelect: () => void,
): HTMLElement {
  const pairs: Array<[string, string]> = [
    [
      "Family",
      proposal.version === null
        ? proposal.family_label
        : `${proposal.family_label}, ${proposal.version}`,
    ],
    ["Read-only roots", rootLines(proposal)],
    ["PATH entries", proposal.path_entries.join("\n") || "none"],
    [
      "Variables",
      proposal.environment.map((variable) => `${variable.name}=${variable.value}`).join("\n") ||
        "none",
    ],
  ];
  if (proposal.notes.length > 0) {
    pairs.push(["Notes", proposal.notes.join("\n")]);
  }
  const item = row(proposal.name, facts(pairs));
  item.classList.add("proposal");
  const id = `toolchain-proposal-${index}`;
  const select = el("input", { attrs: { id, type: "checkbox" } });
  select.checked = state.selected.has(proposal.digest);
  select.disabled = proposal.refusal !== null;
  select.setAttribute("aria-label", neutralise(`Select the proposed profile ${proposal.name}`));
  select.addEventListener("change", () => {
    if (select.checked) {
      state.selected.add(proposal.digest);
    } else {
      state.selected.delete(proposal.digest);
    }
    onSelect();
  });
  item.querySelector(".row-head")?.prepend(select);
  // Under the facts: a child of the row itself would sit beside them like a row action.
  const body = item.querySelector(".row-body");
  if (proposal.recorded) {
    body?.append(el("p", { class: "muted", text: "A profile with this name is recorded." }));
  } else if (proposal.refusal !== null) {
    body?.append(el("p", { class: "muted", text: `Cannot be added: ${proposal.refusal}` }));
  }
  return item;
}

/**
 * The workspace choice for container access, on Windows with a registered workspace. A list on
 * screen is generated again for the new choice, so its grant states never describe another
 * workspace.
 */
function workspaceChoice(ctx: SectionContext): HTMLElement | null {
  const { overview, actions } = ctx;
  const state = ctx.view.executionForms.discovery;
  if (!overview.container_grants_supported || overview.workspaces.length === 0) {
    return null;
  }
  const choice = el("select", { attrs: { id: "discovery-workspace" } });
  choice.append(el("option", { text: NO_WORKSPACE, attrs: { value: "" } }));
  for (const entry of overview.workspaces) {
    choice.append(
      el("option", { text: entry.folder_display, attrs: { value: entry.workspace_id } }),
    );
  }
  choice.value = chosenWorkspace(ctx) ?? "";
  choice.addEventListener("change", () => {
    state.chosenWorkspace = choice.value;
    if (state.proposals !== null) {
      run(ctx, actions.discoverToolchains(chosenWorkspace(ctx)), true);
    }
  });
  return el(
    "div",
    { class: "field" },
    el("label", { text: "Show container access for", attrs: { for: "discovery-workspace" } }),
    choice,
  );
}

/** The chosen workspace if it is still registered and grants exist here, else `null`. */
function chosenWorkspace(ctx: SectionContext): string | null {
  const { overview } = ctx;
  const chosen = ctx.view.executionForms.discovery.chosenWorkspace;
  if (!overview.container_grants_supported || chosen === "") {
    return null;
  }
  return overview.workspaces.some((entry) => entry.workspace_id === chosen) ? chosen : null;
}

/**
 * The discovery panel: a button that asks Rust for proposals, then each proposal with a checkbox,
 * and buttons that add the selected ones or, on Windows, grant the folders they need.
 */
export function discoveryPanel(ctx: SectionContext): HTMLElement {
  const { overview, actions } = ctx;
  const state = ctx.view.executionForms.discovery;
  const problem = el("p", { class: "error", attrs: { role: "alert" } });
  problem.hidden = true;
  const generate = button(DISCOVER_LABEL, () =>
    run(ctx, actions.discoverToolchains(chosenWorkspace(ctx)), true),
  );
  const panel = el(
    "div",
    { class: "discovery" },
    el("p", { class: "section-help", text: DISCOVERY_HELP }),
  );
  const choice = workspaceChoice(ctx);
  if (choice !== null) {
    panel.append(el("p", { class: "muted", text: DISCOVERY_GRANT_HELP }), choice);
  }
  panel.append(el("div", { class: "row-actions" }, generate));
  const proposals = state.proposals;
  if (proposals === null) {
    return panel;
  }
  if (proposals.length === 0) {
    panel.append(emptyLine(NOTHING_FOUND));
    return panel;
  }
  if (state.omitted > 0) {
    panel.append(el("p", { class: "muted", text: omittedText(state.omitted) }));
  }
  // The list's own workspace, not the one chosen since, decides the grants.
  const listWorkspace = overview.workspaces.some((w) => w.workspace_id === state.workspaceId)
    ? state.workspaceId
    : null;
  if (listWorkspace !== null && state.workspaceGrantActive === false) {
    panel.append(el("p", { class: "muted", text: WORKSPACE_GRANT_MISSING }));
  }
  const add = button(ADD_SELECTED_LABEL, () => {
    const digests = selectedDigests();
    if (digests.length > MAX_SELECTED) {
      problem.textContent = SELECTION_LIMIT;
      problem.hidden = false;
      return;
    }
    run(ctx, actions.addDiscoveredProfiles(digests, listWorkspace), false);
  });
  const grant = button(GRANT_SELECTED_LABEL, () => {
    const digests = selectedDigests();
    if (digests.length > MAX_SELECTED) {
      problem.textContent = SELECTION_LIMIT;
      problem.hidden = false;
      return;
    }
    if (listWorkspace !== null) {
      run(ctx, actions.grantDiscoveredRoots(listWorkspace, digests), true);
    }
  });
  /** The selected digests in list order, so the confirmation follows the list. */
  function selectedDigests(): string[] {
    return (proposals ?? [])
      .map((proposal) => proposal.digest)
      .filter((digest) => state.selected.has(digest));
  }
  function updateButtons(): void {
    problem.hidden = true;
    const chosen = (proposals ?? []).filter((proposal) => state.selected.has(proposal.digest));
    add.disabled = !chosen.some((proposal) => proposal.refusal === null);
    grant.disabled =
      listWorkspace === null ||
      !chosen.some((proposal) =>
        proposal.read_only_roots.some((root) => root.grant === "needs_grant"),
      );
  }
  panel.append(
    el(
      "ul",
      { class: "row-list" },
      ...proposals.map((proposal, index) => proposalRow(proposal, index, state, updateButtons)),
    ),
    problem,
  );
  const buttons = el("div", { class: "row-actions" }, add);
  if (overview.container_grants_supported) {
    buttons.append(grant);
  }
  panel.append(buttons);
  updateButtons();
  return panel;
}
