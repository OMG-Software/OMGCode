import { button, el, replaceChildren } from "../dom";
import { utf8Length } from "../format";
import { api } from "../ipc/client";
import { toIpcError } from "../ipc/errors";
import type { ChatDefaults, ChatPreferences, ChatSettings } from "../ipc/types";
import { type ComposerState, MESSAGE_MAX_BYTES, messageProblem, settingsValid } from "./model";

/** The most skills one turn may load; the core enforces the same bound (ADR-0057). */
export const SKILLS_PER_TURN = 4;

export interface ComposerHandlers {
  /** `skills` are the names selected for this turn only. */
  onSend(message: string, settings: ChatSettings, skills: string[]): void;
  onCancel(): void;
}

export interface Composer {
  element: HTMLElement;
  /** `running` shows the Running state and Cancel button. */
  update(state: ComposerState, running: boolean): void;
  /** Clears the draft and the skill selection after a successful send. */
  clear(): void;
  readSettings(): ChatSettings;
  /** Offers these skill names for the next turn; an empty list hides the choice. */
  setSkills(names: readonly string[]): void;
}

function labelled(label: string, input: HTMLElement): HTMLElement {
  return el("label", { class: "field" }, el("span", { text: label }), input);
}

function numberInput(label: string, value: number, min: number): [HTMLElement, HTMLInputElement] {
  const input = el("input", { attrs: { type: "number", min: String(min), step: "1" } });
  input.value = String(value);
  return [labelled(label, input), input];
}

/**
 * Builds the composer. Model settings are kept in this closure for the app run and are
 * filled from `chat_defaults` the first time they are available.
 */
