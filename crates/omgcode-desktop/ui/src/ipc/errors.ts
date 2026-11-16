import type { IpcError } from "./types";

/** Error code of a chat turn the operator cancelled. The core sets it; the window matches only this code. */
export const CANCELLED_CODE = "cancelled";

/** Error thrown by every IPC call. `code` and `message` come from the Rust core. */
export class IpcFailure extends Error {
  readonly code: string;

  constructor(error: IpcError) {
    super(error.message);
    this.name = "IpcFailure";
    this.code = error.code;
  }
}

function isIpcError(value: unknown): value is IpcError {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const record = value as Record<string, unknown>;
  return typeof record.code === "string" && typeof record.message === "string";
}

/** Normalises any rejection into an `IpcError`. The message is data and is shown as text. */
export function toIpcError(reason: unknown): IpcError {
  if (reason instanceof IpcFailure) {
    return { code: reason.code, message: reason.message };
  }
  if (isIpcError(reason)) {
    return { code: reason.code, message: reason.message };
  }
  if (typeof reason === "string") {
    return { code: "unknown", message: reason };
  }
  return { code: "unknown", message: "The command failed." };
}
