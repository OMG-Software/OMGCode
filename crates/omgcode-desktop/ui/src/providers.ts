import type { FixedHostedKind, LocalProviderKind, NewConnectionKind } from "./ipc/types";

/**
 * How every window names the provider kinds the core reports (ADR-0064, ADR-0092). Shared by the
 * main window's model box and the settings window, so it imports no window code.
 */
export const PROVIDER_NAMES: Readonly<Record<NewConnectionKind, string>> = {
  lm_studio: "LM Studio",
  ollama_local: "Ollama",
  ollama_cloud: "Ollama Cloud",
  anthropic: "Anthropic (Claude)",
  openai: "OpenAI",
  google: "Google (Gemini)",
  meta: "Meta (Llama API)",
  moonshot: "Moonshot AI (Kimi)",
  tencent: "Tencent (Hunyuan)",
  thinking_machines: "Thinking Machines Lab (Tinker)",
  minimax: "MiniMax",
  open_router: "OpenRouter",
  z_ai: "Z.ai (GLM)",
  deepseek: "DeepSeek",
  xai: "xAI (Grok)",
  openai_compatible: "OpenAI compatible",
};

/** Every provider the settings window can add, in the order its dropdown lists them. */
export const NEW_CONNECTION_KINDS: readonly NewConnectionKind[] = [
  "ollama_local",
  "lm_studio",
  "ollama_cloud",
  "anthropic",
  "openai",
  "google",
  "meta",
  "moonshot",
  "tencent",
  "thinking_machines",
  "minimax",
  "open_router",
  "z_ai",
  "deepseek",
  "xai",
  "openai_compatible",
];

/** The provider's display name. `null` is the default connection, an LM Studio server. */
export function providerName(kind: string | null): string {
  if (kind === null) {
    return "LM Studio";
  }
  return Object.hasOwn(PROVIDER_NAMES, kind) ? PROVIDER_NAMES[kind as NewConnectionKind] : kind;
}

/** The address each local server listens on by default. The core validates whatever is sent. */
export const DEFAULT_ENDPOINTS: Readonly<Record<LocalProviderKind, string>> = {
  lm_studio: "http://127.0.0.1:1234/v1",
  ollama_local: "http://127.0.0.1:11434/v1",
};

/**
 * The one endpoint each named hosted provider uses. Rust supplies it when the connection is
 * added; the window only shows it. It must match core's `ProviderKind::fixed_endpoint`.
 */
export const FIXED_ENDPOINTS: Readonly<Record<FixedHostedKind, string>> = {
  ollama_cloud: "https://ollama.com/v1",
  anthropic: "https://api.anthropic.com/v1",
  openai: "https://api.openai.com/v1",
  google: "https://generativelanguage.googleapis.com/v1beta/openai",
  meta: "https://api.llama.com/compat/v1",
  moonshot: "https://api.moonshot.ai/v1",
  tencent: "https://api.hunyuan.cloud.tencent.com/v1",
  thinking_machines: "https://tinker.thinkingmachines.dev/services/tinker-prod/oai/api/v1",
  minimax: "https://api.minimax.io/v1",
  open_router: "https://openrouter.ai/api/v1",
  z_ai: "https://api.z.ai/api/paas/v4",
  deepseek: "https://api.deepseek.com",
  xai: "https://api.x.ai/v1",
};

/** Whether `kind` is LM Studio or Ollama on this computer. */
export function isLocalKind(kind: string | null): kind is LocalProviderKind {
  return kind === "lm_studio" || kind === "ollama_local";
}

/** Whether `kind` is a hosted service, which sends every turn off this computer. */
export function isHostedKind(kind: string | null): boolean {
  return kind !== null && !isLocalKind(kind);
}

/** Whether `kind` is a named hosted provider with one fixed endpoint. */
export function isFixedHostedKind(kind: string): kind is FixedHostedKind {
  return Object.hasOwn(FIXED_ENDPOINTS, kind);
}

/** The host an endpoint names, or the endpoint itself when it cannot be parsed. */
export function hostOf(endpoint: string): string {
  try {
    return new URL(endpoint).host || endpoint;
  } catch {
    return endpoint;
  }
}

/** What a hosted connection's row and the add form say about where prompts go (ADR-0092). */
export function hostedNotice(endpoint: string | null): string {
  const where = endpoint === null || endpoint === "" ? "the address you enter" : hostOf(endpoint);
  return `Hosted: prompts, workspace text read by tools, and command output leave this computer for ${where}.`;
}

/** How a dropdown names one connection: provider, profile, and address. */
export function connectionLabel(kind: string | null, name: string, endpoint: string): string {
  return `${providerName(kind)}: ${name} (${endpoint})`;
}
