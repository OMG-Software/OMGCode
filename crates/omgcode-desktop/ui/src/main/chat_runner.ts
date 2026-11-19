import { chatSubscribe } from "../ipc/chat_events";
import { api } from "../ipc/client";
import { toIpcError } from "../ipc/errors";
import type { ChatEvent, ChatSettings, ChatStatus } from "../ipc/types";
import { createPoller, type Poller } from "../poll";

/** Status poll interval when the push channel could not be subscribed. */
export const CHAT_POLL_MS = 750;
/** Safety check while subscribed, in case a `finished` event is lost. */
export const CHAT_RECONCILE_MS = 5000;
/** Client-side bound on one preview, in UTF-16 code units. Defence in depth; the core bounds it too. */
export const PREVIEW_LIMIT = 64 * 1024;
/** Sessions the runner does not track whose replayed preview is kept until they are watched. */
export const UNTRACKED_PREVIEW_SESSIONS = 4;

/** Provisional assistant text of one model request. It is not the recorded reply. */
export interface ChatPreview {
  request: number;
  text: string;
  truncated: boolean;
}

export interface ChatRunner {
  /** `skills` are names the operator selected for this turn only; none by default. */
  send(
    sessionId: string,
    message: string,
    settings: ChatSettings,
    skills?: readonly string[],
  ): Promise<void>;
  cancel(sessionId: string): Promise<void>;
  /** Tracks a session the core reports as running (for example after switching back). */
  watch(sessionId: string): void;
  isActive(sessionId: string): boolean;
  /** The current preview of a tracked session, or `null`. */
  preview(sessionId: string): ChatPreview | null;
  dispose(): void;
}

export interface ChatRunnerEvents {
  /** Called on every status change while a turn is active or has just settled. */
  onStatus(status: ChatStatus): void;
  /** Called when the preview of a session changes. `null` means there is no preview. */
  onPreview(sessionId: string, preview: ChatPreview | null): void;
}

function settled(status: ChatStatus): boolean {
  return status.state !== "running" && status.state !== "cancelling";
}

function clip(text: string, room: number): string {
  if (text.length <= room) {
    return text;
  }
  const end = room > 0 && /[\ud800-\udbff]/.test(text.charAt(room - 1)) ? room - 1 : room;
  return text.slice(0, end);
}

/**
 * Tracks the chat turns this window started. It subscribes to pushed events once, lazily on the
 * first `send` or `watch`. While subscribed, `finished` triggers one `chat_status` read and a
 * five-second check covers a lost event. If subscribing fails, each active session is polled
 * every 750 ms instead. The runner owns the provisional preview of every tracked session.
 */
