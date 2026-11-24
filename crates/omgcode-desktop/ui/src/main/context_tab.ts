import { el, replaceChildren } from "../dom";
import { api } from "../ipc/client";
import { toIpcError } from "../ipc/errors";
import type { ContextBreakdown, ContextPart, ContextPartId } from "../ipc/types";
import type { PanelView } from "./panel_tab";

/** What the tab says while no session is open. */
export const CONTEXT_EMPTY = "Open a session to see how its context is made up.";
/** What the tab says while the first estimate loads. */
export const CONTEXT_LOADING = "Estimating the context...";
/** What the tab states about the estimate. */
export const CONTEXT_SCOPE =
  "Conservative estimate before the next turn: one byte counts as one token, and your next message is not included.";

const PART_LABELS: Readonly<Record<ContextPartId, string>> = {
  system: "System prompt",
  global_instructions: "Global instructions",
  workspace_instructions: "Workspace instructions",
  tools: "Tool definitions",
  summary: "Compaction summary",
  history: "Conversation history",
  skills: "Selected skills",
  framing: "Message framing",
};

/** Why a part is not included, by the fixed code Rust reports. */
const REASON_LABELS: Readonly<Record<string, string>> = {
  none_saved: "None saved",
  no_compaction: "No compaction yet",
  selected_per_turn: "Added only when selected for a turn",
  preference_disabled: "Model tools are off for this workspace",
  workspace_unregistered: "Workspace is not registered",
  settings_absent: "No settings saved yet",
  platform_unsupported: "Model tools are not supported here",
  session_schema_v1: "This session predates tool support",
};

const NOT_INCLUDED = "Not included";
const BAR_WIDTH = 1000;
const BAR_HEIGHT = 24;
const SVG_NS = "http://www.w3.org/2000/svg";

const NUMBER = new Intl.NumberFormat("en-US");

function tokens(value: number): string {
  return NUMBER.format(Math.max(0, Math.round(value)));
}

function percent(value: number, whole: number): string {
  if (!(whole > 0) || !Number.isFinite(value)) {
    return "0%";
  }
  const share = (value / whole) * 100;
  return share > 0 && share < 0.1 ? "<0.1%" : `${share.toFixed(1)}%`;
}

function partLabel(id: ContextPartId): string {
  return PART_LABELS[id] ?? id;
}

function noteOf(part: ContextPart): string {
  if (!part.included) {
    return (part.reason === undefined ? undefined : REASON_LABELS[part.reason]) ?? NOT_INCLUDED;
  }
  if (part.id === "history" && part.messages !== undefined) {
    return `${part.messages} ${part.messages === 1 ? "message" : "messages"} after the compaction boundary`;
  }
  return "";
}

function svg(tag: "svg" | "rect" | "line" | "title"): SVGElement {
  return document.createElementNS(SVG_NS, tag);
}

function setNumbers(node: SVGElement, values: Readonly<Record<string, number>>): void {
  for (const [name, value] of Object.entries(values)) {
    node.setAttribute(name, String(Math.round(value * 100) / 100));
  }
}

