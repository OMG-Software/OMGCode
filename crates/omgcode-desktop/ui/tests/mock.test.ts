import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ChatEvent, SettingsOverview, WorkspaceList } from "../src/ipc/types";
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

  it("serves the status bar's connection choice and branch, like core", async () => {
    const { invoke } = createMockBackend();
    const before = (await invoke("connection_status", {})) as {
      connection: { profile: string | null };
      choices: Array<{ name: string; active: boolean }>;
    };
    expect(before.connection.profile).toBeNull();
    expect(before.choices.map((choice) => choice.name)).toContain("ollama");
    expect(before.choices.every((choice) => !choice.active)).toBe(true);

    const after = (await invoke("connection_select", { profile: "ollama" })) as typeof before;
    expect(after.connection.profile).toBe("ollama");
    expect(after.choices.find((choice) => choice.active)?.name).toBe("ollama");
    const { settings } = (await invoke("chat_defaults", {})) as {
      settings: { endpoint: string };
    };
    expect(settings.endpoint).toBe("http://127.0.0.1:11434/v1");
    await expect(invoke("connection_select", { profile: "missing" })).rejects.toMatchObject({
      code: "control",
    });
    // The settings window's revision moves on, as a change through core would.
    const overview = (await invoke("settings_overview", {})) as { revision: number };
    expect(overview.revision).toBeGreaterThan(12);

    await expect(
      invoke("session_context", { session_id: "00000000-0000-4000-8000-000000000001" }),
    ).resolves.toMatchObject({ branch: { state: "branch", name: "main" } });
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
    const added = (await invoke("settings_add_local_connection", {
      revision,
      name: "v6",
      kind: "ollama_local",
      endpoint: "http://[::1]:11434/v1/",
    })) as SettingsOverview;
    expect(added.provider_profiles.some((p) => p.name === "v6")).toBe(true);
    revision = added.revision;
    await expect(
      invoke("settings_add_local_connection", {
        revision,
        name: "zero",
        kind: "lm_studio",
        endpoint: "http://127.0.0.1:0/v1",
      }),
    ).rejects.toMatchObject({ code: "control" });
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
        invoke("settings_add_local_connection", {
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
      await expect(
        invoke("settings_remove_connection", { revision, name: "lms" }),
      ).rejects.toMatchObject({ code: "credential_bound" });
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
