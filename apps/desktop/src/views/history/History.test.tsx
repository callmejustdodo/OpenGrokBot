import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import m3 from "../../../../../tools/fake-formationd/fixtures/worlds/m3.json";
import "../../i18n";
import type { Activity } from "../../ipc/generated/Activity";
import type { Snapshot } from "../../ipc/generated/Snapshot";
import { appStore } from "../../store/store";

// fake-formationd does not simulate history_*, so the transport is mocked.
const call = vi.fn();
vi.mock("../../ipc/transport", () => ({ tauriTransport: { call: (...a: unknown[]) => call(...a) } }));

const { default: History } = await import("./History");

const ME = "01930000-0000-7000-8000-000000000001";
const MINSU = "01930000-0000-7000-8000-000000000002";
const JIWOO = "01930000-0000-7000-8000-000000000003";
const BACKEND = "01930000-0000-7000-8000-000000000101";
const SUPPORT = "01930000-0000-7000-8000-000000000104";
const AUTH = "01930000-0000-7000-8000-000000000201";
const QUESTION = "01930000-0000-7000-8000-000000000301";
const JIWOO_QUESTION = "01930000-0000-7000-8000-000000000305";
const REQUEST = "01930000-0000-7000-8000-000000000401";

const activities: Activity[] = [
  {
    id: "a-question",
    kind: "question_asked",
    actor: ME,
    agent_id: AUTH,
    target: { kind: "question", question_id: QUESTION },
    at: "2026-11-09T01:42:00Z",
    summary: "How far is the login work?",
  },
  {
    id: "a-request",
    kind: "request_received",
    actor: MINSU,
    agent_id: BACKEND,
    target: { kind: "request", request_id: REQUEST },
    at: "2026-11-09T01:53:00Z",
    summary: "Add token expiry tests",
  },
  {
    id: "a-approval",
    kind: "approval_decided",
    actor: null,
    agent_id: BACKEND,
    target: { kind: "approval", approval_id: "01930000-0000-7000-8000-000000000501" },
    at: "2026-11-09T01:51:00Z",
    summary: "npm test -- auth/token.spec.ts",
  },
  {
    id: "a-result",
    kind: "schedule_ran",
    actor: null,
    agent_id: SUPPORT,
    target: null,
    at: "2026-11-09T00:00:00Z",
    summary: "Summarize yesterday's tickets",
  },
];

const snapshot = m3 as unknown as Snapshot;

function historyReturns(items: Activity[]) {
  call.mockImplementation((method: string) => (method === "history_list" ? Promise.resolve(items) : Promise.resolve(null)));
}

async function renderLoaded() {
  render(<History />);
  return screen.findByRole("list");
}

beforeEach(() => {
  call.mockReset();
  appStore.setState({ mirror: null, connected: true, stale: false, pending: {} });
  appStore.getState().loadSnapshot(snapshot);
});

