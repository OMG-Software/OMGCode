import type {
  ChatBudgets,
  NewConnectionKind,
  PermissionFlags,
  SettingsOverview,
  WorkspaceSettingsView,
} from "../ipc/types";

/** What one control asks the core to do, given the revision the window last read. */
export type Change = (revision: number) => Promise<SettingsOverview>;

/** Runs one change, or ignores it while another waits. */
export type Submit = (change: Change) => void;

/**
 * The changes a control can ask for. Only the window shell reaches the command client; each
 * section names a change here and passes it to `submit`. Every one sends identifiers and the
 * revision, never a path.
 */
export interface Actions {
  chooseRoot(): Change;
  registerWorkspace(): Change;
  setPermissions(workspaceId: string, permissions: PermissionFlags): Change;
  choosePackageCache(workspaceId: string): Change;
  clearPackageCache(workspaceId: string): Change;
  /** Removes one workspace's registration; Rust confirms and touches no file (ADR-0081). */
  unregisterWorkspace(workspaceId: string): Change;
  selectConnection(profile: string | null): Change;
  /**
   * `endpoint` is for a local kind or an OpenAI-compatible service; `apiKey` is for a hosted
   * kind. A field the kind does not take is `null`.
   */
  addConnection(
    name: string,
    kind: NewConnectionKind,
    endpoint: string | null,
    apiKey: string | null,
  ): Change;
  /** Replaces a hosted profile's stored API key. */
  setApiKey(name: string, apiKey: string): Change;
  removeConnection(name: string): Change;
  /**
   * Signs a connection in with ChatGPT, recording an `openai` subscription profile with this name
   * when none exists (ADR-0093). Rust confirms natively and opens the browser.
   */
  signInChatGpt(name: string): Change;
  /** Revokes and forgets a ChatGPT sign-in after a native confirmation. */
  signOutChatGpt(name: string): Change;
  /** Saves the chat budgets and the global custom instructions (ADR-0080). */
  saveChatDefaults(defaults: ChatBudgets): Change;
  /** Replaces one workspace's custom instructions; empty text removes them. */
  setWorkspaceInstructions(workspaceId: string, instructions: string): Change;
  /** Opens Rust's system folder dialog and imports the chosen skill folder, disabled (ADR-0082). */
  importSkill(): Change;
  /** Sets a skill's application setting (`workspaceId` null) or one workspace's override. */
  setSkillState(name: string, workspaceId: string | null, state: SkillState): Change;
  /** Removes a skill and its stored copy after a system confirmation. */
  removeSkill(name: string): Change;
  /**
   * Records a toolchain profile (ADR-0088). Rust opens `rootCount` folder dialogs, then
   * `pathEntryCount` more, and confirms natively; the window sends no path.
   */
  addToolchainProfile(
    name: string,
    environment: Array<{ name: string; value: string }>,
    rootCount: number,
    pathEntryCount: number,
  ): Change;
  removeToolchainProfile(name: string): Change;
  /** Records a package-network profile, which is configuration only. */
  addPackageProfile(name: string, origins: string[]): Change;
  removePackageProfile(name: string): Change;
  /** Adds one host pattern to a workspace's `web_fetch` allowlist; Rust confirms natively. */
  addWebFetchHost(workspaceId: string, pattern: string): Change;
  /** Removes one host pattern from a workspace's allowlist; Rust confirms natively. */
  removeWebFetchHost(workspaceId: string, pattern: string): Change;
  /** Windows only. Rust opens the folder dialog. */
  grantContainerFolder(workspaceId: string, access: ContainerAccess): Change;
  revokeContainerGrant(grantId: string): Change;
}

/**
 * The workspace the window is scoped to (ADR-0071), or `null` when it shows every workspace: no
 * scope is set, or the scoped workspace is no longer registered. The scope is display state only.
 */
export function scopedWorkspace(overview: SettingsOverview): WorkspaceSettingsView | null {
  const scope = overview.scope;
  if (scope === null) {
    return null;
  }
  return overview.workspaces.find((w) => w.workspace_id === scope.workspace_id) ?? null;
}

/** A skill's setting at one scope. `inherit` applies only to a workspace. */
export type SkillState = "enabled" | "disabled" | "inherit";

export type SectionId = "model" | "workspaces" | "advanced";

/** What the window remembers between renders. It lives in memory only; nothing is stored. */
export interface ViewState {
  section: SectionId;
  /** The workspace chosen under Workspaces in the navigation, by identifier. */
  workspaceId: string | null;
  /** Whether the Workspaces section shows that workspace, not the overview. */
  workspacePage: boolean;
  /** Whether the navigation lists the workspaces under Workspaces. */
  workspacesExpanded: boolean;
  toolLimitsOpen: boolean;
  addConnectionOpen: boolean;
  /** What the operator has typed into the add-connection form, kept until an add succeeds. */
  connectionDraft: ConnectionDraft;
  /** Typed but unsaved chat defaults, or `null` to show the saved ones (ADR-0080). */
  chatDraft: ChatDraft | null;
  /** Typed but unsaved workspace instructions, by workspace identifier. */
  instructionDrafts: Map<string, string>;
  /** Typed but unadded web host patterns, by workspace identifier. */
  webHostDrafts: Map<string, string>;
  /** The Advanced section's three add forms, kept until an add succeeds (ADR-0088). */
  executionForms: ExecutionForms;
}

/** The chat defaults form's values as typed; numbers stay text until a save checks them. */
export interface ChatDraft {
  context_tokens: string;
  max_output_tokens: string;
  max_tool_calls: string;
  turn_timeout_seconds: string;
  instructions: string;
}

/** The add-connection form's values. It never holds an API key: that stays in its input. */
export interface ConnectionDraft {
  kind: NewConnectionKind;
  name: string;
  endpoint: string;
}

export interface SectionContext {
  overview: SettingsOverview;
  view: ViewState;
  submit: Submit;
  actions: Actions;
  /** Draws the current overview again, for a choice that sends nothing to the core. */
  rerender(): void;
}

/** What a container grant gives. */
export type ContainerAccess = "read_execute" | "read_write";

/**
 * The add-profile and grant forms' typed values and whether each is open. Counts and the
 * environment stay text until the operator clicks. Variable values are typed here and nowhere
 * else; they are plain configuration, never secrets.
 */
export interface ExecutionForms {
  toolchainOpen: boolean;
  toolchain: { name: string; roots: string; pathEntries: string; environment: string };
  packageOpen: boolean;
  package: { name: string; origins: string };
  grantOpen: boolean;
  grant: { workspaceId: string; access: ContainerAccess };
}

/** The forms' starting values. */
export function newExecutionForms(): ExecutionForms {
  return {
    toolchainOpen: false,
    toolchain: { name: "", roots: "1", pathEntries: "0", environment: "" },
    packageOpen: false,
    package: { name: "", origins: "" },
    grantOpen: false,
    grant: { workspaceId: "", access: "read_execute" },
  };
}
