import { describe, expect, it } from "vitest";
import approvalRaised from "../../../../tools/fake-formationd/fixtures/scripts/approval-raised.json";
import inboxCleared from "../../../../tools/fake-formationd/fixtures/scripts/inbox-cleared.json";
import m3 from "../../../../tools/fake-formationd/fixtures/worlds/m3.json";
import type { Event } from "../ipc/generated/Event";
import type { EventEnvelope } from "../ipc/generated/EventEnvelope";
import type { Snapshot } from "../ipc/generated/Snapshot";
import type { StreamDelta } from "../ipc/generated/StreamDelta";
import { applyConversationPage, applyEnvelope, fromSnapshot, type MirrorState } from "./reducer";

const snapshot = m3 as unknown as Snapshot;
const BACKEND = "01930000-0000-7000-8000-000000000101";
const RESEARCH = "01930000-0000-7000-8000-000000000103";
const MINSU = "01930000-0000-7000-8000-000000000002";
const JIWOO = "01930000-0000-7000-8000-000000000003";
const AT = "2026-11-09T01:53:00Z";

function envelope(seq: number, event: Event): EventEnvelope {
  return { seq, at: AT, event };
}

function apply(state: MirrorState, event: Event): MirrorState {
  const result = applyEnvelope(state, envelope(state.seq + 1, event));
  expect(result.needsSnapshot).toBe(false);
  return result.state;
}

function stream(agentId: string, streamSeq: number, delta: StreamDelta): Event {
  return {
    type: "agent.stream",
    payload: { agent_id: agentId, session_id: `session-${agentId}`, stream_seq: streamSeq, delta },
  };
}

/** Plays a fixture script the way fake-formationd does: `seq` and each agent's `stream_seq` continue from the state. */
function play(state: MirrorState, script: { steps: { event: unknown }[] }): MirrorState {
  const streamSeqs: Record<string, number> = {};
  for (const step of script.steps) {
    const event = structuredClone(step.event) as Event;
    if (event.type === "agent.stream") {
      const id = event.payload.agent_id;
      streamSeqs[id] = (streamSeqs[id] ?? state.conversations[id].stream_seq) + 1;
      event.payload.stream_seq = streamSeqs[id];
    }
    state = apply(state, event);
  }
  return state;
}

describe("fromSnapshot", () => {
  it("normalizes the M3 world into slices", () => {
    const state = fromSnapshot(snapshot);
    expect(state.seq).toBe(0);
    expect(Object.keys(state.agents)).toHaveLength(6);
    expect(state.agents[BACKEND].name).toBe("Backend");
    expect(state.conversations[BACKEND].stream_seq).toBe(40);
    expect(state.conversations[BACKEND].queue).toHaveLength(1);
    expect(state.shares[BACKEND].agent_id).toBe(BACKEND);
    expect(state.teammate_agents[MINSU]).toHaveLength(3);
    expect(state.teammate_agents[JIWOO]).toHaveLength(1);
    expect(Object.keys(state.inbox)).toHaveLength(2);
    expect(Object.keys(state.questions)).toHaveLength(5);
    expect(state.team?.members).toHaveLength(4);
    expect(state.staleConversations.size).toBe(0);
  });
});

