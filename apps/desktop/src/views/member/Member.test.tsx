import { fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import "../../i18n";
import { uiStore } from "../../store/ui";
import { AUTH, DESIGN, JIWOO, MINSU, SEOYEON, loadWorld } from "../ask-conversation/test-world";
import Member from "./Member";

vi.mock("../../ipc/transport", () => ({
  tauriTransport: { call: vi.fn(), onEvent: () => () => {}, onStatus: () => () => {} },
}));

beforeEach(() => loadWorld());

describe("member's agents", () => {
  it("online: lists shared agents with harness, role, state, and actions", () => {
    render(<Member memberId={MINSU} />);
    const page = screen.getByRole("region", { name: "Member" });
    expect(page).toHaveAttribute("data-state", "online");
    expect(within(page).getByRole("heading", { name: "Minsu" })).toBeInTheDocument();
    expect(within(page).getByText("Online")).toBeInTheDocument();
    expect(within(page).queryByRole("status")).toBeNull();

    const rows = within(page).getAllByRole("listitem");
    expect(rows).toHaveLength(3);
    const auth = within(rows[0]);
    expect(auth.getByText("Auth")).toBeInTheDocument();
    expect(auth.getByText("Codex")).toBeInTheDocument();
    expect(auth.getByText("Login and tokens")).toBeInTheDocument();
    expect(auth.getByText("Working")).toBeInTheDocument();
    expect(within(rows[1]).getByText("No requests")).toBeInTheDocument();
    expect(within(rows[1]).queryByRole("button", { name: /Request work/ })).toBeNull();

    fireEvent.click(auth.getByRole("button", { name: "Request work from Auth" }));
    expect(uiStore.getState().sheet).toEqual({ kind: "work-request", owner: MINSU, agentId: AUTH, fromQuestion: null });
    fireEvent.click(auth.getByRole("button", { name: "Ask Auth" }));
    expect(uiStore.getState().view).toEqual({ kind: "teammate-agent", owner: MINSU, agentId: AUTH });
    fireEvent.click(within(page).getByRole("button", { name: "Ask Minsu" }));
    expect(uiStore.getState().view).toEqual({ kind: "person", memberId: MINSU });
  });

  it("offline: says questions wait until the member is back", () => {
    render(<Member memberId={JIWOO} />);
    const page = screen.getByRole("region", { name: "Member" });
    expect(page).toHaveAttribute("data-state", "offline");
    expect(within(page).getByText("Offline")).toBeInTheDocument();
    expect(within(page).getByRole("status")).toHaveTextContent("Jiwoo's Mac is offline");
    fireEvent.click(within(page).getByRole("button", { name: "Ask Design" }));
    expect(uiStore.getState().view).toEqual({ kind: "teammate-agent", owner: JIWOO, agentId: DESIGN });
  });

  it("no-shared: shows the empty state without Ask", () => {
    render(<Member memberId={SEOYEON} />);
    const page = screen.getByRole("region", { name: "Member" });
    expect(page).toHaveAttribute("data-state", "no-shared");
    expect(within(page).getByText("Seoyeon hasn't shared any agents yet")).toBeInTheDocument();
    expect(within(page).queryByRole("button")).toBeNull();
  });
});
