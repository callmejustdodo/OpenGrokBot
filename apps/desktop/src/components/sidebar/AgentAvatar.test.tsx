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

describe("AgentAvatar unknown shapes", () => {
  it("draws a set character for a shape formationd sends outside open-bot-avatar", () => {
    const el = ava(<AgentAvatar avatar={{ shape: "round", color: "blue" }} seed="agent-1" />);
    expect(el).not.toHaveClass("sq");
    expect(el.querySelector("svg path")).not.toBeNull();
    expect(el).toHaveAttribute("data-color", "#3a6df0");
  });

  it("picks the same shape for the same agent every time", () => {
    const a = ava(<AgentAvatar avatar={{ shape: "round", color: "blue" }} seed="agent-1" />).getAttribute("data-shape");
    const b = ava(<AgentAvatar avatar={{ shape: "round", color: "blue" }} seed="agent-1" />).getAttribute("data-shape");
    expect(a).toBe(b);
    expect(a).not.toBe("round");
  });

  it("keeps a known shape and color as they are", () => {
    const el = ava(<AgentAvatar avatar={avatar} />);
    expect(el).toHaveAttribute("data-shape", "rocket");
    expect(el).toHaveAttribute("data-color", "#5B8DEF");
  });
});
