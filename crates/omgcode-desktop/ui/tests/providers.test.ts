import { describe, expect, it } from "vitest";
import {
  FIXED_ENDPOINTS,
  hostedNotice,
  isFixedHostedKind,
  isHostedKind,
  NEW_CONNECTION_KINDS,
  PROVIDER_NAMES,
  providerName,
} from "../src/providers";

describe("provider names and endpoints (ADR-0090)", () => {
  it("names every kind the settings window can add", () => {
    for (const kind of NEW_CONNECTION_KINDS) {
      expect(PROVIDER_NAMES[kind]).toBeTruthy();
      expect(providerName(kind)).toBe(PROVIDER_NAMES[kind]);
    }
    expect(new Set(NEW_CONNECTION_KINDS).size).toBe(16);
    expect(providerName(null)).toBe("LM Studio");
    // An unknown or inherited name is shown as given, never looked up on the prototype.
    expect(providerName("toString")).toBe("toString");
    expect(providerName("something_new")).toBe("something_new");
  });

  it("gives every named hosted kind one HTTPS endpoint and no other kind one", () => {
    for (const kind of NEW_CONNECTION_KINDS) {
      const fixed = isFixedHostedKind(kind);
      expect(fixed).toBe(isHostedKind(kind) && kind !== "openai_compatible");
      if (fixed) {
        expect(FIXED_ENDPOINTS[kind].startsWith("https://")).toBe(true);
      }
    }
    expect(isHostedKind(null)).toBe(false);
    expect(isHostedKind("lm_studio")).toBe(false);
    expect(isHostedKind("ollama_local")).toBe(false);
    expect(isFixedHostedKind("toString")).toBe(false);
  });

  it("names the host that receives a hosted turn", () => {
    expect(hostedNotice("https://api.x.ai/v1")).toContain("leave this computer for api.x.ai.");
    expect(hostedNotice("https://llm.example.org:8443/v1")).toContain("llm.example.org:8443");
    expect(hostedNotice(null)).toContain("the address you enter");
    expect(hostedNotice("not a url")).toContain("not a url");
  });
});
