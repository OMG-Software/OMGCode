import { afterEach, describe, expect, it } from "vitest";
import { setInvoker } from "../src/ipc/client";
import type { ConnectionStatus } from "../src/ipc/types";
import { createConnectionModel } from "../src/main/connection_state";
import { connectionStatus } from "./helpers";

// Every write for the open session runs after the one before it settled, so Rust sees them in
// the operator's order, and an answer from before a provider change never reaches the new state.

interface Deferred {
  command: string;
  args: Record<string, unknown>;
  resolve: (status: ConnectionStatus) => void;
}

const status = (model: string, profile: string | null = null): ConnectionStatus =>
  connectionStatus({ model, connection: { ...connectionStatus().connection, profile } });

/** Every write waits for the test to settle it; reads answer at once. */
function install(): Deferred[] {
  const writes: Deferred[] = [];
  setInvoker((command, args) => {
    if (command === "connection_status") {
      return Promise.resolve(status("read"));
    }
    return new Promise<ConnectionStatus>((resolve) => {
      writes.push({ command, args: args as Record<string, unknown>, resolve });
    });
  });
  return writes;
}

const flush = async (): Promise<void> => {
  for (let turn = 0; turn < 10; turn += 1) {
    await Promise.resolve();
  }
};

afterEach(() => {
  setInvoker(null);
});

describe("serialized connection writes", () => {
  it("sends the second model save only after the first settled, and the last pick wins", async () => {
    const writes = install();
    const model = createConnectionModel();
    await model.open("s-a");

    const first = model.chooseModel("m1");
    const second = model.chooseModel("m2");
    await flush();
    expect(writes.map((write) => write.args.model)).toEqual(["m1"]);

    writes[0]?.resolve(status("m1"));
    await flush();
    expect(writes.map((write) => write.args.model)).toEqual(["m1", "m2"]);

    writes[1]?.resolve(status("m2"));
    await Promise.all([first, second]);
    expect(model.status()?.model).toBe("m2");
  });

  it("makes a provider change wait for a pending model save and drops that save's reply", async () => {
    const writes = install();
    const model = createConnectionModel();
    await model.open("s-a");

    const saving = model.chooseModel("old-model");
    const switching = model.select("cloud");
    await flush();
    expect(writes.map((write) => write.command)).toEqual(["session_model_select"]);

    writes[0]?.resolve(status("old-model"));
    await flush();
    expect(writes.map((write) => write.command)).toEqual([
      "session_model_select",
      "connection_select",
    ]);
    // The reply to the older model save did not overwrite the state.
    expect(model.status()?.model).not.toBe("old-model");

    writes[1]?.resolve(status("", "cloud"));
    await Promise.all([saving, switching]);
    expect(model.status()?.connection.profile).toBe("cloud");
    expect(model.status()?.model).toBe("");
  });
});
