import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import m3 from "../../../../../tools/fake-formationd/fixtures/worlds/m3.json";
import "../../i18n";
import type { Event } from "../../ipc/generated/Event";
import type { Schedule } from "../../ipc/generated/Schedule";
import type { Snapshot } from "../../ipc/generated/Snapshot";
import { appStore } from "../../store/store";
import { uiStore } from "../../store/ui";
import SheetHost from "../SheetHost";
import SchedulesPanelSection from "./SchedulesPanelSection";

// Radix Switch measures itself; jsdom has no ResizeObserver.
globalThis.ResizeObserver ??= class {
  observe() {}
  unobserve() {}
  disconnect() {}
};

const call = vi.fn();
vi.mock("../../ipc/transport", () => ({
  tauriTransport: { call: (...a: unknown[]) => call(...a), onEvent: () => () => {}, onStatus: () => () => {} },
}));

const snapshot = m3 as unknown as Snapshot;
const SUPPORT = "01930000-0000-7000-8000-000000000104";
const BACKEND = "01930000-0000-7000-8000-000000000101";
const SCHEDULE = "01930000-0000-7000-8000-000000000801";
const base = snapshot.schedules[0];

let seq = snapshot.seq;
function receive(event: Event) {
  seq += 1;
  act(() => appStore.getState().receive({ seq, at: "2026-11-09T02:00:00Z", event }));
}

function upsert(schedule: Schedule) {
  receive({ type: "schedule.update", payload: { change: "upsert", schedule } });
}

function renderPanelAndSheet(agentId: string) {
  return render(
    <>
      <SchedulesPanelSection agentId={agentId} />
      <SheetHost />
    </>,
  );
}

function panel() {
  return screen.getByRole("region", { name: "Schedules" });
}

beforeEach(() => {
  call.mockReset();
  call.mockImplementation((method: string) =>
    method === "schedule_run_list" ? Promise.resolve([]) : Promise.reject(new Error(method)),
  );
  seq = snapshot.seq;
  appStore.setState({ mirror: null, connected: true, stale: false, pending: {} });
  appStore.getState().loadSnapshot(snapshot);
  uiStore.setState({ view: { kind: "agent", agentId: SUPPORT }, sheet: null, panelOpen: true });
});

describe("SC-007 panel", () => {
  it("panel: shows the rule in words, the time, the next run and the last result", () => {
    upsert({ ...base, last_run: { ...base.last_run!, started_at: "2026-11-09T00:00:00Z", result: "completed" } });
    renderPanelAndSheet(SUPPORT);
    const card = panel();
    expect(card).toHaveAttribute("data-state", "list");
    expect(within(card).getByText("Summarize yesterday's customer tickets")).toBeInTheDocument();
    expect(within(card).getByText(/Weekdays · 9:00\sAM · Next: /)).toBeInTheDocument();
    expect(within(card).getByText(/completed$/)).toHaveAttribute("data-run", "completed");
    expect(within(card).getByRole("button", { name: "Add" })).toBeInTheDocument();
  });

  it("shows other rules and a paused schedule", () => {
    upsert({ ...base, rule: { kind: "weekly", weekday: "fri" }, time: "17:30:00", paused: true });
    upsert({ ...base, id: "s2", rule: { kind: "every_hours", hours: 4 }, time: "08:00:00", last_run: null });
    renderPanelAndSheet(SUPPORT);
    const card = panel();
    expect(within(card).getByText(/Weekly on Friday · 5:30\sPM/)).toBeInTheDocument();
    expect(within(card).getByText("Paused")).toHaveAttribute("data-state", "paused");
    expect(within(card).getByText(/Every 4 hours · from 8:00\sAM/)).toBeInTheDocument();
  });

  it("missed: says the run was missed and ran on wake", () => {
    renderPanelAndSheet(SUPPORT);
    const line = within(panel()).getByText(/^Missed → ran on wake at \d{1,2}:\d{2}\s[AP]M$/);
    expect(line).toHaveAttribute("data-run", "missed");
  });

  it("failed: shows the failed run with a retry hint", () => {
    upsert({ ...base, last_run: { ...base.last_run!, result: "failed" } });
    renderPanelAndSheet(SUPPORT);
    const card = panel();
    expect(within(card).getByRole("alert")).toHaveTextContent(/failed$/);
    expect(within(card).getByText(/runs again at the next scheduled time/)).toBeInTheDocument();
  });

  it("failed: a schedule_failed alert shows as the failed state, until it is resolved", () => {
    const item = {
      id: "01930000-0000-7000-8000-000000000699",
      kind: "schedule_failed" as const,
      target: { kind: "schedule" as const, schedule_id: SCHEDULE },
      subject: "Support · couldn't reach the ticket system",
      at: "2026-11-09T00:05:00Z",
      resolved_at: null,
    };
    receive({ type: "notify", payload: { item, alert: true } });
    renderPanelAndSheet(SUPPORT);
    expect(within(panel()).getByRole("alert")).toHaveTextContent("Failed: Support · couldn't reach the ticket system");
    expect(screen.getByRole("listitem")).toHaveAttribute("data-run", "failed");
    receive({ type: "notify", payload: { item: { ...item, resolved_at: "2026-11-09T00:10:00Z" }, alert: false } });
    expect(within(panel()).queryByRole("alert")).toBeNull();
    expect(screen.getByRole("listitem")).toHaveAttribute("data-run", "missed");
  });

  it("empty: says there are no schedules and offers Add, which opens a new schedule", async () => {
    renderPanelAndSheet(BACKEND);
    const card = panel();
    expect(card).toHaveAttribute("data-state", "empty");
    expect(within(card).getByText("No schedules")).toBeInTheDocument();
    fireEvent.click(within(card).getByRole("button", { name: "Add" }));
    expect(uiStore.getState().sheet).toEqual({ kind: "schedules", agentId: BACKEND });
    expect(await screen.findByRole("form", { name: "New schedule" })).toBeInTheDocument();
  });
});

