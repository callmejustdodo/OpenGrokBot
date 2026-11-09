// The preview page's states: every entry of fake-formationd's `states.json`, plus
// the M3 states of screens it doesn't list but the app draws from the same world.
// Each state loads its world into the app store the way fake-formationd does
// (fixture times moved so the M3 clock reads now, then the scripts' events in
// order), and opens its view and sheet with `uiStore`.
import statesJson from "../../../../tools/fake-formationd/fixtures/states.json";
import i18n from "../i18n";
import type { Event } from "../ipc/generated/Event";
import type { Snapshot } from "../ipc/generated/Snapshot";
import { appStore } from "../store/store";
import { uiStore, type Sheet, type View } from "../store/ui";
import { setDetect } from "./tauriMock";

type Script = { steps: { after_ms?: number; event: unknown }[] };

const worlds = byName(import.meta.glob<Snapshot>("../../../../tools/fake-formationd/fixtures/worlds/*.json", { eager: true, import: "default" }));
const scripts = byName(import.meta.glob<Script>("../../../../tools/fake-formationd/fixtures/scripts/*.json", { eager: true, import: "default" }));

function byName<T>(modules: Record<string, T>): Record<string, T> {
  const out: Record<string, T> = {};
  for (const [path, value] of Object.entries(modules)) out[path.replace(/^.*\/|\.json$/g, "")] = value;
  return out;
}

/** fake-formationd's anchor: fixture times are written as if it were this moment. */
export const ANCHOR = "2026-11-09T01:53:00Z";

const MINSU = "01930000-0000-7000-8000-000000000002";
const JIWOO = "01930000-0000-7000-8000-000000000003";
const BACKEND = "01930000-0000-7000-8000-000000000101";
const RESEARCH = "01930000-0000-7000-8000-000000000103";
const PLANNING = "01930000-0000-7000-8000-000000000105";
const SHARED_WEB = "01930000-0000-7000-8000-000000000106";
const AUTH = "01930000-0000-7000-8000-000000000201";
const QA = "01930000-0000-7000-8000-000000000203";
const DESIGN = "01930000-0000-7000-8000-000000000204";
const MY_REQUEST = "01930000-0000-7000-8000-000000000402";
const MINSU_APPROVAL = "01930000-0000-7000-8000-000000000501";

/** A click the state needs after the app renders, for states held in a component. */
export type Step = "keep-similar" | "deny" | "stop";

type Ui = { view?: View; sheet?: Sheet; step?: Step; detect?: "hang" };

const agent = (agentId: string): View => ({ kind: "agent", agentId });
const auth: View = { kind: "teammate-agent", owner: MINSU, agentId: AUTH };
const minsu: View = { kind: "person", memberId: MINSU };
const approval: Ui = { view: { kind: "inbox" }, sheet: { kind: "approval", approvalId: MINSU_APPROVAL } };

/** What each state opens: `states.json`'s `ui` notes, done with `uiStore`. */
const UI: Record<string, Ui> = {
  "SC-003/detecting": { sheet: { kind: "connect" }, detect: "hang" },
  "SC-005/working": { view: agent(BACKEND) },
  "SC-005/idle": { view: agent(BACKEND) },
  "SC-005/queued": { view: agent(BACKEND) },
  "SC-005/permission": { view: agent(BACKEND) },
  "SC-005/approval": { view: agent(BACKEND) },
  "SC-005/stalled": { view: agent(RESEARCH) },
  "SC-005/error": { view: agent(PLANNING) },
  "SC-005/stopped": { view: agent(PLANNING) },
  "SC-005/detected": { view: agent(SHARED_WEB) },
  "SC-005/stop-confirm": { view: agent(BACKEND), step: "stop" },
  "SC-010/default": { view: { kind: "inbox" } },
  "SC-010/empty": { view: { kind: "inbox" } },
  "SC-011/default": approval,
  "SC-011/keep-similar": { ...approval, step: "keep-similar" },
  "SC-011/expiring": approval,
  "SC-011/expired": approval,
  "SC-011/deny-confirm": { ...approval, step: "deny" },
  "SC-012/default": { view: { kind: "rules" } },
  "SC-012/empty": { view: { kind: "rules" } },
  "SC-015/answered": { view: auth },
  "SC-015/new": { view: { kind: "new-question" } },
  "SC-015/received": { view: auth },
  "SC-015/answering": { view: auth },
  "SC-015/unsure": { view: minsu },
  "SC-015/stale": { view: auth },
  "SC-015/offline-waiting": { view: { kind: "teammate-agent", owner: JIWOO, agentId: DESIGN } },
  "SC-015/sent-to-owner": { view: minsu },
  "SC-015/unshared": { view: auth },
  "SC-015/error": { view: auth },
  "SC-016/compose": { view: auth, sheet: { kind: "work-request", owner: MINSU, agentId: AUTH, fromQuestion: null } },
  "SC-016/status": { view: auth, sheet: { kind: "request-status", requestId: MY_REQUEST } },
  "SC-016/not-accepted": { view: auth, sheet: { kind: "work-request", owner: MINSU, agentId: QA, fromQuestion: null } },
};

