import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import AgentAvatar from "./AgentAvatar";

const avatar = { shape: "rocket", color: "#5B8DEF" };

function ava(ui: React.ReactElement) {
  return render(ui).container.querySelector(".ava")!;
}

describe("AgentAvatar motion (D-042)", () => {
  it("moves while the agent works", () => {
    expect(ava(<AgentAvatar avatar={avatar} state="working" />)).toHaveAttribute("data-motion", "working");
  });

  it("stays still in any other state", () => {
    for (const state of ["idle", "stalled", "waiting_for_approval", "error", "stopped"] as const) {
      expect(ava(<AgentAvatar avatar={avatar} state={state} />)).not.toHaveAttribute("data-motion");
    }
    expect(ava(<AgentAvatar avatar={avatar} />)).not.toHaveAttribute("data-motion");
  });

  it("moves without a state dot when told the agent works", () => {
    const el = ava(<AgentAvatar avatar={avatar} working />);
    expect(el).toHaveAttribute("data-motion", "working");
    expect(el).not.toHaveAttribute("data-state");
  });

  it("marks the eyes for the blink", () => {
    expect(ava(<AgentAvatar avatar={avatar} state="working" />).querySelectorAll(".eye")).toHaveLength(2);
  });
});
