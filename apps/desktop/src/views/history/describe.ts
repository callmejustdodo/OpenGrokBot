// How a history entry reads: its filter group, who acted, on whom, and the
// current status of what it points at. Pure functions over the mirror.
import type { Activity } from "../../ipc/generated/Activity";
import type { ActivityKind } from "../../ipc/generated/ActivityKind";
import type { AgentId } from "../../ipc/generated/AgentId";
import type { MemberId } from "../../ipc/generated/MemberId";
import type { Question } from "../../ipc/generated/Question";
import type { MirrorState } from "../../store/reducer";

/** The filters SC-017 offers; agent start and stop entries show under "all" only. */
export type HistoryFilter = "all" | "question" | "request" | "approval" | "result";

/** The tag a row shows, one per kind of entry. */
export type HistoryTag = Exclude<HistoryFilter, "all"> | "agent";

export const TAG_OF: Record<ActivityKind, HistoryTag> = {
  question_asked: "question",
  question_answered: "question",
  request_sent: "request",
  request_received: "request",
  request_finished: "result",
  schedule_ran: "result",
  approval_decided: "approval",
  agent_started: "agent",
  agent_stopped: "agent",
  agent_failed: "agent",
};

export function memberName(mirror: MirrorState, id: MemberId): string | null {
  return mirror.team?.members.find((m) => m.id === id)?.display_name ?? null;
}

export function agentName(mirror: MirrorState, id: AgentId): string | null {
  const mine = mirror.agents[id];
  if (mine) return mine.name;
  for (const agents of Object.values(mirror.teammate_agents)) {
    const shared = agents.find((a) => a.agent_id === id);
    if (shared) return shared.name;
  }
  return null;
}

/** The person who acted; `null` means me (the owner of this Mac). */
export function actorOf(mirror: MirrorState, activity: Activity): MemberId | null {
  const me = mirror.team?.me ?? null;
  return activity.actor === me ? null : activity.actor;
}

/** Whom the entry was about: the agent or person it went to. */
export function subjectOf(mirror: MirrorState, activity: Activity): string | null {
  const target = activity.target;
  if (target?.kind === "question") {
    const question = mirror.questions[target.question_id];
    if (question) {
      return question.target.kind === "agent"
        ? agentName(mirror, question.target.agent_id)
        : memberName(mirror, question.target.member_id);
    }
  }
  if (target?.kind === "request") {
    const request = mirror.requests[target.request_id];
    if (request) return agentName(mirror, request.agent_id);
  }
  return activity.agent_id ? agentName(mirror, activity.agent_id) : null;
}

/** The current status of what the entry points at, when this Mac still has it. */
export function statusOf(mirror: MirrorState, activity: Activity): string | null {
  const target = activity.target;
  switch (target?.kind) {
    case "question":
      return mirror.questions[target.question_id]?.status ?? null;
    case "request":
      return mirror.requests[target.request_id]?.status ?? null;
    case "approval": {
      const approval = mirror.approvals[target.approval_id];
      return approval?.choice ?? approval?.status ?? null;
    }
    default:
      return null;
  }
}

/** The question an entry is about, when I may delete it: I asked it, or it was asked of me or my agent. */
export function deletableQuestion(mirror: MirrorState, activity: Activity): Question | null {
  if (activity.target?.kind !== "question") return null;
  const question = mirror.questions[activity.target.question_id];
  const me = mirror.team?.me;
  if (!question || !me) return null;
  return question.asker === me || question.target.member_id === me ? question : null;
}

/** "sent_to_owner" → "Sent to owner", the English default for a status key. */
export function humanize(value: string): string {
  const words = value.replace(/_/g, " ");
  return words.charAt(0).toUpperCase() + words.slice(1);
}