describe("applyEnvelope", () => {
  it("applies events in order and advances seq", () => {
    let state = fromSnapshot(snapshot);
    state = apply(state, {
      type: "agent.state",
      payload: { agent_id: RESEARCH, state: "working", reason: null, exit_code: null },
    });
    state = apply(state, {
      type: "settings.update",
      payload: { settings: { language: "ko", notifications: { enabled: false, muted_until: null } } },
    });
    expect(state.seq).toBe(2);
    expect(state.agents[RESEARCH].state).toBe("working");
    expect(state.settings.language).toBe("ko");
  });

  it("returns new objects and leaves the previous state untouched", () => {
    const before = fromSnapshot(snapshot);
    const after = apply(before, {
      type: "agent.state",
      payload: { agent_id: RESEARCH, state: "working", reason: null, exit_code: null },
    });
    expect(after).not.toBe(before);
    expect(after.agents).not.toBe(before.agents);
    expect(before.agents[RESEARCH].state).toBe("stalled");
    expect(after.conversations).toBe(before.conversations);
  });

  it("ignores a duplicate seq after a replay", () => {
    const start = fromSnapshot(snapshot);
    const event: Event = {
      type: "agent.queue",
      payload: { agent_id: BACKEND, items: [] },
    };
    const once = applyEnvelope(start, envelope(1, event)).state;
    const again = applyEnvelope(once, envelope(1, { type: "agent.update", payload: { change: "removed", agent_id: BACKEND } }));
    expect(again.needsSnapshot).toBe(false);
    expect(again.state).toBe(once);
    expect(again.state.agents[BACKEND]).toBeDefined();
  });

  it("asks for a snapshot on a seq gap without changing state", () => {
    const start = fromSnapshot(snapshot);
    const result = applyEnvelope(
      start,
      envelope(2, { type: "agent.queue", payload: { agent_id: BACKEND, items: [] } }),
    );
    expect(result.needsSnapshot).toBe(true);
    expect(result.state).toBe(start);
  });

  it("plays the approval-raised script: request, queue, approval, stream item, state, inbox", () => {
    const state = play(fromSnapshot(snapshot), approvalRaised);
    const approvalId = "01930000-0000-7000-8000-000000000501";
    expect(state.seq).toBe(approvalRaised.steps.length);
    expect(state.requests["01930000-0000-7000-8000-000000000401"].status).toBe("awaiting_approval");
    expect(state.conversations[BACKEND].queue).toEqual([]);
    expect(state.approvals[approvalId].status).toBe("pending");
    expect(state.conversations[BACKEND].stream_seq).toBe(41);
    expect(state.conversations[BACKEND].items[state.conversations[BACKEND].items.length - 1]).toMatchObject({ kind: "approval", approval_id: approvalId });
    expect(state.agents[BACKEND].state).toBe("waiting_for_approval");
    expect(state.inbox["01930000-0000-7000-8000-000000000604"].kind).toBe("approval");
  });

  it("drops inbox items once resolved (inbox-cleared script)", () => {
    const state = play(fromSnapshot(snapshot), inboxCleared);
    expect(state.inbox).toEqual({});
    expect(state.agents[RESEARCH].state).toBe("working");
  });

  it("removes a question on question.update removed", () => {
    const id = "01930000-0000-7000-8000-000000000304";
    const state = apply(fromSnapshot(snapshot), {
      type: "question.update",
      payload: { change: "removed", question_id: id },
    });
    expect(state.questions[id]).toBeUndefined();
    expect(Object.keys(state.questions)).toHaveLength(4);
  });

  it("replaces a teammate's agents and presence on peer.presence", () => {
    const start = fromSnapshot(snapshot);
    const design = start.teammate_agents[JIWOO][0];
    const state = apply(start, {
      type: "peer.presence",
      payload: {
        member_id: JIWOO,
        online: true,
        last_seen_at: null,
        agents: [{ ...design, state: "working" }, { ...design, agent_id: "new-agent", slug: "copy", name: "Copy" }],
      },
    });
    expect(state.teammate_agents[JIWOO].map((a) => a.slug)).toEqual([design.slug, "copy"]);
    expect(state.teammate_agents[JIWOO][0].state).toBe("working");
    expect(state.teammate_agents[MINSU]).toBe(start.teammate_agents[MINSU]);
    const jiwoo = state.team?.members.find((m) => m.id === JIWOO);
    expect(jiwoo).toMatchObject({ online: true, last_seen_at: null });
  });

  it("upserts and removes agents and schedules", () => {
    let state = fromSnapshot(snapshot);
    const agent = { ...state.agents[BACKEND], id: "new", slug: "new", name: "New" };
    state = apply(state, { type: "agent.update", payload: { change: "upsert", agent } });
    expect(state.agents.new.name).toBe("New");
    state = apply(state, { type: "agent.update", payload: { change: "removed", agent_id: BACKEND } });
    expect(state.agents[BACKEND]).toBeUndefined();
    expect(state.conversations[BACKEND]).toBeUndefined();

    const schedule = Object.values(state.schedules)[0];
    state = apply(state, { type: "schedule.update", payload: { change: "upsert", schedule: { ...schedule, paused: true } } });
    expect(state.schedules[schedule.id].paused).toBe(true);
    state = apply(state, { type: "schedule.update", payload: { change: "removed", schedule_id: schedule.id } });
    expect(state.schedules[schedule.id]).toBeUndefined();
  });
});

