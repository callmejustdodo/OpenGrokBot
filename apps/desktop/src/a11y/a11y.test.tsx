// NFR-009: every screen of the M3 world, checked for controls VoiceOver would
// read without a name, and for the landmarks and live regions it navigates by.
import { act, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import reactions from "../../../../tools/fake-formationd/fixtures/reactions.json";
import "../i18n";
import App from "../App";
import MenubarPanel from "../MenubarPanel";
import { appStore } from "../store/store";
import { type Sheet, type View, uiStore } from "../store/ui";
import {
  BACKEND,
  M3_NOW,
  MINSU,
  MINSU_APPROVAL,
  MY_REQUEST,
  QA,
  loadWorld,
} from "../views/approval/m3World";
import Settings, { type SettingsTab } from "../views/settings/Settings";
import { unnamedControls } from "./names";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));

/** formationd's answers for the screens that load on open; `null` for the rest. */
function answer(cmd: string, method?: string): unknown {
  if (cmd !== "formationd_call") return null;
  if (method === "history_list") return [];
  if (method === "agents_detect") return reactions.agents_detect[0].result;
  if (method === "agent_files_read") {
    return {
      agent_id: BACKEND,
      revision: appStore.getState().mirror?.agents[BACKEND]?.files_revision,
      files: [
        { path: "AGENTS.md", content: "# Backend agent" },
        { path: ".claude/skills/api-style/SKILL.md", content: "x" },
      ],
    };
  }
  if (method === "share_preview") {
    const view = appStore.getState().mirror?.agents[BACKEND];
    return { view: { ...view, owner: MINSU, agent_id: BACKEND, accepts_requests: true }, visibility: "answer_excerpts" };
  }
  return null;
}

// Radix measures some popovers; jsdom has no ResizeObserver.
globalThis.ResizeObserver ??= class {
  observe() {}
  unobserve() {}
  disconnect() {}
};

beforeEach(() => {
  invoke.mockReset();
  invoke.mockImplementation((cmd: string, args?: { method: string }) => Promise.resolve(answer(cmd, args?.method)));
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(M3_NOW);
  appStore.setState({ connected: true, stale: false });
  loadWorld(["approval-raised", "backend-permission", "request-status"]);
  uiStore.setState({ view: { kind: "home" }, sheet: null, panelOpen: true });
});

/** Lets formationd's answers arrive, so loaded screens are checked, not spinners. */
async function settle() {
  await act(async () => {});
}

const VIEWS: View[] = [
  { kind: "home" },
  { kind: "agent", agentId: BACKEND },
  { kind: "teammate-agent", owner: MINSU, agentId: QA },
  { kind: "person", memberId: MINSU },
  { kind: "new-question" },
  { kind: "inbox" },
  { kind: "team" },
  { kind: "member", memberId: MINSU },
  { kind: "history" },
  { kind: "rules" },
];

const SHEETS: Sheet[] = [
  { kind: "new-agent" },
  { kind: "instructions", agentId: BACKEND },
  { kind: "schedules", agentId: BACKEND },
  { kind: "share-preview", agentId: BACKEND },
  { kind: "connect" },
  { kind: "approval", approvalId: MINSU_APPROVAL },
  { kind: "work-request", owner: MINSU, agentId: QA, fromQuestion: null },
  { kind: "request-status", requestId: MY_REQUEST },
];

const TABS: SettingsTab[] = ["general", "harnesses", "notifications", "team"];

describe("accessible names (NFR-009)", () => {
  it.each(VIEWS.map((v) => [v.kind, v] as const))("main window, %s view", async (_, view) => {
    act(() => uiStore.setState({ view }));
    render(<App />);
    await settle();
    expect(unnamedControls()).toEqual([]);
  });

  it.each(SHEETS.map((s) => [s.kind, s] as const))("%s sheet", async (_, sheet) => {
    act(() => uiStore.setState({ sheet }));
    render(<App />);
    await settle();
    expect(screen.getByRole("dialog")).toHaveAccessibleName();
    expect(unnamedControls()).toEqual([]);
  });

  it.each(TABS)("settings window, %s tab", (tab) => {
    render(<Settings initialTab={tab} />);
    expect(unnamedControls()).toEqual([]);
  });

  it("menu bar panel", () => {
    render(<MenubarPanel />);
    expect(unnamedControls()).toEqual([]);
  });
});

describe("landmarks and live regions (NFR-009)", () => {
  it("main window: navigation, main and complementary landmarks", () => {
    act(() => uiStore.setState({ view: { kind: "agent", agentId: BACKEND } }));
    render(<App />);
    expect(screen.getByRole("navigation", { name: "Sidebar" })).toBeInTheDocument();
    expect(screen.getByRole("main")).toBeInTheDocument();
    expect(screen.getByRole("complementary")).toHaveAccessibleName();
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
  });

  it("the selected chat is marked current", () => {
    act(() => uiStore.setState({ view: { kind: "agent", agentId: BACKEND } }));
    render(<App />);
    const chats = screen.getByRole("navigation", { name: "Conversations" });
    expect(chats.querySelectorAll("[aria-current]")).toHaveLength(1);
  });

  it("a conversation's messages are a log, announced politely", () => {
    act(() => uiStore.setState({ view: { kind: "agent", agentId: BACKEND } }));
    render(<App />);
    expect(screen.getByRole("log")).toHaveAccessibleName("Messages");
  });

  it("the connection banner is a polite status", () => {
    appStore.setState({ connected: false });
    render(<App />);
    const banner = screen.getByRole("status");
    expect(banner).not.toHaveAttribute("aria-live", "assertive");
  });
});