/** M3 states `states.json` doesn't list, drawn from the M3 world without a script. */
const EXTRA: Record<string, { description: string; world?: string } & Ui> = {
  "SC-001/default": { description: "Welcome: no team yet", world: "empty-team" },
  "SC-004/default": { description: "New agent sheet", sheet: { kind: "new-agent" } },
  "SC-007/panel": { description: "Backend's schedules in the information panel", view: agent(BACKEND) },
  "SC-008/shared": { description: "Backend's sharing section in the information panel", view: agent(BACKEND) },
  "SC-009/connectable": { description: "Agents found on this Mac", sheet: { kind: "connect" } },
};

export type PreviewState = {
  /** `SC-005/working`. */
  id: string;
  screen: string;
  state: string;
  description: string;
  world: string;
  scripts: string[];
  ui: Ui;
};

type StateEntry = { description: string; world?: string; scripts?: string[] };

export const STATES: PreviewState[] = [
  ...Object.entries(statesJson as Record<string, StateEntry>).map(([id, s]) => entry(id, s.description, s.world, s.scripts, UI[id] ?? {})),
  ...Object.entries(EXTRA).map(([id, s]) => entry(id, s.description, s.world, [], s)),
].sort((a, b) => a.screen.localeCompare(b.screen));

function entry(id: string, description: string, world = "m3", scriptNames: string[] = [], ui: Ui): PreviewState {
  const [screen, state] = id.split("/");
  return { id, screen, state, description, world, scripts: scriptNames, ui };
}

export const SCREENS: string[] = [...new Set(STATES.map((s) => s.screen))];

export function findState(id: string): PreviewState | undefined {
  return STATES.find((s) => s.id === id);
}

const RFC3339 = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$/;

/**
 * Fills fake-formationd's placeholders (`{{now}}` is the anchor; `{{id:name}}` a
 * fixed ID per name) and moves every timestamp by `shift` milliseconds.
 */
export function prepare<T>(value: T, shift: number): T {
  const ids = new Map<string, string>();
  const fill = (s: string): string => {
    if (s === "{{now}}") s = ANCHOR;
    s = s.replace(/\{\{id:([\w-]+)\}\}/g, (_, name: string) => {
      if (!ids.has(name)) ids.set(name, `01930000-0000-7000-8000-${String(900000000000 + ids.size).padStart(12, "0")}`);
      return ids.get(name)!;
    });
    if (RFC3339.test(s)) return new Date(Date.parse(s) + shift).toISOString().replace(/\.\d{3}Z$/, "Z");
    return s;
  };
  const walk = (v: unknown): unknown => {
    if (typeof v === "string") return fill(v);
    if (Array.isArray(v)) return v.map(walk);
    if (v && typeof v === "object") return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, walk(x)]));
    return v;
  };
  return walk(value) as T;
}

function world(name: string): Snapshot {
  // SC-001: the empty world still has a team (the sidebar's first run), so
  // Welcome shows the M3 world before a team exists.
  if (name === "empty-team") return { ...worlds.m3, team: null } as Snapshot;
  const w = worlds[name];
  if (!w) throw new Error(`no world ${name}`);
  return w;
}

/** Loads `state` into the app and UI stores. `now` is when the M3 clock reads the anchor. */
export function loadState(state: PreviewState, now: number = Date.now()): void {
  const shift = now - Date.parse(ANCHOR);
  setDetect(state.ui.detect ?? "answer");
  appStore.setState({ mirror: null, connected: true, stale: false, pending: {} });
  appStore.getState().loadSnapshot(prepare(world(state.world), shift));
  const at = new Date(now).toISOString();
  for (const name of state.scripts) {
    const script = scripts[name];
    if (!script) throw new Error(`no script ${name}`);
    for (const step of script.steps) {
      const mirror = appStore.getState().mirror!;
      let event = prepare(step.event, shift) as Event;
      if (event.type === "agent.stream") {
        const streamSeq = (mirror.conversations[event.payload.agent_id]?.stream_seq ?? 0) + 1;
        event = { ...event, payload: { ...event.payload, stream_seq: streamSeq } };
      }
      appStore.getState().receive({ seq: mirror.seq + 1, at, event });
    }
  }
  uiStore.setState({ view: state.ui.view ?? { kind: "home" }, sheet: state.ui.sheet ?? null, panelOpen: true });
}

/** A button by its accessible label or its first line of text. */
function button(scope: ParentNode, text: string): HTMLButtonElement | undefined {
  return [...scope.querySelectorAll("button")].find(
    (b) =>
      b.getAttribute("aria-label") === text ||
      b.textContent?.trim() === text ||
      b.querySelector("span")?.textContent?.trim() === text,
  );
}

/** Does the state's click, once the app has rendered. Returns whether it found its button. */
export function runStep(step: Step, doc: Document = document): boolean {
  const t = i18n.t.bind(i18n);
  const dialog = doc.querySelector("[role=dialog]") ?? doc;
  const target =
    step === "stop"
      ? button(doc, t("conversation.stop", "Stop ⌘."))
      : step === "deny"
        ? button(dialog, t("approval.deny", "Deny"))
        : button(dialog, t("approval.keepSimilar", "Keep allowing similar commands…"));
  target?.click();
  return target !== undefined;
}

