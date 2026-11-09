// Names, times, and error copy shared by the approval and request screens
// (SC-005 cards, SC-010, SC-011, SC-016).
import { useEffect, useState } from "react";
import type { TFunction } from "i18next";
import type { AgentId } from "../../ipc/generated/AgentId";
import type { Approval } from "../../ipc/generated/Approval";
import type { MemberId } from "../../ipc/generated/MemberId";
import type { MirrorState } from "../../store/reducer";

export function memberName(state: MirrorState, id: MemberId | null): string | null {
  if (id === null) return null;
  return state.team?.members.find((m) => m.id === id)?.display_name ?? null;
}

/** One of my agents, or a teammate's shared agent. */
export function agentName(state: MirrorState, owner: MemberId | null, agentId: AgentId): string | null {
  const mine = state.agents[agentId];
  if (mine) return mine.name;
  const lists = owner === null ? Object.values(state.teammate_agents) : [state.teammate_agents[owner] ?? []];
  for (const list of lists) {
    const shared = list.find((a) => a.agent_id === agentId);
    if (shared) return shared.name;
  }
  return null;
}

/** The folder the command runs in: the tool input's `cwd`, else the proposed rule's, else the agent's. */
export function approvalFolder(state: MirrorState, approval: Approval): string | null {
  const input = approval.call.input;
  if (input && typeof input === "object" && !Array.isArray(input) && typeof input.cwd === "string") return input.cwd;
  const scope = approval.proposed_rule?.scope;
  if (scope?.kind === "command") return scope.folder;
  return state.agents[approval.agent_id]?.folder_path ?? null;
}

/** The current time, updated every `intervalMs`, so remaining times count down. */
export function useNow(intervalMs = 30_000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(id);
  }, [intervalMs]);
  return now;
}

export type TimeLeft = { kind: "expired" } | { kind: "expiring"; minutes: number } | { kind: "left"; hours: number; minutes: number };

/** Under an hour left counts as expiring (SC-011 "expiring soon"). */
export function timeLeft(expiresAt: string, now: number): TimeLeft {
  const ms = Date.parse(expiresAt) - now;
  if (ms <= 0) return { kind: "expired" };
  const totalMinutes = Math.ceil(ms / 60_000);
  if (totalMinutes < 60) return { kind: "expiring", minutes: totalMinutes };
  return { kind: "left", hours: Math.floor(totalMinutes / 60), minutes: totalMinutes % 60 };
}

export function timeLeftText(t: TFunction, left: TimeLeft): string {
  switch (left.kind) {
    case "expired":
      return t("approval.expiredLead", "Expired");
    case "expiring":
      return t("approval.expiresIn", "Expires in {{minutes}} min", { minutes: left.minutes });
    case "left":
      return t("approval.timeLeft", "{{hours}} h {{minutes}} min left", left);
  }
}

/** "3 min ago" style wait time for Inbox rows. */
export function agoText(t: TFunction, at: string, now: number): string {
  const minutes = Math.max(0, Math.round((now - Date.parse(at)) / 60_000));
  if (minutes < 1) return t("time.justNow", "Just now");
  if (minutes < 60) return t("time.minutesAgo", "{{count}} min ago", { count: minutes });
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return t("time.hoursAgo", "{{count}} h ago", { count: hours });
  return t("time.daysAgo", "{{count}} d ago", { count: Math.floor(hours / 24) });
}

/** The ApiError code of a failed call (`CallError.data.code`), or the transport's own code. */
export function errorCode(error: unknown): string {
  if (error && typeof error === "object") {
    const e = error as { code?: unknown; data?: { code?: unknown } | null };
    if (e.data && typeof e.data.code === "string") return e.data.code;
    if (typeof e.code === "string") return e.code;
  }
  return "internal";
}

/** What kind of action an approval asks for, from the tool name. */
export function actionText(t: TFunction, toolName: string): string {
  switch (toolName) {
    case "Bash":
      return t("approval.action.command", "Run a command");
    case "Write":
    case "Edit":
    case "MultiEdit":
    case "NotebookEdit":
      return t("approval.action.write", "Change a file");
    case "WebFetch":
    case "WebSearch":
      return t("approval.action.network", "Use the network");
    default:
      return t("approval.action.other", "Use {{tool}}", { tool: toolName });
  }
}

/** Stops Return from activating a decision button (SC-011 keyboard rule). */
export function blockReturn(event: { key: string; preventDefault: () => void }): void {
  if (event.key === "Enter") event.preventDefault();
}
