import { button, el, replaceChildren } from "../dom";
import {
  CANCELLED_CODE,
  DATA_FOLDER_CHANGED_CODE,
  DATA_FOLDER_REPAIRED_CODE,
  DATA_FOLDER_UNREADABLE_CODE,
  SETTINGS_CHANGE_RUNNING_CODE,
  STORAGE_NOT_PRIVATE_CODE,
  toIpcError,
} from "../ipc/errors";
import { settingsApi } from "../ipc/settings_client";
import type { SettingsOverview } from "../ipc/types";
import { renderAdvanced } from "./advanced_section";
import {
  type Actions,
  type Change,
  newExecutionForms,
  type SectionContext,
  type SectionId,
  scopedWorkspace,
  type ViewState,
} from "./context";
import { newConnectionDraft, renderModel } from "./model_section";
import { createNav } from "./nav";
import { renderWorkspaces } from "./workspaces_section";

export { DEFAULT_ENDPOINTS, PROVIDER_NAMES } from "../providers";
export { ADVANCED_HELP } from "./advanced_section";
export type { Change } from "./context";
export { choiceLabel, DEFAULT_CHOICE, MODEL_HELP } from "./model_section";
export {
  ACCESS_HELP,
  ACCESS_LABELS,
  PERMISSION_LABELS,
  WORKSPACES_HELP,
  WRITE_TOOL_LABELS,
} from "./workspaces_section";

export interface SettingsApp {
  /** Reads the configuration again. */
  refresh(): Promise<void>;
  dispose(): void;
}

/** Shown when the operator closed a dialog or declined. */
export const NOTHING_CHANGED = "No change was made.";

/** Offered with `credential_storage_not_private`; Rust explains and confirms natively (ADR-0073). */
export const REPAIR_FOLDER_LABEL = "Remove extra permissions…";
export const REPAIR_FOLDER_HELP =
  "OMGCode can remove the other accounts from its data folder. It names each one in a system dialog first.";
/**
 * Shown after the repair command succeeds. Rust also succeeds, without a dialog, when the folder
 * already granted no other account, so the text states what is true in both cases.
 */
export const FOLDER_REPAIRED =
  "OMGCode's data folder now gives access only to your account, SYSTEM, and Administrators.";
/** The next step after a repair whose settings were read again; `data_folder_repaired` has its own. */
export const RETRY_AFTER_REPAIR = "Try the change again.";
/**
 * Repair outcomes that leave the folder as it was, so the repair stays on offer. Rust takes the
 * one-change-at-a-time guard before it reads the folder, so `settings_change_running` changes
 * nothing either.
 */
const REPAIR_RETRY_CODES: ReadonlySet<string> = new Set([
  CANCELLED_CODE,
  DATA_FOLDER_CHANGED_CODE,
  DATA_FOLDER_UNREADABLE_CODE,
  SETTINGS_CHANGE_RUNNING_CODE,
]);

/** Shown while Rust waits for the browser step of a ChatGPT sign-in (ADR-0093). */
export const SIGN_IN_WAITING =
  "Waiting for the ChatGPT sign-in in your browser. It ends after five minutes.";

/** Shown when OpenAI did not confirm a sign-out's revocation. */
export const SIGN_OUT_UNCONFIRMED =
  "Signed out here, but OpenAI did not confirm the revocation. You can also disconnect OMGCode in your ChatGPT settings.";

