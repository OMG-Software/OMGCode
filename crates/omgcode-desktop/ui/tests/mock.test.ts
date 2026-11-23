import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ChatDefaults, ChatEvent, SettingsOverview, WorkspaceList } from "../src/ipc/types";
import { createMockBackend, isLocalEndpoint } from "../src/mock/backend";
import { OMGCODE_MOCK_FIXTURE_MARKER } from "../src/mock/fixtures";

interface Detail {
  review_token: string | null;
  binding: string;
  tool: string | null;
  fields: Array<{ label: string }>;
}

describe("mock backend", () => {
  it("has a marker constant for the dist scan", () => {
    expect(OMGCODE_MOCK_FIXTURE_MARKER).toBe("OMGCODE_MOCK_FIXTURE_MARKER");
  });

  it("serves 12 sessions across both views and pages a long transcript", async () => {
    const { invoke } = createMockBackend();
    const count = async (view: string): Promise<number> => {
      const tree = (await invoke("session_tree", { view, sort: "updated", descending: true })) as {
        groups: Array<{ sessions: unknown[] }>;
      };
      return tree.groups.reduce((n, g) => n + g.sessions.length, 0);
    };
    expect((await count("active")) + (await count("archived"))).toBe(12);
    const first = (await invoke("session_transcript", {
      session_id: "00000000-0000-4000-8000-000000000001",
      after_seq: null,
      limit: 200,
    })) as { entries: unknown[]; next_after_seq: number | null };
    expect(first.entries).toHaveLength(200);
    expect(first.next_after_seq).toBe(200);
  });

  it("issues single-use review tokens and enforces decide rules", async () => {
    const { invoke } = createMockBackend();
    const exact = (await invoke("approval_detail", { id: "a1" })) as Detail;
    const token = exact.review_token;
    expect(token).not.toBeNull();
    await expect(
      invoke("approval_decide", {
        id: "a1",
        decision: "approve",
        writers_stopped: false,
        review_token: "nope",
      }),
    ).rejects.toMatchObject({ code: "review_mismatch" });
    await expect(
      invoke("approval_decide", {
        id: "a1",
        decision: "approve",
        writers_stopped: false,
        review_token: token,
      }),
    ).resolves.toMatchObject({ state: "approved" });
    await expect(
      invoke("approval_decide", {
        id: "a1",
        decision: "approve",
        writers_stopped: false,
        review_token: token,
      }),
    ).rejects.toMatchObject({ code: "approval_not_pending" });
    const stale = (await invoke("approval_detail", { id: "a3" })) as Detail;
    expect(stale.binding).toBe("stale");
    expect(stale.review_token).toBeNull();
  });

  it("reviews a multi-edit with numbered old and new text fields", async () => {
    const { invoke } = createMockBackend();
    const detail = (await invoke("approval_detail", { id: "a2" })) as Detail;
    expect(detail.tool).toBe("workspace_multi_edit_file");
    expect(detail.fields.map((field) => field.label)).toEqual([
      "File",
      "Edits",
      "Old text 1",
      "New text 1",
      "Old text 2",
      "New text 2",
    ]);
  });

  it("requires the acknowledgement for a file edit", async () => {
    const { invoke } = createMockBackend();
    const detail = (await invoke("approval_detail", { id: "a2" })) as Detail;
    await expect(
      invoke("approval_decide", {
        id: "a2",
        decision: "approve",
        writers_stopped: false,
        review_token: detail.review_token,
      }),
    ).rejects.toMatchObject({ code: "writers_not_stopped" });
  });

  it("rejects a chat endpoint that differs from chat_defaults", async () => {
    const { invoke } = createMockBackend();
    const { settings: defaults } = (await invoke("chat_defaults", {})) as {
      settings: { endpoint: string };
    };
    const send = (endpoint: string) =>
      invoke("chat_send", {
        session_id: "00000000-0000-4000-8000-000000000001",
        message: "hello",
        settings: { ...defaults, model: "m", endpoint },
        skills: [],
      });
    await expect(send("http://127.0.0.1:9/v1")).rejects.toEqual({
      code: "connection_changed",
      message: "The model connection changed in settings. Check the model name, then send again.",
    });
    await expect(send(defaults.endpoint)).resolves.toMatchObject({ state: "running" });
  });

  it("moves the chat endpoint to the connection chosen in settings, like core", async () => {
    const { invoke } = createMockBackend();
    const overview = (await invoke("settings_overview", {})) as { revision: number };
    await invoke("settings_select_connection", {
      revision: overview.revision,
      profile: "ollama",
    });
    const { settings } = (await invoke("chat_defaults", {})) as {
      settings: { endpoint: string };
    };
    expect(settings.endpoint).toBe("http://127.0.0.1:11434/v1");
    await expect(
      invoke("settings_select_connection", { revision: overview.revision, profile: null }),
    ).rejects.toMatchObject({ code: "stale_revision" });
  });

  it("serves a session's own connection and model, like core", async () => {
    const { invoke } = createMockBackend();
    const id = "00000000-0000-4000-8000-000000000001";
    const other = "00000000-0000-4000-8000-000000000002";
    type Status = {
      connection: { profile: string | null; endpoint: string };
      origin: string;
      model: string;
      choices: Array<{ name: string; active: boolean }>;
    };
    const before = (await invoke("connection_status", { session_id: id })) as Status;
    expect(before.connection.profile).toBeNull();
    expect(before.origin).toBe("default");
    expect(before.choices.map((choice) => choice.name)).toContain("ollama");
    expect(before.choices.every((choice) => !choice.active)).toBe(true);

    // A model alone keeps following the default; a provider choice is the session's own.
    await invoke("session_model_select", { session_id: id, model: "pre" });
    const after = (await invoke("connection_select", {
      session_id: id,
      profile: "ollama",
    })) as Status;
    expect(after.connection.profile).toBe("ollama");
    expect(after.origin).toBe("session");
    expect(after.model).toBe("");
    expect(after.choices.find((choice) => choice.active)?.name).toBe("ollama");
    const listed = (await invoke("chat_models", { session_id: id })) as {
      session_id: string;
      endpoint: string;
      kind: string;
    };
    expect(listed).toMatchObject({
      session_id: id,
      endpoint: "http://127.0.0.1:11434/v1",
      kind: "ollama_local",
    });

    // The other session and the global default are untouched.
    const untouched = (await invoke("connection_status", { session_id: other })) as Status;
    expect(untouched.connection.profile).toBeNull();
    const { settings } = (await invoke("chat_defaults", {})) as {
      settings: { endpoint: string };
    };
    expect(settings.endpoint).toBe("http://127.0.0.1:1234/v1");

    const chosen = (await invoke("session_model_select", {
      session_id: id,
      model: "lfm2.5:latest",
    })) as Status;
    expect(chosen.model).toBe("lfm2.5:latest");
    // The limit is 256 UTF-8 bytes, not 256 characters: 129 two-byte characters are refused.
    await expect(
      invoke("session_model_select", { session_id: id, model: "é".repeat(129) }),
    ).rejects.toMatchObject({ code: "invalid_request" });
    await expect(
      invoke("session_model_select", { session_id: id, model: "é".repeat(128) }),
    ).resolves.toMatchObject({ model: "é".repeat(128) });
    await expect(
      invoke("session_model_select", { session_id: id, model: "a\nb" }),
    ).rejects.toMatchObject({ code: "invalid_request" });
    await expect(
      invoke("connection_select", { session_id: id, profile: "missing" }),
    ).rejects.toMatchObject({ code: "control" });
    await expect(
      invoke("connection_select", { session_id: id, profile: "ollama" }),
    ).rejects.toMatchObject({ code: "control" });
    await expect(
      invoke("connection_select", { session_id: "nope", profile: null }),
    ).rejects.toMatchObject({ code: "not_found" });

    await expect(invoke("session_context", { session_id: id })).resolves.toMatchObject({
      branch: { state: "branch", name: "main" },
    });
  });

  // The cases core's `validate_base_url` decides (crates/omgcode-core/src/provider.rs tests).
  it.each([
    ["http://127.0.0.1:1234/v1", true],
    ["http://127.0.0.1:11434/v1/", true],
    ["http://127.9.8.7:65535/v1", true],
    ["http://[::1]:1234/v1", true],
    ["http://[0:0:0:0:0:0:0:1]:1234/v1/", true],
    ["http://[0::1]:1234/v1", true],
    ["http://127.0.0.1:0/v1", false],
    ["http://127.0.0.1:65536/v1", false],
    ["http://127.0.0.01:1234/v1", false],
    ["http://128.0.0.1:1234/v1", false],
    ["http://localhost:1234/v1", false],
    ["http://[::2]:1234/v1", false],
    ["http://[::ffff:127.0.0.1]:1234/v1", false],
    ["http://[1::2::1]:1234/v1", false],
    ["http://127.0.0.1/v1", false],
    ["https://127.0.0.1:1234/v1", false],
    ["http://user@127.0.0.1:1234/v1", false],
    ["http://127.0.0.1:1234/v1?x=1", false],
    ["http://127.0.0.1:1234/v1#x", false],
    ["http://127.0.0.1:1234/v2", false],
    [`http://127.0.0.1:1234/v1${String.fromCharCode(10)}`, false],
    [`http://127.0.0.1:1234/v1/${"a".repeat(240)}`, false],
  ])("decides the local endpoint %s as core does", (endpoint, accepted) => {
    expect(isLocalEndpoint(endpoint)).toBe(accepted);
  });

  it("records an IPv6 loopback connection and refuses port 0", async () => {
    const { invoke } = createMockBackend();
    let { revision } = (await invoke("settings_overview", {})) as SettingsOverview;
    const added = (await invoke("settings_add_connection", {
      revision,
      name: "v6",
      kind: "ollama_local",
      endpoint: "http://[::1]:11434/v1/",
    })) as SettingsOverview;
    expect(added.provider_profiles.some((p) => p.name === "v6")).toBe(true);
    revision = added.revision;
    await expect(
      invoke("settings_add_connection", {
        revision,
        name: "zero",
        kind: "lm_studio",
        endpoint: "http://127.0.0.1:0/v1",
      }),
    ).rejects.toMatchObject({ code: "control" });
  });

  it("refuses an API key with a control character or over 16 KiB on both key routes", async () => {
    const { invoke } = createMockBackend();
    const { revision } = (await invoke("settings_overview", {})) as SettingsOverview;
    const added = (await invoke("settings_add_connection", {
      revision,
      name: "cloud",
      kind: "ollama_cloud",
      endpoint: null,
      api_key: "test-key-not-real",
    })) as SettingsOverview;
    // 16 KiB of UTF-8 bytes: 5462 three-byte characters is one byte over.
    for (const key of ["a\nb", "a\u0000b", "k".repeat(16 * 1024 + 1), "€".repeat(5462)]) {
      await expect(
        invoke("settings_add_connection", {
          revision: added.revision,
          name: "other",
          kind: "ollama_cloud",
          endpoint: null,
          api_key: key,
        }),
      ).rejects.toMatchObject({ code: "invalid_request" });
      await expect(
        invoke("settings_set_api_key", { revision: added.revision, name: "cloud", api_key: key }),
      ).rejects.toMatchObject({ code: "invalid_request" });
    }
    // Nothing was recorded by the refused calls.
    const after = (await invoke("settings_overview", {})) as SettingsOverview;
    expect(after.revision).toBe(added.revision);
    await expect(
      invoke("settings_set_api_key", {
        revision: added.revision,
        name: "cloud",
        api_key: "k".repeat(16 * 1024),
      }),
    ).resolves.toMatchObject({ revision: added.revision + 1 });
  });

  it("keeps a session whose profile was removed stale until a provider is chosen again", async () => {
    const { invoke } = createMockBackend();
    const id = "00000000-0000-4000-8000-000000000001";
    await invoke("connection_select", { session_id: id, profile: "ollama" });
    const { revision } = (await invoke("settings_overview", {})) as SettingsOverview;
    await invoke("settings_remove_connection", { revision, name: "ollama" });

    const status = (await invoke("connection_status", { session_id: id })) as {
      stale: boolean;
      connection: { profile: string | null; endpoint: string };
    };
    expect(status.stale).toBe(true);
    expect(status.connection).toMatchObject({
      profile: "ollama",
      endpoint: "http://127.0.0.1:11434/v1",
    });
    const { settings: defaults } = (await invoke("chat_defaults", {})) as {
      settings: Record<string, unknown>;
    };
    const send = () =>
      invoke("chat_send", {
        session_id: id,
        message: "hello",
        settings: { ...defaults, model: "m", endpoint: status.connection.endpoint },
        skills: [],
      });
    await expect(send()).rejects.toMatchObject({ code: "session_connection_stale" });
    await expect(invoke("chat_models", { session_id: id })).rejects.toMatchObject({
      code: "session_connection_stale",
    });

    // Added again under the same name at another endpoint: offered, but not active, as in Rust.
    const { revision: next } = (await invoke("settings_overview", {})) as SettingsOverview;
    await invoke("settings_add_connection", {
      revision: next,
      name: "ollama",
      kind: "ollama_local",
      endpoint: "http://127.0.0.1:11435/v1",
      api_key: null,
    });
    const replaced = (await invoke("connection_status", { session_id: id })) as {
      stale: boolean;
      choices: Array<{ name: string; active: boolean }>;
    };
    expect(replaced.stale).toBe(true);
    expect(replaced.choices.map((choice) => choice.name)).toContain("ollama");
    expect(replaced.choices.some((choice) => choice.active)).toBe(false);

    // Choosing again clears it.
    const chosen = (await invoke("connection_select", { session_id: id, profile: null })) as {
      stale: boolean;
    };
    expect(chosen.stale).toBe(false);
  });

  it("opens Workspace settings for the workspace a new session belongs to", async () => {
    const { invoke } = createMockBackend();
    const { workspaces } = (await invoke("workspace_list", {})) as WorkspaceList;
    const second = workspaces[1]?.workspace_id;
    expect(second).toBeDefined();
    const created = (await invoke("session_create", { workspace_id: second, title: "two" })) as {
      session_id: string;
    };
    await invoke("settings_show_workspace", { session_id: created.session_id });
    const { scope } = (await invoke("settings_overview", {})) as SettingsOverview;
    expect(scope).toEqual({ workspace_id: second });
  });

  it("adds Ollama Cloud with a key, replaces the key, and removes a bound profile", async () => {
    const { invoke } = createMockBackend();
    let { revision } = (await invoke("settings_overview", {})) as SettingsOverview;
    const cloud = (fields: Record<string, unknown>) =>
      invoke("settings_add_connection", {
        revision,
        name: "cloud",
        kind: "ollama_cloud",
        endpoint: null,
        api_key: "test-key-not-real",
        ...fields,
      });
    // Like Rust: a key and no endpoint for the cloud, an endpoint and no key for a local kind.
    await expect(cloud({ api_key: null })).rejects.toMatchObject({ code: "invalid_request" });
    await expect(cloud({ endpoint: "https://example.com/v1" })).rejects.toMatchObject({
      code: "invalid_request",
    });
    await expect(
      invoke("settings_add_connection", {
        revision,
        name: "box",
        kind: "ollama_local",
        endpoint: "http://127.0.0.1:11500/v1",
        api_key: "test-key-not-real",
      }),
    ).rejects.toMatchObject({ code: "invalid_request" });
    const added = (await cloud({})) as SettingsOverview;
    expect(added.provider_profiles.at(-1)).toEqual({
      name: "cloud",
      kind: "ollama_cloud",
      auth: "api_key",
      endpoint: "https://ollama.com/v1",
      credential_bound: true,
      selectable: true,
      active: false,
    });
    // Nothing returned holds the key.
    expect(JSON.stringify(added)).not.toContain("test-key-not-real");
    revision = added.revision;

    await expect(
      invoke("settings_set_api_key", { revision, name: "ollama", api_key: "test-key-not-real" }),
    ).rejects.toMatchObject({ code: "invalid_request" });
    await expect(
      invoke("settings_set_api_key", { revision, name: "missing", api_key: "test-key-not-real" }),
    ).rejects.toMatchObject({ code: "control" });
    const rotated = (await invoke("settings_set_api_key", {
      revision,
      name: "cloud",
      api_key: "test-key-not-real-2",
    })) as SettingsOverview;
    expect(JSON.stringify(rotated)).not.toContain("test-key-not-real");

    const removed = (await invoke("settings_remove_connection", {
      revision: rotated.revision,
      name: "cloud",
    })) as SettingsOverview;
    expect(removed.provider_profiles.some((p) => p.name === "cloud")).toBe(false);
  });

  it("refuses a chat request without a skills list, like the Rust command", async () => {
    const { invoke } = createMockBackend();
    const { settings: defaults } = (await invoke("chat_defaults", {})) as {
      settings: { endpoint: string };
    };
    await expect(
      invoke("chat_send", {
        session_id: "00000000-0000-4000-8000-000000000001",
        message: "hello",
        settings: { ...defaults, model: "m" },
      }),
    ).rejects.toEqual({ code: "invalid_argument", message: "Missing skills." });
  });

  describe("chat_subscribe", () => {
    const SESSION = "00000000-0000-4000-8000-000000000001";

    beforeEach(() => {
      vi.useFakeTimers();
    });
    afterEach(() => {
      vi.useRealTimers();
    });

    async function send(invoke: ReturnType<typeof createMockBackend>["invoke"]): Promise<void> {
      const { settings: defaults } = (await invoke("chat_defaults", {})) as {
        settings: { endpoint: string };
      };
      await invoke("chat_send", {
        session_id: SESSION,
        message: "hello",
        settings: { ...defaults, model: "m" },
        skills: [],
      });
    }

    it("resolves with null and rejects a missing channel object", async () => {
      const { invoke } = createMockBackend();
      await expect(
        invoke("chat_subscribe", { on_event: { onmessage: () => undefined } }),
      ).resolves.toBeNull();
      await expect(invoke("chat_subscribe", {})).rejects.toMatchObject({
        code: "invalid_argument",
      });
    });

    it("pushes request_started, deltas, then finished when the turn completes", async () => {
      const { invoke } = createMockBackend();
      const events: ChatEvent[] = [];
      await invoke("chat_subscribe", { on_event: { onmessage: (e: ChatEvent) => events.push(e) } });
      await send(invoke);
      expect(events.map((e) => e.kind)).toEqual(["request_started"]);
      await vi.advanceTimersByTimeAsync(1999);
      const kinds = events.map((e) => e.kind);
      expect(kinds.filter((k) => k === "delta").length).toBeGreaterThanOrEqual(3);
      expect(kinds).not.toContain("finished");
      await vi.advanceTimersByTimeAsync(1);
      expect(events[events.length - 1]).toEqual({ kind: "finished", session_id: SESSION });
      const text = events.flatMap((e) => (e.kind === "delta" ? [e.text] : [])).join("");
      expect(text).toBe("This is a mock reply.");
      const status = (await invoke("chat_status", { session_id: SESSION })) as { state: string };
      expect(status.state).toBe("completed");
    });

    it("ends a cancelled turn at once as failed with the cancelled code", async () => {
      const { invoke } = createMockBackend();
      const events: ChatEvent[] = [];
      await invoke("chat_subscribe", { on_event: { onmessage: (e: ChatEvent) => events.push(e) } });
      await send(invoke);
      await vi.advanceTimersByTimeAsync(1000);
      const before = events.filter((e) => e.kind === "delta").length;
      const status = await invoke("chat_cancel", { session_id: SESSION });
      const failed = {
        session_id: SESSION,
        state: "failed",
        outcome: null,
        error: { code: "cancelled", message: "turn was cancelled" },
      };
      expect(status).toEqual(failed);
      await vi.advanceTimersByTimeAsync(5000);
      expect(await invoke("chat_status", { session_id: SESSION })).toEqual(failed);
      expect(events.filter((e) => e.kind === "delta")).toHaveLength(before);
      expect(events.filter((e) => e.kind === "finished")).toEqual([
        { kind: "finished", session_id: SESSION },
      ]);
      expect(events[events.length - 1]).toEqual({ kind: "finished", session_id: SESSION });
    });

    it("replays the preview of a running turn when subscribing", async () => {
      const { invoke } = createMockBackend();
      await send(invoke);
      await vi.advanceTimersByTimeAsync(1000);
      const events: ChatEvent[] = [];
      await invoke("chat_subscribe", { on_event: { onmessage: (e: ChatEvent) => events.push(e) } });
      await vi.advanceTimersByTimeAsync(0);
      expect(events[0]).toEqual({ kind: "request_started", session_id: SESSION, request: 1 });
      expect(events[1]).toMatchObject({
        kind: "delta",
        request: 1,
        text: expect.stringContaining("This"),
      });
    });

    it("replaces the first subscription", async () => {
      const { invoke } = createMockBackend();
      const first: ChatEvent[] = [];
      const second: ChatEvent[] = [];
      await invoke("chat_subscribe", { on_event: { onmessage: (e: ChatEvent) => first.push(e) } });
      await invoke("chat_subscribe", { on_event: { onmessage: (e: ChatEvent) => second.push(e) } });
      await send(invoke);
      expect(first).toEqual([]);
      expect(second).toHaveLength(1);
    });
  });

  describe("settings", () => {
    const WORKSPACE = "77777777-7777-4777-8777-777777777777";
    const FLAGS = {
      model_tools: true,
      read: "allow",
      write: "allow",
      execute: "prompt",
      task_network: "deny",
      host_services: "deny",
      write_tools: {
        edit: "allow",
        multi_edit: "allow",
        create: "allow",
        rename: "allow",
        delete: "prompt",
        create_directory: "allow",
      },
    };

    it("applies each change after the revision check and bumps the revision", async () => {
      const { invoke } = createMockBackend();
      const start = (await invoke("settings_overview", {})) as SettingsOverview;
      const root = (await invoke("settings_choose_root", {
        revision: start.revision,
      })) as SettingsOverview;
      expect(root.revision).toBe(start.revision + 1);
      expect(root.development_root).toBe("/home/dev");

      const changed = (await invoke("settings_set_permissions", {
        workspace_id: WORKSPACE,
        revision: root.revision,
        permissions: FLAGS,
      })) as SettingsOverview;
      expect(changed.workspaces[0]).toMatchObject(FLAGS);

      const cached = (await invoke("settings_choose_package_cache", {
        workspace_id: WORKSPACE,
        revision: changed.revision,
      })) as SettingsOverview;
      expect(cached.workspaces[0]?.package_cache_root).toBe("/home/dev/.cache/omgcode-packages");
      const cleared = (await invoke("settings_clear_package_cache", {
        workspace_id: WORKSPACE,
        revision: cached.revision,
      })) as SettingsOverview;
      expect(cleared.workspaces[0]?.package_cache_root).toBeNull();
      expect(cleared.revision).toBe(start.revision + 4);
    });

    it("refuses a stale revision and an unknown workspace", async () => {
      const { invoke } = createMockBackend();
      const { revision } = (await invoke("settings_overview", {})) as SettingsOverview;
      await expect(
        invoke("settings_choose_root", { revision: revision - 1 }),
      ).rejects.toMatchObject({ code: "stale_revision" });
      await expect(
        invoke("settings_clear_package_cache", { workspace_id: "missing", revision }),
      ).rejects.toMatchObject({ code: "unknown_workspace" });
      const after = (await invoke("settings_overview", {})) as SettingsOverview;
      expect(after.revision).toBe(revision);
    });

    it("adds, chooses, and removes local connections with core's refusals", async () => {
      const { invoke } = createMockBackend();
      let { revision } = (await invoke("settings_overview", {})) as SettingsOverview;
      const add = (fields: Record<string, unknown>) =>
        invoke("settings_add_connection", {
          revision,
          name: "box",
          kind: "ollama_local",
          endpoint: "http://127.0.0.1:11500/v1",
          ...fields,
        });
      await expect(add({ kind: "openai" })).rejects.toMatchObject({ code: "invalid_argument" });
      await expect(add({ endpoint: "http://example.com:11434/v1" })).rejects.toMatchObject({
        code: "control",
      });
      await expect(add({ name: "ollama" })).rejects.toMatchObject({ code: "control" });

      const added = (await add({})) as SettingsOverview;
      expect(added.revision).toBe(revision + 1);
      expect(added.provider_profiles.at(-1)).toEqual({
        name: "box",
        kind: "ollama_local",
        auth: "none",
        endpoint: "http://127.0.0.1:11500/v1",
        credential_bound: false,
        selectable: true,
        active: false,
      });
      revision = added.revision;

      await expect(
        invoke("settings_select_connection", { revision, profile: 7 }),
      ).rejects.toMatchObject({ code: "invalid_argument" });
      await expect(
        invoke("settings_select_connection", { revision, profile: "missing" }),
      ).rejects.toMatchObject({ code: "control" });
      const chosen = (await invoke("settings_select_connection", {
        revision,
        profile: "box",
      })) as SettingsOverview;
      expect(chosen.connection).toMatchObject({ profile: "box", kind: "ollama_local" });
      revision = chosen.revision;

      await expect(
        invoke("settings_remove_connection", { revision, name: "missing" }),
      ).rejects.toMatchObject({ code: "control" });
      // Removing the active connection returns to the default, as core's event does.
      const removed = (await invoke("settings_remove_connection", {
        revision,
        name: "box",
      })) as SettingsOverview;
      expect(removed.provider_profiles.map((p) => p.name)).toEqual(["lms", "ollama"]);
      expect(removed.connection).toMatchObject({ profile: null, kind: null, source: "active" });
      const defaults = (await invoke("chat_defaults", {})) as { settings: { endpoint: string } };
      expect(defaults.settings.endpoint).toBe(removed.connection.endpoint);
    });

    it("lists a registered workspace for a new session", async () => {
      const { invoke } = createMockBackend();
      const { revision } = (await invoke("settings_overview", {})) as SettingsOverview;
      const registered = (await invoke("settings_register_workspace", {
        revision,
      })) as SettingsOverview;
      const added = registered.workspaces.at(-1);
      expect(added).toMatchObject({ read: "allow", write: "prompt", execute: "deny" });
      const list = (await invoke("workspace_list", {})) as WorkspaceList;
      expect(list.development_root_set).toBe(true);
      expect(list.workspaces.map((w) => w.workspace_id)).toEqual(
        registered.workspaces.map((w) => w.workspace_id),
      );
      expect(list.workspaces.at(-1)).toEqual({
        workspace_id: added?.workspace_id,
        folder_display: added?.folder_display,
        model_tools: false,
        read: "allow",
        write: "prompt",
        execute: "deny",
      });
    });
  });
});

