import { button, el } from "../dom";
import type {
  ClaudeCodeView,
  ConnectionView,
  ProviderKind,
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
  isProgramKind,
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

/** The address field's hint for an OpenAI-compatible service (ADR-0092). */
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

/** Whether a profile is a ChatGPT plan connection: `openai` with `subscription` (ADR-0093). */
export function isChatGpt(profile: ProviderProfileView): boolean {
  return profile.kind === "openai" && profile.auth === "subscription";
}

function credentialLine(profile: ProviderProfileView): string {
  if (isProgramKind(profile.kind)) {
    return "No credential: the program keeps its own sign-in";
  }
  if (isChatGpt(profile)) {
    if (!profile.credential_bound) {
      return "Not signed in";
    }
    return profile.account_label === null
      ? "Signed in with ChatGPT"
      : `Signed in with ChatGPT as ${profile.account_label}`;
  }
  if (!profile.credential_bound) {
    return "No credential";
  }
  return isHostedKind(profile.kind) ? "API key stored" : "Credential stored";
}

/** The sign-in or sign-out button of a ChatGPT connection. Rust confirms both natively. */
function chatGptButton(profile: ProviderProfileView, { submit, actions }: SectionContext) {
  return profile.credential_bound
    ? button("Sign out", () => submit(actions.signOutChatGpt(profile.name)))
    : button("Sign in with ChatGPT", () => submit(actions.signInChatGpt(profile.name)));
}

/**
 * The line under a Claude Code connection (ADR-0109, ADR-0111). A session chooses it in the main
 * window's provider box; it is never the default connection.
 */
export const PROGRAM_CONNECTION_NOTE =
  "Runs your signed-in Claude Code CLI. Choose it for a session in the main window's provider box; it cannot be the default connection. Removing it ends its running turns.";

/** What the add form says about a Claude Code connection before Rust's checks and confirmation. */
export const CLAUDE_CODE_ADD_NOTE =
  "Add connection finds your claude program (claude.exe on Windows), or asks for it if it is not installed in the usual place, creates the connection's own sign-in folder, and asks you once to confirm. On Windows it also grants the connection's sandbox its folders and opens Claude's own sign-in in a new window: finish it in your browser. Elsewhere, sign in from a terminal with the command the confirmation shows. OMGCode never reads the sign-in.";

/** Why a Claude Code connection on Windows has its folders granted, and how to grant them again. */
export const CLAUDE_CODE_GRANT_NOTE =
  "On Windows the claude program runs in its own sandbox. Add grants its folders; after revoking them, grant them again before a session uses it. The relay is the omgcode program beside this application.";

/**
 * How to sign a Claude Code connection in again on Windows. OMGCode opens Claude's sign-in only
 * for the folder Add has just created, because the sign-in runs outside the sandbox (ADR-0111).
 */
export const CLAUDE_CODE_SIGN_IN_AGAIN_NOTE =
  "To sign in again, revoke its folders, remove the connection, and add it again: OMGCode opens Claude's sign-in only for a new sign-in folder, because the sign-in runs outside the sandbox.";

/**
 * How to sign a Claude Code connection in again where no folder is granted: a new connection
 * and folder, then the terminal command its confirmation shows (ADR-0111).
 */
export const CLAUDE_CODE_SIGN_IN_AGAIN_TERMINAL_NOTE =
  "To sign in again, remove the connection, add it again, and sign in from a terminal with the command the confirmation shows. Use a new sign-in folder, because the sign-in runs outside the sandbox.";

const GRANT_ROLE_NAMES: Readonly<Record<ClaudeCodeView["grants"][number]["role"], string>> = {
  program: "Program folder",
  configuration: "Sign-in folder",
  runtime: "Runtime folder",
  relay: "Relay folder",
};

/** The program, sign-in folder, and on Windows the grants and their buttons. */
function claudeCodeDetails(
  name: string,
  setup: ClaudeCodeView,
  { overview, submit, actions }: SectionContext,
): HTMLElement[] {
  const lines = [
    el("div", { class: "row-meta", text: `Program: ${setup.program}` }),
    el("div", { class: "row-meta", text: `Sign-in folder: ${setup.config_folder}` }),
    el("div", {
      class: "row-meta",
      text: overview.container_grants_supported
        ? CLAUDE_CODE_SIGN_IN_AGAIN_NOTE
        : CLAUDE_CODE_SIGN_IN_AGAIN_TERMINAL_NOTE,
    }),
  ];
  if (!overview.container_grants_supported) {
    return lines;
  }
  const grants =
    setup.grants.length === 0
      ? [el("div", { class: "row-meta", text: "No folder is granted." })]
      : setup.grants.map((grant) =>
          el("div", {
            class: "row-meta",
            text: `${GRANT_ROLE_NAMES[grant.role]}: ${grant.directory} (${grant.state})`,
          }),
        );
  return [
    ...lines,
    el("div", { class: "row-meta", text: CLAUDE_CODE_GRANT_NOTE }),
    ...grants,
    el(
      "div",
      { class: "row-actions" },
      button("Grant folders", () => submit(actions.grantClaudeCode(name))),
      setup.grants.length === 0
        ? null
        : button("Revoke folders", () => submit(actions.revokeClaudeCode(name))),
    ),
  ];
}

/** The line under a hosted profile: where prompts go, or why it cannot be used at all. */
export const NO_ENDPOINT_NOTE =
  "No endpoint is recorded, so this connection cannot be used. An endpoint cannot be added later: remove it and add it again.";

export const UNSUPPORTED_AUTH_NOTE =
  "No adapter reads this connection's authentication, so it cannot be used and sends nothing. Remove it and add an API-key connection instead.";

// A hosted profile that cannot be used must not claim that prompts leave for its host.
function hostedRowNote(profile: ProviderProfileView): string {
  if (profile.auth !== "api_key") {
    return UNSUPPORTED_AUTH_NOTE;
  }
  return profile.endpoint === null ? NO_ENDPOINT_NOTE : hostedNotice(profile.endpoint);
}

function profileRow(profile: ProviderProfileView, ctx: SectionContext): HTMLElement {
  const { submit, actions } = ctx;
  const plan = isChatGpt(profile);
  const hosted = isHostedKind(profile.kind) && !plan;
  const program = isProgramKind(profile.kind);
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
        plan ? el("span", { class: "chip", text: "ChatGPT plan" }) : null,
        program ? el("span", { class: "chip", text: "Per session" }) : null,
      ),
      el("div", {
        class: "row-meta",
        text: `${providerName(profile.kind)} \u00b7 ${profile.endpoint ?? "no endpoint recorded"} \u00b7 ${profile.auth}`,
      }),
      el("div", { class: "row-meta", text: credentialLine(profile) }),
      hosted ? el("div", { class: "row-meta", text: hostedRowNote(profile) }) : null,
      program ? el("div", { class: "row-meta", text: PROGRAM_CONNECTION_NOTE }) : null,
      ...(program && profile.claude_code !== undefined
        ? claudeCodeDetails(profile.name, profile.claude_code, ctx)
        : []),
      // A subscription profile never holds a key, and a profile without an endpoint can never
      // be used, so neither has a key field.
      hosted && profile.auth === "api_key" && profile.endpoint !== null
        ? setKeyDisclosure(profile, ctx)
        : null,
      plan
        ? el("div", {
            class: "row-meta",
            text: profile.endpoint === null ? NO_ENDPOINT_NOTE : CHATGPT_NOTICE,
          })
        : null,
    ),
    // A ChatGPT profile recorded without OpenAI's endpoint can never sign in (ADR-0093).
    plan && profile.endpoint !== null ? chatGptButton(profile, ctx) : null,
    // Rust refuses to remove a signed-in ChatGPT connection, so it offers Sign out first.
    plan && profile.credential_bound ? null : remove,
  );
}

