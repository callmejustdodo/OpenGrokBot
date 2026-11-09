import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import "../../i18n";
import App from "../../App";
import { uiStore } from "../../store/ui";
import { AUTH, BACKEND, MINSU, QA, SHARED_WEB, loadWorld } from "../../views/my-conversation/world.testing";
import InfoPanel from "./InfoPanel";

beforeEach(() => loadWorld());

function show(view: Parameters<ReturnType<typeof uiStore.getState>["navigate"]>[0]) {
  act(() => uiStore.getState().navigate(view));
}

describe("SC-020 my-agent", () => {
  it("shows the agent's details, the instructions card, and the schedules and sharing sections", () => {
    show({ kind: "agent", agentId: BACKEND });
    render(<InfoPanel />);
    const panel = screen.getByRole("complementary", { name: "Agent information" });
    expect(within(panel).getByText("Backend")).toBeInTheDocument();
    expect(within(panel).getByText("Backend API development")).toBeInTheDocument();
    expect(within(panel).getByText("Claude Code")).toBeInTheDocument();
    expect(within(panel).getByText("~/Formation/Agents/backend")).toBeInTheDocument();
    expect(within(panel).getByText("Working")).toHaveAttribute("data-state", "working");
    expect(panel.querySelector(".ava")).toHaveAttribute("data-shape", "rocket");
    expect(panel.querySelector(".ava")).toHaveAttribute("data-color", "#3a6df0");
    fireEvent.click(within(panel).getByRole("button", { name: /Instructions & skills/ }));
    expect(uiStore.getState().sheet).toEqual({ kind: "instructions", agentId: BACKEND });
  });

  it("opens the schedules sheet from the compact Schedules card", () => {
    show({ kind: "agent", agentId: BACKEND });
    render(<InfoPanel />);
    const card = screen.getByRole("button", { name: /^Schedules/ });
    expect(card.querySelector(".chev")).toBeInTheDocument();
    fireEvent.click(card);
    expect(uiStore.getState().sheet).toEqual({ kind: "schedules", agentId: BACKEND });
  });

  it("expands the sharing controls inline from the compact Sharing card", () => {
    show({ kind: "agent", agentId: BACKEND });
    render(<InfoPanel />);
    const card = screen.getByRole("button", { name: /^Sharing/ });
    expect(card).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByRole("region", { name: "Sharing" })).toBeNull();
    fireEvent.click(card);
    expect(card).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("region", { name: "Sharing" })).toBeInTheDocument();
    expect(screen.getByRole("switch", { name: /Share with team/ })).toBeInTheDocument();
    fireEvent.click(card);
    expect(screen.queryByRole("region", { name: "Sharing" })).toBeNull();
  });

  it("for a detected agent, explains that instructions, schedules and sharing don't apply", () => {
    show({ kind: "agent", agentId: SHARED_WEB });
    render(<InfoPanel />);
    expect(screen.getByText(/Detected agent\. Instructions, schedules, and sharing/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Instructions & skills/ })).toBeNull();
    expect(screen.queryByRole("region", { name: "Sharing" })).toBeNull();
  });
});

describe("SC-020 teammate-agent", () => {
  it("shows what a teammate may see and Request work… when requests are accepted", () => {
    show({ kind: "teammate-agent", owner: MINSU, agentId: AUTH });
    render(<InfoPanel />);
    expect(screen.getByText("Minsu's agent")).toBeInTheDocument();
    expect(screen.getByText("Login and tokens")).toBeInTheDocument();
    expect(screen.getByText(/Accepted\. Actions follow Minsu's approval rules/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Request work…" }));
    expect(uiStore.getState().sheet).toEqual({ kind: "work-request", owner: MINSU, agentId: AUTH, fromQuestion: null });
  });

  it("says when the agent does not accept work requests", () => {
    show({ kind: "teammate-agent", owner: MINSU, agentId: QA });
    render(<InfoPanel />);
    expect(screen.getByText("Not accepted")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Request work…" })).toBeNull();
  });
});

describe("SC-020 person", () => {
  it("lists the person's shared agents", () => {
    show({ kind: "person", memberId: MINSU });
    render(<InfoPanel />);
    const panel = screen.getByRole("complementary", { name: "Teammate information" });
    expect(within(panel).getByText("Minsu")).toBeInTheDocument();
    expect(within(panel).getByText("Online")).toBeInTheDocument();
    for (const name of ["Auth", "Frontend", "QA"]) expect(within(panel).getByText(name)).toBeInTheDocument();
    fireEvent.click(within(panel).getByText("Auth"));
    expect(uiStore.getState().view).toEqual({ kind: "teammate-agent", owner: MINSU, agentId: AUTH });
  });
});

describe("SC-020 collapsed", () => {
  it("collapses with its button and the conversation brings it back", () => {
    show({ kind: "agent", agentId: BACKEND });
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Hide information panel" }));
    expect(uiStore.getState().panelOpen).toBe(false);
    expect(screen.queryByRole("complementary", { name: "Agent information" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Information panel" }));
    expect(screen.getByRole("complementary", { name: "Agent information" })).toBeInTheDocument();
  });

  it("shows nothing for views without a panel", () => {
    show({ kind: "inbox" });
    const { container } = render(<InfoPanel />);
    expect(container).toBeEmptyDOMElement();
  });
});