describe("agent.stream", () => {
  it("appends started items and text deltas, then replaces the completed item", () => {
    let state = fromSnapshot(snapshot);
    const before = state.conversations[BACKEND].items.length;
    state = apply(state, stream(BACKEND, 41, { kind: "turn_started", message_id: "m1" }));
    state = apply(state, stream(BACKEND, 42, { kind: "item_started", item: { kind: "assistant_text", id: "r1", at: AT, text: "" } }));
    state = apply(state, stream(BACKEND, 43, { kind: "text_delta", item_id: "r1", text: "Got it. " }));
    state = apply(state, stream(BACKEND, 44, { kind: "text_delta", item_id: "r1", text: "On it." }));
    let items = state.conversations[BACKEND].items;
    expect(items).toHaveLength(before + 1);
    expect(items[items.length - 1]).toMatchObject({ kind: "assistant_text", id: "r1", text: "Got it. On it." });

    state = apply(state, stream(BACKEND, 45, { kind: "item_completed", item: { kind: "assistant_text", id: "r1", at: AT, text: "Done." } }));
    state = apply(state, stream(BACKEND, 46, { kind: "turn_ended", message_id: "m1", outcome: "completed" }));
    items = state.conversations[BACKEND].items;
    expect(items).toHaveLength(before + 1);
    expect(items[items.length - 1]).toMatchObject({ text: "Done." });
    expect(state.conversations[BACKEND].stream_seq).toBe(46);
    expect(state.staleConversations.size).toBe(0);
  });

  it("ignores a repeated stream_seq", () => {
    const start = fromSnapshot(snapshot);
    const state = apply(start, stream(BACKEND, 40, { kind: "item_started", item: { kind: "assistant_text", id: "dup", at: AT, text: "" } }));
    expect(state.conversations).toBe(start.conversations);
    expect(state.seq).toBe(1);
  });

  it("marks the conversation stale on a stream gap until a conversation_page reload", () => {
    let state = fromSnapshot(snapshot);
    state = apply(state, stream(BACKEND, 42, { kind: "turn_started", message_id: "m1" }));
    expect(state.staleConversations.has(BACKEND)).toBe(true);
    expect(state.conversations[BACKEND].stream_seq).toBe(40);

    // Later deltas wait for the reload.
    state = apply(state, stream(BACKEND, 43, { kind: "item_started", item: { kind: "assistant_text", id: "r1", at: AT, text: "" } }));
    expect(state.conversations[BACKEND].items.some((i) => i.id === "r1")).toBe(false);

    state = applyConversationPage(state, BACKEND, {
      items: [{ kind: "assistant_text", id: "r1", at: AT, text: "" }],
      has_more: true,
      stream_seq: 43,
    });
    expect(state.staleConversations.has(BACKEND)).toBe(false);
    expect(state.conversations[BACKEND].queue).toHaveLength(1);
    state = apply(state, stream(BACKEND, 44, { kind: "text_delta", item_id: "r1", text: "Hi" }));
    expect(state.conversations[BACKEND].items).toEqual([{ kind: "assistant_text", id: "r1", at: AT, text: "Hi" }]);
  });

  it("marks the conversation stale when a text delta has no item to extend", () => {
    const state = apply(fromSnapshot(snapshot), stream(BACKEND, 41, { kind: "text_delta", item_id: "missing", text: "x" }));
    expect(state.staleConversations.has(BACKEND)).toBe(true);
  });
});
