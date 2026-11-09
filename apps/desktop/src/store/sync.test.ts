import { describe, expect, it, vi } from "vitest";
import m3 from "../../../../tools/fake-formationd/fixtures/worlds/m3.json";
import type { Snapshot } from "../ipc/generated/Snapshot";
import { createAppStore } from "./store";
import { act, reloadStaleConversations } from "./sync";
import type { Transport } from "./transport";

const BACKEND = "01930000-0000-7000-8000-000000000101";

function transport(call: (method: string, params: unknown) => Promise<unknown>): Transport {
  return {
    call: call as Transport["call"],
    onEvent: () => () => {},
    onStatus: () => () => {},
  };
}

describe("sync", () => {
  it("reloads a conversation whose stream skipped a delta", async () => {
    const store = createAppStore();
    store.getState().loadSnapshot(m3 as unknown as Snapshot);
    store.getState().setConnected(true);
    const call = vi.fn(() => Promise.resolve({ items: [], has_more: false, stream_seq: 99 }));
    reloadStaleConversations(transport(call), store);

    // stream_seq 45 after 40: a gap marks Backend's conversation stale.
    store.getState().receive({
      seq: 1,
      at: "2026-11-09T01:54:00Z",
      event: {
        type: "agent.stream",
        payload: {
          agent_id: BACKEND,
          session_id: `session-${BACKEND}`,
          stream_seq: 45,
          delta: { kind: "turn_started", message_id: "m" },
        },
      },
    });
    await vi.waitFor(() => expect(store.getState().mirror?.staleConversations.has(BACKEND)).toBe(false));
    expect(call).toHaveBeenCalledTimes(1);
    expect(call).toHaveBeenCalledWith("conversation_page", {
      agent_id: BACKEND,
      session_id: null,
      before: null,
      limit: 50,
    });
    expect(store.getState().mirror?.conversations[BACKEND].stream_seq).toBe(99);
  });

  it("marks an action pending until its reply", async () => {
    const store = createAppStore(() => 7);
    let resolve: (v: unknown) => void = () => {};
    const call = vi.fn(() => new Promise((r) => (resolve = r)));
    const done = act(transport(call), "send:backend", "agent_start", { agent_id: BACKEND }, store);
    expect(store.getState().pending).toEqual({ "send:backend": 7 });
    resolve({});
    await done;
    expect(store.getState().pending).toEqual({});
  });
});
