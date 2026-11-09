// Test helpers for the asking screens: the M3 world in the store, and fixture
// scripts applied as events in `seq` order.
import m3 from "../../../../../tools/fake-formationd/fixtures/worlds/m3.json";
import type { Event } from "../../ipc/generated/Event";
import type { Snapshot } from "../../ipc/generated/Snapshot";
import { appStore } from "../../store/store";
import { uiStore } from "../../store/ui";

export const ME = "01930000-0000-7000-8000-000000000001";
export const MINSU = "01930000-0000-7000-8000-000000000002";
export const JIWOO = "01930000-0000-7000-8000-000000000003";
export const SEOYEON = "01930000-0000-7000-8000-000000000004";
export const BACKEND = "01930000-0000-7000-8000-000000000101";
export const AUTH = "01930000-0000-7000-8000-000000000201";
export const FRONTEND = "01930000-0000-7000-8000-000000000202";
export const DESIGN = "01930000-0000-7000-8000-000000000204";

/** A fresh copy of the M3 snapshot. */
export function m3Snapshot(): Snapshot {
  return structuredClone(m3) as unknown as Snapshot;
}

/** Resets both stores and loads the M3 world (optionally changed first). */
export function loadWorld(change?: (snapshot: Snapshot) => void): void {
  const snapshot = m3Snapshot();
  change?.(snapshot);
  appStore.setState({ mirror: null, connected: true, stale: false, pending: {} });
  appStore.getState().loadSnapshot(snapshot);
  uiStore.setState({ view: { kind: "home" }, sheet: null, panelOpen: true });
}

/** Applies a fixture script's events after the current `seq`. */
export function play(script: { steps: { event: unknown }[] }): void {
  for (const step of script.steps) {
    const seq = (appStore.getState().mirror?.seq ?? 0) + 1;
    appStore.getState().receive({ seq, at: "2026-11-09T01:55:00Z", event: step.event as Event });
  }
}