/** What each control asks the core to do. Folders are chosen in a system dialog Rust opens. */
const ACTIONS: Actions = {
  chooseRoot: () => (revision) => settingsApi.chooseRoot(revision),
  registerWorkspace: () => (revision) => settingsApi.registerWorkspace(revision),
  setPermissions: (workspaceId, permissions) => (revision) =>
    settingsApi.setPermissions(workspaceId, revision, permissions),
  choosePackageCache: (workspaceId) => (revision) =>
    settingsApi.choosePackageCache(workspaceId, revision),
  clearPackageCache: (workspaceId) => (revision) =>
    settingsApi.clearPackageCache(workspaceId, revision),
  unregisterWorkspace: (workspaceId) => (revision) =>
    settingsApi.unregisterWorkspace(workspaceId, revision),
  selectConnection: (profile) => (revision) => settingsApi.selectConnection(revision, profile),
  addConnection: (name, kind, endpoint, apiKey) => (revision) =>
    settingsApi.addConnection(revision, name, kind, endpoint, apiKey),
  setApiKey: (name, apiKey) => (revision) => settingsApi.setApiKey(revision, name, apiKey),
  removeConnection: (name) => (revision) => settingsApi.removeConnection(revision, name),
  signInChatGpt: (name) => (revision) => settingsApi.signInChatGpt(revision, name),
  signOutChatGpt: (name) => async (revision) =>
    (await settingsApi.signOutChatGpt(revision, name)).overview,
  // Neither is a permission, so neither names the settings revision.
  saveChatDefaults: (defaults) => () => settingsApi.saveChatDefaults(defaults),
  setWorkspaceInstructions: (workspaceId, instructions) => () =>
    settingsApi.setWorkspaceInstructions(workspaceId, instructions),
  importSkill: () => (revision) => settingsApi.importSkill(revision),
  setSkillState: (name, workspaceId, state) => (revision) =>
    settingsApi.setSkillState(revision, name, workspaceId, state),
  removeSkill: (name) => (revision) => settingsApi.removeSkill(revision, name),
  addToolchainProfile: (name, environment, rootCount, pathEntryCount) => (revision) =>
    settingsApi.addToolchainProfile(revision, name, environment, rootCount, pathEntryCount),
  removeToolchainProfile: (name) => (revision) =>
    settingsApi.removeToolchainProfile(revision, name),
  addPackageProfile: (name, origins) => (revision) =>
    settingsApi.addPackageProfile(revision, name, origins),
  removePackageProfile: (name) => (revision) => settingsApi.removePackageProfile(revision, name),
  grantContainerFolder: (workspaceId, access) => (revision) =>
    settingsApi.grantContainerFolder(revision, workspaceId, access),
  revokeContainerGrant: (grantId) => (revision) =>
    settingsApi.revokeContainerGrant(revision, grantId),
};

/**
 * Builds the settings window. It renders only operator-recorded configuration that the core
 * has made display-safe; never transcript, tool, or skill text. One change runs at a time; while
 * it waits for the operator's answer in a system dialog, every control is disabled.
 */
