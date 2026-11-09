import type { Agent } from "../ipc/generated/Agent";
import type { AgentId } from "../ipc/generated/AgentId";
import type { Conversation } from "../ipc/generated/Conversation";
import type { Member } from "../ipc/generated/Member";
import type { MemberId } from "../ipc/generated/MemberId";
import type { Question } from "../ipc/generated/Question";
import type { QuestionId } from "../ipc/generated/QuestionId";
import type { SharedAgentView } from "../ipc/generated/SharedAgentView";
import type { MirrorState } from "./reducer";

export type ChatListItem =
  | { kind: "agent"; agent_id: AgentId; agent: Agent; at: string | null }
  | {
      kind: "teammate_agent";
      owner: MemberId;
      agent_id: AgentId;
      /** `null` when the agent is no longer shared with me. */
      agent: SharedAgentView | null;
      at: string | null;
    }
  | { kind: "person"; member_id: MemberId; member: Member | null; at: string | null };

function later(a: string | null, b: string | null): string | null {
  if (a === null) return b;
  if (b === null) return a;
  return Date.parse(b) > Date.parse(a) ? b : a;
}

function newestFirst(a: { at: string | null }, b: { at: string | null }): number {
  if (a.at === b.at) return 0;
  if (a.at === null) return 1;
  if (b.at === null) return -1;
  return Date.parse(b.at) - Date.parse(a.at);
}

// Selectors that build new arrays cache per mirror, so React sees the same array
// until the mirror changes.
const chatListCache = new WeakMap<MirrorState, ChatListItem[]>();
const threadCache = new WeakMap<MirrorState, Map<QuestionId, Question[]>>();

/**
 * The sidebar chat list, most recent first: my agents, teammates' agents I asked,
 * and people in person questions either way.
 */
export function selectChatList(state: MirrorState): ChatListItem[] {
  const cached = chatListCache.get(state);
  if (cached) return cached;

  const items: ChatListItem[] = Object.values(state.agents).map((agent) => {
    const conversationItems = state.conversations[agent.id]?.items ?? [];
    const lastItemAt = conversationItems[conversationItems.length - 1]?.at ?? null;
    return { kind: "agent", agent_id: agent.id, agent, at: later(agent.last_activity_at, lastItemAt) };
  });

  const me = state.team?.me;
  const teammateAgents = new Map<string, Extract<ChatListItem, { kind: "teammate_agent" }>>();
  const people = new Map<MemberId, Extract<ChatListItem, { kind: "person" }>>();
  for (const question of Object.values(state.questions)) {
    const target = question.target;
    if (target.kind === "agent") {
      if (target.member_id === me) continue;
      const key = `${target.member_id}/${target.agent_id}`;
      const existing = teammateAgents.get(key);
      if (existing) {
        existing.at = later(existing.at, question.updated_at);
        continue;
      }
      const agent =
        state.teammate_agents[target.member_id]?.find((a) => a.agent_id === target.agent_id) ?? null;
      teammateAgents.set(key, {
        kind: "teammate_agent",
        owner: target.member_id,
        agent_id: target.agent_id,
        agent,
        at: question.updated_at,
      });
    } else {
      const other = question.asker === me ? target.member_id : question.asker;
      if (other === me) continue;
      const existing = people.get(other);
      if (existing) {
        existing.at = later(existing.at, question.updated_at);
        continue;
      }
      const member = state.team?.members.find((m) => m.id === other) ?? null;
      people.set(other, { kind: "person", member_id: other, member, at: question.updated_at });
    }
  }

  const list = [...items, ...teammateAgents.values(), ...people.values()].sort(newestFirst);
  chatListCache.set(state, list);
  return list;
}

/** The "Needs you" count: unresolved Inbox items plus pending approvals the Inbox does not already hold. */
export function selectNeedsYouCount(state: MirrorState): number {
  const inbox = Object.values(state.inbox);
  const inInbox = new Set(
    inbox.flatMap((item) => (item.target.kind === "approval" ? [item.target.approval_id] : [])),
  );
  const pendingApprovals = Object.values(state.approvals).filter(
    (approval) => approval.status === "pending" && !inInbox.has(approval.id),
  );
  return inbox.length + pendingApprovals.length;
}

export function selectConversation(state: MirrorState, agentId: AgentId): Conversation | undefined {
  return state.conversations[agentId];
}

/** The questions of one thread, oldest first. */
export function selectThreadQuestions(state: MirrorState, threadId: QuestionId): Question[] {
  let threads = threadCache.get(state);
  if (!threads) {
    threads = new Map();
    threadCache.set(state, threads);
  }
  const cached = threads.get(threadId);
  if (cached) return cached;
  const questions = Object.values(state.questions)
    .filter((q) => q.thread_id === threadId)
    .sort((a, b) => Date.parse(a.created_at) - Date.parse(b.created_at));
  threads.set(threadId, questions);
  return questions;
}
