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
 * The one source of truth for the open session's provider and model (ADR-0071 decision 11).
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
  /** Counts accepted provider choices; a model answer from before one is stale. */
  let providerCalls = 0;
  /**
   * Model saves not yet answered, by session. While one is pending a status read could still
   * hold the old model, so its answer is dropped: the save's own answer is the newer truth.
   */
  const pendingModels = new Map<string, number>();
  /**
   * Sessions whose status read was dropped for a pending model save. The read is repeated once
   * the last save settles, even when that save's own answer is discarded, so the session never
   * stays without a status.
   */
  const droppedReads = new Set<string>();
  /**
   * Every write (a model or a provider choice) runs after the one before it settled, because each
   * IPC call runs on its own worker and two in flight could land out of order in Rust.
   */
  let writes: Promise<unknown> = Promise.resolve();
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

  /** Starts `write` once every earlier write has settled, whether it succeeded or failed. */
  function enqueue<T>(write: () => Promise<T>): Promise<T> {
    const run = writes.then(write, write);
    writes = run.catch(() => undefined);
    return run;
  }

  function modelSavePending(id: string | null): boolean {
    return id !== null && (pendingModels.get(id) ?? 0) > 0;
  }

  /** Settles one model save; true when it was the session's last and a read was dropped. */
  function settleModelSave(id: string): boolean {
    const left = (pendingModels.get(id) ?? 1) - 1;
    if (left > 0) {
      pendingModels.set(id, left);
      return false;
    }
    pendingModels.delete(id);
    return droppedReads.delete(id);
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
      if (id !== null && modelSavePending(id)) {
        droppedReads.add(id);
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
    providerCalls += 1;
    emit("busy");
    let status: ConnectionStatus | null = null;
    let failure: unknown = null;
    try {
      status = await enqueue(() => api.connectionSelect(id, profile));
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
      const provider = providerCalls;
      const stale = (): boolean =>
        disposed || session !== id || mine !== modelCalls || provider !== providerCalls;
      note = "";
      // A status read already in flight may hold the old model; it must not undo this choice.
      generation += 1;
      pendingModels.set(id, (pendingModels.get(id) ?? 0) + 1);
      let status: ConnectionStatus | null = null;
      let failure: unknown = null;
      try {
        status = await enqueue(() => api.sessionModelSelect(id, model));
      } catch (reason) {
        failure = reason;
      }
      const repeatRead = settleModelSave(id);
      if (stale()) {
        // This answer is discarded, but a read dropped while it was pending must still happen,
        // or a reopened session would stay without a status.
        if (repeatRead && !disposed && session === id) {
          await read();
        }
        return;
      }
      if (status !== null) {
        current = status;
        generation += 1;
        emit("model");
        return;
      }
      note = describe(failure);
      emit("message");
      // The recorded model decides what the dropdown shows after a failure.
      await read();
    },
    dispose() {
      disposed = true;
      listeners.clear();
    },
  };
}