export function startSettings(root: HTMLElement): SettingsApp {
  const view: ViewState = {
    section: "workspaces",
    workspaceId: null,
    workspacePage: false,
    workspacesExpanded: true,
    toolLimitsOpen: false,
    addConnectionOpen: false,
    connectionDraft: newConnectionDraft(),
    chatDraft: null,
    instructionDrafts: new Map(),
    executionForms: newExecutionForms(),
  };
  // A change's error stays until the next change starts; a read's failure until a read succeeds.
  // Two elements, so neither outcome can overwrite or clear the other.
  const problem = el("p", { class: "error change-error", attrs: { role: "alert" } });
  problem.hidden = true;
  const readProblem = el("p", { class: "error read-error", attrs: { role: "alert" } });
  readProblem.hidden = true;
  const notice = el("p", { class: "notice", attrs: { role: "status" } });
  notice.hidden = true;
  // Shown while Rust waits for the ChatGPT sign-in in the browser (ADR-0093). It sits outside
  // the disabled body, so its Cancel button stays usable while the change runs.
  const cancelSignIn = button("Cancel sign-in", () => {
    void settingsApi.cancelChatGptSignIn().catch(() => undefined);
  });
  const signInBar = el(
    "div",
    { class: "notice sign-in-wait", attrs: { role: "status" } },
    el("span", { text: SIGN_IN_WAITING }),
    cancelSignIn,
  );
  signInBar.hidden = true;
  const actions: Actions = {
    ...ACTIONS,
    signInChatGpt: (name) => async (revision) => {
      signInBar.hidden = false;
      try {
        return await settingsApi.signInChatGpt(revision, name);
      } finally {
        signInBar.hidden = true;
      }
    },
    signOutChatGpt: (name) => async (revision) => {
      const result = await settingsApi.signOutChatGpt(revision, name);
      if (!result.revoked) {
        replaceChildren(notice, SIGN_OUT_UNCONFIRMED);
        notice.hidden = false;
      }
      return result.overview;
    },
  };
  // A disabled fieldset disables every control inside it without touching their own state.
  const body = el("fieldset", { class: "settings-body" });
  // The one scroll container for every section.
  // Shown only while the window is scoped to one workspace (ADR-0071).
  const allSettings = button("All settings", () => submit(() => settingsApi.clearScope()));
  const scopeBar = el("div", { class: "scope-bar" }, allSettings);
  scopeBar.hidden = true;
  const content = el(
    "div",
    { class: "settings-content" },
    scopeBar,
    problem,
    readProblem,
    notice,
    signInBar,
    body,
  );
  const nav = createNav({
    onSelect: (id) => select(id),
    onSelectWorkspace: (id) => {
      view.workspaceId = id;
      select("workspaces", true);
    },
    onToggleWorkspaces: () => {
      view.workspacesExpanded = !view.workspacesExpanded;
      draw();
    },
    onRefresh: () => void refresh(),
  });
  const windowElement = el("main", { class: "settings-window" }, nav.element, content);
  let current: SettingsOverview | null = null;
  let working = false;
  /**
   * A read was asked for while another read or change ran. That one's answer may predate a scope
   * the main window set meanwhile, so one more read follows it.
   */
  let readAgain = false;
  /** While a folder repair runs, the shared-folder problem its button was offered with. */
  let repairing: string | null = null;

  function setWorking(value: boolean): void {
    working = value;
    body.disabled = value;
    nav.setDisabled(value);
    allSettings.disabled = value;
  }

  /** Ends the current read or change, then makes the read that was asked for meanwhile. */
  function finishWork(): void {
    setWorking(false);
    if (readAgain) {
      readAgain = false;
      void refresh();
    }
  }

  function renderSection(id: SectionId, context: SectionContext): HTMLElement[] {
    switch (id) {
      case "model":
        return renderModel(context);
      case "workspaces":
        return renderWorkspaces(context);
      case "advanced":
        return renderAdvanced(context);
    }
  }

  function draw(): void {
    if (current === null) {
      return;
    }
    const context: SectionContext = {
      overview: current,
      view,
      submit,
      actions,
      rerender: draw,
    };
    const scoped = scopedWorkspace(current) !== null;
    // A scoped window shows one workspace and no section list; All settings restores both.
    nav.element.hidden = scoped;
    scopeBar.hidden = !scoped;
    windowElement.classList.toggle("scoped", scoped);
    // Drawing replaces every control. A focused control with an id gets focus back on its
    // replacement, so a keyboard user keeps their place.
    const active = document.activeElement;
    const focusedId = active instanceof HTMLElement && body.contains(active) ? active.id : "";
    const unsent = takeUnsentKeys();
    replaceChildren(body, ...renderSection(scoped ? "workspaces" : view.section, context));
    restoreUnsentKeys(unsent);
    // After the section, which settles the workspace shown when the chosen one is gone.
    nav.update({
      section: view.section,
      workspacePage: view.workspacePage,
      workspaceId: view.workspaceId,
      expanded: view.workspacesExpanded,
      workspaces: current.workspaces
        .filter((w) => !scoped || w.workspace_id === view.workspaceId)
        .map((w) => ({ id: w.workspace_id, label: w.folder_display })),
    });
    const replacement = focusedId === "" ? null : document.getElementById(focusedId);
    if (replacement !== null && body.contains(replacement)) {
      replacement.focus();
    }
  }

  /**
   * Takes every API key typed but not sent out of its field, by the field's id, which names the
   * form and its connection. The keys move from the old fields to the new ones and are never put
   * in view state. A key in a closed form is cleared and not carried.
   */
  function takeUnsentKeys(): Map<string, string> {
    const unsent = new Map<string, string>();
    for (const input of Array.from(
      body.querySelectorAll<HTMLInputElement>('input[type="password"]'),
    )) {
      const value = input.value;
      input.value = "";
      if (input.id !== "" && value !== "" && input.closest("details")?.open !== false) {
        unsent.set(input.id, value);
      }
    }
    return unsent;
  }

  // Closing a form drops the key typed into it, so nothing hidden keeps a key or brings it back.
  body.addEventListener(
    "toggle",
    (event) => {
      const disclosure = event.target;
      if (disclosure instanceof HTMLDetailsElement && !disclosure.open) {
        for (const input of Array.from(
          disclosure.querySelectorAll<HTMLInputElement>('input[type="password"]'),
        )) {
          input.value = "";
        }
      }
    },
    true,
  );

  /**
   * Puts each key back into the new field with the same id, opening its disclosure again. A key
   * whose form is gone (its connection removed or no longer hosted) is dropped.
   */
  function restoreUnsentKeys(unsent: Map<string, string>): void {
    for (const [id, value] of unsent) {
      const input = document.getElementById(id);
      if (
        !(input instanceof HTMLInputElement) ||
        input.type !== "password" ||
        !body.contains(input)
      ) {
        continue;
      }
      input.value = value;
      const disclosure = input.closest("details");
      if (disclosure !== null) {
        disclosure.open = true;
      }
    }
    unsent.clear();
  }

  function select(id: SectionId, workspacePage = false): void {
    view.section = id;
    view.workspacePage = id === "workspaces" && workspacePage;
    // A new section starts at its heading, not at the last section's scroll offset.
    content.scrollTop = 0;
    draw();
  }

  /** Whether two overviews hold the same data. Both are plain IPC data in Rust's field order. */
  function sameOverview(next: SettingsOverview, shown: SettingsOverview | null): boolean {
    return shown !== null && JSON.stringify(next) === JSON.stringify(shown);
  }

  function show(overview: SettingsOverview): void {
    current = overview;
    nav.setRevision(overview.revision);
    draw();
  }

  /**
   * Shows the outcome of a change: its error (perhaps credential cleanup instructions) or
   * "nothing changed". It stays until the operator starts another change, even when a read
   * queued meanwhile follows at once, as one does when a native dialog returns focus here.
   */
  function reportChange(reason: unknown): void {
    const error = toIpcError(reason);
    // Set only while a repair runs, so a failure of any other change never offers it.
    const offered = repairing;
    repairing = null;
    if (error.code === STORAGE_NOT_PRIVATE_CODE) {
      offerRepair(`${error.message} (${error.code})`);
      return;
    }
    if (error.code === DATA_FOLDER_REPAIRED_CODE) {
      // The repair itself succeeded; only reading the settings again failed.
      replaceChildren(notice, FOLDER_REPAIRED);
      notice.hidden = false;
      replaceChildren(problem, `${error.message} (${error.code})`);
      problem.hidden = false;
      return;
    }
    if (offered !== null && REPAIR_RETRY_CODES.has(error.code)) {
      if (error.code === CANCELLED_CODE) {
        replaceChildren(notice, NOTHING_CHANGED);
        notice.hidden = false;
        offerRepair(offered);
      } else {
        offerRepair(offered, `${error.message} (${error.code})`);
      }
      return;
    }
    if (error.code === CANCELLED_CODE) {
      replaceChildren(notice, NOTHING_CHANGED);
      notice.hidden = false;
      return;
    }
    replaceChildren(problem, `${error.message} (${error.code})`);
    problem.hidden = false;
  }

  /**
   * Shows the shared-folder problem with its repair button, after `outcome` when a repair left
   * the folder as it was. The button only asks Rust to look; Rust lists the accounts and asks in
   * a native dialog.
   */
  function offerRepair(storageProblem: string, outcome?: string): void {
    const repair = button(REPAIR_FOLDER_LABEL, () => submit(repairDataFolder(storageProblem)));
    repair.classList.add("repair-folder");
    const lead = outcome === undefined ? storageProblem : `${outcome} ${storageProblem}`;
    replaceChildren(problem, `${lead} ${REPAIR_FOLDER_HELP} `, repair);
    problem.hidden = false;
  }

  /** The repair change. On success it says so, since the overview itself does not change. */
  function repairDataFolder(storageProblem: string): Change {
    return async () => {
      repairing = storageProblem;
      const overview = await settingsApi.repairDataFolder();
      repairing = null;
      replaceChildren(notice, `${FOLDER_REPAIRED} ${RETRY_AFTER_REPAIR}`);
      notice.hidden = false;
      return overview;
    };
  }

  /** Shows why the configuration could not be read. The next successful read hides it. */
  function reportRead(reason: unknown): void {
    const error = toIpcError(reason);
    replaceChildren(readProblem, `${error.message} (${error.code})`);
    readProblem.hidden = false;
  }

  function clearMessages(): void {
    problem.hidden = true;
    readProblem.hidden = true;
    notice.hidden = true;
  }

  async function refresh(): Promise<void> {
    if (working) {
      readAgain = true;
      return;
    }
    setWorking(true);
    try {
      const overview = await settingsApi.overview();
      // An unchanged overview is not drawn again: a redraw would rebuild every form and drop
      // what the operator typed but has not sent, such as an API key pasted from elsewhere
      // just before this window regained focus.
      if (!sameOverview(overview, current)) {
        show(overview);
      }
      readProblem.hidden = true;
    } catch (reason) {
      reportRead(reason);
    } finally {
      finishWork();
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
        reportChange(reason);
        try {
          show(await settingsApi.overview());
        } catch (again) {
          show(recorded);
          reportRead(again);
        }
      } finally {
        finishWork();
      }
    })();
  }

  const onVisible = (): void => {
    if (document.visibilityState === "visible") {
      void refresh();
    }
  };
  document.addEventListener("visibilitychange", onVisible);
  // The window can already be visible when the main window scopes it, which fires no visibility
  // change; it gains focus, so it reads the scope then.
  window.addEventListener("focus", onVisible);

  replaceChildren(root, windowElement);
  void refresh();

  return {
    refresh,
    dispose() {
      document.removeEventListener("visibilitychange", onVisible);
      window.removeEventListener("focus", onVisible);
    },
  };
}
