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

/**
 * Whether `id` carries Ollama's cloud tag: the tag after the last `:` is `cloud` or ends
 * in `-cloud`. The same rule as core's, applied to the chosen id itself, so a saved model
 * is warned about before a list arrives or when listing fails.
 */
export function hasCloudTag(id: string): boolean {
  const colon = id.lastIndexOf(":");
  if (colon < 0) {
    return false;
  }
  const tag = id.slice(colon + 1);
  return tag === "cloud" || tag.endsWith("-cloud");
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
  // Separate from the list status, so loading or a failure never hides it.
  const warning = el("span", { class: "model-warning", attrs: { role: "status" } });

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
    return models.length === 0 && endpoint !== "" ? EMPTY_NOTE : "";
  }

  /** The chosen model is cloud-tagged by its own id or by the server's list. */
  function chosenIsCloud(): boolean {
    return hasCloudTag(chosen) || models.find((model) => model.id === chosen)?.cloud_tag === true;
  }

  function render(): void {
    const options = [option("", models.length === 0 ? "No model listed" : "Choose a model")];
    if (chosen !== "" && !models.some((model) => model.id === chosen)) {
      const cloud = hasCloudTag(chosen) ? CLOUD_SUFFIX : "";
      options.push(option(chosen, `${chosen}${cloud}${UNLISTED_SUFFIX}`));
    }
    for (const model of models) {
      const cloud = model.cloud_tag || hasCloudTag(model.id);
      options.push(option(model.id, cloud ? `${model.id}${CLOUD_SUFFIX}` : model.id));
    }
    select.replaceChildren(...options);
    select.value = chosen;
    select.disabled = disabled;
    refreshButton.disabled = disabled || loading || endpoint === "";
    replaceChildren(note, noteText());
    const cloud = chosenIsCloud();
    replaceChildren(warning, cloud ? CLOUD_WARNING : "");
    warning.hidden = !cloud;
  }

  select.addEventListener("change", () => {
    chosen = select.value;
    render();
    onChange();
  });

  async function load(forEndpoint: string): Promise<void> {
    if (forEndpoint !== endpoint) {
      // Another server's models must not be offered while the new list loads; the chosen
      // model stays, marked as not reported, until the operator changes it.
      models = [];
    }
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
    warning,
  );
  render();

  return {
    element,
    // The exact id: a listed id is sent as the server reported it, never trimmed.
    value: () => chosen,
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
