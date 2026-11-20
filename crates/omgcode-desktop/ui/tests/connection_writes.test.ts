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

/** Every call, reads included, waits for the test to settle it. */
function installDeferred(): Deferred[] {
  const calls: Deferred[] = [];
  setInvoker(
    (command, args) =>
      new Promise<ConnectionStatus>((resolve) => {
        calls.push({ command, args: args as Record<string, unknown>, resolve });
      }),
  );
  return calls;
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

  it("drops a status read that started before a model choice", async () => {
    const calls = installDeferred();
    const model = createConnectionModel();
    const opening = model.open("s-a");
    await flush();
    calls[0]?.resolve(status("old"));
    await opening;

    // The composer sets its dropdown from every "read", so a late one would undo the choice.
    let reads = 0;
    model.subscribe((change) => {
      if (change === "read") {
        reads += 1;
      }
    });

    // A focus refresh reads the old model; the operator chooses another before it answers.
    const refreshing = model.refresh();
    const choosing = model.chooseModel("new");
    await flush();
    calls[1]?.resolve(status("old"));
    await refreshing;
    expect(reads).toBe(0);

    calls[2]?.resolve(status("new"));
    await choosing;
    expect(calls.map((call) => call.command)).toEqual([
      "connection_status",
      "connection_status",
      "session_model_select",
    ]);
    expect(model.status()?.model).toBe("new");
  });

  it("drops a status read that answers while the session's model save is pending", async () => {
    const calls = installDeferred();
    const model = createConnectionModel();
    const opening = model.open("s-a");
    await flush();
    calls[0]?.resolve(status("old"));
    await opening;
    let changes = 0;
    model.subscribe((change) => {
      if (change === "read") {
        changes += 1;
      }
    });

    const choosing = model.chooseModel("new");
    await flush();
    const refreshing = model.refresh();
    await flush();
    // Rust has not seen the save yet, so this read still holds the old model.
    calls[2]?.resolve(status("old"));
    await refreshing;
    expect(changes).toBe(0);

    calls[1]?.resolve(status("new"));
    await choosing;
    expect(model.status()?.model).toBe("new");
  });

  it("still reads another session while one session's model save is pending", async () => {
    const calls = installDeferred();
    const model = createConnectionModel();
    const opening = model.open("s-a");
    await flush();
    calls[0]?.resolve(status("a-model"));
    await opening;

    const choosing = model.chooseModel("a-new");
    await flush();
    const switching = model.open("s-b");
    await flush();
    calls[2]?.resolve(status("b-model"));
    await switching;
    expect(model.sessionId()).toBe("s-b");
    expect(model.status()?.model).toBe("b-model");

    // The save answers for a session that is no longer open; it changes nothing shown.
    calls[1]?.resolve(status("a-new"));
    await choosing;
    expect(model.status()?.model).toBe("b-model");
  });
});
