import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import i18n from "../../i18n";
import type { Settings as SettingsData } from "../../ipc/generated/Settings";
import { loadM3, send } from "../../notifications/world.testing";
import { appStore } from "../../store/store";
import Settings, { type SettingsTab } from "./Settings";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));

type Call = { method: string; params: { settings?: SettingsData } };

/** formationd: echoes settings, answers the other commands with `null`. */
function answer(cmd: string, args?: Call) {
  if (cmd === "formationd_call" && args?.method === "settings_update") return Promise.resolve(args.params.settings);
  return Promise.resolve(null);
}

function lastCall(): Call {
  const calls = invoke.mock.calls.filter(([cmd]) => cmd === "formationd_call");
  return calls[calls.length - 1]?.[1] as Call;
}

function open(tab: SettingsTab) {
  render(<Settings initialTab={tab} />);
}

beforeEach(() => {
  invoke.mockReset();
  invoke.mockImplementation(answer);
  loadM3();
});

afterEach(async () => {
  await i18n.changeLanguage("en");
});

describe("Settings window (SC-018)", () => {
  it("waits for formationd before showing settings", () => {
    appStore.setState({ mirror: null, connected: false });
    open("general");
    expect(screen.getByText("Connecting to formationd…")).toBeInTheDocument();
    expect(screen.queryByRole("radiogroup")).toBeNull();
  });

  it("switches tabs with the tab list", () => {
    open("general");
    expect(screen.getByRole("tab", { name: "General" })).toHaveAttribute("aria-selected", "true");
    fireEvent.mouseDown(screen.getByRole("tab", { name: "Team" }));
    expect(screen.getByRole("tab", { name: "Team" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByText("Dohyeon's team")).toBeInTheDocument();
  });

  it("general: follows macOS by default and saves a language choice", async () => {
    open("general");
    const group = screen.getByRole("radiogroup", { name: "Language" });
    expect(within(group).getByRole("radio", { name: "Follow macOS" })).toHaveAttribute("aria-checked", "true");

    await act(async () => fireEvent.click(within(group).getByRole("radio", { name: "한국어" })));
    expect(lastCall()).toEqual({
      method: "settings_update",
      params: { settings: { language: "ko", notifications: { enabled: true, muted_until: null } } },
    });
    expect(i18n.language).toBe("ko");
  });

  it("general: disables changes while formationd is unreachable", () => {
    act(() => appStore.getState().setConnected(false));
    open("general");
    expect(screen.getByText(/Lost the connection to formationd/)).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "English" })).toBeDisabled();
  });

  it("general: shows an error when saving fails", async () => {
    invoke.mockRejectedValue({ code: "rpc", message: "x", rpc_code: -32000, data: null });
    open("general");
    await act(async () => fireEvent.click(screen.getByRole("radio", { name: "English" })));
    expect(screen.getByRole("alert")).toHaveTextContent("Couldn't save the change");
  });

  it("harnesses: install and sign-in state, the problem, and Check again", async () => {
    open("harnesses");
    const rows = screen.getAllByRole("listitem");
    expect(rows.map((r) => r.getAttribute("data-state"))).toEqual(["signed-in", "signed-in"]);
    expect(rows[0]).toHaveTextContent("Claude Code~/.local/bin/claude · 2.1.319Signed in");

    act(() =>
      send({
        type: "harness.update",
        payload: {
          harnesses: [
            { harness: "claude_code", installed: true, signed_in: false, version: "2.1.319", path: "/c", problem: "Run `claude` and sign in." },
            { harness: "codex", installed: false, signed_in: null, version: null, path: null, problem: null },
          ],
        },
      }),
    );
    const updated = screen.getAllByRole("listitem");
    expect(updated[0]).toHaveAttribute("data-state", "sign-in-needed");
    expect(updated[0]).toHaveTextContent("Run `claude` and sign in.");
    expect(updated[1]).toHaveAttribute("data-state", "not-installed");
    expect(updated[1]).toHaveTextContent("Not found");

    let reply: (v: unknown) => void = () => {};
    invoke.mockImplementationOnce(() => new Promise((resolve) => (reply = resolve)));
    fireEvent.click(screen.getByRole("button", { name: "Check again" }));
    expect(lastCall()).toEqual({ method: "harness_check", params: {} });
    expect(screen.getByRole("button", { name: "Checking…" })).toBeDisabled();
    await act(async () => reply([]));
    expect(screen.getByRole("button", { name: "Check again" })).toBeEnabled();
  });

  it("notifications: turns alerts off", async () => {
    open("notifications");
    const toggle = screen.getByRole("switch", { name: "Notify me when I'm needed" });
    expect(toggle).toHaveAttribute("aria-checked", "true");
    await act(async () => fireEvent.click(toggle));
    expect(lastCall().params.settings?.notifications).toEqual({ enabled: false, muted_until: null });
  });

  it("notifications: pauses for an hour and resumes", async () => {
    open("notifications");
    const before = Date.now();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "For 1 hour" })));
    const settings = lastCall().params.settings!;
    expect(Date.parse(settings.notifications.muted_until!)).toBeGreaterThanOrEqual(before + 3_600_000);

    act(() => send({ type: "settings.update", payload: { settings } }));
    expect(screen.getByText(/^Paused until/)).toBeInTheDocument();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Resume" })));
    expect(lastCall().params.settings?.notifications.muted_until).toBeNull();
  });

  it("notifications: pauses until tomorrow morning", async () => {
    open("notifications");
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Until tomorrow" })));
    const until = new Date(lastCall().params.settings!.notifications.muted_until!);
    expect(until.getHours()).toBe(9);
    expect(until.getTime()).toBeGreaterThan(Date.now());
  });

  it("team: name, role, members", () => {
    open("team");
    expect(screen.getByText("Dohyeon's team")).toBeInTheDocument();
    expect(screen.getByText("Team creator")).toBeInTheDocument();
    const members = within(screen.getByRole("region", { name: "Members" })).getAllByRole("listitem");
    expect(members).toHaveLength(4);
    expect(members[2]).toHaveAttribute("data-online", "false");
    expect(members[2]).toHaveTextContent("Jiwoo @jiwooOffline");
  });

  it("team: leaving asks first; Cancel does nothing", () => {
    open("team");
    fireEvent.click(screen.getByRole("button", { name: "Leave team" }));
    const alert = screen.getByRole("alertdialog", { name: "Leave the team?" });
    fireEvent.click(within(alert).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
  });

  it("team: confirming leaves and takes a new snapshot", async () => {
    const requestSnapshot = vi.fn();
    appStore.setState({ requestSnapshot });
    open("team");
    fireEvent.click(screen.getByRole("button", { name: "Leave team" }));
    const alert = screen.getByRole("alertdialog");
    await act(async () => fireEvent.click(within(alert).getByRole("button", { name: "Leave team" })));
    expect(lastCall()).toEqual({ method: "team_leave", params: {} });
    expect(requestSnapshot).toHaveBeenCalled();
  });

  it("team: without a team there is nothing to leave", () => {
    appStore.setState((s) => ({ mirror: s.mirror && { ...s.mirror, team: null } }));
    open("team");
    expect(screen.getByText("You're not in a team.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Leave team" })).toBeNull();
  });
});
