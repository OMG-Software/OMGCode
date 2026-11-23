import { button, el, replaceChildren } from "../dom";
import { utf8Length } from "../format";
import { api } from "../ipc/client";
import { toIpcError } from "../ipc/errors";
import type { ChatDefaults, ChatPreferences, ChatSettings } from "../ipc/types";
import { createPopover } from "../ui/popover";
import type { ConnectionChange, ConnectionModel } from "./connection_state";
import {
  type ComposerState,
  isSendKey,
  MESSAGE_MAX_BYTES,
  messageProblem,
  settingsValid,
} from "./model";
import { createModelPicker } from "./model_picker";
import { createProviderSelect } from "./provider_select";

/** The most skills one turn may load; the core enforces the same bound (ADR-0057). */
export const SKILLS_PER_TURN = 4;

/** Message box hints for each "Enter to send" choice. */
export const PLACEHOLDER_ENTER = "Message (Enter to send, Shift+Enter for a new line)";
export const PLACEHOLDER_CTRL_ENTER = "Message (Ctrl+Enter to send)";
/** Shown while Rust reports that the saved per-session provider and model choices are unreadable. */
export const SESSION_CHOICES_UNREADABLE =
  "Saved provider and model choices could not be read, so this session follows the default " +
  "connection. Choosing a provider or model replaces the damaged file and every other session's " +
  "saved choice.";

/** The counter appears once the message uses more than this share of the byte limit. */
export const COUNTER_SHOW_RATIO = 0.8;

export interface ComposerHandlers {
  /** `skills` are the names selected for this turn only. */
  onSend(message: string, settings: ChatSettings, skills: string[]): void;
  onCancel(): void;
}

