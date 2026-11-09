// Labels and times shared by the asking screens.
import type { TFunction } from "i18next";
import type { AgentState } from "../../ipc/generated/AgentState";
import type { HarnessKind } from "../../ipc/generated/HarnessKind";

/** The time alone for today, otherwise a short date and the time. */
export function formatTime(iso: string, language: string, now: Date = new Date()): string {
  const at = new Date(iso);
  const time = at.toLocaleTimeString(language, { hour: "2-digit", minute: "2-digit" });
  if (at.toDateString() === now.toDateString()) return time;
  return `${at.toLocaleDateString(language, { month: "short", day: "numeric" })} ${time}`;
}

export function stateLabel(t: TFunction, state: AgentState): string {
  switch (state) {
    case "working":
      return t("agentState.working", "Working");
    case "idle":
      return t("agentState.idle", "Idle");
    case "waiting_for_approval":
      return t("agentState.waiting_for_approval", "Waiting for approval");
    case "stalled":
      return t("agentState.stalled", "Stalled");
    case "error":
      return t("agentState.error", "Error");
    case "stopped":
      return t("agentState.stopped", "Stopped");
    case "unknown":
      return t("agentState.unknown", "Unknown");
  }
}

export function harnessLabel(t: TFunction, harness: HarnessKind): string {
  switch (harness) {
    case "claude_code":
      return t("harness.claude_code", "Claude Code");
    case "codex":
      return t("harness.codex", "Codex");
    case "detected":
      return t("harness.detected", "Detected");
  }
}
