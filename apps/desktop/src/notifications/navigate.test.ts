import { beforeEach, describe, expect, it, vi } from "vitest";
import approvalRaised from "../../../../tools/fake-formationd/fixtures/scripts/approval-raised.json";
import { appStore } from "../store/store";
import { uiStore } from "../store/ui";
import { destinationFor, NAVIGATE, openTarget, startNavigateListener } from "./navigate";
import { loadM3, play } from "./world.testing";

const listeners = vi.hoisted(() => new Map<string, (e: { payload: unknown }) => void>());
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: { payload: unknown }) => void) => {
    listeners.set(name, cb);
    return Promise.resolve(() => listeners.delete(name));
  },
}));

const BACKEND = "01930000-0000-7000-8000-000000000101";
const APPROVAL = "01930000-0000-7000-8000-000000000501";

beforeEach(() => {
  loadM3();
  uiStore.setState({ view: { kind: "home" }, sheet: null });
});

describe("opening an item in the main window", () => {
  it("an agent opens its conversation", () => {
    expect(destinationFor({ kind: "agent", agent_id: BACKEND }, appStore.getState().mirror)).toEqual({
      view: { kind: "agent", agentId: BACKEND },
      sheet: null,
    });
  });

  it("an approval opens its agent's conversation with the approval sheet", () => {
    play(approvalRaised);
    openTarget({ kind: "approval", approval_id: APPROVAL });
    expect(uiStore.getState().view).toEqual({ kind: "agent", agentId: BACKEND });
    expect(uiStore.getState().sheet).toEqual({ kind: "approval", approvalId: APPROVAL });
  });

  it("an approval not mirrored yet still opens its sheet", () => {
    expect(destinationFor({ kind: "approval", approval_id: APPROVAL }, appStore.getState().mirror)).toEqual({
      view: null,
      sheet: { kind: "approval", approvalId: APPROVAL },
    });
  });

  it("a question sent to me opens the Inbox", () => {
    expect(
      destinationFor(
        { kind: "question", question_id: "01930000-0000-7000-8000-000000000305" },
        appStore.getState().mirror,
      ).view,
    ).toEqual({ kind: "inbox" });
  });

  it("a request opens its status sheet over the current view", () => {
    expect(destinationFor({ kind: "request", request_id: "r1" }, appStore.getState().mirror)).toEqual({
      view: null,
      sheet: { kind: "request-status", requestId: "r1" },
    });
  });

  it("an unknown schedule falls back to the Inbox", () => {
    expect(destinationFor({ kind: "schedule", schedule_id: "s-missing" }, appStore.getState().mirror)).toEqual({
      view: { kind: "inbox" },
      sheet: null,
    });
  });

  it("follows app://navigate events and closes an unrelated sheet", async () => {
    uiStore.setState({ sheet: { kind: "connect" } });
    const stop = startNavigateListener();
    await Promise.resolve();
    listeners.get(NAVIGATE)?.({ payload: { kind: "agent", agent_id: BACKEND } });
    expect(uiStore.getState().view).toEqual({ kind: "agent", agentId: BACKEND });
    expect(uiStore.getState().sheet).toBeNull();
    stop();
    expect(listeners.has(NAVIGATE)).toBe(false);
  });
});
