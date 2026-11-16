import { api } from "../ipc/client";
import { toIpcError } from "../ipc/errors";
import type { ChatSettings, ChatStatus } from "../ipc/types";
import { createPoller, type Poller } from "../poll";

export const CHAT_POLL_MS = 750;

export interface ChatRunner {
  send(sessionId: string, message: string, settings: ChatSettings): Promise<void>;
  cancel(sessionId: string): Promise<void>;
  /** Starts polling a session the core reports as running (for example after switching back). */
  watch(sessionId: string): void;
  isActive(sessionId: string): boolean;
  dispose(): void;
}

export interface ChatRunnerEvents {
  /** Called on every status change while a turn is active or has just settled. */
  onStatus(status: ChatStatus): void;
}

function settled(status: ChatStatus): boolean {
  return status.state !== "running" && status.state !== "cancelling";
}

/**
 * Tracks the chat turns this window started. Each active session has one poller that
 * calls `chat_status` every 750 ms and stops when the turn settles.
 */
export function createChatRunner(events: ChatRunnerEvents): ChatRunner {
  const pollers = new Map<string, Poller>();

  function track(status: ChatStatus): void {
    events.onStatus(status);
    if (settled(status) || pollers.has(status.session_id)) {
      return;
    }
    const id = status.session_id;
    const poller = createPoller(
      async () => {
        const next = await api.chatStatus(id);
        events.onStatus(next);
        if (settled(next)) {
          stop(id);
        }
      },
      CHAT_POLL_MS,
      (reason) => {
        stop(id);
        events.onStatus({
          session_id: id,
          state: "failed",
          outcome: null,
          error: toIpcError(reason),
        });
      },
    );
    pollers.set(id, poller);
    poller.start();
  }

  function stop(id: string): void {
    pollers.get(id)?.stop();
    pollers.delete(id);
  }

  return {
    async send(sessionId, message, settings) {
      track(await api.chatSend(sessionId, message, settings));
    },
    async cancel(sessionId) {
      track(await api.chatCancel(sessionId));
    },
    watch(sessionId) {
      if (!pollers.has(sessionId)) {
        track({ session_id: sessionId, state: "running", outcome: null, error: null });
      }
    },
    isActive: (sessionId) => pollers.has(sessionId),
    dispose() {
      for (const id of Array.from(pollers.keys())) {
        stop(id);
      }
    },
  };
}
