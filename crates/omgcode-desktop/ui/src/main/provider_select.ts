import { el, neutralise } from "../dom";
import { connectionLabel, DEFAULT_ENDPOINTS, providerName } from "../providers";
import type { ConnectionModel } from "./connection_state";

/** The value of the provider entry that chooses the default endpoint. */
export const DEFAULT_CONNECTION = "";
/** The value of the entry shown while the session's recorded choice no longer holds. */
export const STALE_CONNECTION = "?stale";

export interface ProviderSelect {
  element: HTMLSelectElement;
  /** Draws the options from the connection state. Called on every change. */
  render(): void;
  /** Keeps the dropdown disabled while `locked`, for example while a turn runs. */
  setLocked(locked: boolean): void;
}

/**
 * A provider dropdown over the shared connection state (ADR-0069 decision 11). The composer and
 * the footer each build one, and both render from the same state, so they always agree. Choosing
 * an entry only names a profile: Rust asks for a native Yes/No confirmation before anything is
 * recorded, so this window cannot switch on its own.
 */
export function createProviderSelect(connection: ConnectionModel, label: string): ProviderSelect {
  const element = el("select", { class: "status-provider", attrs: { "aria-label": label } });
  element.disabled = true;
  let baseDisabled = true;
  let locked = false;

  function apply(): void {
    element.disabled = baseDisabled || locked;
  }

  function render(): void {
    const status = connection.status();
    element.replaceChildren();
    if (status === null) {
      element.append(el("option", { text: "Reading the provider…", attrs: { value: "" } }));
      baseDisabled = true;
      apply();
      return;
    }
    const current = status.connection;
    if (!current.selectable) {
      const text = `Set at launch: ${providerName(current.kind)} (${current.endpoint})`;
      element.append(el("option", { text: neutralise(text), attrs: { value: "" } }));
      baseDisabled = true;
      apply();
      return;
    }
    if (status.stale) {
      const name = current.profile ?? "default";
      element.append(
        el("option", {
          text: neutralise(`Choose again: ${name} changed (${current.endpoint})`),
          attrs: { value: STALE_CONNECTION },
        }),
      );
    }
    element.append(
      el("option", {
        text: `LM Studio: default (${DEFAULT_ENDPOINTS.lm_studio})`,
        attrs: { value: DEFAULT_CONNECTION },
      }),
    );
    for (const choice of status.choices) {
      element.append(
        el("option", {
          text: neutralise(connectionLabel(choice.kind, choice.name, choice.endpoint)),
          attrs: { value: choice.name },
        }),
      );
    }
    element.value = status.stale
      ? STALE_CONNECTION
      : (status.choices.find((choice) => choice.active)?.name ?? DEFAULT_CONNECTION);
    baseDisabled = connection.busy() || connection.sessionId() === null;
    apply();
  }

  element.addEventListener("change", () => {
    if (element.value === STALE_CONNECTION) {
      return;
    }
    void connection.select(element.value === DEFAULT_CONNECTION ? null : element.value);
  });

  return {
    element,
    render,
    setLocked(next) {
      locked = next;
      apply();
    },
  };
}