/** What a ChatGPT connection sends where, and what a sign-in does not show. */
export const CHATGPT_NOTICE =
  "Chat turns go to OpenAI under your ChatGPT plan. A sign-in does not show that the plan will accept OMGCode's requests.";

/**
 * Signs a new ChatGPT connection in (ADR-0093). Rust records an `openai` subscription profile
 * with the name, asks a system confirmation, and opens OpenAI's sign-in page in the browser; this
 * window never sees the address or a token.
 */
function chatGptSignIn({ submit, actions }: SectionContext): HTMLElement {
  const name = el("input", {
    attrs: {
      id: "chatgpt-connection-name",
      type: "text",
      "aria-label": "ChatGPT connection name",
      value: "chatgpt",
    },
  });
  const signIn = button("Sign in with ChatGPT", () =>
    submit(actions.signInChatGpt(name.value.trim())),
  );
  return el(
    "details",
    { class: "disclosure" },
    el("summary", { text: "Sign in with ChatGPT" }),
    el("p", {
      class: "muted",
      text: "Uses your ChatGPT plan instead of an API key. OMGCode asks you to confirm, then opens OpenAI's sign-in page in your browser. The tokens stay in this computer's protected credential store.",
    }),
    el("p", { class: "muted", text: CHATGPT_NOTICE }),
    el(
      "div",
      { class: "form" },
      labelled("Name", "chatgpt-connection-name", name),
      el("div", { class: "row-actions" }, signIn),
    ),
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
const DEFAULT_NAMES: Readonly<Record<ProviderKind, string>> = {
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
  claude_code: "claude-code",
};

/** The add-connection form's starting values: Ollama at its usual address. */
export function newConnectionDraft(): ConnectionDraft {
  return { kind: "ollama_local", name: "ollama", endpoint: DEFAULT_ENDPOINTS.ollama_local };
}

/** How the provider dropdown names one kind; hosted ones say so. */
function kindOption(kind: ProviderKind): string {
  return isHostedKind(kind) ? `${PROVIDER_NAMES[kind]} (hosted)` : PROVIDER_NAMES[kind];
}

/**
 * A form that records a local LM Studio or Ollama connection without a credential, a named
 * hosted provider with its API key, or an OpenAI-compatible service with its HTTPS address and
 * API key (ADR-0092). The endpoint field starts at the chosen local provider's usual address; it
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
  const programText = el("p", { class: "muted", text: CLAUDE_CODE_ADD_NOTE });
  // Shows the fields the chosen provider takes. The endpoint field is hidden, not rebuilt, so
  // what was typed in it stays. The key field is cleared on a provider change instead: a key
  // belongs to one provider and must not be sent to another.
  const arrange = (): void => {
    const chosen = kind.value as ProviderKind;
    const fixed = isFixedHostedKind(chosen) ? FIXED_ENDPOINTS[chosen] : null;
    const hosted = isHostedKind(chosen);
    // A Claude Code connection takes a name only; Rust's dialogs choose the rest (ADR-0111).
    const program = isProgramKind(chosen);
    programText.hidden = !program;
    endpointField.hidden = fixed !== null || program;
    endpoint.placeholder = chosen === "openai_compatible" ? CUSTOM_ENDPOINT_HINT : "";
    fixedEndpoint.hidden = fixed === null;
    fixedText.textContent = fixed === null ? "" : `Endpoint: ${fixed} (fixed)`;
    key.field.hidden = !hosted;
    hostedText.hidden = !hosted;
    hostedText.textContent = hosted ? hostedNotice(fixed) : "";
  };
  const remember = (): void => {
    view.connectionDraft = {
      kind: kind.value as ProviderKind,
      name: name.value,
      endpoint: endpoint.value,
    };
  };
  name.addEventListener("input", remember);
  endpoint.addEventListener("input", remember);
  kind.addEventListener("change", () => {
    const chosen = kind.value as ProviderKind;
    key.input.value = "";
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
    const chosen = kind.value as ProviderKind;
    // Take the key out of the field before anything else can fail, then send it once.
    const typed = key.input.value;
    key.input.value = "";
    let change: ReturnType<typeof actions.addConnection>;
    if (isProgramKind(chosen)) {
      change = actions.addClaudeCode(name.value.trim());
    } else if (isLocalKind(chosen)) {
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
    programText,
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
    chatGptSignIn(ctx),
  ];
}
