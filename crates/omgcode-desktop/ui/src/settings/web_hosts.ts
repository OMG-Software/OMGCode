import { button, el } from "../dom";
import type { WorkspaceSettingsView } from "../ipc/types";
import { iconButton } from "../ui/icons";
import type { SectionContext } from "./context";

/** The most host patterns one workspace may list. Core enforces it; the form shows it. */
export const MAX_WEB_HOSTS = 32;

/** Shown above the list: what a listed host changes and what it does not. */
export const WEB_HOSTS_HELP =
  "A host here skips approval only while Web fetch is Allow. Use an exact name, such as " +
  "docs.example.com, or *.example.com for every subdomain of example.com but not example.com " +
  "itself. A system dialog asks before a host is added or removed.";

export const WEB_HOSTS_EMPTY = "No host is listed. Every fetch asks first.";
export const ADD_HOST_LABEL = "Add";
export const HOST_INPUT_LABEL = "Web host to allow";
export const HOSTS_FULL = `This workspace lists ${MAX_WEB_HOSTS} hosts, the most allowed. Remove one to add another.`;

/** The `web_host_*` codes Rust returns, as sentences for the operator. */
const WEB_HOST_MESSAGES: Readonly<Record<string, string>> = {
  web_host_empty: "Type a host name first.",
  web_host_not_ascii: "A host name must be ASCII. Use punycode (xn--) for an international name.",
  web_host_too_long: "A host pattern may be at most 253 bytes.",
  web_host_bad_label:
    "Each part of a host name must be 1 to 63 characters and must not start or end with a hyphen.",
  web_host_bad_character:
    "Use only letters, digits, hyphens, dots, and one leading *. for subdomains.",
  web_host_ip_literal: "An IP address is not a host name. Use the site's DNS name.",
  web_host_reserved: "Local and private names, such as localhost, cannot be allowed.",
  web_host_too_few_labels: "A host needs at least two parts, such as example.com.",
  web_host_not_canonical: "The stored host pattern is not in its canonical form.",
  web_host_duplicate: "That host is already on the list.",
  web_host_not_listed: "That host is not on the list.",
  web_host_too_many: `A workspace may list at most ${MAX_WEB_HOSTS} hosts. Remove one first.`,
};

/** The sentence for a `web_host_*` code, or `null` for any other code. */
export function webHostMessage(code: string): string | null {
  return Object.hasOwn(WEB_HOST_MESSAGES, code) ? (WEB_HOST_MESSAGES[code] ?? null) : null;
}

/** Text the list shows for the number of hosts, such as `3 of 32`. */
export function hostCount(count: number): string {
  return `${count} of ${MAX_WEB_HOSTS}`;
}

function hostRow(
  workspace: WorkspaceSettingsView,
  pattern: string,
  { submit, actions }: SectionContext,
): HTMLElement {
  const remove = iconButton("cross", `Remove ${pattern}`, () =>
    submit(actions.removeWebFetchHost(workspace.workspace_id, pattern)),
  );
  return el("li", { class: "row web-host" }, el("span", { class: "root", text: pattern }), remove);
}

/**
 * The workspace's allowed web hosts: each pattern with a remove button, and a field with an Add
 * button. Rust parses the pattern, names the folder and the pattern in a native confirmation,
 * and applies the change with the revision this window read; the window only sends the typed
 * text. What the operator typed stays in view state until an add succeeds.
 */
export function webHostsBlock(workspace: WorkspaceSettingsView, ctx: SectionContext): HTMLElement {
  const { view, submit, actions } = ctx;
  const id = workspace.workspace_id;
  const full = workspace.web_fetch_hosts.length >= MAX_WEB_HOSTS;
  const input = el("input", {
    attrs: {
      id: `web-host-input-${id}`,
      type: "text",
      "aria-label": HOST_INPUT_LABEL,
      placeholder: "docs.example.com or *.example.com",
      spellcheck: "false",
      autocomplete: "off",
    },
  });
  input.value = view.webHostDrafts.get(id) ?? "";
  input.disabled = full;
  input.addEventListener("input", () => {
    view.webHostDrafts.set(id, input.value);
  });
  const add = button(ADD_HOST_LABEL, () => {
    const pattern = input.value.trim();
    const change = actions.addWebFetchHost(id, pattern);
    submit(async (revision) => {
      const result = await change(revision);
      view.webHostDrafts.delete(id);
      return result;
    });
  });
  add.disabled = full;
  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      add.click();
    }
  });
  return el(
    "div",
    { class: "web-hosts" },
    el(
      "div",
      { class: "row-actions" },
      el("span", { class: "perm-label", text: "Allowed web hosts" }),
      el("span", {
        class: "muted web-host-count",
        text: hostCount(workspace.web_fetch_hosts.length),
      }),
    ),
    el("p", { class: "perm-help", text: WEB_HOSTS_HELP }),
    workspace.web_fetch_hosts.length === 0
      ? el("p", { class: "empty", text: WEB_HOSTS_EMPTY })
      : el(
          "ul",
          { class: "row-list web-host-list" },
          ...workspace.web_fetch_hosts.map((pattern) => hostRow(workspace, pattern, ctx)),
        ),
    el("div", { class: "row-actions" }, input, add),
    full ? el("p", { class: "notice", text: HOSTS_FULL }) : null,
  );
}
