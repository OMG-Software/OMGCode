import { button, el } from "../dom";
import type {
  ConnectionView,
  NewConnectionKind,
  ProviderProfileView,
  SettingsOverview,
} from "../ipc/types";
import {
  DEFAULT_ENDPOINTS,
  FIXED_ENDPOINTS,
  hostedNotice,
  isFixedHostedKind,
  isHostedKind,
  isLocalKind,
  NEW_CONNECTION_KINDS,
  PROVIDER_NAMES,
  providerName,
} from "../providers";
import { chatDefaultsBlock } from "./chat_section";
import type { ConnectionDraft, SectionContext } from "./context";
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

/** The address field's hint for an OpenAI-compatible service (ADR-0090). */
export const CUSTOM_ENDPOINT_HINT = "https://llm.example.org/v1";

/**
 * A write-only API key control: a password field and a button that sends its value once and
 * empties the field before the answer returns, so the key is held nowhere after the click.
 */
function keyEntry(
  id: string,
  label: string,
  action: string,
  send: (key: string) => void,
): { field: HTMLElement; input: HTMLInputElement; save: HTMLButtonElement } {
  const input = el("input", {
    attrs: {
      id,
      type: "password",
      autocomplete: "off",
      spellcheck: "false",
      "aria-label": label,
    },
  });
  const save = button(action, () => {
    const key = input.value;
    input.value = "";
    send(key);
  });
  return { field: labelled(label, id, input), input, save };
}

/** The "Set key..." disclosure of one hosted profile. */
function setKeyDisclosure(
  profile: ProviderProfileView,
  { submit, actions }: SectionContext,
): HTMLElement {
  const entry = keyEntry(
    `set-key-${profile.name}`,
    `New API key for ${profile.name}`,
    "Save key",
    (key) => submit(actions.setApiKey(profile.name, key)),
  );
  return el(
    "details",
    { class: "disclosure" },
    el("summary", { text: "Set key\u2026" }),
    el("p", {
      class: "muted",
      text: "Replaces the stored key. It is sent once, then cleared, and is never shown again.",
    }),
    el("div", { class: "form" }, entry.field, el("div", { class: "row-actions" }, entry.save)),
  );
}

function credentialLine(profile: ProviderProfileView): string {
  if (!profile.credential_bound) {
    return "No credential";
  }
  return isHostedKind(profile.kind) ? "API key stored" : "Credential stored";
}

function profileRow(profile: ProviderProfileView, ctx: SectionContext): HTMLElement {
  const { submit, actions } = ctx;
  const hosted = isHostedKind(profile.kind);
  // Removing a profile with a stored key deletes the key too; Rust asks a system Yes/No first.
  const remove = button("Remove", () => submit(actions.removeConnection(profile.name)));
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
        hosted ? el("span", { class: "chip", text: "Hosted" }) : null,
      ),
      el("div", {
        class: "row-meta",
        text: `${providerName(profile.kind)} \u00b7 ${profile.endpoint ?? "no endpoint recorded"} \u00b7 ${profile.auth}`,
      }),
      el("div", { class: "row-meta", text: credentialLine(profile) }),
      hosted ? el("div", { class: "row-meta", text: hostedNotice(profile.endpoint) }) : null,
      hosted ? setKeyDisclosure(profile, ctx) : null,
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

/** The name the form suggests for each provider, replaced only while the operator kept it. */
const DEFAULT_NAMES: Readonly<Record<NewConnectionKind, string>> = {
  ollama_local: "ollama",
  lm_studio: "lmstudio",
  ollama_cloud: "ollama-cloud",
  anthropic: "claude",
  openai: "openai",
  google: "gemini",
  meta: "llama",
  moonshot: "kimi",
  tencent: "hunyuan",
  thinking_machines: "tinker",
  minimax: "minimax",
  open_router: "openrouter",
  z_ai: "glm",
  deepseek: "deepseek",
  xai: "grok",
  openai_compatible: "custom",
};

/** The add-connection form's starting values: Ollama at its usual address. */
export function newConnectionDraft(): ConnectionDraft {
  return { kind: "ollama_local", name: "ollama", endpoint: DEFAULT_ENDPOINTS.ollama_local };
}

/** How the provider dropdown names one kind; hosted ones say so. */
function kindOption(kind: NewConnectionKind): string {
  return isHostedKind(kind) ? `${PROVIDER_NAMES[kind]} (hosted)` : PROVIDER_NAMES[kind];
}

