import { el, replaceChildren } from "../dom";
import type { ConnectionChange, ConnectionModel } from "./connection_state";
import { createModelPicker } from "./model_picker";
import { createProviderSelect } from "./provider_select";

/** Shown while Rust reports that the saved per-session provider and model choices are unreadable. */
export const SESSION_CHOICES_UNREADABLE =
  "Saved provider and model choices could not be read, so this session follows the default " +
  "connection. Choosing a provider or model replaces the damaged file and every other session's " +
  "saved choice.";

/** The tooltip and accessible name of the collapsed rail's button that opens the box. */
export const MODEL_RAIL_LABEL = "Model";

export interface ModelBox {
  /** The box for the side panel's fixed footer (ADR-0089). */
  element: HTMLElement;
  /** The chosen model id, or an empty string when none is chosen. */
  model(): string;
  /** Disabled while no session accepts a message or a turn runs; the composer sets it. */
  setEnabled(enabled: boolean): void;
  /** Called when the chosen model or the listed models change; returns the unsubscribe function. */
  onChange(listener: () => void): () => void;
  dispose(): void;
}

/**
 * Builds the provider and model choice of the open session (ADR-0071, ADR-0089). It is the same
 * controls and the same shared connection the composer used to hold: only the place changed. A
 * different session, a confirmed provider choice, or a changed connection clears the model and
 * lists the new session's models; the model shown is always the one the connection holds. The
 * composer asks `model()` for each send, so a send uses what the box shows.
 */
export function createModelBox(connection: ConnectionModel): ModelBox {
  const listeners = new Set<() => void>();
  function notify(): void {
    for (const listener of listeners) {
      listener();
    }
  }

  const providerNote = el("span", { class: "muted", attrs: { role: "status" } });
  providerNote.hidden = true;
  // Rust could not read the saved per-session choices; it follows the global choice meanwhile.
  const choicesNote = el("span", {
    class: "model-warning",
    text: SESSION_CHOICES_UNREADABLE,
    attrs: { role: "status" },
  });
  choicesNote.hidden = true;
  const picker = createModelPicker(
    () => {
      refresh();
      notify();
    },
    (id) => void connection.chooseModel(id),
  );
  const providerSelect = createProviderSelect(connection, "Provider");
  /** What the model list was last requested for, so only a real change reloads it. */
  let loadedKey: string | null = null;
  let enabled = false;

  function refresh(): void {
    picker.setDisabled(!enabled || connection.busy());
    providerSelect.setLocked(!enabled);
    replaceChildren(providerNote, connection.message());
    providerNote.hidden = connection.message() === "";
    choicesNote.hidden = connection.status()?.session_choices_unreadable !== true;
  }

  function onConnection(change: ConnectionChange): void {
    providerSelect.render();
    const status = connection.status();
    const id = connection.sessionId();
    if (change === "opening") {
      loadedKey = null;
      picker.reset();
      picker.setHosted(false);
    } else if (status !== null && id !== null && change !== "busy" && change !== "message") {
      picker.setHosted(status.connection.kind === "ollama_cloud");
      const key = [
        id,
        status.connection.profile ?? "",
        status.connection.endpoint,
        status.origin,
        String(status.stale),
      ].join("|");
      if (change === "selected" || key !== loadedKey) {
        loadedKey = key;
        picker.reset();
        picker.setValue(status.model);
        void picker.load(id);
      } else {
        picker.setValue(status.model);
      }
    }
    refresh();
  }
  const unsubscribe = connection.subscribe(onConnection);
  providerSelect.render();
  refresh();

  const element = el(
    "section",
    { class: "model-box", attrs: { "aria-label": "Provider and model" } },
    el("h3", { class: "model-box-title", text: "Model" }),
    el(
      "label",
      { class: "model-box-field" },
      el("span", { text: "Provider" }),
      providerSelect.element,
    ),
    el("div", { class: "model-box-field" }, el("span", { text: "Model" }), picker.element),
    el(
      "div",
      { class: "model-box-status" },
      picker.note,
      picker.warning,
      providerNote,
      choicesNote,
    ),
  );

  return {
    element,
    model: () => picker.value(),
    setEnabled(next) {
      if (next !== enabled) {
        enabled = next;
        refresh();
      }
    },
    onChange(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    dispose() {
      unsubscribe();
      listeners.clear();
    },
  };
}
