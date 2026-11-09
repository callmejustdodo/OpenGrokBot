// A pure mirror of formationd's state: built from a `snapshot`, then kept current by
// `event` notifications applied in `seq` order (doc 5, "Data flow").

import type { Agent } from "../ipc/generated/Agent";
import type { AgentId } from "../ipc/generated/AgentId";
import type { AgentStreamEvent } from "../ipc/generated/AgentStreamEvent";
import type { Approval } from "../ipc/generated/Approval";
import type { Attention } from "../ipc/generated/Attention";
import type { Conversation } from "../ipc/generated/Conversation";
import type { ConversationItem } from "../ipc/generated/ConversationItem";
import type { ConversationPageResult } from "../ipc/generated/ConversationPageResult";
import type { EventEnvelope } from "../ipc/generated/EventEnvelope";
import type { HarnessStatus } from "../ipc/generated/HarnessStatus";
import type { MemberId } from "../ipc/generated/MemberId";
import type { Question } from "../ipc/generated/Question";
import type { Rule } from "../ipc/generated/Rule";
import type { Schedule } from "../ipc/generated/Schedule";
import type { Settings } from "../ipc/generated/Settings";
import type { SharedAgentView } from "../ipc/generated/SharedAgentView";
import type { ShareSettings } from "../ipc/generated/ShareSettings";
import type { Snapshot } from "../ipc/generated/Snapshot";
import type { Team } from "../ipc/generated/Team";
import type { WorkRequest } from "../ipc/generated/WorkRequest";

type ById<T> = Readonly<Record<string, T>>;

export type MirrorState = {
  /** The last event `seq` applied (or the snapshot's). */
  seq: number;
  team: Team | null;
  agents: ById<Agent>;
  /** Keyed by agent ID; includes the queue and the last applied `stream_seq`. */
  conversations: ById<Conversation>;
  /** Keyed by agent ID. */
  shares: ById<ShareSettings>;
  schedules: ById<Schedule>;
  /** Keyed by owner (member ID); `peer.presence` replaces one owner's list. */
  teammate_agents: Readonly<Record<MemberId, readonly SharedAgentView[]>>;
  /** Unresolved needs-attention items only. */
  inbox: ById<Attention>;
  questions: ById<Question>;
  requests: ById<WorkRequest>;
  approvals: ById<Approval>;
  rules: ById<Rule>;
  harnesses: readonly HarnessStatus[];
  settings: Settings;
  /** Agents whose conversation missed an `agent.stream` delta and must be reloaded with `conversation_page`. */
  staleConversations: ReadonlySet<AgentId>;
};

export type ApplyResult = { state: MirrorState; needsSnapshot: boolean };

function byId<T>(items: readonly T[], key: (item: T) => string): Record<string, T> {
  const out: Record<string, T> = {};
  for (const item of items) out[key(item)] = item;
  return out;
}

function without<T>(record: ById<T>, id: string): ById<T> {
  if (!(id in record)) return record;
  const next = { ...record };
  delete next[id];
  return next;
}

function groupByOwner(agents: readonly SharedAgentView[]): Record<MemberId, SharedAgentView[]> {
  const out: Record<MemberId, SharedAgentView[]> = {};
  for (const agent of agents) (out[agent.owner] ??= []).push(agent);
  return out;
}

export function fromSnapshot(snapshot: Snapshot): MirrorState {
  return {
    seq: snapshot.seq,
    team: snapshot.team,
    agents: byId(snapshot.agents, (a) => a.id),
    conversations: byId(snapshot.conversations, (c) => c.agent_id),
    shares: byId(snapshot.shares, (s) => s.agent_id),
    schedules: byId(snapshot.schedules, (s) => s.id),
    teammate_agents: groupByOwner(snapshot.teammate_agents),
    inbox: byId(
      snapshot.inbox.filter((i) => i.resolved_at === null),
      (i) => i.id,
    ),
    questions: byId(snapshot.questions, (q) => q.id),
    requests: byId(snapshot.requests, (r) => r.id),
    approvals: byId(snapshot.approvals, (a) => a.id),
    rules: byId(snapshot.rules, (r) => r.id),
    harnesses: snapshot.harnesses,
    settings: snapshot.settings,
    staleConversations: new Set(),
  };
}

function withStale(state: MirrorState, agentId: AgentId): MirrorState {
  if (state.staleConversations.has(agentId)) return state;
  return { ...state, staleConversations: new Set(state.staleConversations).add(agentId) };
}

function upsertItem(items: readonly ConversationItem[], item: ConversationItem): ConversationItem[] {
  const index = items.findIndex((i) => i.id === item.id);
  if (index === -1) return [...items, item];
  const next = [...items];
  next[index] = item;
  return next;
}

