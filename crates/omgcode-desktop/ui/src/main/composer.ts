import { button, el, replaceChildren } from "../dom";
import { utf8Length } from "../format";
import { api } from "../ipc/client";
import { toIpcError } from "../ipc/errors";
import type { ChatDefaults, ChatSettings, ComposerPreferences } from "../ipc/types";
import { iconButton } from "../ui/icons";
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
/** Shown while no context size is saved; the settings window owns it (ADR-0080). */
/** Opens the message shown when the saved chat defaults cannot be read. */
export const DEFAULTS_UNREADABLE = "The chat defaults could not be read; Send tries again.";
export const SET_CONTEXT_IN_SETTINGS =
  "Set the context size in Settings, under Model, then send again.";
/** The tooltip and accessible name of the workspace settings icon button. */
export const WORKSPACE_SETTINGS_LABEL = "Workspace settings";

/** The counter appears once the message uses more than this share of the byte limit. */
export const COUNTER_SHOW_RATIO = 0.8;

/**
 * The compaction slider's range and step until Rust's defaults arrive (ADR-0080). Rust refuses a
 * saved value outside its own range, which it reports in `chat_defaults`.
 */
export const COMPACT_THRESHOLD_MIN = 128_000;
export const COMPACT_THRESHOLD_MAX = 1_000_000;
export const COMPACT_THRESHOLD_STEP = 8_000;

/** A token count as the slider shows it: "200K", "1M". */
export function tokenLabel(tokens: number): string {
  if (tokens >= 1_000_000 && tokens % 1_000_000 === 0) {
    return `${tokens / 1_000_000}M`;
  }
  return `${Math.round(tokens / 1000)}K`;
}

/** `value` moved into `[min, max]` and onto the slider's step from `min`. */
export function clampThreshold(value: number, min: number, max: number): number {
  if (!Number.isFinite(value)) {
    return min;
  }
  const stepped = min + Math.round((value - min) / COMPACT_THRESHOLD_STEP) * COMPACT_THRESHOLD_STEP;
  return Math.min(max, Math.max(min, stepped));
}

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
  /**
   * Removes what a successful send sent: the message, when the draft still starts with it, and
   * the sent skills' selection. Text typed and skills ticked or unticked after the click are kept,
   * since the send waits for the saved defaults and then for the turn.
   */
  clearSent(message: string, skills: readonly string[]): void;
  readSettings(): ChatSettings;
  /** Offers these skill names for the next turn; an empty list hides the choice. */
  setSkills(names: readonly string[]): void;
  /** Removes the listeners the menus and the window focus put on the document. */
  dispose(): void;
}

/** The saved chat defaults the settings window owns; the composer only reads them (ADR-0080). */
interface Budgets {
  context_tokens: number;
  max_output_tokens: number;
  max_tool_calls: number;
  turn_timeout_seconds: number;
  instructions: string;
}

