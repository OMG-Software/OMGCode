import { afterEach, describe, expect, it, vi } from "vitest";
import {
  clearSizeProperty,
  MAX_SECTION_SIZE,
  type SizeProperty,
  setSizeProperty,
} from "../src/ui/size_property";
import {
  createSplitter,
  KEY_STEP,
  LARGE_KEY_STEP,
  type SplitAxis,
  type SplitOrientation,
  type Splitter,
  type SplitterOptions,
} from "../src/ui/splitter";

const WIDTH: SplitAxis = {
  property: "--queue-width",
  min: 100,
  max: 500,
  initial: 200,
  neighbourMin: 150,
};
const HEIGHT: SplitAxis = {
  property: "--message-height",
  min: 40,
  max: 400,
  initial: null,
  neighbourMin: 100,
};

let made: Splitter[] = [];

afterEach(() => {
  for (const splitter of made) {
    splitter.dispose();
  }
  made = [];
  document.body.replaceChildren();
  vi.unstubAllGlobals();
});

/** Stands in for the browser's size observer and frame scheduler, which jsdom lacks. */
function stubFrames(): { observed: () => void; frames: Map<number, () => void> } {
  let callback: (() => void) | null = null;
  vi.stubGlobal(
    "ResizeObserver",
    class {
      constructor(observe: () => void) {
        callback = observe;
      }
      observe(): void {}
      disconnect(): void {
        callback = null;
      }
    },
  );
  const frames = new Map<number, () => void>();
  let next = 1;
  vi.stubGlobal("requestAnimationFrame", (run: () => void) => {
    frames.set(next, run);
    next += 1;
    return next - 1;
  });
  vi.stubGlobal("cancelAnimationFrame", (id: number) => frames.delete(id));
  return { observed: () => callback?.(), frames };
}

function parts(): { section: HTMLElement; neighbour: HTMLElement } {
  const section = document.createElement("div");
  section.id = "section";
  const neighbour = document.createElement("div");
  document.body.append(section, neighbour);
  return { section, neighbour };
}

function make(options: Partial<SplitterOptions> & Pick<SplitterOptions, "section" | "neighbour">) {
  const splitter = createSplitter({ label: "list", placement: "after", ...options });
  options.section.after(splitter.element);
  made.push(splitter);
  return splitter;
}

/** Gives an element a laid-out size, which jsdom never computes. */
function sized(element: HTMLElement, width: number, height: number): void {
  element.getBoundingClientRect = () => new DOMRect(0, 0, width, height);
}

function key(target: HTMLElement, name: string, shiftKey = false): void {
  target.dispatchEvent(new KeyboardEvent("keydown", { key: name, shiftKey, bubbles: true }));
}

/**
 * Dispatches a pointer event as a browser would for the primary button: a press and the moves
 * after it report the button held, a release reports none, and a move changes no button.
 */
function pointer(
  target: EventTarget,
  type: string,
  x: number,
  y: number,
  options: { button?: number; buttons?: number; pointerId?: number } = {},
): void {
  const held = type === "pointerdown" || type === "pointermove";
  const init: PointerEventInit = {
    clientX: x,
    clientY: y,
    button: options.button ?? (type === "pointermove" ? -1 : 0),
    buttons: options.buttons ?? (held ? 1 : 0),
    pointerId: options.pointerId ?? 1,
    bubbles: true,
  };
  target.dispatchEvent(new PointerEvent(type, init));
}

