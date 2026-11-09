// Selectors for the asking screens (SC-013 to SC-015).
import type { AgentId } from "../../ipc/generated/AgentId";
import type { Member } from "../../ipc/generated/Member";
import type { MemberId } from "../../ipc/generated/MemberId";
import type { Question } from "../../ipc/generated/Question";
import type { SharedAgentView } from "../../ipc/generated/SharedAgentView";
import type { MirrorState } from "../../store/reducer";
import type { View } from "../../store/ui";

export type AskTargetView = Extract<View, { kind: "teammate-agent" | "person" }>;

export function targetKey(view: AskTargetView): string {
  return view.kind === "person" ? `person/${view.memberId}` : `agent/${view.owner}/${view.agentId}`;
}

// Cached per mirror, so React sees the same array until the mirror changes.
const askCache = new WeakMap<MirrorState, Map<string, Question[]>>();

/** My questions to one teammate's agent or to one person, oldest first. */
export function selectAskQuestions(state: MirrorState, view: AskTargetView): Question[] {
  let byTarget = askCache.get(state);
  if (!byTarget) {
    byTarget = new Map();
    askCache.set(state, byTarget);
  }
  const key = targetKey(view);
  const cached = byTarget.get(key);
  if (cached) return cached;
  const me = state.team?.me;
  const questions = Object.values(state.questions)
    .filter((q) => {
      if (q.asker !== me) return false;
      if (view.kind === "person") return q.target.kind === "person" && q.target.member_id === view.memberId;
      return q.target.kind === "agent" && q.target.member_id === view.owner && q.target.agent_id === view.agentId;
    })
    .sort((a, b) => Date.parse(a.created_at) - Date.parse(b.created_at));
  byTarget.set(key, questions);
  return questions;
}

/** A teammate's agent as shared with me; `null` when it is not (or no longer) shared. */
export function selectSharedAgent(state: MirrorState, owner: MemberId, agentId: AgentId): SharedAgentView | null {
  return state.teammate_agents[owner]?.find((a) => a.agent_id === agentId) ?? null;
}

export function selectMember(state: MirrorState, memberId: MemberId): Member | null {
  return state.team?.members.find((m) => m.id === memberId) ?? null;
}

/** The `ApiError` code of a failed command, when formationd sent one. */
export function errorCode(error: unknown): string | null {
  if (typeof error !== "object" || error === null || !("data" in error)) return null;
  const data = (error as { data: { code?: unknown } | null }).data;
  return typeof data?.code === "string" ? data.code : null;
}
