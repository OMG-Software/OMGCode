import { invokeWithChannel } from "./client";
import { type ChatEvent, isChatEvent } from "./types";

/**
 * Subscribes to pushed chat events (`chat_subscribe`, main window only). Only well-formed events
 * reach `onEvent`, and a handler that throws does not stop later events. This module is imported
 * by the main window alone, so the approvals bundle never contains the command name.
 */
export function chatSubscribe(onEvent: (event: ChatEvent) => void): Promise<void> {
  return invokeWithChannel("chat_subscribe", (value) => {
    if (!isChatEvent(value)) {
      return;
    }
    try {
      onEvent(value);
    } catch {
      // A failing handler must not break later events.
    }
  });
}
