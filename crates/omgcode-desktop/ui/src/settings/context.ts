import type {
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
  selectConnection(profile: string | null): Change;
  /** `endpoint` is for a local kind; `apiKey` is for Ollama Cloud. The other is `null`. */
  addConnection(
    name: string,
    kind: NewConnectionKind,
    endpoint: string | null,
    apiKey: string | null,
  ): Change;
  /** Replaces an Ollama Cloud profile's stored API key. */
  setApiKey(name: string, apiKey: string): Change;
  removeConnection(name: string): Change;
}

/**
 * The workspace the window is scoped to (ADR-0070), or `null` when it shows every workspace: no
 * scope is set, or the scoped workspace is no longer registered. The scope is display state only.
 */
export function scopedWorkspace(overview: SettingsOverview): WorkspaceSettingsView | null {
  const scope = overview.scope;
  if (scope === null) {
    return null;
  }
  return overview.workspaces.find((w) => w.workspace_id === scope.workspace_id) ?? null;
}

export type SectionId = "model" | "workspaces" | "advanced";

/** What the window remembers between renders. It lives in memory only; nothing is stored. */
export interface ViewState {
  section: SectionId;
  /** The workspace shown in the Workspaces section, by identifier. */
  workspaceId: string | null;
  toolLimitsOpen: boolean;
  addConnectionOpen: boolean;
  /** What the operator has typed into the add-connection form, kept until an add succeeds. */
  connectionDraft: ConnectionDraft;
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