export interface Composer {
  element: HTMLElement;
  /** `running` shows the Running state and Cancel button. */
  update(state: ComposerState, running: boolean): void;
  /** Names the open session for the Workspace settings button; `null` disables it. */
  setSession(id: string | null): void;
  /** Clears the draft and the skill selection after a successful send. */
  clear(): void;
  readSettings(): ChatSettings;
  /** Offers these skill names for the next turn; an empty list hides the choice. */
  setSkills(names: readonly string[]): void;
  /** Removes the listeners the two menus put on the document. */
  dispose(): void;
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
 * Builds the composer. The numeric chat settings are kept in this closure for the app run and are
 * filled from `chat_defaults` the first time they are available. The provider, the endpoint, and
 * the model belong to the open session (ADR-0071): they come from `connection`, which the footer's
 * provider dropdown shares.
 */
export function createComposer(handlers: ComposerHandlers, connection: ConnectionModel): Composer {
  const box = el("textarea", {
    class: "message-box",
    attrs: { rows: "3", "aria-label": "Message", placeholder: PLACEHOLDER_ENTER },
  });
  const counter = el("span", { class: "counter" });
  counter.hidden = true;
  const reason = el("p", { class: "note composer-note" });
  const problem = el("p", { class: "error composer-error" });
  const sendButton = button("Send", submit, { class: "primary" });
  const cancelButton = button("Cancel", handlers.onCancel);
  cancelButton.hidden = true;
  const runningLabel = el("span", { class: "running", text: "Running…" });
  runningLabel.hidden = true;

  // The open session, for the Workspace settings button (ADR-0071). Rust resolves its workspace.
  let sessionId: string | null = null;
  let workspaceError: string | null = null;
  /** Counts Workspace settings requests; a failure is shown only for the latest, in its session. */
  let workspaceRequest = 0;
  const workspaceButton = button("Workspace settings", () => {
    const id = sessionId;
    if (id === null) {
      return;
    }
    workspaceError = null;
    workspaceRequest += 1;
    const mine = workspaceRequest;
    api.settingsShowWorkspace(id).catch((failure: unknown) => {
      if (mine !== workspaceRequest || sessionId !== id) {
        return;
      }
      workspaceError = toIpcError(failure).message;
      refresh();
    });
    refresh();
  });

  const endpointValue = el("span", { class: "fixed-value" });
  const providerNote = el("span", { class: "muted", attrs: { role: "status" } });
  providerNote.hidden = true;
  // Rust could not read the saved per-session choices; it follows the global choice meanwhile.
  const choicesNote = el("span", {
    class: "model-warning",
    text: SESSION_CHOICES_UNREADABLE,
    attrs: { role: "status" },
  });
  choicesNote.hidden = true;
  const model = createModelPicker(
    () => refresh(),
    (id) => void connection.chooseModel(id),
  );
  const providerSelect = createProviderSelect(connection, "Provider");
  /** What the model list was last requested for, so only a real change reloads it. */
  let loadedKey: string | null = null;
  const [contextField, contextTokens] = numberInput("Context tokens", 0, 0);
  const [outputField, maxOutput] = numberInput("Max output tokens", 0, 0);
  const [toolCallsField, toolCalls] = numberInput("Tool calls per turn", 128, 1);
  toolCalls.max = "256";
  toolCalls.required = true;
  const [timeoutField, turnTimeout] = numberInput("Tool-enabled turn timeout (seconds)", 60, 1);
  turnTimeout.max = "3600";
  turnTimeout.required = true;
  const [compactField, compactThreshold] = numberInput("Compaction threshold", 0, 0);
  const autoCompact = el("input", { attrs: { type: "checkbox" } });
  const instructions = el("textarea", { attrs: { rows: "3" } });
  // On until `chat_defaults` says otherwise, so it is on even when they cannot be read.
  const enterToSend = el("input", { attrs: { type: "checkbox" } });
  enterToSend.checked = true;
  /** Set once the operator changes the check box, so defaults that arrive later do not undo it. */
  let enterToSendChanged = false;
  const preferencesNote = el("span", { class: "muted", attrs: { role: "status" } });
  const saveDefaultsButton = button("Save as defaults", () => void saveDefaults());

  const settingsMenu = createPopover(
    "Chat settings",
    { class: "open-up settings", label: "Chat settings" },
    el(
      "div",
      { class: "settings-grid" },
      el(
        "div",
        { class: "field" },
        el("span", { text: "Endpoint" }),
        endpointValue,
        el("span", {
          class: "muted",
          text: "Follows the session's provider, or is fixed when OMGCode starts.",
        }),
      ),
      contextField,
      outputField,
      toolCallsField,
      timeoutField,
      compactField,
      el(
        "label",
        { class: "field field-check" },
        autoCompact,
        el("span", { text: "Auto-compact" }),
      ),
      labelled("Custom instructions", instructions),
      el(
        "label",
        { class: "field field-check" },
        enterToSend,
        el("span", { text: "Enter to send" }),
      ),
      el("div", { class: "field preferences-row" }, saveDefaultsButton, preferencesNote),
    ),
  );
  settingsMenu.panel.classList.add("composer-panel");

  const skillsList = el("div", { class: "skill-choices" });
  const skillsMenu = createPopover(
    "Skills (0)",
    { class: "open-up skills", label: "Skills for this turn" },
    el("p", { class: "popover-title", text: `Skills for this turn (at most ${SKILLS_PER_TURN})` }),
    skillsList,
  );
  skillsMenu.panel.classList.add("composer-panel");
  skillsMenu.element.hidden = true;
  let skillBoxes: Array<[string, HTMLInputElement]> = [];

  let state: ComposerState = { visible: false, enabled: false, reason: null };
  let running = false;
  let defaultsError: string | null = null;
  /** The open session's endpoint from the connection, or the launch default until it arrives. */
  let fixedEndpoint = "";

  function readSettings(): ChatSettings {
    return {
      endpoint: fixedEndpoint,
      model: model.value(),
      context_tokens: Number(contextTokens.value),
      max_output_tokens: Number(maxOutput.value),
      max_tool_calls: Number(toolCalls.value),
      turn_timeout_seconds: Number(turnTimeout.value),
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
    skillsMenu.element.hidden = skillBoxes.length === 0;
    if (skillBoxes.length === 0) {
      skillsMenu.close();
    }
    refresh();
  }

  function preferencesOf(settings: ChatSettings): ChatPreferences {
    return {
      model: settings.model,
      context_tokens: settings.context_tokens,
      max_output_tokens: settings.max_output_tokens,
      max_tool_calls: settings.max_tool_calls,
      turn_timeout_seconds: settings.turn_timeout_seconds,
      instructions: settings.instructions,
      compact_threshold: settings.compact_threshold,
      auto_compact: settings.auto_compact,
      enter_to_send: enterToSend.checked,
    };
  }

  function applyDefaults(defaults: ChatDefaults): void {
    writeSettings(defaults.settings);
    if (!enterToSendChanged) {
      enterToSend.checked = defaults.enter_to_send;
      showSendKey();
    }
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

  /** Fills the numeric settings. The endpoint and model come from the session's connection. */
  function writeSettings(settings: ChatSettings): void {
    if (connection.status() === null) {
      fixedEndpoint = settings.endpoint;
      replaceChildren(endpointValue, settings.endpoint);
    }
    contextTokens.value = String(settings.context_tokens);
    maxOutput.value = String(settings.max_output_tokens);
    toolCalls.value = String(settings.max_tool_calls);
    turnTimeout.value = String(settings.turn_timeout_seconds);
    compactThreshold.value = String(settings.compact_threshold);
    autoCompact.checked = settings.auto_compact;
    instructions.value = settings.instructions;
  }

  function currentProblem(): string | null {
    const status = connection.status();
    if (status === null) {
      return "The session's provider is being read.";
    }
    if (status.stale) {
      return "The provider chosen for this session changed. Choose the provider again.";
    }
    if (!settingsValid(readSettings())) {
      return "Choose a model and set a positive context size in Chat settings.";
    }
    if (!toolCalls.validity.valid) {
      return `Set Tool calls per turn to a whole number from ${toolCalls.min} to ${toolCalls.max}.`;
    }
    if (!turnTimeout.validity.valid) {
      return `Set Tool-enabled turn timeout to a whole number from ${turnTimeout.min} to ${turnTimeout.max} seconds.`;
    }
    return messageProblem(box.value);
  }

  function refresh(): void {
    const bytes = utf8Length(box.value);
    counter.textContent = `${bytes} / ${MESSAGE_MAX_BYTES} bytes`;
    counter.classList.toggle("over", bytes > MESSAGE_MAX_BYTES);
    // Only near the limit, or over it, so a short message shows no number.
    counter.hidden = bytes <= MESSAGE_MAX_BYTES * COUNTER_SHOW_RATIO;
    replaceChildren(skillsMenu.summary, `Skills (${selectedSkills().length})`);
    const issue = currentProblem();
    element.hidden = !state.visible;
    box.disabled = !state.enabled;
    sendButton.disabled = !state.enabled || issue !== null;
    sendButton.hidden = running;
    cancelButton.hidden = !running;
    runningLabel.hidden = !running;
    replaceChildren(reason, state.reason ?? "");
    reason.hidden = state.reason === null;
    const shown = defaultsError ?? workspaceError;
    replaceChildren(problem, shown ?? "");
    problem.hidden = shown === null;
    workspaceButton.disabled = sessionId === null;
    model.setDisabled(!state.enabled || connection.busy());
    providerSelect.setLocked(!state.enabled);
    replaceChildren(providerNote, connection.message());
    providerNote.hidden = connection.message() === "";
    choicesNote.hidden = connection.status()?.session_choices_unreadable !== true;
    for (const input of [
      contextTokens,
      maxOutput,
      toolCalls,
      turnTimeout,
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

  /**
   * Follows the shared connection (ADR-0071 decision 11). A different session, a confirmed
   * provider choice, or a changed connection clears the model and lists the new session's models;
   * the model shown is always the one the connection holds for the session.
   */
  function onConnection(change: ConnectionChange): void {
    providerSelect.render();
    const status = connection.status();
    const id = connection.sessionId();
    if (change === "opening") {
      loadedKey = null;
      model.reset();
      model.setHosted(false);
    } else if (status !== null && id !== null && change !== "busy" && change !== "message") {
      fixedEndpoint = status.connection.endpoint;
      replaceChildren(endpointValue, fixedEndpoint);
      model.setHosted(status.connection.kind === "ollama_cloud");
      const key = [
        id,
        status.connection.profile ?? "",
        status.connection.endpoint,
        status.origin,
        String(status.stale),
      ].join("|");
      if (change === "selected" || key !== loadedKey) {
        loadedKey = key;
        model.reset();
        model.setValue(status.model);
        void model.load(id);
      } else {
        model.setValue(status.model);
      }
    }
    refresh();
  }
  const unsubscribe = connection.subscribe(onConnection);
  providerSelect.render();

  box.addEventListener("input", refresh);
  contextTokens.addEventListener("input", refresh);
  toolCalls.addEventListener("input", refresh);
  turnTimeout.addEventListener("input", refresh);
  /** Shows the send key in the message box hint. */
  function showSendKey(): void {
    box.placeholder = enterToSend.checked ? PLACEHOLDER_ENTER : PLACEHOLDER_CTRL_ENTER;
  }

  enterToSend.addEventListener("change", () => {
    enterToSendChanged = true;
    showSendKey();
  });
  box.addEventListener("keydown", (event) => {
    // A send key never inserts a newline, even when the message cannot be sent yet;
    // `submit` then shows why.
    if (isSendKey(event, enterToSend.checked)) {
      event.preventDefault();
      submit();
    }
  });

  const element = el(
    "section",
    { class: "composer", attrs: { "aria-label": "Compose a message" } },
    box,
    el(
      "div",
      { class: "composer-row" },
      el(
        "div",
        { class: "composer-tools" },
        skillsMenu.element,
        el(
          "label",
          { class: "toolbar-field" },
          el("span", { text: "Provider" }),
          providerSelect.element,
        ),
        el("div", { class: "toolbar-field" }, el("span", { text: "Model" }), model.element),
        settingsMenu.element,
        workspaceButton,
      ),
      el("div", { class: "composer-send" }, counter, runningLabel, cancelButton, sendButton),
    ),
    el(
      "div",
      { class: "composer-model-status" },
      model.note,
      model.warning,
      providerNote,
      choicesNote,
    ),
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
    setSession(id) {
      if (id !== sessionId) {
        workspaceError = null;
        // A request for the session being left must not show its failure here.
        workspaceRequest += 1;
      }
      sessionId = id;
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
    dispose() {
      unsubscribe();
      settingsMenu.dispose();
      skillsMenu.dispose();
    },
  };
}
