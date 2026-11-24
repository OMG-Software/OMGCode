import { button, el, replaceChildren } from "../dom";
import { api } from "../ipc/client";
import { toIpcError } from "../ipc/errors";
import type { ModelChoice } from "../ipc/types";

/** Added to a model whose id carries Ollama's cloud tag (ADR-0066). */
export const CLOUD_SUFFIX = " (Ollama cloud)";
/** Shown while a cloud-tagged model is chosen. */
export const CLOUD_WARNING =
  "This Ollama model runs on Ollama's hosted service: prompts and workspace text sent to it leave this computer.";
/** Added to every model of a hosted connection (ADR-0071, ADR-0092). */
export const HOSTED_SUFFIX = " (hosted)";
/** Shown while the session's connection is a hosted service. */
export const HOSTED_WARNING =
  "This connection is a hosted service: prompts, workspace text read by tools, and command output sent to it leave this computer.";
/** Added to a chosen model the server did not report, such as a saved default. */
export const UNLISTED_SUFFIX = " (not reported by the server)";
export const LISTING_NOTE = "Listing models…";
export const EMPTY_NOTE = "The server reported no models.";

export interface ModelPicker {
  /** The model choice and its Refresh button, for the model box. */
  element: HTMLElement;
  /** Why the list could not be read, or that it is being read. */
  note: HTMLElement;
  /** The off-machine warning, shown while a hosted or cloud-tagged model is chosen. */
  warning: HTMLElement;
  /** The chosen model id, or an empty string when none is chosen. */
  value(): string;
  /** Chooses `id`. A model the server did not report stays offered and is marked. */
  setValue(id: string): void;
  /** Forgets the list, the chosen model, and any request in flight. */
  reset(): void;
  /**
   * Lists the models of `sessionId`'s connection. The answer is kept only if it belongs to that
   * session and no later `load` or `reset` happened; the endpoint it names is never compared.
   * Never rejects: a failure is shown in the note.
   */
  load(sessionId: string): Promise<void>;
  /** Whether the connection is a hosted service, so every model runs off this computer. */
  setHosted(hosted: boolean): void;
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
 * Builds the model choice of the model box (ADR-0066, ADR-0071, ADR-0089). The ids are untrusted
 * provider text: they are shown as text only, and choosing one grants nothing, since `chat_send`
 * validates the model as it validates any other. The list belongs to a session and a request, so
 * it is keyed on the session id and a request counter, never on endpoint text.
 */
export function createModelPicker(
  onChange: () => void,
  onChoose: (id: string) => void,
): ModelPicker {
  const select = el("select", { attrs: { "aria-label": "Model" } });
  const refreshButton = button(
    "Refresh",
    () => {
      if (session !== null) {
        void load(session);
      }
    },
    {
      attrs: { "aria-label": "Refresh the model list" },
    },
  );
  const note = el("span", { class: "muted model-note", attrs: { role: "status" } });
  // Separate from the list status, so loading or a failure never hides it.
  const warning = el("span", { class: "model-warning", attrs: { role: "status" } });

  let models: ModelChoice[] = [];
  let chosen = "";
  /** The session whose list is shown or being requested; `null` after `reset`. */
  let session: string | null = null;
  let loading = false;
  let failure: string | null = null;
  let disabled = false;
  let hosted = false;
  /** Counts `load` and `reset` calls, so only the latest request's answer is used. */
  let generation = 0;

  function noteText(): string {
    if (loading) {
      return LISTING_NOTE;
    }
    if (failure !== null) {
      return failure;
    }
    return models.length === 0 && session !== null ? EMPTY_NOTE : "";
  }

  function isCloud(id: string, tagged: boolean): boolean {
    return hosted || tagged || hasCloudTag(id);
  }

  function suffix(id: string, tagged: boolean): string {
    if (hosted) {
      return HOSTED_SUFFIX;
    }
    return isCloud(id, tagged) ? CLOUD_SUFFIX : "";
  }

  /** The chosen model runs off this computer: hosted connection, cloud tag, or server list. */
  function chosenIsCloud(): boolean {
    if (chosen === "") {
      return false;
    }
    return isCloud(chosen, models.find((model) => model.id === chosen)?.cloud_tag === true);
  }

  function render(): void {
    const options = [option("", models.length === 0 ? "No model listed" : "Choose a model")];
    if (chosen !== "" && !models.some((model) => model.id === chosen)) {
      options.push(option(chosen, `${chosen}${suffix(chosen, false)}${UNLISTED_SUFFIX}`));
    }
    for (const model of models) {
      options.push(option(model.id, `${model.id}${suffix(model.id, model.cloud_tag)}`));
    }
    select.replaceChildren(...options);
    select.value = chosen;
    // Empty until the list arrives, so a model of another server is never offered.
    select.disabled = disabled || loading || session === null;
    refreshButton.disabled = disabled || loading || session === null;
    replaceChildren(note, noteText());
    const cloud = chosenIsCloud();
    replaceChildren(warning, cloud ? (hosted ? HOSTED_WARNING : CLOUD_WARNING) : "");
    warning.hidden = !cloud;
  }

  select.addEventListener("change", () => {
    chosen = select.value;
    render();
    // Only the operator's own choice is recorded for the session, never a list that arrived.
    onChoose(chosen);
    onChange();
  });

  async function load(forSession: string): Promise<void> {
    if (forSession !== session) {
      // Another session's models must not be offered while this list loads.
      models = [];
    }
    session = forSession;
    generation += 1;
    const mine = generation;
    loading = true;
    failure = null;
    render();
    let listed: ModelChoice[] = [];
    let problem: string | null = null;
    try {
      const answer = await api.chatModels(forSession);
      if (answer.session_id === forSession) {
        listed = answer.models;
      } else {
        problem = "The model list belongs to another session.";
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

  const element = el("div", { class: "model-row" }, select, refreshButton);
  render();

  return {
    element,
    note,
    warning,
    // The exact id: a listed id is sent as the server reported it, never trimmed.
    value: () => chosen,
    setValue(id) {
      chosen = id;
      render();
    },
    reset() {
      generation += 1;
      models = [];
      chosen = "";
      session = null;
      loading = false;
      failure = null;
      render();
    },
    load,
    setHosted(next) {
      if (next !== hosted) {
        hosted = next;
        render();
      }
    },
    setDisabled(next) {
      // The model box calls this on every composer refresh; rebuilding the options then
      // would close an open list, so only a change re-renders.
      if (next !== disabled) {
        disabled = next;
        render();
      }
    },
  };
}
