// Needs-attention items as the menu bar panel (SC-002) and notifications
// (SC-019) show them: unresolved Inbox items plus pending approvals the Inbox
// does not hold yet, the same set `selectNeedsYouCount` counts. Also the
// panel's offline rule and the state and kind labels both use.
import type { TFunction } from "i18next";
import type { AgentState } from "../ipc/generated/AgentState";
import type { AttentionKind } from "../ipc/generated/AttentionKind";
import type { AttentionTarget } from "../ipc/generated/AttentionTarget";
import type { MirrorState } from "../store/reducer";

export type NeedsYouItem = {
  /** The Inbox item ID, or the approval ID for an approval not in the Inbox yet. */
  key: string;
  kind: AttentionKind;
  subject: string;
  at: string;
  target: AttentionTarget;
};

const cache = new WeakMap<MirrorState, NeedsYouItem[]>();

/** What needs the owner, newest first. */
export function selectNeedsYouItems(state: MirrorState): NeedsYouItem[] {
  const cached = cache.get(state);
  if (cached) return cached;
  const inbox = Object.values(state.inbox);
  const inInbox = new Set(
    inbox.flatMap((item) => (item.target.kind === "approval" ? [item.target.approval_id] : [])),
  );
  const items: NeedsYouItem[] = inbox.map((item) => ({
    key: item.id,
    kind: item.kind,
    subject: item.subject,
    at: item.at,
    target: item.target,
  }));
  for (const approval of Object.values(state.approvals)) {
    if (approval.status !== "pending" || inInbox.has(approval.id)) continue;
    const agent = state.agents[approval.agent_id]?.name ?? approval.agent_id;
    items.push({
      key: approval.id,
      kind: "approval",
      subject: `${agent} · ${approval.summary}`,
      at: approval.created_at,
      target: { kind: "approval", approval_id: approval.id },
    });
  }
  items.sort((a, b) => Date.parse(b.at) - Date.parse(a.at));
  cache.set(state, items);
  return items;
}

export function attentionKindLabel(t: TFunction, kind: AttentionKind): string {
  switch (kind) {
    case "approval":
      return t("attention.kind.approval", "Waiting for approval");
    case "question_for_you":
      return t("attention.kind.question_for_you", "Question for you");
    case "agent_error":
      return t("attention.kind.agent_error", "Error");
    case "agent_stalled":
      return t("attention.kind.agent_stalled", "Stalled");
    case "schedule_failed":
      return t("attention.kind.schedule_failed", "Scheduled run failed");
    case "schedule_missed":
      return t("attention.kind.schedule_missed", "Scheduled run missed");
    case "request_finished":
      return t("attention.kind.request_finished", "Request finished");
  }
}

export function agentStateLabel(t: TFunction, state: AgentState): string {
  switch (state) {
    case "working":
      return t("agent.state.working", "Working");
    case "idle":
      return t("agent.state.idle", "Idle");
    case "waiting_for_approval":
      return t("agent.state.waiting_for_approval", "Waiting for approval");
    case "stalled":
      return t("agent.state.stalled", "Stalled");
    case "error":
      return t("agent.state.error", "Error");
    case "stopped":
      return t("agent.state.stopped", "Stopped");
    case "unknown":
      return t("agent.state.unknown", "Unknown");
  }
}

export type PanelState = "default" | "all-clear" | "offline";
/** Why the panel is offline: formationd is unreachable, or every teammate is. */
export type OfflineReason = "formationd" | "tailnet" | "team" | null;

export function offlineReason(connected: boolean, mirror: MirrorState | null): OfflineReason {
  if (!connected) return "formationd";
  if (mirror && !mirror.network.tailnet) return "tailnet";
  const team = mirror?.team;
  const teammates = team?.members.filter((m) => m.id !== team.me) ?? [];
  return teammates.length > 0 && teammates.every((m) => !m.online) ? "team" : null;
}
