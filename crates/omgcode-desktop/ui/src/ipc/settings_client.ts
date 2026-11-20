import { invokeCommand } from "./client";
import type { NewConnectionKind, PermissionFlags, SettingsOverview } from "./types";

/**
 * Commands that only the settings window may call (ADR-0059, ADR-0060, ADR-0064). Imported by
 * that entry alone. No command takes a path: Rust opens a native folder dialog where one is
 * needed, and asks for a native confirmation before any change that widens access or chooses
 * where chat turns are sent. Every change returns the new overview.
 */
export const settingsApi = {
  overview: () => invokeCommand<SettingsOverview>("settings_overview", {}),
  chooseRoot: (revision: number) =>
    invokeCommand<SettingsOverview>("settings_choose_root", { revision }),
  registerWorkspace: (revision: number) =>
    invokeCommand<SettingsOverview>("settings_register_workspace", { revision }),
  setPermissions: (workspaceId: string, revision: number, permissions: PermissionFlags) =>
    invokeCommand<SettingsOverview>("settings_set_permissions", {
      workspace_id: workspaceId,
      revision,
      permissions,
    }),
  choosePackageCache: (workspaceId: string, revision: number) =>
    invokeCommand<SettingsOverview>("settings_choose_package_cache", {
      workspace_id: workspaceId,
      revision,
    }),
  clearPackageCache: (workspaceId: string, revision: number) =>
    invokeCommand<SettingsOverview>("settings_clear_package_cache", {
      workspace_id: workspaceId,
      revision,
    }),
  /** Chooses the active model connection: a recorded profile, or `null` for the default. */
  selectConnection: (revision: number, profile: string | null) =>
    invokeCommand<SettingsOverview>("settings_select_connection", { revision, profile }),
  /**
   * Records a connection. A local kind sends `endpoint` and a `null` key; Ollama Cloud sends a
   * `null` endpoint (Rust uses the fixed one) and the API key. The key is write-only: this is
   * the one place it is sent, and nothing here keeps it.
   */
  addConnection: (
    revision: number,
    name: string,
    kind: NewConnectionKind,
    endpoint: string | null,
    apiKey: string | null,
  ) =>
    invokeCommand<SettingsOverview>("settings_add_connection", {
      revision,
      name,
      kind,
      endpoint,
      api_key: apiKey,
    }),
  /** Replaces an Ollama Cloud profile's stored API key. Write-only, like the add command. */
  setApiKey: (revision: number, name: string, apiKey: string) =>
    invokeCommand<SettingsOverview>("settings_set_api_key", {
      revision,
      name,
      api_key: apiKey,
    }),
  /** Clears the window's workspace scope and returns the unscoped overview (ADR-0067). */
  clearScope: () => invokeCommand<SettingsOverview>("settings_clear_scope", {}),
  removeConnection: (revision: number, name: string) =>
    invokeCommand<SettingsOverview>("settings_remove_connection", { revision, name }),
};
