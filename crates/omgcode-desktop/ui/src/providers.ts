import type { LocalProviderKind } from "./ipc/types";

/**
 * How every window names the provider kinds the core reports (ADR-0064). Shared by the main
 * window's status bar and the settings window, so it imports no window code.
 */
export const PROVIDER_NAMES: Readonly<Record<string, string>> = {
  lm_studio: "LM Studio",
  ollama_local: "Ollama",
  ollama_cloud: "Ollama Cloud",
  openai: "OpenAI",
  anthropic: "Anthropic",
  open_router: "OpenRouter",
};

/** The provider's display name. `null` is the default connection, an LM Studio server. */
export function providerName(kind: string | null): string {
  return kind === null ? "LM Studio" : (PROVIDER_NAMES[kind] ?? kind);
}

/** The address each local server listens on by default. The core validates whatever is sent. */
export const DEFAULT_ENDPOINTS: Readonly<Record<LocalProviderKind, string>> = {
  lm_studio: "http://127.0.0.1:1234/v1",
  ollama_local: "http://127.0.0.1:11434/v1",
};

/** How a dropdown names one connection: provider, profile, and address. */
export function connectionLabel(kind: string | null, name: string, endpoint: string): string {
  return `${providerName(kind)}: ${name} (${endpoint})`;
}