/** The stacked bar: one rect per included part, and the compaction trigger as a marker. */
function buildBar(data: ContextBreakdown, scale: number): SVGElement {
  const bar = svg("svg");
  bar.setAttribute("viewBox", `0 0 ${BAR_WIDTH} ${BAR_HEIGHT}`);
  bar.setAttribute("preserveAspectRatio", "none");
  bar.setAttribute("role", "img");
  bar.setAttribute(
    "aria-label",
    `${tokens(data.estimated_input_tokens)} estimated input tokens, shown by part in the table below`,
  );
  bar.classList.add("ctx-bar");
  const track = svg("rect");
  setNumbers(track, { x: 0, y: 0, width: BAR_WIDTH, height: BAR_HEIGHT });
  track.classList.add("ctx-track");
  bar.appendChild(track);
  let used = 0;
  for (const part of data.parts) {
    if (!part.included || !(part.tokens > 0)) {
      continue;
    }
    const rect = svg("rect");
    setNumbers(rect, {
      x: (used / scale) * BAR_WIDTH,
      y: 0,
      width: (part.tokens / scale) * BAR_WIDTH,
      height: BAR_HEIGHT,
    });
    rect.classList.add("ctx-seg", `ctx-fill-${part.id}`);
    const title = svg("title");
    title.textContent = `${partLabel(part.id)}: ${tokens(part.tokens)} tokens`;
    rect.appendChild(title);
    bar.appendChild(rect);
    used += part.tokens;
  }
  if (data.effective_trigger > 0 && data.effective_trigger <= scale) {
    const x = (data.effective_trigger / scale) * BAR_WIDTH;
    const marker = svg("line");
    setNumbers(marker, { x1: x, x2: x, y1: 0, y2: BAR_HEIGHT });
    marker.classList.add("ctx-trigger");
    const title = svg("title");
    title.textContent = `Compaction trigger: ${tokens(data.effective_trigger)} tokens`;
    marker.appendChild(title);
    bar.appendChild(marker);
  }
  return bar;
}

function row(label: string, value: string): HTMLElement {
  return el("div", { class: "ctx-total" }, el("dt", { text: label }), el("dd", { text: value }));
}

function buildTable(data: ContextBreakdown): HTMLTableElement {
  const body = el("tbody");
  for (const part of data.parts) {
    const swatch = el("span", {
      class: part.included ? `ctx-swatch ctx-fill-${part.id}` : "ctx-swatch ctx-swatch-off",
      attrs: { "aria-hidden": "true" },
    });
    body.appendChild(
      el(
        "tr",
        {
          class: part.included ? "ctx-row" : "ctx-row ctx-row-off",
          attrs: { "data-part": part.id },
        },
        el("th", { attrs: { scope: "row" } }, swatch, el("span", { text: partLabel(part.id) })),
        el("td", { class: "ctx-num", text: part.included ? tokens(part.tokens) : "0" }),
        el("td", {
          class: "ctx-num",
          text: part.included ? percent(part.tokens, data.estimated_input_tokens) : "0%",
        }),
        el("td", { class: "ctx-note", text: noteOf(part) }),
      ),
    );
  }
  body.appendChild(
    el(
      "tr",
      { class: "ctx-row ctx-sum", attrs: { "data-part": "total" } },
      el("th", { attrs: { scope: "row" }, text: "Estimated input" }),
      el("td", { class: "ctx-num", text: tokens(data.estimated_input_tokens) }),
      el("td", { class: "ctx-num", text: data.estimated_input_tokens > 0 ? "100%" : "0%" }),
      el("td", { class: "ctx-note" }),
    ),
  );
  return el(
    "table",
    { class: "ctx-table" },
    el(
      "thead",
      {},
      el(
        "tr",
        {},
        el("th", { attrs: { scope: "col" }, text: "Part" }),
        el("th", { class: "ctx-num", attrs: { scope: "col" }, text: "Tokens" }),
        el("th", { class: "ctx-num", attrs: { scope: "col" }, text: "Share" }),
        el("th", { attrs: { scope: "col" }, text: "Note" }),
      ),
    ),
    body,
  );
}

function buildTotals(data: ContextBreakdown): HTMLElement {
  const capacity = data.context_tokens;
  const headroom = data.effective_trigger - data.estimated_input_tokens;
  return el(
    "dl",
    { class: "ctx-totals" },
    row(
      "Context capacity",
      capacity === null ? "Unknown (not saved in settings)" : `${tokens(capacity)} tokens`,
    ),
    row("Estimated input", `${tokens(data.estimated_input_tokens)} tokens`),
    row("Output reserve", `${tokens(data.max_output_tokens)} tokens`),
    row(
      "Compaction trigger",
      `${tokens(data.effective_trigger)} tokens, auto-compaction ${data.auto_compaction ? "on" : "off"}`,
    ),
    row(
      headroom >= 0 ? "Room before the trigger" : "Over the trigger by",
      `${tokens(Math.abs(headroom))} tokens`,
    ),
  );
}

