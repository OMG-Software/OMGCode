import { button, el } from "../dom";
import type {
  ConnectionView,
  LocalProviderKind,
  ProviderProfileView,
  SettingsOverview,
} from "../ipc/types";
import { DEFAULT_ENDPOINTS, providerName } from "../providers";
import type { SectionContext } from "./context";
import { facts, sectionHead } from "./parts";

export const MODEL_HELP =
  "Choose where chat turns are sent. Choosing a connection asks you to confirm in a system dialog.";

/** The value of the dropdown entry that chooses the default endpoint. */
export const DEFAULT_CHOICE = "";

function connectionSource(connection: ConnectionView): string {
  switch (connection.source) {
    case "profile":
      return `Provider profile ${connection.profile ?? "(unnamed)"}, set at launch`;
    case "environment":
      return "Launch endpoint, token from OMGCODE_LM_TOKEN";
    case "active":
      return connection.profile === null
        ? "Default endpoint, token from OMGCODE_LM_TOKEN"
        : `Provider profile ${connection.profile}, chosen here`;
    case "injected":
      return "Supplied by the embedding application";
  }
}

/** The dropdown label of one selectable profile: its provider, name, and endpoint. */
export function choiceLabel(profile: ProviderProfileView): string {
  return `${providerName(profile.kind)}: ${profile.name} (${profile.endpoint ?? "no endpoint"})`;
}

/**
 * The provider dropdown. It lists the default endpoint and every selectable recorded profile;
 * choosing one asks the core to switch, and the core asks for a system confirmation first. When
 * a launch argument fixed the connection the dropdown is shown but disabled.
 */
function connectionPicker(
  overview: SettingsOverview,
  { submit, actions }: SectionContext,
): HTMLElement {
  const { connection } = overview;
  const picker = el("select", { attrs: { id: "provider-picker", "aria-label": "Model provider" } });
  const fixedAtLaunch = !connection.selectable;
  const defaultLabel = fixedAtLaunch
    ? `Set at launch (${connection.endpoint})`
    : `LM Studio: default (${DEFAULT_ENDPOINTS.lm_studio})`;
  picker.append(el("option", { text: defaultLabel, attrs: { value: DEFAULT_CHOICE } }));
  for (const profile of overview.provider_profiles.filter((p) => p.selectable)) {
    picker.append(el("option", { text: choiceLabel(profile), attrs: { value: profile.name } }));
  }
  const chosen = fixedAtLaunch ? null : connection.profile;
  picker.value = chosen ?? DEFAULT_CHOICE;
  picker.disabled = fixedAtLaunch;
  picker.addEventListener("change", () => {
    const profile = picker.value === DEFAULT_CHOICE ? null : picker.value;
    submit(actions.selectConnection(profile));
  });
  return el(
    "div",
    { class: "row-actions" },
    el("label", { class: "muted", text: "Provider", attrs: { for: "provider-picker" } }),
    picker,
    fixedAtLaunch
      ? el("span", {
          class: "muted",
          text: "Fixed by --endpoint or --provider-profile at launch.",
        })
      : null,
  );
}

function activeConnection(ctx: SectionContext): HTMLElement {
  const { overview } = ctx;
  return el(
    "section",
    { class: "block" },
    el("h3", { text: "Active connection" }),
    connectionPicker(overview, ctx),
    facts([
      ["Provider", providerName(overview.connection.kind)],
      ["Endpoint", overview.connection.endpoint],
      ["Source", connectionSource(overview.connection)],
    ]),
    el("p", {
      class: "muted",
      text: "The chat window uses the new connection for its next message. Set the model name there to one the server has.",
    }),
  );
}