export function createChatRunner(events: ChatRunnerEvents): ChatRunner {
  const pollers = new Map<string, Poller>();
  const previews = new Map<string, ChatPreview>();
  /** Sessions with a `chat_send` in flight: their events may arrive before the reply does. */
  const starting = new Map<string, { finished: boolean }>();
  let mode: "pending" | "events" | "polling" = "pending";
  let subscription: Promise<void> | null = null;
  let disposed = false;

  function ensureSubscribed(): Promise<void> {
    subscription ??= chatSubscribe(handleEvent).then(
      () => {
        mode = "events";
      },
      () => {
        mode = "polling";
        for (const id of Array.from(pollers.keys())) {
          dropPoller(id);
          startPoller(id);
        }
      },
    );
    return subscription;
  }

  /** Stores a preview. Only tracked sessions are reported to the view unless `notify` says so. */
  function setPreview(
    id: string,
    preview: ChatPreview | null,
    notify: boolean = accepts(id),
  ): void {
    if (preview === null) {
      if (previews.delete(id) && notify) {
        events.onPreview(id, null);
      }
      return;
    }
    previews.set(id, preview);
    if (notify) {
      events.onPreview(id, preview);
      return;
    }
    const untracked = Array.from(previews.keys()).filter((key) => !accepts(key));
    for (const key of untracked.slice(
      0,
      Math.max(0, untracked.length - UNTRACKED_PREVIEW_SESSIONS),
    )) {
      previews.delete(key);
    }
  }

  function dropPoller(id: string): void {
    pollers.get(id)?.stop();
    pollers.delete(id);
  }

  function finish(id: string): void {
    dropPoller(id);
    setPreview(id, null, true);
  }

  function failed(id: string, reason: unknown): void {
    finish(id);
    events.onStatus({ session_id: id, state: "failed", outcome: null, error: toIpcError(reason) });
  }

  /** Reads `chat_status` once for a tracked session. */
  async function check(id: string): Promise<void> {
    const next = await api.chatStatus(id);
    if (disposed || !pollers.has(id)) {
      return;
    }
    if (settled(next)) {
      finish(id);
    }
    events.onStatus(next);
  }

  function startPoller(id: string): void {
    const poller = createPoller(
      () => check(id),
      mode === "polling" ? CHAT_POLL_MS : CHAT_RECONCILE_MS,
      (reason) => failed(id, reason),
    );
    pollers.set(id, poller);
    poller.start();
  }

  function track(status: ChatStatus): void {
    if (settled(status)) {
      finish(status.session_id);
      events.onStatus(status);
      return;
    }
    events.onStatus(status);
    if (!pollers.has(status.session_id)) {
      startPoller(status.session_id);
      const stored = previews.get(status.session_id);
      if (stored !== undefined && !starting.has(status.session_id)) {
        events.onPreview(status.session_id, stored);
      }
    }
  }

  function accepts(id: string): boolean {
    return pollers.has(id) || starting.has(id);
  }

  function applyPreview(event: ChatEvent): void {
    if (event.kind === "finished") {
      return;
    }
    const id = event.session_id;
    const current = previews.get(id);
    if (event.kind === "request_started") {
      setPreview(id, { request: event.request, text: "", truncated: false });
      return;
    }
    if (current !== undefined && event.request < current.request) {
      return;
    }
    const base: ChatPreview =
      current !== undefined && event.request === current.request
        ? current
        : { request: event.request, text: "", truncated: false };
    if (event.kind === "preview_truncated") {
      setPreview(id, { ...base, truncated: true });
      return;
    }
    const room = PREVIEW_LIMIT - base.text.length;
    if (room <= 0) {
      if (!base.truncated) {
        setPreview(id, { ...base, truncated: true });
      }
      return;
    }
    const piece = clip(event.text, room);
    setPreview(id, {
      ...base,
      text: base.text + piece,
      truncated: base.truncated || piece.length < event.text.length,
    });
  }

  function handleEvent(event: ChatEvent): void {
    if (disposed) {
      return;
    }
    if (!accepts(event.session_id)) {
      if (event.kind === "finished") {
        previews.delete(event.session_id);
      } else {
        applyPreview(event);
      }
      return;
    }
    if (event.kind === "finished") {
      const early = starting.get(event.session_id);
      if (early !== undefined) {
        early.finished = true;
      } else {
        check(event.session_id).catch((reason) => failed(event.session_id, reason));
      }
      return;
    }
    applyPreview(event);
  }

  return {
    async send(sessionId, message, settings, skills = []) {
      await ensureSubscribed();
      setPreview(sessionId, null, false);
      const early = { finished: false };
      starting.set(sessionId, early);
      try {
        track(await api.chatSend(sessionId, message, settings, [...skills]));
      } catch (reason) {
        if (!pollers.has(sessionId)) {
          setPreview(sessionId, null);
        }
        throw reason;
      } finally {
        starting.delete(sessionId);
      }
      if (early.finished && pollers.has(sessionId)) {
        check(sessionId).catch((reason) => failed(sessionId, reason));
      }
    },
    async cancel(sessionId) {
      track(await api.chatCancel(sessionId));
    },
    watch(sessionId) {
      void ensureSubscribed();
      if (!pollers.has(sessionId)) {
        track({ session_id: sessionId, state: "running", outcome: null, error: null });
      }
    },
    isActive: (sessionId) => pollers.has(sessionId),
    preview: (sessionId) => previews.get(sessionId) ?? null,
    dispose() {
      disposed = true;
      for (const id of Array.from(pollers.keys())) {
        dropPoller(id);
      }
      previews.clear();
      starting.clear();
    },
  };
}
