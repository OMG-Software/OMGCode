import { describe, expect, it } from "vitest";
import { createMockBackend } from "../src/mock/backend";
import { OMGCODE_MOCK_FIXTURE_MARKER } from "../src/mock/fixtures";

interface Detail {
  review_token: string | null;
  binding: string;
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
    const defaults = (await invoke("chat_defaults", {})) as { endpoint: string };
    const send = (endpoint: string) =>
      invoke("chat_send", {
        session_id: "00000000-0000-4000-8000-000000000001",
        message: "hello",
        settings: { ...defaults, model: "m", endpoint },
      });
    await expect(send("http://127.0.0.1:9/v1")).rejects.toEqual({
      code: "invalid_request",
      message: "The endpoint is fixed by the application.",
    });
    await expect(send(defaults.endpoint)).resolves.toMatchObject({ state: "running" });
  });
});
