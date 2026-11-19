import type { LocalProviderKind, PermissionFlags, SettingsOverview } from "../ipc/types";

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
  addLocalConnection(name: string, kind: LocalProviderKind, endpoint: string): Change;
  removeConnection(name: string): Change;
}

export type SectionId = "model" | "workspaces" | "advanced";

/** What the window remembers between renders. It lives in memory only; nothing is stored. */
export interface ViewState {
  section: SectionId;
  /** The workspace shown in the Workspaces section, by identifier. */
  workspaceId: string | null;
  toolLimitsOpen: boolean;
  addConnectionOpen: boolean;
}

export interface SectionContext {
  overview: SettingsOverview;
  view: ViewState;
  submit: Submit;
  actions: Actions;
  /** Draws the current overview again, for a choice that sends nothing to the core. */
  rerender(): void;
}
