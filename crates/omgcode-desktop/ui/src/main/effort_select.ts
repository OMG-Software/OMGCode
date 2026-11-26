import { el } from "../dom";
import type { ReasoningEffort } from "../ipc/types";
import type { ConnectionModel } from "./connection_state";

/** The value of the entry that sends no effort, so the provider's own default applies. */
export const DEFAULT_EFFORT = "";

/** Shown under the dropdown when the session's connection takes no effort (ADR-0102). */
export const EFFORT_NOT_TAKEN = "This connection does not take an effort setting.";

/** The label of each effort the core accepts, in the order the dropdown lists them. */
export const EFFORT_LABELS: Readonly<Record<ReasoningEffort, string>> = {
  none: "None",
  minimal: "Minimal",
  low: "Low",
  medium: "Medium",
  high: "High",
  xhigh: "Extra high",
  max: "Max",
};

const EFFORT_ORDER = Object.keys(EFFORT_LABELS) as ReasoningEffort[];

export interface EffortSelect {
  element: HTMLSelectElement;
  /** The note shown when the connection takes no effort; hidden otherwise. */
  note: HTMLElement;
  /** Draws the options from the connection state. Called on every change. */
  render(): void;
  /** Keeps the dropdown disabled while `locked`, for example while a turn runs. */
  setLocked(locked: boolean): void;
  /** The chosen effort, or `null` for the provider's default or a connection that takes none. */
  value(): ReasoningEffort | null;
}

/**
 * The reasoning effort dropdown of the open session (ADR-0102). It offers only what Rust reports
 * the session's connection accepts; Rust refuses anything else again on save and on send. The
 * values are a closed list, so no server text reaches an option.
 */
export function createEffortSelect(connection: ConnectionModel): EffortSelect {
  const element = el("select", { class: "status-provider", attrs: { "aria-label": "Effort" } });
  element.disabled = true;
  const note = el("span", { class: "muted", text: EFFORT_NOT_TAKEN, attrs: { role: "status" } });
  note.hidden = true;
  let baseDisabled = true;
  let locked = false;

  function offered(): readonly ReasoningEffort[] {
    const accepted = connection.status()?.reasoning_efforts ?? [];
    return EFFORT_ORDER.filter((effort) => accepted.includes(effort));
  }

  function apply(): void {
    element.disabled = baseDisabled || locked;
  }

  function render(): void {
    const status = connection.status();
    const efforts = offered();
    element.replaceChildren(el("option", { text: "Default", attrs: { value: DEFAULT_EFFORT } }));
    for (const effort of efforts) {
      element.append(el("option", { text: EFFORT_LABELS[effort], attrs: { value: effort } }));
    }
    const chosen = status?.reasoning_effort ?? null;
    element.value = chosen !== null && efforts.includes(chosen) ? chosen : DEFAULT_EFFORT;
    note.hidden = status === null || efforts.length > 0;
    baseDisabled =
      status === null ||
      efforts.length === 0 ||
      connection.busy() ||
      connection.sessionId() === null;
    apply();
  }

  element.addEventListener("change", () => {
    const chosen = EFFORT_ORDER.find((effort) => effort === element.value) ?? null;
    void connection.chooseEffort(chosen);
  });

  return {
    element,
    note,
    render,
    setLocked(next) {
      locked = next;
      apply();
    },
    value() {
      const efforts = offered();
      const chosen = EFFORT_ORDER.find((effort) => effort === element.value) ?? null;
      return chosen !== null && efforts.includes(chosen) ? chosen : null;
    },
  };
}
