// The Inbox's groups (SC-010), cached per mirror so React sees stable arrays.
import type { Approval } from "../../ipc/generated/Approval";
import type { Attention } from "../../ipc/generated/Attention";
import type { AttentionKind } from "../../ipc/generated/AttentionKind";
import type { MirrorState } from "../../store/reducer";

export type InboxApproval = { approval: Approval; at: string };
export type InboxGroups = { approvals: InboxApproval[]; questions: Attention[]; alerts: Attention[]; count: number };

const ALERT_KINDS: readonly AttentionKind[] = ["agent_error", "agent_stalled", "schedule_failed", "request_finished"];

const cache = new WeakMap<MirrorState, InboxGroups>();

/** Pending approvals (oldest first, timed by their Inbox item), questions for me, and alerts (newest first). */
export function selectInbox(state: MirrorState): InboxGroups {
  const cached = cache.get(state);
  if (cached) return cached;
  const items = Object.values(state.inbox);
  const approvalAt = new Map<string, string>();
  for (const item of items) if (item.target.kind === "approval") approvalAt.set(item.target.approval_id, item.at);
  const approvals = Object.values(state.approvals)
    .filter((a) => a.status === "pending")
    .map((approval) => ({ approval, at: approvalAt.get(approval.id) ?? approval.created_at }))
    .sort((a, b) => Date.parse(a.at) - Date.parse(b.at));
  const questions = items.filter((i) => i.kind === "question_for_you").sort((a, b) => Date.parse(a.at) - Date.parse(b.at));
  const alerts = items.filter((i) => ALERT_KINDS.includes(i.kind)).sort((a, b) => Date.parse(b.at) - Date.parse(a.at));
  const groups = { approvals, questions, alerts, count: approvals.length + questions.length + alerts.length };
  cache.set(state, groups);
  return groups;
}