export function createComposer(handlers: ComposerHandlers): Composer {
  const box = el("textarea", {
    class: "message-box",
    attrs: { rows: "3", "aria-label": "Message", placeholder: "Message (Ctrl+Enter to send)" },
  });
  const counter = el("span", { class: "counter" });
  const reason = el("p", { class: "note composer-note" });
  const problem = el("p", { class: "error composer-error" });
  const sendButton = button("Send", submit, { class: "primary" });
  const cancelButton = button("Cancel", handlers.onCancel);
  cancelButton.hidden = true;
  const runningLabel = el("span", { class: "running", text: "Running…" });
  runningLabel.hidden = true;

  const endpointValue = el("span", { class: "fixed-value" });
  const model = el("input", { attrs: { type: "text", autocomplete: "off" } });
  const [contextField, contextTokens] = numberInput("Context tokens", 0, 0);
  const [outputField, maxOutput] = numberInput("Max output tokens", 0, 0);
  const [compactField, compactThreshold] = numberInput("Compaction threshold", 0, 0);
  const autoCompact = el("input", { attrs: { type: "checkbox" } });
  const instructions = el("textarea", { attrs: { rows: "3" } });
  const preferencesNote = el("span", { class: "muted", attrs: { role: "status" } });
  const saveDefaultsButton = button("Save as defaults", () => void saveDefaults());

  const settingsPanel = el(
    "details",
    { class: "settings" },
    el("summary", { text: "Model settings" }),
    el(
      "div",
      { class: "settings-grid" },
      el(
        "div",
        { class: "field" },
        el("span", { text: "Endpoint" }),
        endpointValue,
        el("span", { class: "muted", text: "Set when OMGCode starts." }),
      ),
      labelled("Model", model),
      contextField,
      outputField,
      compactField,
      el(
        "label",
        { class: "field field-check" },
        autoCompact,
        el("span", { text: "Auto-compact" }),
      ),
      labelled("Custom instructions", instructions),
      el("div", { class: "field preferences-row" }, saveDefaultsButton, preferencesNote),
    ),
  );

  const skillsList = el("div", { class: "skill-choices" });
  const skillsPanel = el(
    "fieldset",
    { class: "skills" },
    el("legend", { text: `Skills for this turn (at most ${SKILLS_PER_TURN})` }),
    skillsList,
  );
  skillsPanel.hidden = true;
  let skillBoxes: Array<[string, HTMLInputElement]> = [];

  let state: ComposerState = { visible: false, enabled: false, reason: null };
  let running = false;
  let defaultsError: string | null = null;
  /** Loaded once from `chat_defaults`; the operator cannot change it. */
  let fixedEndpoint = "";

  function readSettings(): ChatSettings {
    return {
      endpoint: fixedEndpoint,
      model: model.value.trim(),
      context_tokens: Number(contextTokens.value),
      max_output_tokens: Number(maxOutput.value),
      instructions: instructions.value,
      compact_threshold: Number(compactThreshold.value),
      auto_compact: autoCompact.checked,
    };
  }

  function selectedSkills(): string[] {
    return skillBoxes.filter(([, box]) => box.checked).map(([name]) => name);
  }

  function setSkills(names: readonly string[]): void {
    skillBoxes = names.map((name) => {
      const box = el("input", { attrs: { type: "checkbox" } });
      box.addEventListener("change", refresh);
      return [name, box];
    });
    replaceChildren(
      skillsList,
      ...skillBoxes.map(([name, box]) =>
        el("label", { class: "field-check skill-choice" }, box, el("span", { text: name })),
      ),
    );
    skillsPanel.hidden = skillBoxes.length === 0;
    refresh();
  }

  function preferencesOf(settings: ChatSettings): ChatPreferences {
    return {
      model: settings.model,
      context_tokens: settings.context_tokens,
      max_output_tokens: settings.max_output_tokens,
      instructions: settings.instructions,
      compact_threshold: settings.compact_threshold,
      auto_compact: settings.auto_compact,
    };
  }

  function applyDefaults(defaults: ChatDefaults): void {
    writeSettings(defaults.settings);
    replaceChildren(
      preferencesNote,
      defaults.preferences === "unreadable"
        ? "Saved defaults could not be read; built-in defaults are shown."
        : "",
    );
  }

  async function saveDefaults(): Promise<void> {
    saveDefaultsButton.disabled = true;
    try {
      // Only the note changes: the inputs keep any edit made while the save
      // was in flight, so the returned copy is not written back.
      await api.chatPreferencesSave(preferencesOf(readSettings()));
      replaceChildren(preferencesNote, "Saved as defaults.");
    } catch (reason) {
      const error = toIpcError(reason);
      replaceChildren(preferencesNote, `${error.message} (${error.code})`);
    } finally {
      saveDefaultsButton.disabled = false;
      refresh();
    }
  }

  function writeSettings(settings: ChatSettings): void {
    fixedEndpoint = settings.endpoint;
    replaceChildren(endpointValue, settings.endpoint);
    model.value = settings.model;
    contextTokens.value = String(settings.context_tokens);
    maxOutput.value = String(settings.max_output_tokens);
    compactThreshold.value = String(settings.compact_threshold);
    autoCompact.checked = settings.auto_compact;
    instructions.value = settings.instructions;
  }

  function currentProblem(): string | null {
    if (!settingsValid(readSettings())) {
      return "Set a model and a positive context size in Model settings.";
    }
    return messageProblem(box.value);
  }

  function refresh(): void {
    const bytes = utf8Length(box.value);
    counter.textContent = `${bytes} / ${MESSAGE_MAX_BYTES} bytes`;
    counter.classList.toggle("over", bytes > MESSAGE_MAX_BYTES);
    const issue = currentProblem();
    element.hidden = !state.visible;
    box.disabled = !state.enabled;
    sendButton.disabled = !state.enabled || issue !== null;
    sendButton.hidden = running;
    cancelButton.hidden = !running;
    runningLabel.hidden = !running;
    replaceChildren(reason, state.reason ?? "");
    reason.hidden = state.reason === null;
    replaceChildren(problem, defaultsError ?? "");
    problem.hidden = defaultsError === null;
    for (const input of [
      model,
      contextTokens,
      maxOutput,
      compactThreshold,
      autoCompact,
      instructions,
    ]) {
      input.disabled = !state.enabled;
    }
    const full = selectedSkills().length >= SKILLS_PER_TURN;
    for (const [, skillBox] of skillBoxes) {
      skillBox.disabled = !state.enabled || (full && !skillBox.checked);
    }
  }

  function submit(): void {
    const issue = currentProblem();
    if (!state.enabled || issue !== null) {
      replaceChildren(problem, issue ?? "");
      problem.hidden = issue === null;
      return;
    }
    handlers.onSend(box.value, readSettings(), selectedSkills());
  }

  box.addEventListener("input", refresh);
  for (const input of [model, contextTokens]) {
    input.addEventListener("input", refresh);
  }
  box.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
      event.preventDefault();
      submit();
    }
  });

  const element = el(
    "section",
    { class: "composer", attrs: { "aria-label": "Compose a message" } },
    settingsPanel,
    skillsPanel,
    box,
    el("div", { class: "composer-row" }, counter, runningLabel, cancelButton, sendButton),
    reason,
    problem,
  );
  element.hidden = true;

  void api
    .chatDefaults()
    .then((defaults) => {
      applyDefaults(defaults);
      refresh();
    })
    .catch((failure: unknown) => {
      defaultsError = toIpcError(failure).message;
      refresh();
    });

  return {
    element,
    update(next, isRunning) {
      state = next;
      running = isRunning;
      refresh();
    },
    clear() {
      box.value = "";
      for (const [, skillBox] of skillBoxes) {
        skillBox.checked = false;
      }
      refresh();
    },
    readSettings,
    setSkills,
  };
}