describe("a splitter", () => {
  it("needs an axis and a section with an id", () => {
    const { section, neighbour } = parts();
    expect(() => createSplitter({ label: "x", section, neighbour, placement: "after" })).toThrow(
      "axis",
    );
    section.id = "";
    expect(() =>
      createSplitter({ label: "x", section, neighbour, placement: "after", vertical: WIDTH }),
    ).toThrow("id");
  });

  it("is a focusable window splitter for its section", () => {
    const { section, neighbour } = parts();
    const splitter = make({ section, neighbour, vertical: WIDTH });
    const element = splitter.element;
    expect(element.getAttribute("role")).toBe("separator");
    expect(element.tabIndex).toBe(0);
    expect(element.getAttribute("aria-label")).toBe("Resize the list");
    expect(element.getAttribute("aria-controls")).toBe("section");
    expect(element.getAttribute("aria-orientation")).toBe("vertical");
    expect(element.getAttribute("aria-valuemin")).toBe("100");
    expect(element.getAttribute("aria-valuemax")).toBe("500");
    expect(element.getAttribute("aria-valuenow")).toBe("200");
    expect(element.getAttribute("aria-valuetext")).toBe("200 pixels wide");
    expect(element.classList.contains("splitter-vertical")).toBe(true);
    expect(section.style.getPropertyValue("--queue-width")).toBe("200px");
  });

  it("moves by a step, or a large step with Shift, and stops at the limits", () => {
    const { section, neighbour } = parts();
    const splitter = make({ section, neighbour, vertical: WIDTH });
    key(splitter.element, "ArrowRight");
    expect(splitter.size()).toBe(200 + KEY_STEP);
    key(splitter.element, "ArrowLeft", true);
    expect(splitter.size()).toBe(200 + KEY_STEP - LARGE_KEY_STEP);
    key(splitter.element, "Home");
    expect(splitter.size()).toBe(100);
    key(splitter.element, "ArrowLeft");
    expect(splitter.size()).toBe(100);
    key(splitter.element, "End");
    expect(splitter.size()).toBe(500);
    key(splitter.element, "ArrowRight");
    expect(splitter.size()).toBe(500);
    expect(section.style.getPropertyValue("--queue-width")).toBe("500px");
    // Up and down belong to a horizontal splitter; other keys are left alone.
    key(splitter.element, "ArrowUp");
    key(splitter.element, "Enter");
    expect(splitter.size()).toBe(500);
  });

  it("grows a section it comes before with the key that moves it away", () => {
    const { section, neighbour } = parts();
    const splitter = make({
      section,
      neighbour,
      placement: "before",
      horizontal: { ...HEIGHT, initial: 100 },
    });
    expect(splitter.element.getAttribute("aria-orientation")).toBe("horizontal");
    key(splitter.element, "ArrowUp");
    expect(splitter.size()).toBe(100 + KEY_STEP);
    key(splitter.element, "ArrowDown", true);
    expect(splitter.size()).toBe(100 + KEY_STEP - LARGE_KEY_STEP);
  });

  it("follows a drag past its own edge and ends it on release", () => {
    const { section, neighbour } = parts();
    const splitter = make({ section, neighbour, vertical: WIDTH });
    pointer(splitter.element, "pointerdown", 300, 0);
    expect(document.documentElement.classList.contains("resizing-columns")).toBe(true);
    expect(document.activeElement).toBe(splitter.element);
    pointer(document, "pointermove", 340, 0);
    expect(splitter.size()).toBe(240);
    pointer(document, "pointermove", 0, 0);
    expect(splitter.size()).toBe(100);
    pointer(document, "pointerup", 0, 0);
    expect(document.documentElement.classList.contains("resizing-columns")).toBe(false);
    pointer(document, "pointermove", 400, 0);
    expect(splitter.size()).toBe(100);
  });

  it("drags a horizontal splitter by its vertical movement", () => {
    const { section, neighbour } = parts();
    const splitter = make({
      section,
      neighbour,
      placement: "before",
      horizontal: { ...HEIGHT, initial: 100 },
    });
    pointer(splitter.element, "pointerdown", 0, 500);
    expect(document.documentElement.classList.contains("resizing-rows")).toBe(true);
    pointer(document, "pointermove", 999, 450);
    expect(splitter.size()).toBe(150);
    pointer(document, "pointercancel", 0, 0);
    expect(document.documentElement.classList.contains("resizing-rows")).toBe(false);
  });

  it("follows only the pointer that started the drag", () => {
    const { section, neighbour } = parts();
    const splitter = make({ section, neighbour, vertical: WIDTH });
    pointer(splitter.element, "pointerdown", 300, 0, { pointerId: 1 });
    pointer(document, "pointermove", 400, 0, { pointerId: 2 });
    expect(splitter.size()).toBe(200);
    pointer(document, "pointerup", 400, 0, { pointerId: 2 });
    pointer(document, "pointercancel", 400, 0, { pointerId: 2 });
    expect(splitter.element.classList.contains("dragging")).toBe(true);
    pointer(document, "pointermove", 340, 0, { pointerId: 1 });
    expect(splitter.size()).toBe(240);
    pointer(document, "pointerup", 340, 0, { pointerId: 1 });
    expect(splitter.element.classList.contains("dragging")).toBe(false);
  });

  it("cannot be taken over by a second pointer pressing on it", () => {
    const { section, neighbour } = parts();
    const splitter = make({ section, neighbour, vertical: WIDTH });
    pointer(splitter.element, "pointerdown", 300, 0, { pointerId: 1 });
    pointer(splitter.element, "pointerdown", 100, 0, { pointerId: 2 });
    pointer(document, "pointermove", 140, 0, { pointerId: 2 });
    expect(splitter.size()).toBe(200);
    // The owner's drag keeps its origin.
    pointer(document, "pointermove", 340, 0, { pointerId: 1 });
    expect(splitter.size()).toBe(240);
    // The owner pressing again restarts the drag from where it is.
    pointer(splitter.element, "pointerdown", 500, 0, { pointerId: 1 });
    pointer(document, "pointermove", 516, 0, { pointerId: 1 });
    expect(splitter.size()).toBe(256);
  });

  it("ignores a button other than the primary one", () => {
    const { section, neighbour } = parts();
    const splitter = make({ section, neighbour, vertical: WIDTH });
    pointer(splitter.element, "pointerdown", 300, 0, { button: 2, buttons: 2 });
    pointer(document, "pointermove", 400, 0, { buttons: 2 });
    expect(splitter.size()).toBe(200);
  });

  it("ends a drag whose release never arrived at the first move with the button up", () => {
    const { section, neighbour } = parts();
    const splitter = make({ section, neighbour, vertical: WIDTH });
    pointer(splitter.element, "pointerdown", 300, 0);
    pointer(document, "pointermove", 340, 0);
    expect(splitter.size()).toBe(240);
    pointer(document, "pointermove", 400, 0, { buttons: 0 });
    expect(splitter.size()).toBe(240);
    expect(splitter.element.classList.contains("dragging")).toBe(false);
    expect(document.documentElement.classList.contains("resizing-columns")).toBe(false);
    pointer(document, "pointermove", 450, 0);
    expect(splitter.size()).toBe(240);
  });

  it("captures the pointer and ends the drag when the capture is lost", () => {
    const { section, neighbour } = parts();
    const splitter = make({ section, neighbour, vertical: WIDTH });
    const captured = new Set<number>();
    const element = splitter.element;
    element.setPointerCapture = (id: number) => {
      captured.add(id);
    };
    element.hasPointerCapture = (id: number) => captured.has(id);
    element.releasePointerCapture = (id: number) => {
      captured.delete(id);
    };
    pointer(element, "pointerdown", 300, 0, { pointerId: 5 });
    expect(captured.has(5)).toBe(true);
    // Another pointer's lost capture leaves the drag alone.
    pointer(element, "lostpointercapture", 0, 0, { pointerId: 6 });
    expect(element.classList.contains("dragging")).toBe(true);
    // Released over another window: the browser drops the capture and sends no release here.
    captured.delete(5);
    pointer(element, "lostpointercapture", 0, 0, { pointerId: 5 });
    expect(element.classList.contains("dragging")).toBe(false);
    expect(document.documentElement.classList.contains("resizing-columns")).toBe(false);
    // Disposal mid-drag lets the capture go.
    pointer(element, "pointerdown", 300, 0, { pointerId: 5 });
    expect(captured.has(5)).toBe(true);
    splitter.dispose();
    expect(captured.has(5)).toBe(false);
  });

  it("still drags where the pointer cannot be captured", () => {
    const { section, neighbour } = parts();
    const splitter = make({ section, neighbour, vertical: WIDTH });
    splitter.element.setPointerCapture = () => {
      throw new DOMException("No active pointer.", "NotFoundError");
    };
    pointer(splitter.element, "pointerdown", 300, 0);
    pointer(document, "pointermove", 340, 0);
    expect(splitter.size()).toBe(240);
  });

  it("keeps the stylesheet's size until it moves, and a double click restores that", () => {
    const { section, neighbour } = parts();
    const splitter = make({ section, neighbour, placement: "before", horizontal: HEIGHT });
    expect(splitter.size()).toBeNull();
    expect(section.style.getPropertyValue("--message-height")).toBe("");
    // Unknown until laid out; read when the splitter gains focus.
    expect(splitter.element.hasAttribute("aria-valuenow")).toBe(false);
    sized(section, 400, 70);
    sized(neighbour, 400, 500);
    splitter.element.dispatchEvent(new FocusEvent("focus"));
    expect(splitter.element.getAttribute("aria-valuenow")).toBe("70");
    expect(splitter.element.getAttribute("aria-valuetext")).toBe("70 pixels tall");
    // A move starts from the laid-out size.
    key(splitter.element, "ArrowUp");
    expect(splitter.size()).toBe(70 + KEY_STEP);
    expect(section.style.getPropertyValue("--message-height")).toBe(`${70 + KEY_STEP}px`);
    splitter.element.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    expect(splitter.size()).toBeNull();
    expect(section.style.getPropertyValue("--message-height")).toBe("");
  });

  it("keeps the neighbour's minimum when the section grows", () => {
    const { section, neighbour } = parts();
    sized(section, 200, 600);
    sized(neighbour, 300, 600);
    const splitter = make({ section, neighbour, vertical: WIDTH });
    // 200 + 300 - 150 leaves the section at most 350 pixels.
    expect(splitter.element.getAttribute("aria-valuemax")).toBe("350");
    key(splitter.element, "End");
    expect(splitter.size()).toBe(350);
  });

  it("narrows the section when the neighbour shrinks below its minimum", () => {
    const { section, neighbour } = parts();
    const splitter = make({ section, neighbour, vertical: WIDTH });
    key(splitter.element, "End");
    expect(splitter.size()).toBe(500);
    sized(section, 500, 600);
    sized(neighbour, 120, 600);
    window.dispatchEvent(new Event("resize"));
    expect(splitter.size()).toBe(470);
    // Widening again leaves the size alone.
    sized(neighbour, 800, 600);
    window.dispatchEvent(new Event("resize"));
    expect(splitter.size()).toBe(470);
  });

  it("narrows until the neighbour keeps its minimum after the window shrinks past it", () => {
    const { section, neighbour } = parts();
    const splitter = make({ section, neighbour, vertical: WIDTH });
    key(splitter.element, "End");
    expect(splitter.size()).toBe(500);
    // A 400-pixel container: the section takes its width, the line one pixel, and the
    // neighbour what is left, which is nothing while the section is too wide.
    const width = () => Number.parseInt(section.style.getPropertyValue("--queue-width"), 10);
    section.getBoundingClientRect = () => new DOMRect(0, 0, width(), 600);
    neighbour.getBoundingClientRect = () => new DOMRect(0, 0, Math.max(0, 400 - width() - 1), 600);
    window.dispatchEvent(new Event("resize"));
    // 500 shows a 0-pixel neighbour, 350 a 49-pixel one, and 249 leaves it 150.
    expect(splitter.size()).toBe(400 - 1 - WIDTH.neighbourMin);
    expect(splitter.element.getAttribute("aria-valuenow")).toBe("249");
  });

  it("narrows a section still at the stylesheet's size, and leaves one that fits", () => {
    const { section, neighbour } = parts();
    const splitter = make({ section, neighbour, placement: "before", horizontal: HEIGHT });
    sized(section, 400, 70);
    sized(neighbour, 400, 500);
    window.dispatchEvent(new Event("resize"));
    expect(splitter.size()).toBeNull();
    expect(section.style.getPropertyValue("--message-height")).toBe("");
    // A short window: the content-sized section leaves its neighbour 50 of the 100 it keeps.
    sized(section, 400, 300);
    sized(neighbour, 400, 50);
    window.dispatchEvent(new Event("resize"));
    expect(splitter.size()).toBe(250);
    expect(section.style.getPropertyValue("--message-height")).toBe("250px");
  });

  it("keeps a hidden section's size while its neighbour is laid out", () => {
    const { section, neighbour } = parts();
    sized(neighbour, 300, 600);
    const splitter = make({ section, neighbour, vertical: WIDTH });
    // The section measures nothing, so only its own limits apply.
    expect(splitter.element.getAttribute("aria-valuemax")).toBe("500");
    key(splitter.element, "End");
    window.dispatchEvent(new Event("resize"));
    expect(splitter.size()).toBe(500);
  });

  it("narrows on the next frame when the neighbour's own size changes", () => {
    const { observed, frames } = stubFrames();
    const { section, neighbour } = parts();
    const splitter = make({ section, neighbour, vertical: WIDTH });
    key(splitter.element, "End");
    sized(section, 500, 600);
    sized(neighbour, 120, 600);
    observed();
    observed();
    // Not inside the observer's callback, and once however often it reports.
    expect(splitter.size()).toBe(500);
    expect(frames.size).toBe(1);
    for (const run of frames.values()) {
      run();
    }
    expect(splitter.size()).toBe(470);
  });

  it("cancels a pending fit when disposed", () => {
    const { observed, frames } = stubFrames();
    const { section, neighbour } = parts();
    const splitter = make({ section, neighbour, vertical: WIDTH });
    observed();
    expect(frames.size).toBe(1);
    splitter.dispose();
    expect(frames.size).toBe(0);
    observed();
    expect(frames.size).toBe(0);
  });

  it("follows its orientation, keeping a size for each", () => {
    const { section, neighbour } = parts();
    let orientation: SplitOrientation = "vertical";
    const splitter = make({
      section,
      neighbour,
      vertical: WIDTH,
      horizontal: { ...HEIGHT, property: "--queue-height", initial: 120 },
      orientation: () => orientation,
    });
    key(splitter.element, "ArrowRight");
    expect(splitter.size()).toBe(216);
    orientation = "horizontal";
    splitter.fit();
    expect(splitter.orientation()).toBe("horizontal");
    expect(splitter.element.getAttribute("aria-orientation")).toBe("horizontal");
    expect(splitter.element.classList.contains("splitter-horizontal")).toBe(true);
    expect(splitter.element.classList.contains("splitter-vertical")).toBe(false);
    expect(splitter.size()).toBe(120);
    key(splitter.element, "ArrowDown");
    expect(section.style.getPropertyValue("--queue-height")).toBe("136px");
    orientation = "vertical";
    splitter.fit();
    expect(splitter.size()).toBe(216);
  });

  it("uses the axis it has when asked for one it lacks", () => {
    const { section, neighbour } = parts();
    const splitter = make({ section, neighbour, vertical: WIDTH, orientation: () => "horizontal" });
    expect(splitter.orientation()).toBe("vertical");
  });

  it("stops following drags and the window once disposed", () => {
    const { section, neighbour } = parts();
    const splitter = make({ section, neighbour, vertical: WIDTH });
    key(splitter.element, "End");
    pointer(splitter.element, "pointerdown", 300, 0);
    splitter.dispose();
    expect(document.documentElement.classList.contains("resizing-columns")).toBe(false);
    pointer(document, "pointermove", 0, 0);
    expect(splitter.size()).toBe(500);
    sized(section, 500, 600);
    sized(neighbour, 10, 600);
    window.dispatchEvent(new Event("resize"));
    expect(splitter.size()).toBe(500);
  });
});

describe("the section size property", () => {
  it("writes whole pixels to a listed property and removes it", () => {
    const element = document.createElement("div");
    setSizeProperty(element, "--nav-width", 190);
    expect(element.style.getPropertyValue("--nav-width")).toBe("190px");
    clearSizeProperty(element, "--nav-width");
    expect(element.style.getPropertyValue("--nav-width")).toBe("");
  });

  it("refuses any other value or name", () => {
    const element = document.createElement("div");
    for (const pixels of [1.5, -1, MAX_SECTION_SIZE + 1, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(() => setSizeProperty(element, "--nav-width", pixels)).toThrow(RangeError);
    }
    const unlisted = "--color" as SizeProperty;
    expect(() => setSizeProperty(element, unlisted, 10)).toThrow(RangeError);
    expect(() => clearSizeProperty(element, unlisted)).toThrow(RangeError);
    expect(element.getAttribute("style") ?? "").toBe("");
  });
});
