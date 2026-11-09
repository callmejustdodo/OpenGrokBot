import { useStore } from "zustand";
import { createStore, type StoreApi } from "zustand/vanilla";
import type { AgentId } from "../ipc/generated/AgentId";
import type { ConversationPageResult } from "../ipc/generated/ConversationPageResult";
import type { EventEnvelope } from "../ipc/generated/EventEnvelope";
import type { Snapshot } from "../ipc/generated/Snapshot";
import { applyConversationPage, applyEnvelope, fromSnapshot, type MirrorState } from "./reducer";
import type { Transport, Unsubscribe } from "./transport";

export type AppState = {
  /** formationd's state as last mirrored; `null` until the first snapshot. */
  mirror: MirrorState | null;
  connected: boolean;
  /** The mirror may be behind formationd: disconnected, or a gap is waiting for a new snapshot. */
  stale: boolean;
  /** Optimistic actions still waiting for their confirming event: key → start time (ms). */
  pending: Readonly<Record<string, number>>;
  /** Called when an event gap means a new snapshot is needed; set by `connectStore`. */
  requestSnapshot: () => void;
  loadSnapshot: (snapshot: Snapshot) => void;
  receive: (envelope: EventEnvelope) => void;
  loadConversationPage: (agentId: AgentId, page: ConversationPageResult) => void;
  setConnected: (connected: boolean) => void;
  beginPending: (key: string) => void;
  endPending: (key: string) => void;
};

export type AppStore = StoreApi<AppState>;

export function createAppStore(now: () => number = Date.now): AppStore {
  return createStore<AppState>()((set, get) => ({
    mirror: null,
    connected: false,
    stale: true,
    pending: {},
    requestSnapshot: () => {},
    loadSnapshot: (snapshot) => set({ mirror: fromSnapshot(snapshot), stale: false }),
    receive: (envelope) => {
      const { mirror } = get();
      // Before the first snapshot there is nothing to apply to; the snapshot covers it.
      if (!mirror) return;
      const result = applyEnvelope(mirror, envelope);
      if (result.needsSnapshot) {
        set({ stale: true });
        get().requestSnapshot();
      } else if (result.state !== mirror) {
        set({ mirror: result.state });
      }
    },
    loadConversationPage: (agentId, page) => {
      const { mirror } = get();
      if (mirror) set({ mirror: applyConversationPage(mirror, agentId, page) });
    },
    setConnected: (connected) => set(connected ? { connected } : { connected, stale: true }),
    beginPending: (key) => set((s) => ({ pending: { ...s.pending, [key]: now() } })),
    endPending: (key) =>
      set((s) => {
        if (!(key in s.pending)) return s;
        const pending = { ...s.pending };
        delete pending[key];
        return { pending };
      }),
  }));
}

export const appStore = createAppStore();

export function useAppStore<T>(selector: (state: AppState) => T): T {
  return useStore(appStore, selector);
}

/**
 * Keeps `store` mirroring formationd over `transport`: a snapshot on every connect,
 * then events in order, and a new snapshot when an event gap shows up.
 */
export function connectStore(transport: Transport, store: AppStore = appStore): Unsubscribe {
  // Bumped on every connect and disconnect, so a snapshot answered for an old connection is dropped.
  let connection = 0;
  let inFlight: number | null = null;

  const requestSnapshot = () => {
    if (inFlight === connection) return;
    const requested = connection;
    inFlight = requested;
    transport.call("snapshot", {}).then(
      (snapshot) => {
        if (inFlight === requested) inFlight = null;
        if (requested === connection) store.getState().loadSnapshot(snapshot);
      },
      () => {
        // The mirror stays stale; the next gap or reconnect asks again.
        if (inFlight === requested) inFlight = null;
      },
    );
  };

  store.setState({ requestSnapshot });
  const offStatus = transport.onStatus((status) => {
    connection += 1;
    store.getState().setConnected(status === "connected");
    if (status === "connected") requestSnapshot();
  });
  const offEvent = transport.onEvent((envelope) => store.getState().receive(envelope));

  return () => {
    offStatus();
    offEvent();
    connection += 1;
    store.setState({ requestSnapshot: () => {} });
  };
}
