import type { IpcError } from "./types";

/** Error code of a chat turn the operator cancelled. The core sets it; the window matches only this code. */
export const CANCELLED_CODE = "cancelled";
/** The settings window chose another model connection after this window read its endpoint. */
export const CONNECTION_CHANGED_CODE = "connection_changed";

/** The application data folder grants another account access, so no API key is stored (ADR-0073). */
export const STORAGE_NOT_PRIVATE_CODE = "credential_storage_not_private";
/** A folder repair found different grants from those the operator reviewed and changed nothing. */
export const DATA_FOLDER_CHANGED_CODE = "data_folder_changed";
/** A folder repair could not check the folder's permissions before asking, so it changed nothing. */
export const DATA_FOLDER_UNREADABLE_CODE = "data_folder_unreadable";
/** A folder repair removed the grants, but the settings could not be read again afterwards. */
export const DATA_FOLDER_REPAIRED_CODE = "data_folder_repaired";
/** Another settings change held the one-change-at-a-time guard, so this one did nothing. */
export const SETTINGS_CHANGE_RUNNING_CODE = "settings_change_running";

/** The connection recorded for the session no longer matches its profile; the operator must choose again. */
export const SESSION_CONNECTION_STALE_CODE = "session_connection_stale";

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
