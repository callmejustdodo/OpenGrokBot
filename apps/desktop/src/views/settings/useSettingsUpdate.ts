import { useState } from "react";
import type { Settings } from "../../ipc/generated/Settings";
import { tauriTransport } from "../../ipc/transport";
import { useAppStore } from "../../store/store";
import { act } from "../../store/sync";

/** Pending key of `settings_update`, shared by the Settings tabs and the menu bar panel. */
export const SETTINGS_PENDING = "settings";

const NO_SETTINGS: Settings = { language: null, notifications: { enabled: true, muted_until: null } };

/**
 * The current settings and a way to replace them with `settings_update`. While a
 * change is pending, or formationd is unreachable, controls are disabled.
 */
export function useSettingsUpdate() {
  const settings = useAppStore((s) => s.mirror?.settings ?? NO_SETTINGS);
  const connected = useAppStore((s) => s.connected);
  const pending = useAppStore((s) => SETTINGS_PENDING in s.pending);
  const [failed, setFailed] = useState(false);

  const update = (next: Settings): Promise<Settings | null> => {
    setFailed(false);
    return act(tauriTransport, SETTINGS_PENDING, "settings_update", { settings: next }).catch(() => {
      setFailed(true);
      return null;
    });
  };

  return { settings, pending, disabled: pending || !connected, failed, update };
}
