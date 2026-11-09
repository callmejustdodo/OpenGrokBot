import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import approvalRaised from "../../../../../tools/fake-formationd/fixtures/scripts/approval-raised.json";
import empty from "../../../../../tools/fake-formationd/fixtures/worlds/empty.json";
import "../../i18n";
import type { Snapshot } from "../../ipc/generated/Snapshot";
import { appStore } from "../../store/store";
import { uiStore } from "../../store/ui";
import { AUTH, BACKEND, JIWOO, MINSU, loadWorld, play } from "../../views/my-conversation/world.testing";
import Sidebar from "./Sidebar";

beforeEach(() => loadWorld());

function row(name: string) {
  return screen.getByText(name, { selector: ".nm" }).closest("li") as HTMLElement;
}

describe("SC-003 default", () => {
  beforeEach(() => play(approvalRaised));

  it("lists my agents with their state, teammates' agents with the owner, and people", () => {
    render(<Sidebar />);
    expect(screen.getByRole("complementary", { name: "Sidebar" })).toHaveAttribute("data-state", "default");
    expect(row("Backend")).toHaveAttribute("data-state", "waiting_for_approval");
    expect(within(row("Backend")).getByText("Waiting for approval")).toHaveClass("attn");
    expect(row("Research")).toHaveAttribute("data-state", "stalled");
    expect(within(row("Marketing")).getByText("Drafting this week's content ideas…")).not.toHaveClass("attn");
    expect(row("Auth")).toHaveAttribute("data-kind", "teammate_agent");
    expect(within(row("Auth")).getByText(/^Minsu · /)).toBeInTheDocument();
    expect(row("Jiwoo")).toHaveAttribute("data-kind", "person");
    // Most recent first.
    const names = screen.getAllByText(/./, { selector: ".nm" }).map((n) => n.textContent);
    expect(names[0]).toBe("Backend");
  });

  it("shows the Needs-you line and the Inbox count, both opening Inbox", () => {
    render(<Sidebar />);
    const needs = screen.getByRole("button", { name: /need you/ });
    expect(needs).toHaveTextContent("3 need you");
    fireEvent.click(needs);
    expect(uiStore.getState().view).toEqual({ kind: "inbox" });
    act(() => uiStore.getState().navigate({ kind: "home" }));
    fireEvent.click(screen.getByRole("button", { name: "Inbox (3)" }));
    expect(uiStore.getState().view).toEqual({ kind: "inbox" });
    expect(screen.getByRole("button", { name: "Inbox (3)" })).toHaveAttribute("aria-current", "page");
  });

  it("top icons navigate", () => {
    render(<Sidebar />);
    fireEvent.click(screen.getByRole("button", { name: "Team" }));
    expect(uiStore.getState().view).toEqual({ kind: "team" });
    fireEvent.click(screen.getByRole("button", { name: "History" }));
    expect(uiStore.getState().view).toEqual({ kind: "history" });
    fireEvent.click(screen.getByRole("button", { name: "Rules" }));
    expect(uiStore.getState().view).toEqual({ kind: "rules" });
  });

  it("opens conversations and marks the selected one", () => {
    render(<Sidebar />);
    fireEvent.click(within(row("Backend")).getByRole("button"));
    expect(uiStore.getState().view).toEqual({ kind: "agent", agentId: BACKEND });
    expect(within(row("Backend")).getByRole("button")).toHaveAttribute("aria-current", "true");
    fireEvent.click(within(row("Auth")).getByRole("button"));
    expect(uiStore.getState().view).toEqual({ kind: "teammate-agent", owner: MINSU, agentId: AUTH });
    fireEvent.click(within(row("Jiwoo")).getByRole("button"));
    expect(uiStore.getState().view).toEqual({ kind: "person", memberId: JIWOO });
    expect(within(row("Backend")).getByRole("button")).not.toHaveAttribute("aria-current");
  });

  it("moves between chats with the arrow keys", () => {
    render(<Sidebar />);
    const buttons = within(screen.getByRole("navigation", { name: "Conversations" })).getAllByRole("button");
    buttons[0].focus();
    fireEvent.keyDown(buttons[0], { key: "ArrowDown" });
    expect(buttons[1]).toHaveFocus();
    fireEvent.keyDown(buttons[1], { key: "ArrowUp" });
    expect(buttons[0]).toHaveFocus();
  });

  it("the + menu opens New agent and New question", () => {
    render(<Sidebar />);
    fireEvent.keyDown(screen.getByRole("button", { name: "New" }), { key: "Enter" });
    fireEvent.click(screen.getByRole("menuitem", { name: /New agent/ }));
    expect(uiStore.getState().sheet).toEqual({ kind: "new-agent" });
    fireEvent.keyDown(screen.getByRole("button", { name: "New" }), { key: "Enter" });
    fireEvent.click(screen.getByRole("menuitem", { name: /New question/ }));
    expect(uiStore.getState().view).toEqual({ kind: "new-question" });
  });

  it("Connect agents opens the connect sheet and the profile shows me online", () => {
    render(<Sidebar />);
    fireEvent.click(screen.getByRole("button", { name: "Connect agents" }));
    expect(uiStore.getState().sheet).toEqual({ kind: "connect" });
    expect(screen.getByText("Dohyeon").closest(".prof")).toHaveAttribute("data-online", "true");
  });
});

describe("SC-003 empty", () => {
  it("offers to create the first agent or connect running ones", () => {
    loadWorld(empty as unknown as Snapshot);
    render(<Sidebar />);
    expect(screen.getByRole("complementary", { name: "Sidebar" })).toHaveAttribute("data-state", "empty");
    expect(screen.getByText("Create your first agent")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /need you/ })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "New agent" }));
    expect(uiStore.getState().sheet).toEqual({ kind: "new-agent" });
    fireEvent.click(screen.getByRole("button", { name: "Connect agents already running" }));
    expect(uiStore.getState().sheet).toEqual({ kind: "connect" });
  });
});

describe("SC-003 detecting", () => {
  it("shows the detecting line while agents_detect runs", () => {
    render(<Sidebar />);
    expect(screen.queryByText("Looking for agents on this Mac…")).toBeNull();
    act(() => appStore.getState().beginPending("agents_detect"));
    expect(screen.getByRole("status")).toHaveTextContent("Looking for agents on this Mac…");
    expect(screen.getByRole("complementary", { name: "Sidebar" })).toHaveAttribute("data-state", "detecting");
    act(() => appStore.getState().endPending("agents_detect"));
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("replaces the empty state while detecting on a new Mac", () => {
    loadWorld(empty as unknown as Snapshot);
    act(() => appStore.getState().beginPending("agents_detect"));
    render(<Sidebar />);
    expect(screen.queryByText("Create your first agent")).toBeNull();
    expect(screen.getByRole("status")).toBeInTheDocument();
  });
});

it("keeps Research's stalled state readable without color", () => {
  render(<Sidebar />);
  expect(within(row("Research")).getByText("Stalled")).toBeInTheDocument();
  expect(row("Research").querySelector(".ava")).toHaveAttribute("data-state", "stalled");
});
