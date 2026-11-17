import { button, el, replaceChildren } from "../dom";
import { api } from "../ipc/client";
import { toIpcError } from "../ipc/errors";
import type { SessionSummary, WorkspaceChoice } from "../ipc/types";
import { TITLE_MAX_BYTES, titleProblem } from "./model";

export interface NewSessionDialog {
  element: HTMLDialogElement;
  /** Loads the workspace list and opens the dialog. */
  show(): Promise<void>;
}

function permissionText(choice: WorkspaceChoice): string {
  const enabled = (
    [
      ["model tools", choice.model_tools],
      ["read", choice.read],
      ["write", choice.write],
      ["execute", choice.execute],
    ] as const
  )
    .filter(([, on]) => on)
    .map(([name]) => name);
  return enabled.length === 0 ? "No preferences enabled" : `Enabled: ${enabled.join(", ")}`;
}

/**
 * Builds the new-session dialog. `onCreated` receives the new summary and the folder
 * label of the chosen workspace.
 */
export function createNewSessionDialog(
  onCreated: (session: SessionSummary, folder: string) => void,
): NewSessionDialog {
  const dialog = el("dialog", {
    class: "dialog",
    attrs: { "aria-labelledby": "new-session-title" },
  });

  function open(): void {
    if (typeof dialog.showModal === "function") {
      dialog.showModal();
    } else {
      dialog.setAttribute("open", "");
    }
  }

  function close(): void {
    if (typeof dialog.close === "function") {
      dialog.close();
    } else {
      dialog.removeAttribute("open");
    }
  }

  const heading = el("h2", { text: "New session", attrs: { id: "new-session-title" } });

  function showMessage(message: string): void {
    replaceChildren(
      dialog,
      heading,
      el("p", { text: message }),
      el("div", { class: "dialog-actions" }, button("Close", close)),
    );
  }

  function showForm(workspaces: WorkspaceChoice[]): void {
    const select = el("select", { attrs: { "aria-label": "Workspace" } });
    for (const choice of workspaces) {
      const option = el("option", { text: choice.folder_display });
      option.value = choice.workspace_id;
      select.appendChild(option);
    }
    const detail = el("p", { class: "muted" });
    const title = el("input", {
      attrs: { type: "text", "aria-label": "Title", autocomplete: "off" },
    });
    const error = el("p", { class: "error" });
    error.hidden = true;
    const create = el("button", { class: "primary", text: "Create" });
    create.type = "button";

    function syncDetail(): void {
      const choice = workspaces.find((w) => w.workspace_id === select.value);
      replaceChildren(detail, choice === undefined ? "" : permissionText(choice));
    }
    select.addEventListener("change", syncDetail);
    syncDetail();

    create.addEventListener("click", () => {
      const problem = titleProblem(title.value);
      if (problem !== null) {
        replaceChildren(error, problem);
        error.hidden = false;
        return;
      }
      const choice = workspaces.find((w) => w.workspace_id === select.value);
      if (choice === undefined) {
        return;
      }
      create.disabled = true;
      api
        .sessionCreate(choice.workspace_id, title.value.trim())
        .then((session) => {
          close();
          onCreated(session, choice.folder_display);
        })
        .catch((reason: unknown) => {
          const failure = toIpcError(reason);
          replaceChildren(error, `${failure.message} (${failure.code})`);
          error.hidden = false;
          create.disabled = false;
        });
    });

    replaceChildren(
      dialog,
      heading,
      el("label", { class: "field" }, el("span", { text: "Workspace" }), select),
      detail,
      el(
        "label",
        { class: "field" },
        el("span", { text: `Title (up to ${TITLE_MAX_BYTES} bytes)` }),
        title,
      ),
      error,
      el("div", { class: "dialog-actions" }, button("Close", close), create),
    );
    title.focus();
  }

  return {
    element: dialog,
    async show() {
      replaceChildren(dialog, heading, el("p", { class: "muted", text: "Loading workspaces…" }));
      open();
      try {
        const list = await api.workspaceList();
        if (!list.development_root_set || list.workspaces.length === 0) {
          showMessage(
            "No workspaces are available. Workspaces are registered with the trusted CLI: run `omgcode workspace set-root`, then `omgcode workspace register`.",
          );
          return;
        }
        showForm(list.workspaces);
      } catch (reason) {
        const failure = toIpcError(reason);
        showMessage(`${failure.message} (${failure.code})`);
      }
    },
  };
}