function applyStream(state: MirrorState, event: AgentStreamEvent): MirrorState {
  const agentId = event.agent_id;
  const conversation = state.conversations[agentId];
  // Without a base conversation, or while waiting for a reload, deltas cannot be placed.
  if (!conversation || state.staleConversations.has(agentId)) return withStale(state, agentId);
  if (event.stream_seq <= conversation.stream_seq) return state;
  if (event.stream_seq !== conversation.stream_seq + 1) return withStale(state, agentId);

  let items = conversation.items;
  const delta = event.delta;
  switch (delta.kind) {
    case "item_started":
    case "item_completed":
      items = upsertItem(items, delta.item);
      break;
    case "text_delta": {
      const index = items.findIndex((i) => i.id === delta.item_id);
      const target = items[index];
      if (target?.kind !== "assistant_text") return withStale(state, agentId);
      const next = [...items];
      next[index] = { ...target, text: target.text + delta.text };
      items = next;
      break;
    }
    case "turn_started":
    case "turn_ended":
      break;
  }
  return {
    ...state,
    conversations: {
      ...state.conversations,
      [agentId]: {
        ...conversation,
        session_id: event.session_id,
        items,
        stream_seq: event.stream_seq,
      },
    },
  };
}

function applyEvent(state: MirrorState, envelope: EventEnvelope): MirrorState {
  const event = envelope.event;
  switch (event.type) {
    case "agent.state": {
      const agent = state.agents[event.payload.agent_id];
      if (!agent) return state;
      return { ...state, agents: { ...state.agents, [agent.id]: { ...agent, state: event.payload.state } } };
    }
    case "agent.stream":
      return applyStream(state, event.payload);
    case "agent.queue": {
      const agentId = event.payload.agent_id;
      const conversation = state.conversations[agentId];
      if (conversation) {
        return {
          ...state,
          conversations: { ...state.conversations, [agentId]: { ...conversation, queue: event.payload.items } },
        };
      }
      // A conversation the snapshot did not include: keep the queue, load the items later.
      const placeholder: Conversation = {
        agent_id: agentId,
        session_id: null,
        items: [],
        has_more: false,
        queue: event.payload.items,
        stream_seq: 0,
      };
      return withStale({ ...state, conversations: { ...state.conversations, [agentId]: placeholder } }, agentId);
    }
    case "agent.update": {
      const payload = event.payload;
      if (payload.change === "upsert") {
        return { ...state, agents: { ...state.agents, [payload.agent.id]: payload.agent } };
      }
      const staleConversations = new Set(state.staleConversations);
      staleConversations.delete(payload.agent_id);
      return {
        ...state,
        agents: without(state.agents, payload.agent_id),
        conversations: without(state.conversations, payload.agent_id),
        shares: without(state.shares, payload.agent_id),
        staleConversations,
      };
    }
    case "schedule.update": {
      const payload = event.payload;
      if (payload.change === "upsert") {
        return { ...state, schedules: { ...state.schedules, [payload.schedule.id]: payload.schedule } };
      }
      return { ...state, schedules: without(state.schedules, payload.schedule_id) };
    }
    case "notify": {
      const item = event.payload.item;
      if (item.resolved_at !== null) return { ...state, inbox: without(state.inbox, item.id) };
      return { ...state, inbox: { ...state.inbox, [item.id]: item } };
    }
    case "harness.update":
      return { ...state, harnesses: event.payload.harnesses };
    case "approval.raised":
    case "approval.update": {
      const approval = event.payload.approval;
      return { ...state, approvals: { ...state.approvals, [approval.id]: approval } };
    }
    case "question.update": {
      const payload = event.payload;
      if (payload.change === "upsert") {
        return { ...state, questions: { ...state.questions, [payload.question.id]: payload.question } };
      }
      return { ...state, questions: without(state.questions, payload.question_id) };
    }
    case "request.update": {
      const request = event.payload.request;
      return { ...state, requests: { ...state.requests, [request.id]: request } };
    }
    case "share.update": {
      const share = event.payload.share;
      return { ...state, shares: { ...state.shares, [share.agent_id]: share } };
    }
    case "rule.update": {
      const rule = event.payload.rule;
      return { ...state, rules: { ...state.rules, [rule.id]: rule } };
    }
    case "team.update":
      return { ...state, team: event.payload.team };
    case "peer.presence": {
      const { member_id, online, last_seen_at, agents } = event.payload;
      const team = state.team && {
        ...state.team,
        members: state.team.members.map((m) => (m.id === member_id ? { ...m, online, last_seen_at } : m)),
      };
      return { ...state, team, teammate_agents: { ...state.teammate_agents, [member_id]: agents } };
    }
    case "settings.update":
      return { ...state, settings: event.payload.settings };
  }
}

/**
 * Applies one event. Only `seq === state.seq + 1` changes the state; an older or
 * repeated `seq` is ignored, and a gap asks for a new snapshot.
 */
export function applyEnvelope(state: MirrorState, envelope: EventEnvelope): ApplyResult {
  if (envelope.seq <= state.seq) return { state, needsSnapshot: false };
  if (envelope.seq !== state.seq + 1) return { state, needsSnapshot: true };
  return { state: { ...applyEvent(state, envelope), seq: envelope.seq }, needsSnapshot: false };
}

/** Replaces an agent's conversation items with a `conversation_page` reload of the newest items. */
export function applyConversationPage(
  state: MirrorState,
  agentId: AgentId,
  page: ConversationPageResult,
): MirrorState {
  const current = state.conversations[agentId];
  const conversation: Conversation = {
    agent_id: agentId,
    session_id: current?.session_id ?? null,
    queue: current?.queue ?? [],
    items: page.items,
    has_more: page.has_more,
    stream_seq: page.stream_seq,
  };
  const staleConversations = new Set(state.staleConversations);
  staleConversations.delete(agentId);
  return { ...state, conversations: { ...state.conversations, [agentId]: conversation }, staleConversations };
}
