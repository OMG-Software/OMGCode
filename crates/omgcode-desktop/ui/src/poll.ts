export interface Poller {
  start(): void;
  stop(): void;
  /** Runs the task now unless one is in flight or the page is hidden. */
  tick(): Promise<void>;
  readonly running: boolean;
}

/**
 * Calls `task` every `intervalMs` while started. A tick is skipped while the previous
 * call is still pending or `document.hidden` is true. Becoming visible triggers a tick.
 * A rejection from `task` is passed to `onError` and polling continues.
 */
export function createPoller(
  task: () => Promise<void>,
  intervalMs: number,
  onError: (reason: unknown) => void = () => undefined,
): Poller {
  let timer: ReturnType<typeof setInterval> | null = null;
  let inFlight = false;

  async function tick(): Promise<void> {
    if (inFlight || document.hidden) {
      return;
    }
    inFlight = true;
    try {
      await task();
    } catch (reason) {
      onError(reason);
    } finally {
      inFlight = false;
    }
  }

  function onVisibility(): void {
    if (!document.hidden) {
      void tick();
    }
  }

  return {
    start() {
      if (timer !== null) {
        return;
      }
      timer = setInterval(() => void tick(), intervalMs);
      document.addEventListener("visibilitychange", onVisibility);
    },
    stop() {
      if (timer === null) {
        return;
      }
      clearInterval(timer);
      timer = null;
      document.removeEventListener("visibilitychange", onVisibility);
    },
    tick,
    get running() {
      return timer !== null;
    },
  };
}