/** Renders one estimate. The bar's scale is the capacity, or the larger of estimate and trigger. */
function buildReport(data: ContextBreakdown): HTMLElement {
  const known = data.context_tokens !== null && data.context_tokens > 0;
  const scale = Math.max(
    (known ? data.context_tokens : null) ?? 0,
    data.estimated_input_tokens,
    data.effective_trigger,
    1,
  );
  const capacityText = known
    ? `${percent(data.estimated_input_tokens, data.context_tokens ?? 0)} of the ${tokens(data.context_tokens ?? 0)}-token context`
    : "context capacity unknown";
  const legend = el(
    "p",
    { class: "ctx-bar-note" },
    `Bar: ${tokens(data.estimated_input_tokens)} tokens, ${capacityText}. The line marks the compaction trigger at ${tokens(data.effective_trigger)}.`,
  );
  return el(
    "div",
    { class: "ctx-report" },
    buildBar(data, scale),
    legend,
    buildTable(data),
    buildTotals(data),
    el("p", { class: "ctx-scope", text: CONTEXT_SCOPE }),
  );
}

/**
 * Builds the Context tab: a stacked bar and a table of the parts of the open session's next
 * turn (ADR-0087). It reads one command and renders numbers and fixed labels only, as text
 * nodes. It starts nothing and writes nothing. The host calls `showSession` when the open
 * session changes and `refresh` after a turn ends.
 */
export function createContextTab(): PanelView {
  let sessionId: string | null = null;
  let generation = 0;
  let disposed = false;

  const refreshButton = el("button", { class: "ctx-refresh", text: "Refresh" });
  refreshButton.type = "button";
  const body = el("div", { class: "ctx-body", attrs: { "aria-live": "polite" } });
  const element = el(
    "section",
    { class: "context-tab", attrs: { "aria-label": "Context breakdown" } },
    el("div", { class: "ctx-header" }, el("h2", { text: "Context" }), refreshButton),
    body,
  );

  function showEmpty(): void {
    refreshButton.disabled = true;
    body.removeAttribute("aria-busy");
    replaceChildren(body, el("p", { class: "ctx-empty", text: CONTEXT_EMPTY }));
  }

  function load(): void {
    generation += 1;
    const mine = generation;
    if (sessionId === null) {
      showEmpty();
      return;
    }
    const target = sessionId;
    refreshButton.disabled = false;
    body.setAttribute("aria-busy", "true");
    if (body.querySelector(".ctx-report") === null) {
      replaceChildren(body, el("p", { class: "ctx-empty", text: CONTEXT_LOADING }));
    }
    api.sessionContextBreakdown(target).then(
      (data) => {
        if (disposed || mine !== generation) {
          return;
        }
        body.removeAttribute("aria-busy");
        replaceChildren(body, buildReport(data));
      },
      (reason: unknown) => {
        if (disposed || mine !== generation) {
          return;
        }
        body.removeAttribute("aria-busy");
        replaceChildren(
          body,
          el("p", {
            class: "ctx-error",
            attrs: { role: "alert" },
            text: `The context could not be estimated: ${toIpcError(reason).message}`,
          }),
        );
      },
    );
  }

  refreshButton.addEventListener("click", load);
  showEmpty();

  return {
    element,
    showSession(next) {
      if (disposed) {
        return;
      }
      if (next !== sessionId) {
        // Another session's report must not stay up while this one loads; a
        // refresh of the same session keeps its report until the answer.
        replaceChildren(body);
      }
      sessionId = next;
      load();
    },
    refresh() {
      if (!disposed && sessionId !== null) {
        load();
      }
    },
    dispose() {
      disposed = true;
      generation += 1;
    },
  };
}
