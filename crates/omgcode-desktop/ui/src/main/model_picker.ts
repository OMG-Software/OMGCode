import { button, el, replaceChildren } from "../dom";
import { api } from "../ipc/client";
import { toIpcError } from "../ipc/errors";
import type { ModelChoice } from "../ipc/types";

/** Added to a model whose id carries Ollama's cloud tag (ADR-0066). */
export const CLOUD_SUFFIX = " (Ollama cloud)";
/** Shown while a cloud-tagged model is chosen. */
export const CLOUD_WARNING =
  "This Ollama model runs on Ollama's hosted service: prompts and workspace text sent to it leave this computer.";
/** Added to a chosen model the server did not report, such as a saved default. */
export const UNLISTED_SUFFIX = " (not reported by the server)";
export const LISTING_NOTE = "Listing models…";
export const EMPTY_NOTE = "The server reported no models.";
export const CHANGED_NOTE = "The connection changed while models were listed. Refresh the list.";

export interface ModelPicker {
  /** The labelled field: the choice, its Refresh button, and a status note. */
  element: HTMLElement;
  /** The chosen model id, or an empty string when none is chosen. */
  value(): string;
  /** Chooses `id`. A model the server did not report stays offered and is marked. */
  setValue(id: string): void;
  /**
   * Lists the models of the connection the next turn uses. `endpoint` is the one this
   * window shows; a list for another endpoint, or one that a later call replaced, is
   * discarded. Never rejects: a failure is shown in the note.
   */
  load(endpoint: string): Promise<void>;
  setDisabled(disabled: boolean): void;
}

function option(value: string, label: string): HTMLOptionElement {
  const node = el("option", { text: label });
  // The exact id, set as a property; the label went through `neutralise`.
  node.value = value;
  return node;
}

/**
 * Builds the model choice that replaces the typed model name (ADR-0066). The ids are
 * untrusted provider text: they are shown as text only, and choosing one grants nothing,
 * since `chat_send` validates the model as it validates any other.
 */
export function createModelPicker(onChange: () => void): ModelPicker {
  const select = el("select", { attrs: { "aria-label": "Model" } });
  const refreshButton = button("Refresh", () => void load(endpoint), {
    attrs: { "aria-label": "Refresh the model list" },
  });
  const note = el("span", { class: "muted model-note", attrs: { role: "status" } });

  let models: ModelChoice[] = [];
  let chosen = "";
  let endpoint = "";
  let loading = false;
  let failure: string | null = null;
  let disabled = false;
  /** Counts `load` calls, so only the latest one's answer is used. */
  let generation = 0;

  function noteText(): string {
    if (loading) {
      return LISTING_NOTE;
    }
    if (failure !== null) {
      return failure;
    }
    if (models.find((model) => model.id === chosen)?.cloud_tag === true) {
      return CLOUD_WARNING;
    }
    return models.length === 0 && endpoint !== "" ? EMPTY_NOTE : "";
  }

  function render(): void {
    const options = [option("", models.length === 0 ? "No model listed" : "Choose a model")];
    if (chosen !== "" && !models.some((model) => model.id === chosen)) {
      options.push(option(chosen, `${chosen}${UNLISTED_SUFFIX}`));
    }
    for (const model of models) {
      options.push(option(model.id, model.cloud_tag ? `${model.id}${CLOUD_SUFFIX}` : model.id));
    }
    select.replaceChildren(...options);
    select.value = chosen;
    select.disabled = disabled;
    refreshButton.disabled = disabled || loading || endpoint === "";
    replaceChildren(note, noteText());
    note.classList.toggle("warning", failure === null && noteText() === CLOUD_WARNING);
  }

  select.addEventListener("change", () => {
    chosen = select.value;
    render();
    onChange();
  });

  async function load(forEndpoint: string): Promise<void> {
    endpoint = forEndpoint;
    generation += 1;
    const mine = generation;
    loading = true;
    failure = null;
    render();
    let listed: ModelChoice[] = [];
    let problem: string | null = null;
    try {
      const answer = await api.chatModels();
      if (answer.endpoint === forEndpoint) {
        listed = answer.models;
      } else {
        problem = CHANGED_NOTE;
      }
    } catch (reason) {
      const error = toIpcError(reason);
      problem = `Could not list models: ${error.message} (${error.code})`;
    }
    if (mine !== generation) {
      return;
    }
    models = listed;
    failure = problem;
    loading = false;
    render();
    onChange();
  }

  const element = el(
    "div",
    { class: "field model-field" },
    el("span", { text: "Model" }),
    el("div", { class: "model-row" }, select, refreshButton),
    note,
  );
  render();

  return {
    element,
    value: () => chosen.trim(),
    setValue(id) {
      chosen = id;
      render();
    },
    load,
    setDisabled(next) {
      // The composer calls this on every keystroke; rebuilding the options then
      // would close an open list, so only a change re-renders.
      if (next !== disabled) {
        disabled = next;
        render();
      }
    },
  };
}