/**
 * Builds the composer. The budgets and the global instructions are saved in the settings window
 * (ADR-0080); the composer reads them from `chat_defaults` when it starts, when the window gains
 * focus, and again just before each send, so a turn uses what was saved last. The compaction
 * settings and the send key are the composer's own. The provider, the endpoint, and the model
 * belong to the open session (ADR-0071): they come from `connection`, which the footer's provider
 * dropdown shares.
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
  const workspaceButton = iconButton(
    "folder",
    WORKSPACE_SETTINGS_LABEL,
    () => {
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
    },
    "icon-button workspace-settings",
  );

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

  /** The saved budgets, read from `chat_defaults`; `null` until the first read succeeds. */
  let budgets: Budgets | null = null;
  const autoCompact = el("input", { attrs: { type: "checkbox" } });
  const threshold = el("input", {
    attrs: {
      type: "range",
      min: String(COMPACT_THRESHOLD_MIN),
      max: String(COMPACT_THRESHOLD_MAX),
      step: String(COMPACT_THRESHOLD_STEP),
      "aria-label": "Compaction threshold",
    },
  });
  threshold.value = String(COMPACT_THRESHOLD_MIN);
  const thresholdValue = el("output", { class: "slider-value" });
  const composerInputs: Array<HTMLInputElement> = [autoCompact, threshold];
  const editedSettings = new Set<HTMLInputElement>();
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
        { class: "field field-wide" },
        el("span", { text: "Endpoint" }),
        endpointValue,
        el("span", {
          class: "muted",
          text: "Follows the session's provider, or is fixed when OMGCode starts.",
        }),
      ),
      el(
        "label",
        { class: "field field-check field-wide" },
        autoCompact,
        el("span", { text: "Auto-compact" }),
      ),
      el(
        "div",
        { class: "field field-wide" },
        el(
          "span",
          { class: "slider-label" },
          el("span", { text: "Compact when the conversation reaches" }),
          thresholdValue,
        ),
        threshold,
        el("span", {
          class: "muted",
          text: "Or earlier, when the model's context is smaller. Budgets and instructions are in Settings.",
        }),
      ),
      el(
        "label",
        { class: "field field-check field-wide" },
        enterToSend,
        el("span", { text: "Enter to send" }),
      ),
      el("div", { class: "field field-wide preferences-row" }, saveDefaultsButton, preferencesNote),
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
  /** Counts skill ticks and unticks, so a send can tell which came after its click. */
  let skillEdits = 0;
  /** The count at each skill's last change; a skill never changed is absent. */
  const skillEditedAt = new Map<string, number>();
  /** The count when the send now being cleared was clicked. */
  let sentSkillsAt = 0;

  let state: ComposerState = { visible: false, enabled: false, reason: null };
  let running = false;
  /** Set while a send re-reads the defaults, so a second click does not send twice. */
  let sending = false;
  let defaultsError: string | null = null;
  /** The open session's endpoint from the connection, or the launch default until it arrives. */
  let fixedEndpoint = "";

  function readSettings(): ChatSettings {
    return {
      endpoint: fixedEndpoint,
      model: model.value(),
      context_tokens: budgets?.context_tokens ?? 0,
      max_output_tokens: budgets?.max_output_tokens ?? 0,
      max_tool_calls: budgets?.max_tool_calls ?? 0,
      turn_timeout_seconds: budgets?.turn_timeout_seconds ?? 0,
      instructions: budgets?.instructions ?? "",
      compact_threshold: Number(threshold.value),
      auto_compact: autoCompact.checked,
    };
  }

  function selectedSkills(): string[] {
    return skillBoxes.filter(([, box]) => box.checked).map(([name]) => name);
  }

  function setSkills(names: readonly string[]): void {
    skillEditedAt.clear();
    skillBoxes = names.map((name) => {
      const box = el("input", { attrs: { type: "checkbox" } });
      box.addEventListener("change", () => {
        skillEdits += 1;
        skillEditedAt.set(name, skillEdits);
        refresh();
      });
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

  function preferencesOf(): ComposerPreferences {
    return {
      compact_threshold: Number(threshold.value),
      auto_compact: autoCompact.checked,
      enter_to_send: enterToSend.checked,
    };
  }

  function showThreshold(): void {
    replaceChildren(thresholdValue, `${tokenLabel(Number(threshold.value))} tokens`);
  }

  function applyDefaults(defaults: ChatDefaults): void {
    const { settings } = defaults;
    budgets = {
      context_tokens: settings.context_tokens,
      max_output_tokens: settings.max_output_tokens,
      max_tool_calls: settings.max_tool_calls,
      turn_timeout_seconds: settings.turn_timeout_seconds,
      instructions: settings.instructions,
    };
    if (connection.status() === null) {
      fixedEndpoint = settings.endpoint;
      replaceChildren(endpointValue, settings.endpoint);
    }
    threshold.min = String(defaults.compact_threshold_min);
    threshold.max = String(defaults.compact_threshold_max);
    if (!editedSettings.has(threshold)) {
      // A value saved before the slider existed is shown, and sent, at the nearest stop.
      threshold.value = String(
        clampThreshold(
          settings.compact_threshold,
          defaults.compact_threshold_min,
          defaults.compact_threshold_max,
        ),
      );
    }
    showThreshold();
    if (!editedSettings.has(autoCompact)) autoCompact.checked = settings.auto_compact;
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

  /** Reads the saved defaults again. A failure keeps the last ones read and is shown. */
  async function loadDefaults(): Promise<void> {
    try {
      applyDefaults(await api.chatDefaults());
      defaultsError = null;
    } catch (failure) {
      const error = toIpcError(failure);
      defaultsError = `${DEFAULTS_UNREADABLE} ${error.message} (${error.code})`;
    }
    refresh();
  }

  async function saveDefaults(): Promise<void> {
    saveDefaultsButton.disabled = true;
    try {
      // Only the note changes: the inputs keep any edit made while the save
      // was in flight, so the returned copy is not written back.
      await api.chatPreferencesSave(preferencesOf());
      replaceChildren(preferencesNote, "Saved as defaults.");
    } catch (reason) {
      const error = toIpcError(reason);
      replaceChildren(preferencesNote, `${error.message} (${error.code})`);
    } finally {
      saveDefaultsButton.disabled = false;
      refresh();
    }
  }

  function currentProblem(): string | null {
    const status = connection.status();
    if (status === null) {
      return "The session's provider is being read.";
    }
    if (status.stale) {
      return "The provider chosen for this session changed. Choose the provider again.";
    }
    if (model.value().trim() === "") {
      return "Choose a model.";
    }
    if (budgets === null) {
      return defaultsError === null ? "The chat defaults are being read." : null;
    }
    if (!settingsValid(readSettings())) {
      return SET_CONTEXT_IN_SETTINGS;
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
    // A failed read leaves Send enabled: clicking it reads the defaults again.
    const unread = budgets === null && defaultsError === null;
    sendButton.disabled = !state.enabled || sending || unread || issue !== null;
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
    for (const input of composerInputs) {
      input.disabled = !state.enabled;
    }
    const full = selectedSkills().length >= SKILLS_PER_TURN;
    for (const [, skillBox] of skillBoxes) {
      skillBox.disabled = !state.enabled || (full && !skillBox.checked);
    }
  }

  function showProblem(issue: string | null): void {
    replaceChildren(problem, issue ?? "");
    problem.hidden = issue === null;
  }

  /**
   * Sends with the defaults saved last: they are read again first, since the settings window may
   * have changed them. The message, the skills, and the session are taken at the click; a send
   * whose session changed while the defaults were read is dropped.
   */
  async function submitFresh(): Promise<void> {
    const message = box.value;
    const skills = selectedSkills();
    const skillsAt = skillEdits;
    const session = sessionId;
    sending = true;
    refresh();
    try {
      await loadDefaults();
    } finally {
      sending = false;
    }
    refresh();
    const issue = currentProblem();
    if (session !== sessionId || !state.enabled || running) {
      return;
    }
    // Never send with budgets this read could not confirm, even older ones.
    if (defaultsError !== null) {
      showProblem(defaultsError);
      return;
    }
    if (issue !== null || budgets === null) {
      showProblem(issue);
      return;
    }
    sentSkillsAt = skillsAt;
    handlers.onSend(message, readSettings(), skills);
  }

  function submit(): void {
    if (sending) {
      return;
    }
    const issue = currentProblem();
    if (!state.enabled || issue !== null) {
      showProblem(issue);
      return;
    }
    void submitFresh();
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
  autoCompact.addEventListener("change", () => {
    editedSettings.add(autoCompact);
    refresh();
  });
  threshold.addEventListener("input", () => {
    editedSettings.add(threshold);
    showThreshold();
    refresh();
  });
  showThreshold();
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
      ),
      el(
        "div",
        { class: "composer-send" },
        counter,
        runningLabel,
        workspaceButton,
        cancelButton,
        sendButton,
      ),
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

  // The settings window may have saved new budgets while this window was in the background.
  const onFocus = (): void => void loadDefaults();
  window.addEventListener("focus", onFocus);
  void loadDefaults();

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
    clearSent(message, skills) {
      if (box.value.startsWith(message)) {
        box.value = box.value.slice(message.length);
      }
      for (const [name, skillBox] of skillBoxes) {
        // A sent skill the operator changed after the click keeps its new state.
        const changedSince = (skillEditedAt.get(name) ?? 0) > sentSkillsAt;
        if (skills.includes(name) && !changedSince) {
          skillBox.checked = false;
        }
      }
      refresh();
    },
    readSettings,
    setSkills,
    dispose() {
      unsubscribe();
      window.removeEventListener("focus", onFocus);
      settingsMenu.dispose();
      skillsMenu.dispose();
    },
  };
}
