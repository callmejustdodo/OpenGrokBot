// Test support: the M3 world with fake-formationd's scripts applied, to reach
// each M3 state of the approval and request screens.
import m3 from "../../../../../tools/fake-formationd/fixtures/worlds/m3.json";
import approvalExpired from "../../../../../tools/fake-formationd/fixtures/scripts/approval-expired.json";
import approvalExpiring from "../../../../../tools/fake-formationd/fixtures/scripts/approval-expiring.json";
import approvalRaised from "../../../../../tools/fake-formationd/fixtures/scripts/approval-raised.json";
import backendPermission from "../../../../../tools/fake-formationd/fixtures/scripts/backend-permission.json";
import inboxCleared from "../../../../../tools/fake-formationd/fixtures/scripts/inbox-cleared.json";
import requestStatus from "../../../../../tools/fake-formationd/fixtures/scripts/request-status.json";
import type { Event } from "../../ipc/generated/Event";
import type { Snapshot } from "../../ipc/generated/Snapshot";
import { appStore } from "../../store/store";

const SCRIPTS = {
  "approval-raised": approvalRaised,
  "approval-expiring": approvalExpiring,
  "approval-expired": approvalExpired,
  "backend-permission": backendPermission,
  "inbox-cleared": inboxCleared,
  "request-status": requestStatus,
};

export type ScriptName = keyof typeof SCRIPTS;

export const ME = "01930000-0000-7000-8000-000000000001";
export const MINSU = "01930000-0000-7000-8000-000000000002";
export const BACKEND = "01930000-0000-7000-8000-000000000101";
export const RESEARCH = "01930000-0000-7000-8000-000000000103";
export const AUTH = "01930000-0000-7000-8000-000000000201";
export const QA = "01930000-0000-7000-8000-000000000203";
export const MINSU_REQUEST = "01930000-0000-7000-8000-000000000401";
export const MY_REQUEST = "01930000-0000-7000-8000-000000000402";
export const MINSU_APPROVAL = "01930000-0000-7000-8000-000000000501";
export const PERMISSION = "01930000-0000-7000-8000-000000000502";
/** The M3 clock: Minsu's approval was raised three minutes ago. */
export const M3_NOW = new Date("2026-11-09T01:53:00Z");

/** Applies one event with the next `seq` (and the next `stream_seq` for stream deltas). */
export function apply(event: Event): void {
  const mirror = appStore.getState().mirror;
  if (!mirror) throw new Error("load the world first");
  let next = event;
  if (event.type === "agent.stream") {
    const streamSeq = (mirror.conversations[event.payload.agent_id]?.stream_seq ?? 0) + 1;
    next = { ...event, payload: { ...event.payload, stream_seq: streamSeq } };
  }
  appStore.getState().receive({ seq: mirror.seq + 1, at: M3_NOW.toISOString(), event: next });
}

/** Loads the M3 snapshot, then plays the named scripts in order. */
export function loadWorld(scripts: ScriptName[] = []): void {
  appStore.setState({ pending: {} });
  appStore.getState().loadSnapshot(m3 as unknown as Snapshot);
  for (const name of scripts) {
    for (const step of SCRIPTS[name].steps) apply(step.event as unknown as Event);
  }
}
