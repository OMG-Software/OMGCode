import { button, el, replaceChildren } from "../dom";
import { CANCELLED_CODE, toIpcError } from "../ipc/errors";
import { settingsApi } from "../ipc/settings_client";
import type { SettingsOverview } from "../ipc/types";
import { renderAdvanced } from "./advanced_section";
import {
  type Actions,
  type Change,
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
  selectConnection: (profile) => (revision) => settingsApi.selectConnection(revision, profile),
  addConnection: (name, kind, endpoint, apiKey) => (revision) =>
    settingsApi.addConnection(revision, name, kind, endpoint, apiKey),
  setApiKey: (name, apiKey) => (revision) => settingsApi.setApiKey(revision, name, apiKey),
  removeConnection: (name) => (revision) => settingsApi.removeConnection(revision, name),
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
    toolLimitsOpen: false,
    addConnectionOpen: false,
    connectionDraft: newConnectionDraft(),
  };
  // A change's error stays until the next change starts; a read's failure until a read succeeds.
  // Two elements, so neither outcome can overwrite or clear the other.
  const problem = el("p", { class: "error change-error", attrs: { role: "alert" } });
  problem.hidden = true;
  const readProblem = el("p", { class: "error read-error", attrs: { role: "alert" } });
  readProblem.hidden = true;
  const notice = el("p", { class: "notice", attrs: { role: "status" } });
  notice.hidden = true;
  // A disabled fieldset disables every control inside it without touching their own state.
  const body = el("fieldset", { class: "settings-body" });
  // The one scroll container for every section.
  // Shown only while the window is scoped to one workspace (ADR-0069).
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
    body,
  );
  const nav = createNav(
    view.section,
    (id) => select(id),
    () => void refresh(),
  );
  const windowElement = el("main", { class: "settings-window" }, nav.element, content);
  let current: SettingsOverview | null = null;
  let working = false;
  /**
   * A read was asked for while another read or change ran. That one's answer may predate a scope
   * the main window set meanwhile, so one more read follows it.
   */
  let readAgain = false;

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
        return renderAdvanced(context.overview);
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
      actions: ACTIONS,
      rerender: draw,
    };
    const scoped = scopedWorkspace(current) !== null;
    // A scoped window shows one workspace and no section list; All settings restores both.
    nav.element.hidden = scoped;
    scopeBar.hidden = !scoped;
    windowElement.classList.toggle("scoped", scoped);
    nav.select(view.section);
    // Drawing replaces every control. A focused control with an id (the workspace chooser) gets
    // focus back on its replacement, so a keyboard user keeps their place.
    const active = document.activeElement;
    const focusedId = active instanceof HTMLElement && body.contains(active) ? active.id : "";
    replaceChildren(body, ...renderSection(scoped ? "workspaces" : view.section, context));
    const replacement = focusedId === "" ? null : document.getElementById(focusedId);
    if (replacement !== null && body.contains(replacement)) {
      replacement.focus();
    }
  }

  function select(id: SectionId): void {
    view.section = id;
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
    if (error.code === CANCELLED_CODE) {
      replaceChildren(notice, NOTHING_CHANGED);
      notice.hidden = false;
      return;
    }
    replaceChildren(problem, `${error.message} (${error.code})`);
    problem.hidden = false;
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