describe("History (SC-017)", () => {
  it("loads history_list on open with a pending state, then lists entries newest first (default)", async () => {
    let reply: (items: Activity[]) => void = () => {};
    call.mockImplementation(() => new Promise((resolve) => (reply = resolve)));
    render(<History />);
    expect(call).toHaveBeenCalledWith("history_list", { agent_id: null, member_id: null, before: null, limit: 200 });
    expect(screen.getByRole("status")).toHaveTextContent("Loading history");
    expect(appStore.getState().pending).toHaveProperty("history_list");
    await act(async () => reply(activities));
    const rows = within(screen.getByRole("list")).getAllByRole("listitem");
    expect(rows.map((r) => r.querySelector("b")?.textContent)).toEqual([
      "Add token expiry tests",
      "npm test -- auth/token.spec.ts",
      "How far is the login work?",
      "Summarize yesterday's tickets",
    ]);
    expect(rows[0]).toHaveTextContent("Request");
    expect(rows[0]).toHaveTextContent("Minsu → Backend · Queued");
    expect(rows[1]).toHaveTextContent("Approval");
    expect(rows[2]).toHaveTextContent("Question");
    expect(rows[2]).toHaveTextContent("Me → Auth · Answered");
    expect(rows[3]).toHaveTextContent("Result");
    expect(rows[3]).toHaveTextContent("Me → Support");
    expect(rows[0].querySelector("time")).toHaveAttribute("datetime", "2026-11-09T01:53:00Z");
  });

  it("filters by kind and by search text", async () => {
    historyReturns(activities);
    const list = await renderLoaded();
    fireEvent.click(screen.getByRole("radio", { name: "Questions" }));
    expect(within(list).getAllByRole("listitem")).toHaveLength(1);
    expect(list).toHaveTextContent("How far is the login work?");
    fireEvent.click(screen.getByRole("radio", { name: "All" }));
    fireEvent.change(screen.getByRole("searchbox", { name: "Search history" }), { target: { value: "minsu" } });
    expect(within(screen.getByRole("list")).getAllByRole("listitem")).toHaveLength(1);
    expect(screen.getByRole("list")).toHaveTextContent("Add token expiry tests");
  });

  it("shows no matches when a filter leaves nothing (empty-filter)", async () => {
    historyReturns(activities.filter((a) => a.kind !== "approval_decided"));
    await renderLoaded();
    fireEvent.click(screen.getByRole("radio", { name: "Approvals" }));
    expect(screen.queryByRole("list")).toBeNull();
    expect(screen.getByText("Nothing matches this filter")).toBeInTheDocument();
  });

  it("shows an empty state when there is no history", async () => {
    historyReturns([]);
    render(<History />);
    expect(await screen.findByText("No history yet")).toBeInTheDocument();
  });

  it("confirms before deleting this Mac's copy, then calls history_delete (delete-confirm)", async () => {
    historyReturns(activities);
    await renderLoaded();
    fireEvent.click(screen.getByRole("button", { name: "Delete Add token expiry tests" }));
    const alert = screen.getByRole("alertdialog");
    expect(alert).toHaveTextContent("Delete this entry?");
    expect(alert).toHaveTextContent("removed from this Mac's history only");
    expect(within(alert).queryByRole("button", { name: /question and answer/ })).toBeNull();
    expect(call).not.toHaveBeenCalledWith("history_delete", expect.anything());
    await act(async () => fireEvent.click(within(alert).getByRole("button", { name: "Delete" })));
    expect(call).toHaveBeenCalledWith("history_delete", { ids: ["a-request"] });
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(screen.queryByText("Add token expiry tests")).toBeNull();
    expect(within(screen.getByRole("list")).getAllByRole("listitem")).toHaveLength(3);
  });

  it("Cancel closes the confirmation without deleting", async () => {
    historyReturns(activities);
    await renderLoaded();
    fireEvent.click(screen.getByRole("button", { name: "Delete Add token expiry tests" }));
    fireEvent.click(within(screen.getByRole("alertdialog")).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(call).not.toHaveBeenCalledWith("history_delete", expect.anything());
  });

  it("deletes selected rows, and offers question_delete for questions I asked", async () => {
    historyReturns(activities);
    await renderLoaded();
    fireEvent.click(screen.getByRole("checkbox", { name: "Select How far is the login work?" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Select Summarize yesterday's tickets" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete 2 selected" }));
    const alert = screen.getByRole("alertdialog");
    expect(alert).toHaveTextContent("Delete 2 entries?");
    await act(async () =>
      fireEvent.click(within(alert).getByRole("button", { name: "Delete the question and answer too" })),
    );
    expect(call).toHaveBeenCalledWith("question_delete", { question_id: QUESTION });
    expect(call).toHaveBeenCalledWith("history_delete", { ids: ["a-question", "a-result"] });
    expect(within(screen.getByRole("list")).getAllByRole("listitem")).toHaveLength(2);
    expect(screen.queryByRole("button", { name: /selected/ })).toBeNull();
  });

  it("offers question_delete for a question asked of me", async () => {
    historyReturns([
      {
        id: "a-jiwoo",
        kind: "question_asked",
        actor: JIWOO,
        agent_id: null,
        target: { kind: "question", question_id: JIWOO_QUESTION },
        at: "2026-11-09T01:30:00Z",
        summary: "When will the payment refactor be done?",
      },
    ]);
    await renderLoaded();
    expect(screen.getByRole("listitem")).toHaveTextContent("Jiwoo → Dohyeon · Sent to owner");
    fireEvent.click(screen.getByRole("button", { name: "Delete When will the payment refactor be done?" }));
    expect(
      within(screen.getByRole("alertdialog")).getByRole("button", { name: "Delete the question and answer too" }),
    ).toBeInTheDocument();
  });

  it("keeps the confirmation open with an error when the delete fails", async () => {
    call.mockImplementation((method: string) =>
      method === "history_list"
        ? Promise.resolve(activities)
        : Promise.reject({ code: "rpc", message: "x", rpc_code: 1, data: { code: "internal", message: "x" } }),
    );
    await renderLoaded();
    fireEvent.click(screen.getByRole("button", { name: "Delete Add token expiry tests" }));
    const alert = screen.getByRole("alertdialog");
    await act(async () => fireEvent.click(within(alert).getByRole("button", { name: "Delete" })));
    expect(within(alert).getByRole("alert")).toHaveTextContent("Couldn't delete");
    expect(screen.getByText("Add token expiry tests", { selector: "li b" })).toBeInTheDocument();
  });

  it("shows an error state when history_list fails, and retries", async () => {
    call.mockRejectedValueOnce({ code: "disconnected", message: "x", rpc_code: null, data: null });
    render(<History />);
    const error = await screen.findByRole("alert");
    expect(error).toHaveTextContent("Couldn't load history");
    expect(error).toHaveTextContent("isn't reachable");
    historyReturns(activities);
    fireEvent.click(within(error).getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("list")).toBeInTheDocument();
    expect(call).toHaveBeenCalledTimes(2);
  });
});
