import { button, type ElOptions } from "../dom";

/** How long a copy button shows its result before its label returns. */
export const COPY_FEEDBACK_MS = 1500;

function clipboard(): Clipboard {
  const value = globalThis.navigator?.clipboard;
  if (value === undefined) {
    throw new Error("The clipboard is not available.");
  }
  return value;
}

/**
 * Copies text to the system clipboard through the browser's asynchronous clipboard API. It is
 * called only from a click handler, so the webview treats it as user-initiated. Nothing in the
 * frontend reads the clipboard. Rejects when the API is unavailable or refuses the write.
 */
export async function copyText(value: string): Promise<void> {
  await clipboard().writeText(value);
}

/**
 * Copies a PNG image to the clipboard, under the same conditions as `copyText`. The image is
 * passed as a pending blob and the write starts at once, inside the click: WebKit refuses a
 * clipboard write that begins after the gesture's task has ended.
 */
export async function copyPng(image: Promise<Blob>): Promise<void> {
  const target = clipboard();
  if (typeof ClipboardItem !== "function" || typeof target.write !== "function") {
    // Settle the pending image so a rejected render is not reported as unhandled.
    image.catch(() => undefined);
    throw new Error("The clipboard cannot hold an image here.");
  }
  await target.write([new ClipboardItem({ "image/png": image })]);
}

/**
 * A button that runs `action` on click and then shows `done` or "Copy failed" for
 * `COPY_FEEDBACK_MS` before its label returns. A click while an action runs is ignored.
 */
export function feedbackButton(
  label: string,
  done: string,
  action: () => Promise<void>,
  options: ElOptions = {},
): HTMLButtonElement {
  let reset: ReturnType<typeof setTimeout> | undefined;
  let running = false;
  const settle = (message: string): void => {
    running = false;
    node.textContent = message;
    if (reset !== undefined) {
      clearTimeout(reset);
    }
    reset = setTimeout(() => {
      node.textContent = label;
      reset = undefined;
    }, COPY_FEEDBACK_MS);
  };
  const node = button(
    label,
    () => {
      if (running) {
        return;
      }
      running = true;
      // The action runs inside the click so the webview sees the user's gesture.
      let pending: Promise<void>;
      try {
        pending = action();
      } catch (reason) {
        pending = Promise.reject(reason);
      }
      void pending.then(
        () => settle(done),
        () => settle("Copy failed"),
      );
    },
    options,
  );
  return node;
}
