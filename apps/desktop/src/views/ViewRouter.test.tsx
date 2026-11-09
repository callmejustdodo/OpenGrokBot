import { act, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import m3 from "../../../../tools/fake-formationd/fixtures/worlds/m3.json";
import type { Snapshot } from "../ipc/generated/Snapshot";
import { appStore } from "../store/store";
import { uiStore } from "../store/ui";
import SheetHost from "./SheetHost";
import ViewRouter from "./ViewRouter";

const BACKEND = "01930000-0000-7000-8000-000000000101";

beforeEach(() => {
  appStore.getState().loadSnapshot(m3 as unknown as Snapshot);
  uiStore.setState({ view: { kind: "home" }, sheet: null });
});

describe("navigation", () => {
  it("shows the view for the current navigation target", () => {
    render(<ViewRouter />);
    act(() => uiStore.getState().navigate({ kind: "agent", agentId: BACKEND }));
    expect(screen.getByLabelText("MyConversation")).toBeInTheDocument();
    act(() => uiStore.getState().navigate({ kind: "inbox" }));
    expect(screen.getByLabelText("Inbox")).toBeInTheDocument();
  });

  it("shows Welcome until this Mac has a team", () => {
    appStore.getState().loadSnapshot({ ...(m3 as unknown as Snapshot), team: null });
    render(<ViewRouter />);
    expect(screen.getByLabelText("Welcome")).toBeInTheDocument();
  });

  it("opens and closes sheets", () => {
    render(<SheetHost />);
    act(() => uiStore.getState().openSheet({ kind: "new-agent" }));
    expect(screen.getByLabelText("NewAgentSheet")).toBeInTheDocument();
    act(() => uiStore.getState().closeSheet());
    expect(screen.queryByLabelText("NewAgentSheet")).toBeNull();
  });
});
