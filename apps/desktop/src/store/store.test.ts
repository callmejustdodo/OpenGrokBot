import { describe, expect, it, vi } from "vitest";
import m3 from "../../../../tools/fake-formationd/fixtures/worlds/m3.json";
import type { Method, Params, Result } from "../ipc/generated/commands";
import type { EventEnvelope } from "../ipc/generated/EventEnvelope";
import type { Snapshot } from "../ipc/generated/Snapshot";
import { connectStore, createAppStore } from "./store";
import type { ConnectionStatus, Transport } from "./transport";

const snapshot = m3 as unknown as Snapshot;
const RESEARCH = "01930000-0000-7000-8000-000000000103";

function stateEvent(seq: number, state: "working" | "idle"): EventEnvelope {
  return {
    seq,
    at: "2026-11-09T01:53:00Z",
    event: { type: "agent.state", payload: { agent_id: RESEARCH, state, reason: null, exit_code: null } },
  };
}

function fakeTransport() {
  const events = new Set<(e: EventEnvelope) => void>();
  const statuses = new Set<(s: ConnectionStatus) => void>();
  const snapshots: Snapshot[] = [];
  const calls: Method[] = [];
  const transport: Transport = {
    call<M extends Method>(method: M, params: Params<M>): Promise<Result<M>> {
      void params;
      calls.push(method);
      const next = snapshots.shift();
      if (method !== "snapshot" || !next) return Promise.reject(new Error(`unexpected ${method}`));
      return Promise.resolve(next as Result<M>);
    },
    onEvent(callback) {
      events.add(callback);
      return () => events.delete(callback);
    },
    onStatus(callback) {
      statuses.add(callback);
      return () => statuses.delete(callback);
    },
  };
  return {
    transport,
    calls,
    queueSnapshot: (s: Snapshot) => snapshots.push(s),
    emit: (e: EventEnvelope) => events.forEach((cb) => cb(e)),
    status: (s: ConnectionStatus) => statuses.forEach((cb) => cb(s)),
    listeners: () => events.size + statuses.size,
  };
}

describe("app store", () => {
  it("loads a snapshot and applies events in order", () => {
    const store = createAppStore();
    store.getState().loadSnapshot(snapshot);
    store.getState().receive(stateEvent(1, "working"));
    expect(store.getState().stale).toBe(false);
    expect(store.getState().mirror?.agents[RESEARCH].state).toBe("working");
  });

  it("asks for a snapshot on a gap and marks itself stale", () => {
    const store = createAppStore();
    const requestSnapshot = vi.fn();
    store.setState({ requestSnapshot });
    store.getState().loadSnapshot(snapshot);
    store.getState().receive(stateEvent(3, "working"));
    expect(requestSnapshot).toHaveBeenCalledTimes(1);
    expect(store.getState().stale).toBe(true);
    expect(store.getState().mirror?.seq).toBe(0);
  });

  it("tracks pending actions with their start time", () => {
    let now = 1000;
    const store = createAppStore(() => now);
    store.getState().beginPending("send:backend");
    now = 1100;
    store.getState().beginPending("stop:research");
    expect(store.getState().pending).toEqual({ "send:backend": 1000, "stop:research": 1100 });
    store.getState().endPending("send:backend");
    expect(store.getState().pending).toEqual({ "stop:research": 1100 });
  });
});

describe("connectStore", () => {
  it("snapshots on connect, applies events, and re-snapshots after a reconnect", async () => {
    const store = createAppStore();
    const fake = fakeTransport();
    const disconnect = connectStore(fake.transport, store);

    fake.queueSnapshot(snapshot);
    fake.status("connected");
    await vi.waitFor(() => expect(store.getState().mirror).not.toBeNull());
    expect(store.getState()).toMatchObject({ connected: true, stale: false });

    fake.emit(stateEvent(1, "working"));
    expect(store.getState().mirror?.agents[RESEARCH].state).toBe("working");

    fake.status("disconnected");
    expect(store.getState()).toMatchObject({ connected: false, stale: true });

    fake.queueSnapshot({ ...snapshot, seq: 7 });
    fake.status("connected");
    await vi.waitFor(() => expect(store.getState().mirror?.seq).toBe(7));
    expect(store.getState().stale).toBe(false);
    expect(store.getState().mirror?.agents[RESEARCH].state).toBe("stalled");
    expect(fake.calls).toEqual(["snapshot", "snapshot"]);

    disconnect();
    expect(fake.listeners()).toBe(0);
  });

  it("re-snapshots once on an event gap", async () => {
    const store = createAppStore();
    const fake = fakeTransport();
    connectStore(fake.transport, store);
    fake.queueSnapshot(snapshot);
    fake.status("connected");
    await vi.waitFor(() => expect(store.getState().mirror).not.toBeNull());

    fake.queueSnapshot({ ...snapshot, seq: 5 });
    fake.emit(stateEvent(4, "working"));
    fake.emit(stateEvent(5, "idle"));
    await vi.waitFor(() => expect(store.getState().mirror?.seq).toBe(5));
    expect(fake.calls).toEqual(["snapshot", "snapshot"]);
    expect(store.getState().stale).toBe(false);

    fake.emit(stateEvent(6, "working"));
    expect(store.getState().mirror?.agents[RESEARCH].state).toBe("working");
  });
});