describe("mock chat defaults (ADR-0080)", () => {
  it("keeps each window's saved fields when the other window saves", async () => {
    const { invoke } = createMockBackend();
    await invoke("settings_save_chat_defaults", {
      defaults: {
        context_tokens: 65_536,
        max_output_tokens: 2048,
        max_tool_calls: 32,
        turn_timeout_seconds: 300,
        instructions: "Be brief.",
      },
    });
    const saved = (await invoke("chat_preferences_save", {
      preferences: { compact_threshold: 512_000, auto_compact: false, enter_to_send: false },
    })) as ChatDefaults;
    expect(saved.settings).toMatchObject({
      context_tokens: 65_536,
      instructions: "Be brief.",
      compact_threshold: 512_000,
      auto_compact: false,
    });
    expect(saved.enter_to_send).toBe(false);
    const overview = (await invoke("settings_overview", {})) as SettingsOverview;
    expect(overview.chat.defaults.context_tokens).toBe(65_536);
    expect(overview.chat.source).toBe("saved");
    await expect(
      invoke("chat_preferences_save", {
        preferences: { compact_threshold: 6000, auto_compact: false, enter_to_send: true },
      }),
    ).rejects.toMatchObject({ code: "invalid_request" });
  });

  it("stores a workspace's instructions without changing the revision", async () => {
    const { invoke } = createMockBackend();
    const before = (await invoke("settings_overview", {})) as SettingsOverview;
    const id = before.workspaces[1]?.workspace_id ?? "";
    const after = (await invoke("settings_set_workspace_instructions", {
      workspace_id: id,
      instructions: "Use tabs.",
    })) as SettingsOverview;
    expect(after.revision).toBe(before.revision);
    expect(after.workspaces[1]?.instructions).toBe("Use tabs.");
  });

  it("refuses workspace instructions that do not fit beside the global text, in UTF-8 bytes", async () => {
    const { invoke } = createMockBackend();
    const before = (await invoke("settings_overview", {})) as SettingsOverview;
    const id = before.workspaces[0]?.workspace_id ?? "";
    const saved = before.workspaces[0]?.instructions;
    // No global text: 16 KiB less the 24-byte workspace heading.
    const room = before.workspaces[0]?.instructions_max_bytes ?? 0;
    expect(room).toBe(16 * 1024 - 24);
    // 5,454 three-byte characters are 16,362 bytes, though only 5,454 UTF-16 units.
    for (const instructions of ["i".repeat(room + 1), "€".repeat(5454)]) {
      await expect(
        invoke("settings_set_workspace_instructions", { workspace_id: id, instructions }),
      ).rejects.toMatchObject({ code: "invalid_request" });
    }
    const after = (await invoke("settings_overview", {})) as SettingsOverview;
    expect(after.workspaces[0]?.instructions).toBe(saved);
    // The room itself fits.
    await invoke("settings_set_workspace_instructions", {
      workspace_id: id,
      instructions: "i".repeat(room),
    });
  });

  it("keeps the global and every workspace text sendable together", async () => {
    const { invoke } = createMockBackend();
    const start = (await invoke("settings_overview", {})) as SettingsOverview;
    const id = start.workspaces[0]?.workspace_id ?? "";
    await invoke("settings_set_workspace_instructions", {
      workspace_id: id,
      instructions: "w".repeat(10_000),
    });
    const overview = (await invoke("settings_overview", {})) as SettingsOverview;
    // Both headings and the blank line take 47 bytes.
    const globalRoom = 16 * 1024 - 47 - 10_000;
    expect(overview.chat.instructions_max_bytes).toBe(globalRoom);
    const defaults = { ...overview.chat.defaults, context_tokens: 8192 };
    await expect(
      invoke("settings_save_chat_defaults", {
        defaults: { ...defaults, instructions: "g".repeat(globalRoom + 1) },
      }),
    ).rejects.toMatchObject({ code: "invalid_request" });
    const saved = (await invoke("settings_save_chat_defaults", {
      defaults: { ...defaults, instructions: "g".repeat(globalRoom) },
    })) as SettingsOverview;
    // With that global text saved, the workspace has exactly its own text's room.
    expect(saved.workspaces[0]?.instructions_max_bytes).toBe(10_000);
  });
});
