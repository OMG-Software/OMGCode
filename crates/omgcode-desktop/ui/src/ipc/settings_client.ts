import { invokeCommand } from "./client";
import type {
  ChatBudgets,
  ChatGptSignOutResult,
  NewConnectionKind,
  PermissionFlags,
  SettingsOverview,
  ToolchainDiscovery,
} from "./types";

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
  /**
   * Removes one workspace's registration after a native confirmation Rust shows (ADR-0081). The
   * folder and its files are not touched. Sends the identifier and revision, never a path.
   */
  unregisterWorkspace: (workspaceId: string, revision: number) =>
    invokeCommand<SettingsOverview>("settings_unregister_workspace", {
      workspace_id: workspaceId,
      revision,
    }),
  /** Chooses the active model connection: a recorded profile, or `null` for the default. */
  selectConnection: (revision: number, profile: string | null) =>
    invokeCommand<SettingsOverview>("settings_select_connection", { revision, profile }),
  /**
   * Records a connection. A local kind sends `endpoint` and a `null` key; a named hosted
   * provider sends a `null` endpoint (Rust uses its fixed one) and the API key; an
   * OpenAI-compatible service sends both. The key is write-only: this is the one place it is
   * sent, and nothing here keeps it.
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
  /** Replaces a hosted profile's stored API key. Write-only, like the add command. */
  setApiKey: (revision: number, name: string, apiKey: string) =>
    invokeCommand<SettingsOverview>("settings_set_api_key", {
      revision,
      name,
      api_key: apiKey,
    }),
  /**
   * Asks Rust to remove the extra grants on the application data folder (ADR-0073). It sends
   * nothing: Rust reads the folder, names each account in a native confirmation, and removes
   * exactly those grants on Yes.
   */
  repairDataFolder: () => invokeCommand<SettingsOverview>("settings_repair_data_folder", {}),
  /** Clears the window's workspace scope and returns the unscoped overview (ADR-0071). */
  clearScope: () => invokeCommand<SettingsOverview>("settings_clear_scope", {}),
  removeConnection: (revision: number, name: string) =>
    invokeCommand<SettingsOverview>("settings_remove_connection", { revision, name }),
  /**
   * Signs a connection in with ChatGPT (ADR-0093). Rust confirms natively, opens the system
   * browser, and waits for OpenAI's answer; this window never sees the address or a token.
   */
  signInChatGpt: (revision: number, name: string) =>
    invokeCommand<SettingsOverview>("settings_sign_in_chatgpt", { revision, name }),
  /** Ends the running ChatGPT sign-in's wait, if any. */
  cancelChatGptSignIn: () => invokeCommand<null>("settings_cancel_chatgpt_sign_in", {}),
  /** Revokes and forgets a ChatGPT sign-in after a native confirmation. */
  signOutChatGpt: (revision: number, name: string) =>
    invokeCommand<ChatGptSignOutResult>("settings_sign_out_chatgpt", { revision, name }),
  /** Saves the chat budgets and the global custom instructions (ADR-0080). Grants nothing. */
  saveChatDefaults: (defaults: ChatBudgets) =>
    invokeCommand<SettingsOverview>("settings_save_chat_defaults", { defaults }),
  /** Replaces one workspace's custom instructions; empty text removes them (ADR-0080). */
  setWorkspaceInstructions: (workspaceId: string, instructions: string) =>
    invokeCommand<SettingsOverview>("settings_set_workspace_instructions", {
      workspace_id: workspaceId,
      instructions,
    }),
  /**
   * Asks Rust to open its system folder dialog and import the chosen skill folder through core,
   * disabled (ADR-0082). It sends only the revision: the window never names a path.
   */
  importSkill: (revision: number) =>
    invokeCommand<SettingsOverview>("settings_import_skill", { revision }),
  /**
   * Sets a skill's application setting (`workspaceId` null) or one workspace's override. Rust
   * confirms in a system dialog, naming the skill, before any change that leaves it enabled.
   */
  setSkillState: (
    revision: number,
    name: string,
    workspaceId: string | null,
    state: "enabled" | "disabled" | "inherit",
  ) =>
    invokeCommand<SettingsOverview>("settings_set_skill_state", {
      revision,
      name,
      workspace_id: workspaceId,
      skill_state: state,
    }),
  /** Removes a skill and its stored copy after a system confirmation that names it. */
  removeSkill: (revision: number, name: string) =>
    invokeCommand<SettingsOverview>("settings_remove_skill", { revision, name }),
  /**
   * Asks Rust to open a system folder dialog for each read-only root and each `PATH` entry
   * (`rootCount`, `pathEntryCount`), then confirm natively, and record a toolchain profile
   * (ADR-0088). No path is sent. Variable values are plain text by design and never shown in a
   * dialog.
   */
  addToolchainProfile: (
    revision: number,
    name: string,
    environment: Array<{ name: string; value: string }>,
    rootCount: number,
    pathEntryCount: number,
  ) =>
    invokeCommand<SettingsOverview>("settings_add_toolchain_profile", {
      revision,
      name,
      environment,
      root_count: rootCount,
      path_entry_count: pathEntryCount,
    }),
  /** Removes a toolchain profile after a system confirmation. */
  removeToolchainProfile: (revision: number, name: string) =>
    invokeCommand<SettingsOverview>("settings_remove_toolchain_profile", { revision, name }),
  /** Records a package-network profile (configuration only) after a system confirmation. */
  addPackageProfile: (revision: number, name: string, origins: string[]) =>
    invokeCommand<SettingsOverview>("settings_add_package_profile", { revision, name, origins }),
  /** Removes a package-network profile, and unbinds its credentials, after a confirmation. */
  removePackageProfile: (revision: number, name: string) =>
    invokeCommand<SettingsOverview>("settings_remove_package_profile", { revision, name }),
  /**
   * Adds one host pattern to a workspace's `web_fetch` allowlist. Rust parses the pattern first
   * (an invalid one is refused with its `web_host_*` code and no dialog), then confirms natively.
   */
  addWebFetchHost: (revision: number, workspaceId: string, pattern: string) =>
    invokeCommand<SettingsOverview>("settings_add_web_fetch_host", {
      revision,
      workspace_id: workspaceId,
      pattern,
    }),
  /** Removes one host pattern from a workspace's allowlist after a system confirmation. */
  removeWebFetchHost: (revision: number, workspaceId: string, pattern: string) =>
    invokeCommand<SettingsOverview>("settings_remove_web_fetch_host", {
      revision,
      workspace_id: workspaceId,
      pattern,
    }),
  /**
   * Windows only. Asks Rust for a folder in a system dialog, confirms natively, and grants it to
   * the workspace's container. No path is sent.
   */
  grantContainerFolder: (
    revision: number,
    workspaceId: string,
    access: "read_execute" | "read_write",
  ) =>
    invokeCommand<SettingsOverview>("settings_grant_container_folder", {
      revision,
      workspace_id: workspaceId,
      access,
    }),
  /** Windows only. Revokes one container grant after a system confirmation. */
  revokeContainerGrant: (revision: number, grantId: string) =>
    invokeCommand<SettingsOverview>("settings_revoke_container_grant", {
      revision,
      grant_id: grantId,
    }),
  /**
   * Asks Rust to read this computer's install folders and `PATH` and propose toolchain profiles
   * (ADR-0103). Nothing runs and nothing is recorded. With a workspace, on Windows, each root
   * says what that workspace's container needs to read it.
   */
  discoverToolchains: (workspaceId: string | null) =>
    invokeCommand<ToolchainDiscovery>("settings_discover_toolchains", {
      workspace_id: workspaceId,
    }),
  /**
   * Records the selected proposals after one system confirmation that lists everything they
   * hold. Sends their digests, never a path; Rust discovers again and refuses a selection that
   * no longer matches.
   */
  addDiscoveredProfiles: (revision: number, digests: string[], workspaceId: string | null) =>
    invokeCommand<ToolchainDiscovery>("settings_add_discovered_profiles", {
      revision,
      digests,
      workspace_id: workspaceId,
    }),
  /**
   * Windows only. Grants the workspace's container read-execute access to the selected
   * proposals' roots it cannot read, after one system confirmation that names every folder.
   */
  grantDiscoveredRoots: (revision: number, workspaceId: string, digests: string[]) =>
    invokeCommand<ToolchainDiscovery>("settings_grant_discovered_roots", {
      revision,
      workspace_id: workspaceId,
      digests,
    }),
};
