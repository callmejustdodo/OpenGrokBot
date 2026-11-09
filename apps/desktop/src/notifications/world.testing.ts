// Test helpers: the M3 world loaded into a store, and fixture scripts played
// on top of it as numbered events (fake-formationd's fixtures).
import m3 from "../../../../tools/fake-formationd/fixtures/worlds/m3.json";
import type { Event } from "../ipc/generated/Event";
import type { Snapshot } from "../ipc/generated/Snapshot";
import { appStore, type AppStore } from "../store/store";

export type Script = { steps: ReadonlyArray<{ event: unknown }> };

export function loadM3(store: AppStore = appStore, connected = true): void {
  store.setState({ mirror: null, connected, stale: true, pending: {} });
  store.getState().loadSnapshot(structuredClone(m3) as unknown as Snapshot);
}

export function play(script: Script, store: AppStore = appStore): void {
  for (const step of script.steps) send(step.event as Event, store);
}

export function send(event: Event, store: AppStore = appStore): void {
  const seq = (store.getState().mirror?.seq ?? 0) + 1;
  store.getState().receive({ seq, at: "2026-11-09T01:55:00Z", event });
}
