// Test helpers for TASK-015's screens: load the M3 world and play fixture
// scripts the way fake-formationd does (`seq` and each agent's `stream_seq`
// continue from the mirror).
import { act } from "@testing-library/react";
import m3 from "../../../../../tools/fake-formationd/fixtures/worlds/m3.json";
import type { Event } from "../../ipc/generated/Event";
import type { Snapshot } from "../../ipc/generated/Snapshot";
import { appStore } from "../../store/store";
import { uiStore } from "../../store/ui";

export const BACKEND = "01930000-0000-7000-8000-000000000101";
export const MARKETING = "01930000-0000-7000-8000-000000000102";
export const RESEARCH = "01930000-0000-7000-8000-000000000103";
export const SUPPORT = "01930000-0000-7000-8000-000000000104";
export const PLANNING = "01930000-0000-7000-8000-000000000105";
export const SHARED_WEB = "01930000-0000-7000-8000-000000000106";
export const MINSU = "01930000-0000-7000-8000-000000000002";
export const JIWOO = "01930000-0000-7000-8000-000000000003";
export const AUTH = "01930000-0000-7000-8000-000000000201";
export const QA = "01930000-0000-7000-8000-000000000203";

export function loadWorld(snapshot: Snapshot = m3 as unknown as Snapshot) {
  appStore.setState({ mirror: null, connected: true, stale: false, pending: {} });
  appStore.getState().loadSnapshot(structuredClone(snapshot));
  uiStore.setState({ view: { kind: "home" }, sheet: null, panelOpen: true });
}

export function play(script: { steps: { event: unknown }[] }) {
  act(() => {
    for (const step of script.steps) {
      const mirror = appStore.getState().mirror;
      if (!mirror) throw new Error("no mirror");
      const event = structuredClone(step.event) as Event;
      if (event.type === "agent.stream") {
        event.payload.stream_seq = mirror.conversations[event.payload.agent_id].stream_seq + 1;
      }
      appStore.getState().receive({ seq: mirror.seq + 1, at: "2026-11-09T01:53:00Z", event });
    }
  });
}

export function emit(event: Event) {
  play({ steps: [{ event }] });
}

/** A formationd error as `src-tauri` rejects it. */
export function apiError(code: string) {
  return { code: "rpc", message: code, rpc_code: -32000, data: { code, message: code } };
}
