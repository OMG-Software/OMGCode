import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { neutralise } from "../src/dom";
import { createPoller } from "../src/poll";

function setHidden(hidden: boolean): void {
  Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
  document.dispatchEvent(new Event("visibilitychange"));
}

describe("createPoller", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    setHidden(false);
    vi.useRealTimers();
  });

  it("runs on the interval and stops", async () => {
    const task = vi.fn(async () => undefined);
    const poller = createPoller(task, 1000);
    poller.start();
    await vi.advanceTimersByTimeAsync(3000);
    expect(task).toHaveBeenCalledTimes(3);
    poller.stop();
    await vi.advanceTimersByTimeAsync(3000);
    expect(task).toHaveBeenCalledTimes(3);
    expect(poller.running).toBe(false);
  });

  it("does not overlap in-flight calls", async () => {
    let release: () => void = () => undefined;
    const task = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          release = resolve;
        }),
    );
    const poller = createPoller(task, 1000);
    poller.start();
    await vi.advanceTimersByTimeAsync(5000);
    expect(task).toHaveBeenCalledTimes(1);
    release();
    await vi.advanceTimersByTimeAsync(1000);
    expect(task).toHaveBeenCalledTimes(2);
    poller.stop();
  });

  it("skips ticks while hidden and resumes when visible", async () => {
    const task = vi.fn(async () => undefined);
    const poller = createPoller(task, 1000);
    poller.start();
    setHidden(true);
    await vi.advanceTimersByTimeAsync(3000);
    expect(task).not.toHaveBeenCalled();
    setHidden(false);
    await vi.advanceTimersByTimeAsync(0);
    expect(task).toHaveBeenCalledTimes(1);
    poller.stop();
  });

  it("reports task failures and keeps polling", async () => {
    const onError = vi.fn();
    const task = vi.fn(async () => {
      throw new Error("boom");
    });
    const poller = createPoller(task, 1000, onError);
    poller.start();
    await vi.advanceTimersByTimeAsync(2000);
    expect(onError).toHaveBeenCalledTimes(2);
    poller.stop();
  });
});

describe("neutralise", () => {
  it("makes bidirectional controls visible", () => {
    expect(neutralise("a\u202eb")).toBe("a[U+202E]b");
    expect(neutralise("plain")).toBe("plain");
  });
});