function profileRow(
  profile: ProviderProfileView,
  { submit, actions }: SectionContext,
): HTMLElement {
  const remove = button("Remove", () => submit(actions.removeConnection(profile.name)));
  // A stored credential is deleted with the profile only by the trusted CLI.
  remove.disabled = profile.credential_bound;
  return el(
    "li",
    { class: "row" },
    el(
      "div",
      { class: "row-body" },
      el(
        "div",
        { class: "row-head" },
        el("span", { class: "row-title", text: profile.name }),
        profile.active ? el("span", { class: "chip chip-active", text: "Active" }) : null,
      ),
      el("div", {
        class: "row-meta",
        text: `${providerName(profile.kind)} · ${profile.endpoint ?? "no endpoint recorded"} · ${profile.auth}`,
      }),
      el("div", {
        class: "row-meta",
        text: profile.credential_bound
          ? "Credential stored (remove with the trusted CLI)"
          : "No credential",
      }),
    ),
    remove,
  );
}

function connectionList(ctx: SectionContext): HTMLElement {
  const { provider_profiles: profiles } = ctx.overview;
  return el(
    "section",
    { class: "block" },
    el("h3", { text: "Connections" }),
    profiles.length === 0
      ? el("p", { class: "empty", text: "No provider profile is recorded." })
      : el("ul", { class: "row-list" }, ...profiles.map((profile) => profileRow(profile, ctx))),
  );
}

function labelled(label: string, id: string, control: HTMLElement): HTMLElement {
  return el("div", { class: "field" }, el("label", { text: label, attrs: { for: id } }), control);
}

/**
 * A form that records a local LM Studio or Ollama connection without a credential. The
 * endpoint field starts at the chosen provider's usual address; the core validates every value.
 */
function addConnectionForm({ submit, actions }: SectionContext): HTMLElement {
  const kind = el("select", {
    attrs: { id: "new-connection-provider", "aria-label": "New connection provider" },
  });
  kind.append(
    el("option", { text: "Ollama", attrs: { value: "ollama_local" } }),
    el("option", { text: "LM Studio", attrs: { value: "lm_studio" } }),
  );
  const name = el("input", {
    attrs: {
      id: "new-connection-name",
      type: "text",
      "aria-label": "New connection name",
      placeholder: "name",
    },
  });
  name.value = "ollama";
  const endpoint = el("input", {
    attrs: {
      id: "new-connection-endpoint",
      type: "text",
      "aria-label": "New connection endpoint",
      spellcheck: "false",
    },
  });
  endpoint.value = DEFAULT_ENDPOINTS.ollama_local;
  kind.addEventListener("change", () => {
    const chosen = kind.value as LocalProviderKind;
    // Only replace values the operator has not edited.
    if (Object.values(DEFAULT_ENDPOINTS).includes(endpoint.value)) {
      endpoint.value = DEFAULT_ENDPOINTS[chosen];
    }
    if (name.value === "ollama" || name.value === "lmstudio") {
      name.value = chosen === "ollama_local" ? "ollama" : "lmstudio";
    }
  });
  const add = button("Add connection", () => {
    const chosen = kind.value as LocalProviderKind;
    submit(actions.addLocalConnection(name.value.trim(), chosen, endpoint.value.trim()));
  });
  return el(
    "div",
    { class: "form" },
    labelled("Provider", "new-connection-provider", kind),
    labelled("Name", "new-connection-name", name),
    labelled("Endpoint", "new-connection-endpoint", endpoint),
    el("div", { class: "row-actions" }, add),
  );
}

function addConnection(ctx: SectionContext): HTMLElement {
  const details = el(
    "details",
    { class: "disclosure" },
    el("summary", { text: "Add connection" }),
    el("p", {
      class: "muted",
      text: "Records a local server without a credential. A new connection is not used until you choose it above.",
    }),
    addConnectionForm(ctx),
  );
  details.open = ctx.view.addConnectionOpen;
  details.addEventListener("toggle", () => {
    ctx.view.addConnectionOpen = details.open;
  });
  return details;
}

export function renderModel(ctx: SectionContext): HTMLElement[] {
  return [
    ...sectionHead("Model", MODEL_HELP),
    activeConnection(ctx),
    connectionList(ctx),
    addConnection(ctx),
  ];
}
