import { api } from "../ipc/client";
import { CANCELLED_CODE, toIpcError } from "../ipc/errors";
import type { ConnectionStatus } from "../ipc/types";

/** Shown when the operator closes the confirmation or answers No. */
export const CONNECTION_UNCHANGED = "The model provider was not changed.";

/**
 * Why the connection state changed. `opening` means a different session was named and its
 * status has not arrived; `read` is a status read; `selected` is a confirmed provider choice
 * (Rust cleared the session's model); `model` is a model choice, or its failure; `busy` and
 * `message` change only what the dropdowns show.
 */
export type ConnectionChange = "opening" | "read" | "selected" | "model" | "busy" | "message";

export interface ConnectionModel {
  /** The open session, or `null` when none is open. */
  sessionId(): string | null;
  /** The session's connection and model, or `null` while its status is being read. */
  status(): ConnectionStatus | null;
  /** A provider or model choice is in flight (a native dialog may be open). */
  busy(): boolean;
  /** The last failure or "unchanged" note; empty when there is none. */
  message(): string;
  /** Calls `listener` after every change; returns the function that removes it. */
  subscribe(listener: (change: ConnectionChange) => void): () => void;
  /**
   * Names the open session (or `null`) and reads its connection and model. Naming the session
   * that is already open reads again without clearing what is shown.
   */
  open(sessionId: string | null): Promise<void>;
  /** Reads the status again, unless a choice is in flight. */
  refresh(): Promise<void>;
  /** Chooses a provider for the open session; Rust asks for the native confirmation. */
  select(profile: string | null): Promise<void>;
  /** Records the model for the open session. */
  chooseModel(model: string): Promise<void>;
  dispose(): void;
}

/**
 * The one source of truth for the open session's provider and model (ADR-0067 decision 11).
 * The composer's and the footer's dropdowns both render from it, so they cannot disagree. Every
 * answer is matched to the session that asked, so a late answer for a session that was left
 * never touches what is shown.
 */
export function createConnectionModel(): ConnectionModel {
  let session: string | null = null;
  let current: ConnectionStatus | null = null;
  let working = false;
  let note = "";
  /** Counts reads; an answer is used only when no later read or choice began. */
  let generation = 0;
  /** Counts model choices; only the latest one's answer is used, so the operator's last pick wins. */
  let modelCalls = 0;
  let disposed = false;
  const listeners = new Set<(change: ConnectionChange) => void>();

  function emit(change: ConnectionChange): void {
    for (const listener of Array.from(listeners)) {
      listener(change);
    }
  }

  function describe(reason: unknown): string {
    const error = toIpcError(reason);
    return error.code === CANCELLED_CODE
      ? CONNECTION_UNCHANGED
      : `${error.message} (${error.code})`;
  }

  async function read(): Promise<void> {
    const id = session;
    generation += 1;
    const mine = generation;
    try {
      const status = await api.connectionStatus(id);
      if (disposed || mine !== generation || session !== id) {
        return;
      }
      current = status;
      emit("read");
    } catch (reason) {
      if (disposed || mine !== generation || session !== id) {
        return;
      }
      note = describe(reason);
      emit("message");
    }
  }

  /** Asks Rust to choose a provider for the open session; a native dialog may be open meanwhile. */
  async function chooseProvider(profile: string | null): Promise<void> {
    const id = session;
    if (id === null || working) {
      return;
    }
    working = true;
    note = "";
    generation += 1;
    emit("busy");
    let status: ConnectionStatus | null = null;
    let failure: unknown = null;
    try {
      status = await api.connectionSelect(id, profile);
    } catch (reason) {
      failure = reason;
    }
    working = false;
    if (disposed) {
      return;
    }
    if (session !== id) {
      // The operator left the session while the dialog was open; its own status is read by `open`.
      emit("busy");
      return;
    }
    if (status === null) {
      // The recorded choice decides what the dropdowns show, after a No or a failure.
      note = describe(failure);
      emit("message");
      await read();
      return;
    }
    current = status;
    generation += 1;
    emit("selected");
  }

  return {
    sessionId: () => session,
    status: () => current,
    busy: () => working,
    message: () => note,
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    async open(sessionId) {
      if (sessionId === session && current !== null) {
        await read();
        return;
      }
      session = sessionId;
      current = null;
      note = "";
      emit("opening");
      await read();
    },
    async refresh() {
      if (!working) {
        await read();
      }
    },
    select: chooseProvider,
    async chooseModel(model) {
      const id = session;
      if (id === null) {
        return;
      }
      modelCalls += 1;
      const mine = modelCalls;
      note = "";
      try {
        const status = await api.sessionModelSelect(id, model);
        if (disposed || session !== id || mine !== modelCalls) {
          return;
        }
        current = status;
        generation += 1;
        emit("model");
      } catch (reason) {
        if (disposed || session !== id || mine !== modelCalls) {
          return;
        }
        note = describe(reason);
        emit("message");
        // The recorded model decides what the dropdown shows after a failure.
        await read();
      }
    },
    dispose() {
      disposed = true;
      listeners.clear();
    },
  };
}
