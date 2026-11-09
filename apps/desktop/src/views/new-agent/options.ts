// The avatar choices the mockup offers (open-bot-avatar names, D-042) and
// harness readiness.
import type { HarnessStatus } from "../../ipc/generated/HarnessStatus";

export const AVATAR_SHAPES = ["cat", "rocket", "star", "book", "magnifier", "shield", "palette", "cloud"];
export const AVATAR_COLORS = ["#f8a01c", "#ff6a1a", "#3a6df0", "#14b3a5", "#1fbf68", "#8a5cf0", "#ec4899", "#7a7a7a"];

/** Installed, not known to be signed out, and no problem to fix. */
export function isReady(status: HarnessStatus): boolean {
  return status.installed && status.signed_in !== false && status.problem === null;
}
