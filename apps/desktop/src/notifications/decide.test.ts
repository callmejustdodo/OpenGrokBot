import { beforeEach, describe, expect, it, vi } from "vitest";
import approvalRaised from "../../../../tools/fake-formationd/fixtures/scripts/approval-raised.json";
import inboxCleared from "../../../../tools/fake-formationd/fixtures/scripts/inbox-cleared.json";
import planningError from "../../../../tools/fake-formationd/fixtures/scripts/planning-error.json";
import i18n from "../i18n";
import type { EventEnvelope } from "../ipc/generated/EventEnvelope";
import type { NotifyEvent } from "../ipc/generated/NotifyEvent";
import type { Settings } from "../ipc/generated/Settings";
import { appStore } from "../store/store";
import type { Transport } from "../store/transport";
import { startNotificationBridge } from "./bridge";
import { isMuted, noticeFor } from "./decide";
import { loadM3, play, type Script } from "./world.testing";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const t = i18n.getFixedT("en");
const NOW = Date.parse("2026-11-09T02:00:00Z");
const PLANNING = "01930000-0000-7000-8000-000000000105";

/** The `notify` payload of a fixture script's last step. */
function lastNotify(script: Script): NotifyEvent {
  const steps = script.steps.map((s) => s.event as { type: string; payload: NotifyEvent });
  const notifies = steps.filter((e) => e.type === "notify");
  return notifies[notifies.length - 1].payload;
}

function withSettings(notifications: Settings["notifications"]) {
  appStore.setState((s) => ({ mirror: s.mirror && { ...s.mirror, settings: { language: null, notifications } } }));
}

beforeEach(() => loadM3());

describe("noticeFor (SC-019)", () => {
  it("approval: names the requester and the command", () => {
    play(approvalRaised);
    const notice = noticeFor(lastNotify(approvalRaised), appStore.getState().mirror, new Map(), NOW, t);
    expect(notice).toEqual({
      title: "Approval needed",
      body: "Minsu wants to run npm test -- auth/token.spec.ts",
      target: { kind: "approval", approval_id: "01930000-0000-7000-8000-000000000501" },
    });
  });

  it("approval: falls back to the subject when the approval is not mirrored yet", () => {
    const notice = noticeFor(lastNotify(approvalRaised), appStore.getState().mirror, new Map(), NOW, t);
    expect(notice?.body).toBe("Minsu → Backend · npm test -- auth/token.spec.ts");
  });

  it("error: names the agent and the reason", () => {
    const reasons = new Map([[PLANNING, "The process exited unexpectedly"]]);
    const notice = noticeFor(lastNotify(planningError), appStore.getState().mirror, reasons, NOW, t);
    expect(notice).toEqual({
      title: "Agent stopped",
      body: "Planning stopped: The process exited unexpectedly",
      target: { kind: "agent", agent_id: PLANNING },
    });
    const fromSubject = noticeFor(lastNotify(planningError), appStore.getState().mirror, new Map(), NOW, t);
    expect(fromSubject?.body).toBe("Planning stopped: exited with code 1");
  });

  it("does not alert when formationd did not ask, or the item is resolved", () => {
    const mirror = appStore.getState().mirror;
    expect(noticeFor(lastNotify(inboxCleared), mirror, new Map(), NOW, t)).toBeNull();
    const event = lastNotify(planningError);
    expect(noticeFor({ ...event, alert: false }, mirror, new Map(), NOW, t)).toBeNull();
    expect(
      noticeFor({ ...event, item: { ...event.item, resolved_at: "2026-11-09T01:13:00Z" } }, mirror, new Map(), NOW, t),
    ).toBeNull();
  });

  it("does not alert when notifications are off or paused", () => {
    const event = lastNotify(planningError);
    withSettings({ enabled: false, muted_until: null });
    expect(noticeFor(event, appStore.getState().mirror, new Map(), NOW, t)).toBeNull();
    withSettings({ enabled: true, muted_until: "2026-11-09T03:00:00Z" });
    expect(noticeFor(event, appStore.getState().mirror, new Map(), NOW, t)).toBeNull();
    withSettings({ enabled: true, muted_until: "2026-11-09T01:00:00Z" });
    expect(noticeFor(event, appStore.getState().mirror, new Map(), NOW, t)).not.toBeNull();
  });

  it("before the first snapshot, formationd's alert decides", () => {
    expect(noticeFor(lastNotify(planningError), null, new Map(), NOW, t)?.body).toBe(
      "Planning stopped: exited with code 1",
    );
  });

  it("other kinds use the kind as the title and the subject as the body", () => {
    const event = lastNotify(inboxCleared);
    const raised = { alert: true, item: { ...event.item, resolved_at: null } };
    expect(noticeFor(raised, appStore.getState().mirror, new Map(), NOW, t)).toEqual({
      title: "Stalled",
      body: "Research · no activity for 14 minutes",
      target: raised.item.target,
    });
  });
});

describe("isMuted", () => {
  it("is muted when off, or until the pause ends", () => {
    expect(isMuted({ enabled: false, muted_until: null }, NOW)).toBe(true);
    expect(isMuted({ enabled: true, muted_until: null }, NOW)).toBe(false);
    expect(isMuted({ enabled: true, muted_until: "2026-11-09T02:00:01Z" }, NOW)).toBe(true);
    expect(isMuted({ enabled: true, muted_until: "2026-11-09T02:00:00Z" }, NOW)).toBe(false);
  });
});

describe("notification bridge", () => {
  it("shows one notification per alerting notify event, with the last agent.state reason", () => {
    let deliver: (e: EventEnvelope) => void = () => {};
    const transport: Transport = {
      call: () => Promise.reject(new Error("unused")),
      onEvent: (cb) => {
        deliver = cb;
        return () => {};
      },
      onStatus: () => () => {},
    };
    const show = vi.fn(() => Promise.resolve());
    const stop = startNotificationBridge(transport, appStore, show, () => NOW);
    let seq = 0;
    for (const script of [planningError, inboxCleared] as Script[]) {
      for (const step of script.steps) {
        deliver({ seq: ++seq, at: "2026-11-09T01:55:00Z", event: step.event as EventEnvelope["event"] });
      }
    }
    stop();
    expect(show).toHaveBeenCalledTimes(1);
    expect(show).toHaveBeenCalledWith({
      title: "Agent stopped",
      body: "Planning stopped: The process exited unexpectedly",
      target: { kind: "agent", agent_id: PLANNING },
    });
  });
});