/**
 * A form that records a local LM Studio or Ollama connection without a credential, a named
 * hosted provider with its API key, or an OpenAI-compatible service with its HTTPS address and
 * API key (ADR-0090). The endpoint field starts at the chosen local provider's usual address; it
 * is hidden for a named hosted provider, whose fixed endpoint Rust supplies and the form only
 * shows; the core validates every value. The typed values (never the key) live in the view
 * state, so a redraw keeps them; they reset only after the core records the connection. The key
 * lives only in its password field: the click sends it once and empties the field at once,
 * whether or not the core accepts it.
 */
function addConnectionForm({ submit, actions, view }: SectionContext): HTMLElement {
  const draft = view.connectionDraft;
  const kind = el("select", {
    attrs: { id: "new-connection-provider", "aria-label": "New connection provider" },
  });
  kind.append(
    ...NEW_CONNECTION_KINDS.map((value) =>
      el("option", { text: kindOption(value), attrs: { value } }),
    ),
  );
  const name = el("input", {
    attrs: {
      id: "new-connection-name",
      type: "text",
      "aria-label": "New connection name",
      placeholder: "name",
    },
  });
  const endpoint = el("input", {
    attrs: {
      id: "new-connection-endpoint",
      type: "text",
      "aria-label": "New connection endpoint",
      spellcheck: "false",
    },
  });
  const key = keyEntry("new-connection-key", "API key", "Add connection", () => undefined);
  kind.value = draft.kind;
  name.value = draft.name;
  endpoint.value = draft.endpoint;
  const endpointField = labelled("Endpoint", "new-connection-endpoint", endpoint);
  const fixedText = el("span");
  const fixedEndpoint = el(
    "div",
    { class: "field", attrs: { id: "new-connection-fixed-endpoint" } },
    fixedText,
  );
  const hostedText = el("p", { class: "muted" });
  // Shows the fields the chosen provider takes. The endpoint and key fields are hidden, not
  // rebuilt, so what was typed in a visible one stays.
  const arrange = (): void => {
    const chosen = kind.value as NewConnectionKind;
    const fixed = isFixedHostedKind(chosen) ? FIXED_ENDPOINTS[chosen] : null;
    const hosted = isHostedKind(chosen);
    endpointField.hidden = fixed !== null;
    endpoint.placeholder = chosen === "openai_compatible" ? CUSTOM_ENDPOINT_HINT : "";
    fixedEndpoint.hidden = fixed === null;
    fixedText.textContent = fixed === null ? "" : `Endpoint: ${fixed} (fixed)`;
    key.field.hidden = !hosted;
    hostedText.hidden = !hosted;
    hostedText.textContent = hosted ? hostedNotice(fixed) : "";
  };
  const remember = (): void => {
    view.connectionDraft = {
      kind: kind.value as NewConnectionKind,
      name: name.value,
      endpoint: endpoint.value,
    };
  };
  name.addEventListener("input", remember);
  endpoint.addEventListener("input", remember);
  kind.addEventListener("change", () => {
    const chosen = kind.value as NewConnectionKind;
    // Only replace values the operator has not edited.
    const untouched =
      endpoint.value === "" || Object.values(DEFAULT_ENDPOINTS).includes(endpoint.value);
    if (untouched) {
      endpoint.value = isLocalKind(chosen) ? DEFAULT_ENDPOINTS[chosen] : "";
    }
    if (Object.values(DEFAULT_NAMES).includes(name.value)) {
      name.value = DEFAULT_NAMES[chosen];
    }
    arrange();
    remember();
  });
  arrange();
  const add = button("Add connection", () => {
    const chosen = kind.value as NewConnectionKind;
    // Take the key out of the field before anything else can fail, then send it once.
    const typed = key.input.value;
    key.input.value = "";
    let change: ReturnType<typeof actions.addConnection>;
    if (isLocalKind(chosen)) {
      change = actions.addConnection(name.value.trim(), chosen, endpoint.value.trim(), null);
    } else if (isFixedHostedKind(chosen)) {
      change = actions.addConnection(name.value.trim(), chosen, null, typed);
    } else {
      change = actions.addConnection(name.value.trim(), chosen, endpoint.value.trim(), typed);
    }
    submit(async (revision) => {
      const recorded = await change(revision);
      // Reached only when the core recorded the connection; a refusal keeps the draft.
      view.connectionDraft = newConnectionDraft();
      return recorded;
    });
  });
  return el(
    "div",
    { class: "form" },
    labelled("Provider", "new-connection-provider", kind),
    labelled("Name", "new-connection-name", name),
    endpointField,
    fixedEndpoint,
    key.field,
    hostedText,
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
      text: "Records a local server without a credential, or a hosted provider with its API key. A new connection is not used until you choose it above.",
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
    chatDefaultsBlock(ctx),
    connectionList(ctx),
    addConnection(ctx),
  ];
}
