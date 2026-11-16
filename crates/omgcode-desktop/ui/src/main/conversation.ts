import { append, button, el, replaceChildren } from "../dom";
import { formatUpdated } from "../format";
import { api } from "../ipc/client";
import { toIpcError } from "../ipc/errors";
import type {
  ChatSettings,
  ChatStatus,
  SessionSummary,
  TranscriptEntry,
  TranscriptPage,
} from "../ipc/types";
import { createChatRunner } from "./chat_runner";
import { createComposer } from "./composer";
import {
  composerState,
  type LoadFailure,
  outcomeLine,
  sessionActions,
  statusLabel,
  TRANSCRIPT_PAGE_SIZE,
} from "./model";
import { renderEntry } from "./transcript";

const MAX_PAGES_PER_LOAD = 50;

export interface ConversationDeps {
  /** Called when the session list should be reloaded (status, title or archive changes). */
  onTreeChanged(): void;
}

export interface Conversation {
  element: HTMLElement;
  /** Opens a session and, when `seq` is on a loaded page, scrolls to that entry. */
  open(session: SessionSummary, folder: string, seq: number | null): Promise<void>;
  currentId(): string | null;
  dispose(): void;
}

interface Notice {
  text: string;
  error: boolean;
}

/** Builds the conversation pane: header, transcript, paging, and composer. */
export function createConversation(deps: ConversationDeps): Conversation {
  let session: SessionSummary | null = null;
  let folder = "";
  let entries: TranscriptEntry[] = [];
  let nextAfter: number | null = null;
  let incompleteTail = false;
  let failure: LoadFailure = null;
  let failureMessage = "";
  let confirmInterrupt = false;
  let chatState: ChatStatus["state"] = "idle";
  let loadToken = 0;
  let highlightSeq: number | null = null;

  const runner = createChatRunner({ onStatus: handleChatStatus });
  const composer = createComposer({
    onSend: (message, settings) => void send(message, settings),
    onCancel: () => void cancelChat(),
  });

  const header = el("header", { class: "pane-header" });
  const transcript = el("div", { class: "transcript", attrs: { tabindex: "-1" } });
  const noticeLine = el("p", { class: "notice", attrs: { role: "status" } });
  const empty = el(
    "div",
    { class: "pane-empty" },
    el("p", { text: "Select a session, or create a new one." }),
  );
  const content = el(
    "div",
    { class: "pane-content" },
    header,
    transcript,
    noticeLine,
    composer.element,
  );
  content.hidden = true;
  const element = el(
    "main",
    { class: "pane", attrs: { "aria-label": "Conversation" } },
    empty,
    content,
  );

  function chatActive(): boolean {
    return chatState === "running" || chatState === "cancelling";
  }

  function setNotice(next: Notice | null): void {
    replaceChildren(noticeLine, next?.text ?? "");
    noticeLine.hidden = next === null;
    noticeLine.classList.toggle("error", next?.error === true);
  }

  function renderHeader(): void {
    if (session === null) {
      return;
    }
    const current = session;
    const actions = sessionActions(current, chatActive());
    const label = statusLabel(current.status);
    const buttons = el("div", { class: "actions" });
    if (actions.archive) {
      append(
        buttons,
        button("Archive", () => void runAction(() => api.sessionArchive(current.session_id))),
      );
    }
    if (actions.restore) {
      append(
        buttons,
        button("Restore", () => void runAction(() => api.sessionRestore(current.session_id))),
      );
    }
    if (actions.interrupt && !confirmInterrupt) {
      append(
        buttons,
        button("Interrupt", () => {
          confirmInterrupt = true;
          renderHeader();
        }),
      );
    }
    if (actions.interrupt && confirmInterrupt) {
      append(
        buttons,
        el("span", { class: "confirm-text", text: "Interrupt the unfinished turn?" }),
        button(
          "Confirm interrupt",
          () => void runAction(() => api.sessionInterrupt(current.session_id)),
          {
            class: "danger",
          },
        ),
        button("Keep", () => {
          confirmInterrupt = false;
          renderHeader();
        }),
      );
    }
    if (actions.cancel) {
      append(
        buttons,
        button("Cancel", () => void cancelChat()),
      );
    }
    replaceChildren(
      header,
      el(
        "div",
        { class: "pane-heading" },
        el("h2", { class: "clamp-2", text: current.title }),
        el(
          "div",
          { class: "pane-sub" },
          el("span", { class: "pane-folder clamp-1", text: folder }),
          label === null ? null : el("span", { class: `chip chip-${current.status}`, text: label }),
          current.archived ? el("span", { class: "chip chip-archived", text: "Archived" }) : null,
          el("span", { text: `Updated ${formatUpdated(current.updated_at)}` }),
        ),
        el(
          "details",
          { class: "pane-details" },
          el("summary", { text: "Details" }),
          el(
            "div",
            { class: "pane-details-body" },
            el("p", { class: "full-text pane-full-title", text: current.title }),
            el("p", { class: "full-text pane-full-folder", text: folder }),
          ),
        ),
      ),
      buttons,
    );
  }

  function renderBody(): void {
    const nodes: Node[] = [];
    if (failure !== null && session !== null) {
      const explain =
        failure === "busy"
          ? "Another process is writing this session, so it cannot be shown now."
          : "This session journal failed validation. It is read-only and cannot be shown.";
      const panel = el(
        "div",
        { class: "panel" },
        el("p", { text: explain }),
        el("p", { class: "muted", text: failureMessage }),
      );
      if (failure === "invalid" && session.invalid_code !== null) {
        append(panel, el("p", { class: "muted", text: `Code: ${session.invalid_code}` }));
      }
      nodes.push(panel);
    } else if (failureMessage !== "") {
      nodes.push(el("p", { class: "error", text: failureMessage }));
    } else {
      if (incompleteTail) {
        nodes.push(
          el("p", { class: "banner", text: "The end of this journal is still being written." }),
        );
      }
      if (entries.length === 0) {
        nodes.push(el("p", { class: "muted", text: "This session has no entries yet." }));
      }
      for (const entry of entries) {
        nodes.push(renderEntry(entry));
      }
      if (nextAfter !== null) {
        nodes.push(button("Load more", () => void loadMore(), { class: "load-more" }));
      }
    }
    replaceChildren(transcript, ...nodes);
    renderComposer();
    if (highlightSeq !== null) {
      scrollToSeq(highlightSeq);
      highlightSeq = null;
    }
  }

  function renderComposer(): void {
    composer.update(composerState(session, chatActive(), failure), chatActive());
  }

  function scrollToSeq(seq: number): void {
    const target = Array.from(transcript.querySelectorAll<HTMLElement>("[data-seq]")).find(
      (node) => node.dataset.seq === String(seq),
    );
    if (target !== undefined) {
      target.classList.add("highlight");
      if (typeof target.scrollIntoView === "function") {
        target.scrollIntoView({ block: "center" });
      }
    }
  }

  async function fetchPages(minEntries: number, toEnd: boolean): Promise<TranscriptPage | null> {
    const token = ++loadToken;
    const id = session?.session_id;
    if (id === undefined) {
      return null;
    }
    const collected: TranscriptEntry[] = [];
    let after: number | null = null;
    let page: TranscriptPage;
    let pages = 0;
    try {
      do {
        page = await api.sessionTranscript(id, after, TRANSCRIPT_PAGE_SIZE);
        if (token !== loadToken) {
          return null;
        }
        collected.push(...page.entries);
        after = page.next_after_seq;
        pages += 1;
      } while (
        after !== null &&
        (toEnd || collected.length < minEntries) &&
        pages < MAX_PAGES_PER_LOAD
      );
    } catch (reason) {
      if (token === loadToken) {
        applyFailure(reason);
      }
      return null;
    }
    failure = null;
    failureMessage = "";
    session = page.session;
    folder = page.folder_display;
    entries = collected;
    nextAfter = after;
    incompleteTail = page.incomplete_tail;
    return page;
  }

  function applyFailure(reason: unknown): void {
    const error = toIpcError(reason);
    entries = [];
    nextAfter = null;
    incompleteTail = false;
    failureMessage = `${error.message} (${error.code})`;
    failure = error.code === "busy" ? "busy" : error.code === "invalid_journal" ? "invalid" : null;
    renderHeader();
    renderBody();
  }

  async function reload(toEnd: boolean): Promise<void> {
    const page = await fetchPages(entries.length, toEnd);
    if (page !== null) {
      renderHeader();
      renderBody();
      if (toEnd) {
        transcript.scrollTop = transcript.scrollHeight;
      }
    }
  }

  async function loadMore(): Promise<void> {
    if (session === null || nextAfter === null) {
      return;
    }
    const token = ++loadToken;
    try {
      const page = await api.sessionTranscript(session.session_id, nextAfter, TRANSCRIPT_PAGE_SIZE);
      if (token !== loadToken) {
        return;
      }
      entries = entries.concat(page.entries);
      nextAfter = page.next_after_seq;
      incompleteTail = page.incomplete_tail;
      session = page.session;
      renderHeader();
      renderBody();
    } catch (reason) {
      if (token === loadToken) {
        setNotice({ text: toIpcError(reason).message, error: true });
      }
    }
  }

  async function runAction(action: () => Promise<SessionSummary>): Promise<void> {
    confirmInterrupt = false;
    try {
      session = await action();
      setNotice(null);
      renderHeader();
      deps.onTreeChanged();
      await reload(false);
    } catch (reason) {
      const error = toIpcError(reason);
      setNotice({ text: `${error.message} (${error.code})`, error: true });
      renderHeader();
    }
  }

  async function send(message: string, settings: ChatSettings): Promise<void> {
    if (session === null) {
      return;
    }
    setNotice(null);
    try {
      await runner.send(session.session_id, message, settings);
      composer.clear();
    } catch (reason) {
      const error = toIpcError(reason);
      setNotice({ text: `${error.message} (${error.code})`, error: true });
    }
  }

  async function cancelChat(): Promise<void> {
    if (session === null) {
      return;
    }
    try {
      await runner.cancel(session.session_id);
    } catch (reason) {
      const error = toIpcError(reason);
      setNotice({ text: `${error.message} (${error.code})`, error: true });
    }
  }

  function handleChatStatus(status: ChatStatus): void {
    const settled = status.state !== "running" && status.state !== "cancelling";
    if (session === null || status.session_id !== session.session_id) {
      if (settled) {
        deps.onTreeChanged();
      }
      return;
    }
    chatState = status.state;
    if (!settled) {
      session = { ...session, status: "running" };
      renderHeader();
      renderComposer();
      return;
    }
    if (status.state === "failed" && status.error !== null) {
      setNotice({ text: `${status.error.message} (${status.error.code})`, error: true });
    } else if (status.outcome !== null) {
      setNotice({ text: outcomeLine(status.outcome), error: false });
    }
    deps.onTreeChanged();
    void reload(true);
  }

  async function open(next: SessionSummary, nextFolder: string, seq: number | null): Promise<void> {
    const same = session?.session_id === next.session_id;
    session = next;
    folder = nextFolder;
    confirmInterrupt = false;
    highlightSeq = seq;
    if (!same) {
      entries = [];
      setNotice(null);
      chatState = runner.isActive(next.session_id) ? "running" : "idle";
    }
    empty.hidden = true;
    content.hidden = false;
    renderHeader();
    const page = await fetchPages(1, false);
    if (page !== null) {
      renderHeader();
      renderBody();
      if (session?.status === "running" && !runner.isActive(session.session_id)) {
        runner.watch(session.session_id);
      }
    }
  }

  return {
    element,
    open,
    currentId: () => session?.session_id ?? null,
    dispose: () => runner.dispose(),
  };
}
