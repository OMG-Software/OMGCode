import { describe, expect, it } from "vitest";

const files = import.meta.glob<string>("../dist/**/*.{js,css,html}", {
  query: "?raw",
  import: "default",
  eager: true,
});

const byPath = new Map<string, string>(
  Object.entries(files).map(([key, text]) => [key.replace("../dist/", ""), text]),
);

const MARKER = "OMGCODE_MOCK_FIXTURE_MARKER";

const settingsClient = import.meta.glob<string>("../src/ipc/settings_client.ts", {
  query: "?raw",
  import: "default",
  eager: true,
});

/**
 * Commands granted to the settings window alone (ADR-0059, ADR-0060): every command the settings
 * client invokes, matched as `tests/layers.rs` matches them. That Rust test holds the client to
 * exactly the policy's settings commands, so a command added there is checked here too.
 */
const SETTINGS_WINDOW_COMMANDS: string[] = [
  ...new Set(
    Array.from(
      (Object.values(settingsClient)[0] ?? "").matchAll(/invokeCommand<[^;\n]*?>\("([a-z_]+)"/g),
      (match) => match[1] as string,
    ),
  ),
];

/** JS files reachable from an HTML entry, following static and dynamic relative imports. */
function reachableScripts(entry: string): Map<string, string> {
  const html = byPath.get(entry) ?? "";
  const seen = new Map<string, string>();
  const queue: string[] = [];
  for (const match of html.matchAll(/(?:src|href)="\.\/([^"]+\.js)"/g)) {
    queue.push(match[1] as string);
  }
  while (queue.length > 0) {
    const path = queue.pop() as string;
    const text = byPath.get(path);
    if (text === undefined || seen.has(path)) {
      continue;
    }
    seen.set(path, text);
    for (const match of text.matchAll(/(?:from|import)\s*\(?\s*["'](\.{1,2}\/[^"']+\.js)["']/g)) {
      const resolved = new URL(match[1] as string, `file:///${path}`).pathname.slice(1);
      queue.push(resolved);
    }
  }
  return seen;
}

const present = byPath.size > 0;
const suite = present ? describe : describe.skip;

suite(
  present ? "built dist/" : "built dist/ is missing: run `npm run test:dist` to build and check it",
  () => {
    it("main window bundle never mentions approval_decide or approval_detail", () => {
      const scripts = reachableScripts("index.html");
      expect(scripts.size).toBeGreaterThan(0);
      for (const [path, text] of scripts) {
        expect(text, path).not.toContain("approval_decide");
        expect(text, path).not.toContain("approval_detail");
      }
    });

    it("approvals window bundle does call approval_decide", () => {
      const joined = Array.from(reachableScripts("approvals.html").values()).join("\n");
      expect(joined).toContain("approval_decide");
      expect(joined).toContain("approval_detail");
    });

    it("main window bundle calls chat_subscribe and the approvals bundle never mentions it", () => {
      const main = Array.from(reachableScripts("index.html").values()).join("\n");
      expect(main).toContain("chat_subscribe");
      for (const [path, text] of reachableScripts("approvals.html")) {
        expect(text, path).not.toContain("chat_subscribe");
      }
    });

    it("reads every settings-window command from the settings client", () => {
      // 33 in `settings`, as `policy.rs` and the guide count them.
      expect(SETTINGS_WINDOW_COMMANDS).toHaveLength(33);
      expect(SETTINGS_WINDOW_COMMANDS).toContain("settings_overview");
      expect(SETTINGS_WINDOW_COMMANDS).toContain("settings_discover_toolchains");
      expect(SETTINGS_WINDOW_COMMANDS).toContain("settings_add_discovered_profiles");
      expect(SETTINGS_WINDOW_COMMANDS).toContain("settings_assign_toolchain_profile");
      expect(SETTINGS_WINDOW_COMMANDS).toContain("settings_unassign_toolchain_profile");
      expect(SETTINGS_WINDOW_COMMANDS).not.toContain("settings_grant_discovered_roots");
      expect(SETTINGS_WINDOW_COMMANDS).toContain("settings_add_web_fetch_host");
      expect(SETTINGS_WINDOW_COMMANDS).toContain("settings_remove_web_fetch_host");
      expect(SETTINGS_WINDOW_COMMANDS).toContain("settings_revoke_container_grant");
      expect(SETTINGS_WINDOW_COMMANDS).toContain("settings_sign_in_chatgpt");
      expect(SETTINGS_WINDOW_COMMANDS).toContain("settings_cancel_chatgpt_sign_in");
      expect(SETTINGS_WINDOW_COMMANDS).toContain("settings_sign_out_chatgpt");
    });

    it("settings window bundle reads the overview and never decides or subscribes", () => {
      const scripts = reachableScripts("settings.html");
      expect(scripts.size).toBeGreaterThan(0);
      const joined = Array.from(scripts.values()).join("\n");
      for (const command of SETTINGS_WINDOW_COMMANDS) {
        expect(joined).toContain(command);
      }
      for (const [path, text] of scripts) {
        expect(text, path).not.toContain("approval_decide");
        expect(text, path).not.toContain("approval_detail");
        expect(text, path).not.toContain("chat_subscribe");
      }
    });

    it("main and approvals bundles never mention a settings-window command", () => {
      for (const entry of ["index.html", "approvals.html"]) {
        for (const [path, text] of reachableScripts(entry)) {
          for (const command of SETTINGS_WINDOW_COMMANDS) {
            expect(text, `${entry} ${path}`).not.toContain(command);
          }
        }
      }
    });

    it("contains no mock fixture marker in any asset", () => {
      for (const [path, text] of byPath) {
        expect(text, path).not.toContain(MARKER);
      }
    });

    it("has HTML with no inline script, inline style or remote URL", () => {
      for (const entry of ["index.html", "approvals.html", "settings.html"]) {
        const html = byPath.get(entry) ?? "";
        expect(html, entry).not.toBe("");
        for (const match of html.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/gi)) {
          expect(match[1]?.trim(), entry).toBe("");
        }
        expect(html, entry).not.toMatch(/<style\b|\sstyle\s*=|https?:/i);
      }
    });
  },
);
