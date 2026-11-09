import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import approvalRaised from "../../../tools/fake-formationd/fixtures/scripts/approval-raised.json";
import inboxCleared from "../../../tools/fake-formationd/fixtures/scripts/inbox-cleared.json";
import "./i18n";
import type { Settings } from "./ipc/generated/Settings";
import MenubarPanel from "./MenubarPanel";
import { loadM3, play, send } from "./notifications/world.testing";
import { appStore } from "./store/store";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));

const MINSU = "01930000-0000-7000-8000-000000000002";
const SEOYEON = "01930000-0000-7000-8000-000000000004";

function panel() {
  return screen.getByRole("main", { name: "Formation menu bar panel" });
}

/** M3's clock: the approval came in 3 minutes ago, as in the mockup. */
const NOW = Date.parse("2026-11-09T01:53:30Z");

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue(null);
  vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
  vi.setSystemTime(NOW);
  loadM3();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("menu bar panel (SC-002)", () => {
  it("default: summary, needs-you items first, then my agents", () => {
    render(<MenubarPanel />);
    expect(panel()).toHaveAttribute("data-state", "default");
    expect(screen.getByText("5 agents · 2 need you")).toBeInTheDocument();

    const needs = within(screen.getByRole("region", { name: "Needs you" })).getAllByRole("button");
    expect(needs.map((b) => b.textContent)).toEqual([
      "Research · no activity for 14 minutesStalled14m",
      "Jiwoo · When will the payment refactor be done?Question for you23m",
    ]);
    expect(within(needs[0]).getByText("14m")).toHaveAttribute("dateTime", "2026-11-09T01:39:00Z");

    const agents = within(screen.getByRole("region", { name: "My agents" })).getAllByRole("listitem");
    expect(agents).toHaveLength(5);
    expect(agents[2]).toHaveAttribute("data-state", "stalled");
    expect(agents[2]).toHaveTextContent("ResearchStalled");
    // Connected detected agents stay out of the panel, as in the mockup.
    expect(screen.queryByText("claude · shared-web")).toBeNull();
  });

  it("lists a new approval as needing me", () => {
    render(<MenubarPanel />);
    act(() => play(approvalRaised));
    expect(screen.getByText("5 agents · 3 need you")).toBeInTheDocument();
    const first = within(screen.getByRole("region", { name: "Needs you" })).getAllByRole("button")[0];
    expect(first).toHaveTextContent("Minsu → Backend · npm test -- auth/token.spec.tsWaiting for approval3m");
  });

  it("keeps each row's age current, minute by minute", () => {
    render(<MenubarPanel />);
    act(() => play(approvalRaised));
    const region = () => within(screen.getByRole("region", { name: "Needs you" }));
    expect(region().getByText("3m")).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(60_000));
    expect(region().getByText("4m")).toBeInTheDocument();
    expect(region().getByText("15m")).toBeInTheDocument();
    // An hour on, every row reads in hours.
    act(() => vi.advanceTimersByTime(60 * 60_000));
    expect(region().getAllByText("1h")).toHaveLength(3);
  });

  it("shows the tray icon's dot only while something needs me", () => {
    render(<MenubarPanel />);
    expect(invoke).toHaveBeenCalledWith("set_tray_attention", { attention: true });
    invoke.mockClear();
    // A change that leaves something needing me sends nothing.
    act(() => vi.advanceTimersByTime(60_000));
    expect(invoke).not.toHaveBeenCalled();
    act(() => play(inboxCleared));
    expect(invoke).toHaveBeenLastCalledWith("set_tray_attention", { attention: false });
  });

  it("opens the main window on the clicked item or agent", () => {
    render(<MenubarPanel />);
    fireEvent.click(screen.getByRole("button", { name: /When will the payment refactor/ }));
    expect(invoke).toHaveBeenLastCalledWith("focus_item", {
      target: { kind: "question", question_id: "01930000-0000-7000-8000-000000000305" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^Backend/ }));
    expect(invoke).toHaveBeenLastCalledWith("focus_item", {
      target: { kind: "agent", agent_id: "01930000-0000-7000-8000-000000000101" },
    });
  });

  it("all-clear: nothing needs me", () => {
    render(<MenubarPanel />);
    act(() => play(inboxCleared));
    expect(panel()).toHaveAttribute("data-state", "all-clear");
    expect(screen.getByText("Everything is running")).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Needs you" })).toBeNull();
  });

  it("offline: formationd unreachable, local agents still listed", () => {
    act(() => appStore.getState().setConnected(false));
    render(<MenubarPanel />);
    expect(panel()).toHaveAttribute("data-state", "offline");
    expect(panel()).toHaveAttribute("data-offline", "formationd");
    expect(screen.getByText(/Can't reach formationd/)).toBeInTheDocument();
    expect(screen.getByText("Backend")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Pause alerts 1 hr" })).toBeNull();
    // Not a live region: the main window's banner owns that announcement.
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("offline: every teammate is offline", () => {
    render(<MenubarPanel />);
    act(() => {
      for (const member_id of [MINSU, SEOYEON]) {
        send({ type: "peer.presence", payload: { member_id, online: false, last_seen_at: null, agents: [] } });
      }
    });
    expect(panel()).toHaveAttribute("data-state", "offline");
    expect(panel()).toHaveAttribute("data-offline", "team");
    expect(screen.getByText("Can't reach the team · local agents keep running")).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Needs you" })).toBeInTheDocument();
  });

  it("pauses alerts for an hour, then resumes them", async () => {
    render(<MenubarPanel />);
    const before = Date.now();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Pause alerts 1 hr" })));
    const [cmd, args] = invoke.mock.lastCall as [string, { method: string; params: { settings: Settings } }];
    expect(cmd).toBe("formationd_call");
    expect(args.method).toBe("settings_update");
    const until = Date.parse(args.params.settings.notifications.muted_until ?? "");
    expect(until).toBeGreaterThanOrEqual(before + 60 * 60 * 1000);
    expect(args.params.settings.notifications.enabled).toBe(true);

    act(() => send({ type: "settings.update", payload: { settings: args.params.settings } }));
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Resume alerts" })));
    const [, resumed] = invoke.mock.lastCall as [string, { params: { settings: Settings } }];
    expect(resumed.params.settings.notifications.muted_until).toBeNull();
  });

  it("shows an error when pausing fails", async () => {
    invoke.mockRejectedValue({ code: "disconnected", message: "x", rpc_code: null, data: null });
    render(<MenubarPanel />);
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Pause alerts 1 hr" })));
    expect(screen.getByRole("alert")).toHaveTextContent("Couldn't change alerts");
  });

  it("opens Formation, Settings, and quits", () => {
    render(<MenubarPanel />);
    fireEvent.click(screen.getByRole("button", { name: "Open Formation" }));
    expect(invoke).toHaveBeenLastCalledWith("show_main_window");
    fireEvent.click(screen.getByRole("button", { name: "Settings…" }));
    expect(invoke).toHaveBeenLastCalledWith("show_settings_window");
    fireEvent.click(screen.getByRole("button", { name: "Quit Formation" }));
    expect(invoke).toHaveBeenLastCalledWith("quit_app");
  });
});
