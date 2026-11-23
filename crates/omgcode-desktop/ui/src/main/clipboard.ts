import { button, type ElOptions } from "../dom";
import { type IconName, iconButton, setIconButton } from "../ui/icons";

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

/** What a copy button shows: its resting label, or the outcome of its last action. */
type Feedback = "idle" | "done" | "failed";

/** The label a copy button shows after its action fails. */
export const COPY_FAILED = "Copy failed";

/**
 * Wires `node` to run `action` on click and then `show` its outcome for `COPY_FEEDBACK_MS`
 * before it returns to rest. A click while an action runs is ignored.
 */
function withFeedback(
  node: HTMLButtonElement,
  action: () => Promise<void>,
  show: (feedback: Feedback) => void,
): HTMLButtonElement {
  let reset: ReturnType<typeof setTimeout> | undefined;
  let running = false;
  const settle = (feedback: Feedback): void => {
    running = false;
    show(feedback);
    if (reset !== undefined) {
      clearTimeout(reset);
    }
    reset = setTimeout(() => {
      show("idle");
      reset = undefined;
    }, COPY_FEEDBACK_MS);
  };
  node.addEventListener("click", () => {
    if (running) {
      return;
    }
    running = true;
    // A promise executor runs at once, so the action still starts inside the click and the
    // webview sees the user's gesture; a synchronous throw becomes a rejection.
    const pending = new Promise<void>((resolve) => {
      resolve(action());
    });
    void pending.then(
      () => settle("done"),
      () => settle("failed"),
    );
  });
  return node;
}

/**
 * A text button that runs `action` on click and then shows `done` or "Copy failed" for
 * `COPY_FEEDBACK_MS` before its label returns. A click while an action runs is ignored.
 */
export function feedbackButton(
  label: string,
  done: string,
  action: () => Promise<void>,
  options: ElOptions = {},
): HTMLButtonElement {
  const node = button(label, () => undefined, options);
  return withFeedback(node, action, (feedback) => {
    node.textContent = feedback === "idle" ? label : feedback === "done" ? done : COPY_FAILED;
  });
}

/**
 * An icon-only button with the same behaviour as `feedbackButton`. `label` is its accessible
 * name and tooltip; while the outcome shows, a tick or a cross replaces the icon and `done` or
 * "Copy failed" replaces the name and tooltip.
 */
export function iconFeedbackButton(
  name: IconName,
  label: string,
  done: string,
  action: () => Promise<void>,
  className: string,
): HTMLButtonElement {
  const node = iconButton(name, label, () => undefined, className);
  return withFeedback(node, action, (feedback) => {
    if (feedback === "idle") {
      setIconButton(node, name, label);
    } else if (feedback === "done") {
      setIconButton(node, "check", done);
    } else {
      setIconButton(node, "cross", COPY_FAILED);
    }
  });
}
