import { invokeCommand } from "./client";
import type { PermissionFlags, SettingsOverview } from "./types";

/**
 * Commands that only the settings window may call (ADR-0059, ADR-0060). Imported by that entry
 * alone. No command takes a path: Rust opens a native folder dialog where one is needed, and asks
 * for a native confirmation before any change that widens access. Every change returns the new
 * overview.
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
};
