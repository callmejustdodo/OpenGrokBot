// Sidebar-only reads of the mirror.
import type { Attention } from "../../ipc/generated/Attention";
import type { MemberId } from "../../ipc/generated/MemberId";
import type { Question } from "../../ipc/generated/Question";
import type { MirrorState } from "../../store/reducer";

function newest(questions: Question[]): Question | null {
  let best: Question | null = null;
  for (const q of questions) if (!best || Date.parse(q.updated_at) > Date.parse(best.updated_at)) best = q;
  return best;
}

/** The newest question I asked a teammate's agent. */
export function latestAgentQuestion(state: MirrorState, owner: MemberId, agentId: string): Question | null {
  return newest(
    Object.values(state.questions).filter(
      (q) => q.target.kind === "agent" && q.target.member_id === owner && q.target.agent_id === agentId,
    ),
  );
}

/** The newest person-level question between me and a teammate, either way. */
export function latestPersonQuestion(state: MirrorState, memberId: MemberId): Question | null {
  const me = state.team?.me;
  return newest(
    Object.values(state.questions).filter(
      (q) =>
        q.target.kind === "person" &&
        ((q.asker === me && q.target.member_id === memberId) || (q.asker === memberId && q.target.member_id === me)),
    ),
  );
}

/** Unresolved Inbox items, newest first, for the Needs-you line. */
export function inboxNewestFirst(state: MirrorState): Attention[] {
  return Object.values(state.inbox).sort((a, b) => Date.parse(b.at) - Date.parse(a.at));
}

export function memberName(state: MirrorState, memberId: MemberId): string {
  return state.team?.members.find((m) => m.id === memberId)?.display_name ?? "";
}