describe("SC-007 sheet", () => {
  it("creates a schedule with schedule_create and closes", async () => {
    let resolve: (s: Schedule) => void = () => {};
    call.mockImplementation((method: string) =>
      method === "schedule_create" ? new Promise<Schedule>((r) => (resolve = r)) : Promise.reject(new Error(method)),
    );
    renderPanelAndSheet(BACKEND);
    fireEvent.click(within(panel()).getByRole("button", { name: "Add" }));
    const dialog = await screen.findByRole("dialog", { name: "Schedules · Backend" });

    // The task is required.
    fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    expect(within(dialog).getByText("Write what the agent should do.")).toBeInTheDocument();
    expect(call).not.toHaveBeenCalled();

    fireEvent.change(within(dialog).getByLabelText("Task"), { target: { value: "  Run the nightly checks  " } });
    fireEvent.click(within(dialog).getByRole("radio", { name: "Weekly" }));
    fireEvent.click(within(dialog).getByRole("radio", { name: "Wednesday" }));
    fireEvent.change(within(dialog).getByLabelText("Time"), { target: { value: "18:15" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));

    expect(call).toHaveBeenCalledWith("schedule_create", {
      agent_id: BACKEND,
      prompt: "Run the nightly checks",
      rule: { kind: "weekly", weekday: "wed" },
      time: "18:15:00",
    });
    // Pending at once.
    expect(within(dialog).getByRole("button", { name: "Saving…" })).toBeDisabled();
    await act(async () => resolve({ ...base, agent_id: BACKEND }));
    expect(uiStore.getState().sheet).toBeNull();
  });

  it("creates an every-N-hours schedule with ⌘S", async () => {
    call.mockImplementation((method: string) =>
      method === "schedule_create" ? Promise.resolve(base) : Promise.reject(new Error(method)),
    );
    act(() => uiStore.getState().openSheet({ kind: "schedules", agentId: BACKEND }));
    render(<SheetHost />);
    const form = await screen.findByRole("form", { name: "New schedule" });
    fireEvent.change(within(form).getByLabelText("Task"), { target: { value: "Check the queue" } });
    fireEvent.click(within(form).getByRole("radio", { name: "Every N hours" }));
    fireEvent.change(within(form).getByLabelText("Hours between runs"), { target: { value: "30" } });
    fireEvent.keyDown(within(form).getByLabelText("Task"), { key: "s", metaKey: true });
    expect(within(form).getByText("Use a whole number from 1 to 24.")).toBeInTheDocument();
    fireEvent.change(within(form).getByLabelText("Hours between runs"), { target: { value: "6" } });
    fireEvent.keyDown(within(form).getByLabelText("Task"), { key: "s", metaKey: true });
    expect(call).toHaveBeenCalledWith("schedule_create", {
      agent_id: BACKEND,
      prompt: "Check the queue",
      rule: { kind: "every_hours", hours: 6 },
      time: "09:00:00",
    });
    await waitFor(() => expect(uiStore.getState().sheet).toBeNull());
  });

  it("edits a schedule with schedule_update, sending only what changed, and shows its runs", async () => {
    const runs = [
      { ...base.last_run!, id: "r2" },
      { ...base.last_run!, id: "r1", planned_at: "2026-11-06T00:00:00Z", result: "failed" as const },
    ];
    call.mockImplementation((method: string) => {
      if (method === "schedule_run_list") return Promise.resolve(runs);
      if (method === "schedule_update") return Promise.resolve(base);
      return Promise.reject(new Error(method));
    });
    renderPanelAndSheet(SUPPORT);
    fireEvent.click(
      within(panel()).getByRole("button", { name: "Edit schedule: Summarize yesterday's customer tickets" }),
    );
    const form = await screen.findByRole("form", { name: "Edit schedule" });
    expect(within(form).getByLabelText("Task")).toHaveValue("Summarize yesterday's customer tickets");
    expect(within(form).getByRole("radio", { name: "Weekdays" })).toHaveAttribute("data-state", "on");
    expect(within(form).getByLabelText("Time")).toHaveValue("09:00");
    expect(call).toHaveBeenCalledWith("schedule_run_list", { schedule_id: SCHEDULE, before: null, limit: 10 });
    const history = await within(form).findByRole("list", { name: "Recent runs" });
    expect(within(history).getAllByRole("listitem")).toHaveLength(2);
    expect(within(history).getAllByRole("listitem")[0]).toHaveTextContent(/Missed → ran on wake at/);
    expect(within(history).getAllByRole("listitem")[1]).toHaveAttribute("data-run", "failed");

    fireEvent.click(within(form).getByRole("radio", { name: "Daily" }));
    fireEvent.click(within(form).getByRole("switch", { name: "Paused" }));
    fireEvent.click(within(form).getByRole("button", { name: "Save" }));
    expect(call).toHaveBeenCalledWith("schedule_update", {
      schedule_id: SCHEDULE,
      prompt: null,
      rule: { kind: "daily" },
      time: null,
      paused: true,
    });
    await waitFor(() => expect(uiStore.getState().sheet).toBeNull());
  });

  it("shows a save error and keeps the edits", async () => {
    call.mockImplementation((method: string) => {
      if (method === "schedule_run_list") return Promise.resolve([]);
      return Promise.reject({ code: "rpc", message: "bad", rpc_code: -32602, data: { code: "invalid_params", message: "" } });
    });
    renderPanelAndSheet(SUPPORT);
    fireEvent.click(within(panel()).getByRole("button", { name: /^Edit schedule/ }));
    const form = await screen.findByRole("form", { name: "Edit schedule" });
    expect(await within(form).findByText("No runs yet")).toBeInTheDocument();
    fireEvent.change(within(form).getByLabelText("Task"), { target: { value: "Summarize tickets" } });
    fireEvent.click(within(form).getByRole("button", { name: "Save" }));
    expect(await within(form).findByRole("alert")).toHaveTextContent("Check the task, repeat and time");
    expect(within(form).getByLabelText("Task")).toHaveValue("Summarize tickets");
    expect(uiStore.getState().sheet).not.toBeNull();
  });

  it("deletes only after confirming", async () => {
    call.mockImplementation((method: string) => {
      if (method === "schedule_run_list") return Promise.resolve([]);
      if (method === "schedule_delete") return Promise.resolve(null);
      return Promise.reject(new Error(method));
    });
    renderPanelAndSheet(SUPPORT);
    fireEvent.click(within(panel()).getByRole("button", { name: /^Edit schedule/ }));
    const form = await screen.findByRole("form", { name: "Edit schedule" });
    fireEvent.click(within(form).getByRole("button", { name: "Delete" }));
    const alert = await screen.findByRole("alertdialog", { name: "Delete this schedule?" });
    fireEvent.click(within(alert).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(call).not.toHaveBeenCalledWith("schedule_delete", expect.anything());

    fireEvent.click(within(form).getByRole("button", { name: "Delete" }));
    const again = await screen.findByRole("alertdialog");
    fireEvent.click(within(again).getByRole("button", { name: "Delete schedule" }));
    expect(call).toHaveBeenCalledWith("schedule_delete", { schedule_id: SCHEDULE });
    await waitFor(() => expect(uiStore.getState().sheet).toBeNull());
  });

  it("lists the agent's schedules when opened without a focus, and Esc closes", async () => {
    act(() => uiStore.getState().openSheet({ kind: "schedules", agentId: SUPPORT }));
    render(<SheetHost />);
    const dialog = await screen.findByRole("dialog", { name: "Schedules · Support" });
    const list = within(dialog).getByRole("list", { name: "Schedules" });
    fireEvent.click(within(list).getByRole("button", { name: /Summarize yesterday's customer tickets/ }));
    expect(await screen.findByRole("form", { name: "Edit schedule" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "All schedules" }));
    expect(within(dialog).getByRole("button", { name: "New schedule" })).toBeInTheDocument();
    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => expect(uiStore.getState().sheet).toBeNull());
  });
});
