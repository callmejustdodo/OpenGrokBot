import { describe, expect, it } from "vitest";
import m3 from "../../../../tools/fake-formationd/fixtures/worlds/m3.json";
import type { Approval } from "../ipc/generated/Approval";
import type { Snapshot } from "../ipc/generated/Snapshot";
import { applyEnvelope, fromSnapshot } from "./reducer";
import { selectChatList, selectConversation, selectNeedsYouCount, selectThreadQuestions } from "./selectors";

const snapshot = m3 as unknown as Snapshot;
const BACKEND = "01930000-0000-7000-8000-000000000101";
const MINSU = "01930000-0000-7000-8000-000000000002";
const JIWOO = "01930000-0000-7000-8000-000000000003";

function approval(id: string, status: Approval["status"]): Approval {
  return {
    id,
    agent_id: BACKEND,
    request_id: null,
    requester: null,
    requester_agent: null,
    purpose: null,
    call: { tool_use_id: `toolu_${id}`, tool_name: "Bash", input: { command: "npm test" } },
    summary: "npm test",
    proposed_rule: null,
    status,
    choice: null,
    created_at: "2026-11-09T01:50:00Z",
    decided_at: null,
    expires_at: "2026-11-10T01:50:00Z",
  };
}

describe("selectChatList", () => {
  it("mixes my agents, teammates' agents I asked, and people, most recent first", () => {
    const list = selectChatList(fromSnapshot(snapshot));
    const keys = list.map((item) =>
      item.kind === "person" ? `person:${item.member_id.slice(-3)}` : `${item.kind}:${item.agent_id.slice(-3)}`,
    );
    expect(keys[0]).toBe("agent:101");
    // Six agents, Minsu's Auth (two questions, one row), Jiwoo's Design, Minsu and Jiwoo as people.
    expect(keys).toHaveLength(10);
    expect(keys.filter((k) => k === "teammate_agent:201")).toHaveLength(1);
    expect(keys).toEqual(expect.arrayContaining(["teammate_agent:204", "person:002", "person:003"]));

    const times = list.map((item) => (item.at ? Date.parse(item.at) : -Infinity));
    expect(times).toEqual([...times].sort((a, b) => b - a));

    const auth = list.find((item) => item.kind === "teammate_agent" && item.agent_id.endsWith("201"));
    expect(auth).toMatchObject({ owner: MINSU, at: "2026-11-09T01:42:30Z", agent: { slug: "auth" } });
    const jiwoo = list.find((item) => item.kind === "person" && item.member_id === JIWOO);
    expect(jiwoo).toMatchObject({ member: { display_name: "Jiwoo" } });
  });

  it("returns the same array until the state changes", () => {
    const state = fromSnapshot(snapshot);
    expect(selectChatList(state)).toBe(selectChatList(state));
  });
});

describe("selectNeedsYouCount", () => {
  it("counts unresolved inbox items and pending approvals once each", () => {
    let state = fromSnapshot(snapshot);
    expect(selectNeedsYouCount(state)).toBe(2);

    state = {
      ...state,
      approvals: {
        a1: approval("a1", "pending"),
        a2: approval("a2", "pending"),
        a3: approval("a3", "allowed"),
      },
    };
    expect(selectNeedsYouCount(state)).toBe(4);

    // The Inbox row for a1 and the approval itself are one thing that needs me.
    state = applyEnvelope(state, {
      seq: state.seq + 1,
      at: "2026-11-09T01:53:00Z",
      event: {
        type: "notify",
        payload: {
          alert: true,
          item: {
            id: "i1",
            kind: "approval",
            target: { kind: "approval", approval_id: "a1" },
            subject: "npm test",
            at: "2026-11-09T01:50:00Z",
            resolved_at: null,
          },
        },
      },
    }).state;
    expect(selectNeedsYouCount(state)).toBe(4);
  });
});

describe("selectConversation and selectThreadQuestions", () => {
  it("finds an agent's conversation", () => {
    const state = fromSnapshot(snapshot);
    expect(selectConversation(state, BACKEND)?.stream_seq).toBe(40);
    expect(selectConversation(state, "missing")).toBeUndefined();
  });

  it("lists a thread's questions oldest first", () => {
    const state = fromSnapshot(snapshot);
    const thread = selectThreadQuestions(state, "01930000-0000-7000-8000-000000000301");
    expect(thread.map((q) => q.id.slice(-3))).toEqual(["301", "302"]);
    expect(selectThreadQuestions(state, "01930000-0000-7000-8000-000000000301")).toBe(thread);
    expect(selectThreadQuestions(state, "missing")).toEqual([]);
  });
});
