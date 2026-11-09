import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import "../../i18n";
import { uiStore } from "../../store/ui";
import { JIWOO, ME, MINSU, loadWorld } from "../ask-conversation/test-world";
import Team from "./Team";

const { call } = vi.hoisted(() => ({ call: vi.fn() }));
vi.mock("../../ipc/transport", () => ({
  tauriTransport: { call: (...args: unknown[]) => call(...args), onEvent: () => () => {}, onStatus: () => () => {} },
}));

beforeEach(() => {
  call.mockReset();
  call.mockResolvedValue({});
  loadWorld();
});

describe("team", () => {
  it("default: members with online state and shared-agent counts", () => {
    render(<Team />);
    const page = screen.getByRole("region", { name: "Team" });
    expect(page).toHaveAttribute("data-state", "default");
    expect(within(page).getByText("Dohyeon's team · 4 people")).toBeInTheDocument();
    const rows = within(page).getAllByRole("listitem");
    expect(rows.map((r) => r.querySelector("small")?.textContent)).toEqual([
      "Me · team creator",
      "3 shared agents",
      "1 shared agent",
      "No shared agents",
    ]);
    expect(within(rows[2]).getByText("Offline")).toBeInTheDocument();
    expect(within(rows[1]).getByText("Online")).toBeInTheDocument();
    expect(within(rows[0]).queryByRole("button")).toBeNull();
    expect(within(page).queryByText("It's just you so far")).toBeNull();

    fireEvent.click(within(rows[1]).getByRole("button", { name: "View Minsu's agents" }));
    expect(uiStore.getState().view).toEqual({ kind: "member", memberId: MINSU });
  });

  it("Remove asks first, then calls team_remove", async () => {
    render(<Team />);
    fireEvent.click(screen.getByRole("button", { name: "Remove Jiwoo" }));
    let alert = screen.getByRole("alertdialog", { name: "Remove Jiwoo?" });
    fireEvent.click(within(alert).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(call).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Remove Jiwoo" }));
    alert = screen.getByRole("alertdialog", { name: "Remove Jiwoo?" });
    await act(async () => {
      fireEvent.click(within(alert).getByRole("button", { name: "Remove" }));
    });
    expect(call).toHaveBeenCalledWith("team_remove", { member_id: JIWOO });
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });

  it("hides Remove from members who did not create the team", () => {
    loadWorld((s) => {
      s.team!.me = MINSU;
    });
    render(<Team />);
    expect(screen.queryByRole("button", { name: /^Remove/ })).toBeNull();
    expect(screen.getByRole("button", { name: "View Dohyeon's agents" })).toBeInTheDocument();
  });

  it("only-me: invites teammates", () => {
    loadWorld((s) => {
      s.team!.members = s.team!.members.filter((m) => m.id === ME);
      s.teammate_agents = [];
    });
    render(<Team />);
    const page = screen.getByRole("region", { name: "Team" });
    expect(page).toHaveAttribute("data-state", "only-me");
    expect(within(page).getByRole("status")).toHaveTextContent("It's just you so far");
  });

  it("invite: shows the code with Copy and the expiry", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    call.mockResolvedValue({
      id: "inv",
      code: "formation://join/7F3K-Q9PX",
      created_by: ME,
      expires_at: "2026-11-10T01:00:00Z",
      used_by: null,
      used_at: null,
    });
    render(<Team />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Invite" }));
    });
    expect(call).toHaveBeenCalledWith("team_invite", {});
    const page = screen.getByRole("region", { name: "Team" });
    expect(page).toHaveAttribute("data-state", "invite");
    const invite = within(page).getByRole("region", { name: "Invitation link created" });
    expect(within(invite).getByText("formation://join/7F3K-Q9PX")).toBeInTheDocument();
    expect(invite).toHaveTextContent(/Expires .+\. Send it over Slack or chat\./);
    await act(async () => {
      fireEvent.click(within(invite).getByRole("button", { name: "Copy" }));
    });
    expect(writeText).toHaveBeenCalledWith("formation://join/7F3K-Q9PX");
    expect(within(invite).getByRole("button", { name: "Copied" })).toBeInTheDocument();
  });

  it("offers Try again when the invitation fails", async () => {
    call.mockRejectedValueOnce({ code: "disconnected", message: "x", rpc_code: null, data: null });
    render(<Team />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Invite" }));
    });
    const banner = screen.getByRole("alert");
    expect(banner).toHaveTextContent("Couldn't create an invitation");
    await act(async () => {
      fireEvent.click(within(banner).getByRole("button", { name: "Try again" }));
    });
    expect(call).toHaveBeenCalledTimes(2);
  });
});
