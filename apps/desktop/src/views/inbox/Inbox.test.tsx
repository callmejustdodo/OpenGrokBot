import { fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "../../i18n";
import { uiStore } from "../../store/ui";
import { apply, BACKEND, loadWorld, M3_NOW, MINSU_APPROVAL, MY_REQUEST, RESEARCH } from "../approval/m3World";
import Inbox from "./Inbox";

vi.mock("../../ipc/transport", () => ({
  tauriTransport: { call: vi.fn(), onEvent: () => () => {}, onStatus: () => () => {} },
}));

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["Date"], now: M3_NOW });
  uiStore.setState({ view: { kind: "inbox" }, sheet: null });
});

afterEach(() => vi.useRealTimers());

describe("Inbox (SC-010)", () => {
  it("default: approvals waiting, questions sent to me, and alerts, with the count", () => {
    loadWorld(["approval-raised"]);
    render(<Inbox />);
    expect(screen.getByRole("heading", { level: 1, name: "Inbox" })).toBeInTheDocument();
    expect(screen.getByText("3 items")).toBeInTheDocument();

    const approvals = screen.getByRole("list", { name: "Approvals waiting" });
    const row = within(approvals).getByRole("button");
    expect(row).toHaveTextContent("Minsu → Backend · Run a command");
    expect(row).toHaveTextContent("npm test -- auth/token.spec.ts");
    expect(row).toHaveTextContent("3 min ago");
    fireEvent.click(row);
    expect(uiStore.getState().sheet).toEqual({ kind: "approval", approvalId: MINSU_APPROVAL });

    expect(screen.getByRole("heading", { name: "Questions sent to me" })).toBeInTheDocument();
    expect(screen.getByLabelText("QuestionsForMe")).toBeInTheDocument();

    const alerts = screen.getByRole("list", { name: "Alerts" });
    const stalled = within(alerts).getByRole("button");
    expect(stalled).toHaveTextContent("Stalled");
    expect(stalled).toHaveTextContent("Research · no activity for 14 minutes");
    fireEvent.click(stalled);
    expect(uiStore.getState().view).toEqual({ kind: "agent", agentId: RESEARCH });
  });

  it("empty: Nothing needs you", () => {
    loadWorld(["inbox-cleared"]);
    render(<Inbox />);
    expect(screen.getByText("Nothing needs you")).toBeInTheDocument();
    expect(screen.queryByText(/items?$/)).toBeNull();
    expect(screen.queryByRole("list")).toBeNull();
  });

  it("sends my own permission prompt to its conversation, not the sheet", () => {
    loadWorld(["inbox-cleared", "backend-permission"]);
    render(<Inbox />);
    expect(screen.getByText("1 item")).toBeInTheDocument();
    const row = within(screen.getByRole("list", { name: "Approvals waiting" })).getByRole("button");
    expect(row).toHaveTextContent("Backend · Asks for permission");
    fireEvent.click(row);
    expect(uiStore.getState().view).toEqual({ kind: "agent", agentId: BACKEND });
    expect(uiStore.getState().sheet).toBeNull();
  });

  it("opens a finished request's status from its alert", () => {
    loadWorld(["inbox-cleared", "request-status"]);
    apply({
      type: "notify",
      payload: {
        alert: true,
        item: {
          id: "attention-finished",
          kind: "request_finished",
          target: { kind: "request", request_id: MY_REQUEST },
          subject: "Your work request finished",
          at: "2026-11-09T01:52:00Z",
          resolved_at: null,
        },
      },
    });
    render(<Inbox />);
    const row = within(screen.getByRole("list", { name: "Alerts" })).getByRole("button");
    expect(row).toHaveTextContent("Request finished");
    expect(row).toHaveTextContent("1 min ago");
    fireEvent.click(row);
    expect(uiStore.getState().sheet).toEqual({ kind: "request-status", requestId: MY_REQUEST });
  });
});
