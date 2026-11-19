import { invokeCommand } from "./client";
import type { SettingsOverview } from "./types";

/** Commands that only the settings window may call (ADR-0058). Imported by that entry alone. */
export const settingsApi = {
  overview: () => invokeCommand<SettingsOverview>("settings_overview", {}),
};
