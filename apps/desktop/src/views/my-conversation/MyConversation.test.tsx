import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import backendIdle from "../../../../../tools/fake-formationd/fixtures/scripts/backend-idle.json";
import backendPermission from "../../../../../tools/fake-formationd/fixtures/scripts/backend-permission.json";
import backendQueued from "../../../../../tools/fake-formationd/fixtures/scripts/backend-queued.json";
import approvalRaised from "../../../../../tools/fake-formationd/fixtures/scripts/approval-raised.json";
import planningError from "../../../../../tools/fake-formationd/fixtures/scripts/planning-error.json";
import "../../i18n";
import { appStore } from "../../store/store";
import { uiStore } from "../../store/ui";
import { BACKEND, PLANNING, RESEARCH, SHARED_WEB, SUPPORT, apiError, emit, loadWorld, play } from "./world.testing";

const call = vi.fn();
vi.mock("../../ipc/transport", () => ({ tauriTransport: { call: (...a: unknown[]) => call(...a) } }));

const { default: MyConversation } = await import("./MyConversation");

beforeEach(() => {
  call.mockReset();
  call.mockResolvedValue(null);
  loadWorld();
});

function open(agentId: string) {
  return render(<MyConversation agentId={agentId} />);
}

function composer() {
  return screen.getByRole("textbox", { name: "Message" });
}

describe("SC-005 working", () => {
  it("shows bubbles, a collapsible step group, the request card, and the working line", () => {
    open(BACKEND);
    const region = screen.getByRole("region", { name: "Conversation with Backend" });
    expect(region).toHaveAttribute("data-state", "working");
    expect(screen.getByText("Clean up the login API endpoints and add a token expiry check.")).toHaveClass("me");
    expect(screen.getByText("Sure. I'll tidy the three endpoints, then add the expiry check.")).toHaveClass("ag");
    const toggle = screen.getByRole("button", { name: "4 steps · 3 files edited, 1 command run" });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    const list = document.getElementById(toggle.getAttribute("aria-controls") as string) as HTMLElement;
    expect(list).not.toBeVisible();
    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(list).toBeVisible();
    expect(within(list).getByText("Editing auth/token.ts")).toBeInTheDocument();
    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(screen.getByRole("region", { name: "Work request from Minsu" })).toHaveTextContent("Runs after this turn");
    expect(screen.getByRole("status")).toHaveTextContent("Working · Editing auth/token.ts");
    expect(screen.getByRole("button", { name: "Stop ⌘." })).toBeInTheDocument();
    expect(screen.getByText("Teammates never see this conversation")).toBeInTheDocument();
  });

  it("shows only my own queued messages as chips (Minsu's request runs from its card)", () => {
    open(BACKEND);
    expect(screen.queryByRole("button", { name: "Steer now" })).toBeNull();
  });

  it("sends with ⌘↩ and clears the composer", async () => {
    open(BACKEND);
    fireEvent.change(composer(), { target: { value: "Run the tests too" } });
    await act(async () => {
      fireEvent.keyDown(composer(), { key: "Enter", metaKey: true });
    });
    expect(call).toHaveBeenCalledWith("agent_message_send", { agent_id: BACKEND, text: "Run the tests too" });
    expect(composer()).toHaveValue("");
  });

  it("does not send on plain Return or an empty message", () => {
    open(BACKEND);
    fireEvent.keyDown(composer(), { key: "Enter", metaKey: true });
    fireEvent.change(composer(), { target: { value: "hi" } });
    fireEvent.keyDown(composer(), { key: "Enter" });
    expect(call).not.toHaveBeenCalled();
  });

  it("disables send while the send is pending and shows a failure", async () => {
    let fail: (e: unknown) => void = () => {};
    call.mockImplementation(() => new Promise((_, reject) => (fail = reject)));
    open(BACKEND);
    fireEvent.change(composer(), { target: { value: "hi" } });
    fireEvent.click(screen.getByRole("button", { name: "Send ⌘↩" }));
    expect(screen.getByRole("button", { name: "Send ⌘↩" })).toBeDisabled();
    await act(async () => fail(apiError("harness_unavailable")));
    expect(screen.getByRole("alert")).toHaveTextContent("The harness isn't ready");
    expect(composer()).toHaveValue("hi");
  });
});

describe("SC-005 idle", () => {
  it("shows the reply, no working line, and no Stop", () => {
    play(backendIdle);
    open(BACKEND);
    expect(screen.getByText("The expiry check is in. I haven't run the tests yet.")).toBeInTheDocument();
    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.queryByRole("button", { name: "Stop ⌘." })).toBeNull();
    expect(screen.getByRole("button", { name: "4 steps · 3 files edited, 1 command run" })).toHaveAttribute("aria-expanded", "false");
  });

  it("marks scheduled messages", () => {
    open(SUPPORT);
    expect(screen.getByText("Scheduled")).toBeInTheDocument();
  });
});

describe("SC-005 queued", () => {
  beforeEach(() => play(backendQueued));

  it("shows my queued message with Steer now, Edit and Withdraw", async () => {
    open(BACKEND);
    expect(screen.getByText("“Run the tests too”", { exact: false })).toBeInTheDocument();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Steer now" })));
    expect(call).toHaveBeenCalledWith("agent_message_steer", {
      agent_id: BACKEND,
      message_id: "01930000-0000-7000-8000-000000000a02",
    });
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Withdraw" })));
    expect(call).toHaveBeenCalledWith("agent_message_withdraw", {
      agent_id: BACKEND,
      message_id: "01930000-0000-7000-8000-000000000a02",
    });
  });

  it("edits a queued message inline", async () => {
    open(BACKEND);
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    const box = screen.getByRole("textbox", { name: "Edit queued message" });
    expect(box).toHaveValue("Run the tests too");
    fireEvent.change(box, { target: { value: "Run all the tests" } });
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Save" })));
    expect(call).toHaveBeenCalledWith("agent_message_edit", {
      agent_id: BACKEND,
      message_id: "01930000-0000-7000-8000-000000000a02",
      text: "Run all the tests",
    });
    expect(screen.queryByRole("textbox", { name: "Edit queued message" })).toBeNull();
  });

  it("shows a conflict when the message already started", async () => {
    call.mockRejectedValue(apiError("conflict"));
    open(BACKEND);
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Withdraw" })));
    expect(screen.getByRole("alert")).toHaveTextContent("That changed in the meantime");
  });

  it("offers no Steer now once the turn ended", () => {
    play(backendIdle);
    open(BACKEND);
    expect(screen.queryByRole("button", { name: "Steer now" })).toBeNull();
    expect(screen.getByRole("button", { name: "Edit" })).toBeInTheDocument();
  });
});

describe("SC-005 permission and approval", () => {
  it("permission: renders the permission card for the approval item", () => {
    play(backendPermission);
    open(BACKEND);
    const card = screen.getByRole("region", { name: "Claude Code asks for permission" });
    expect(card).toHaveTextContent("rm -rf dist/ && npm run build");
    expect(within(card).getByRole("button", { name: "Allow" })).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Waiting for approval");
    expect(screen.getByRole("button", { name: "Stop ⌘." })).toBeInTheDocument();
  });

  it("approval: Backend waits for approval of Minsu's request", () => {
    play(approvalRaised);
    open(BACKEND);
    expect(screen.getByRole("region", { name: "Conversation with Backend" })).toHaveAttribute(
      "data-state",
      "waiting_for_approval",
    );
    const card = screen.getByRole("region", { name: "Work request from Minsu" });
    expect(card).toHaveTextContent("1 approval waiting");
    // The request card's Review covers the approval; its compact approval row is not repeated.
    expect(screen.getAllByRole("button", { name: "Review" })).toHaveLength(1);
    expect(screen.queryByRole("region", { name: "Approval" })).toBeNull();
    fireEvent.click(within(card).getByRole("button", { name: "Review" }));
    expect(uiStore.getState().sheet).toEqual({ kind: "approval", approvalId: "01930000-0000-7000-8000-000000000501" });
    expect(screen.getByRole("status")).toHaveTextContent("Waiting for approval");
  });
});

describe("SC-005 stalled", () => {
  it("shows the stall banner with the last step and restarts with stop now, then start", async () => {
    open(RESEARCH);
    const banner = screen.getByText(/No activity for \d+ minutes/).closest(".banner") as HTMLElement;
    expect(within(banner).getByText("Last step: Opening competitor-c.com/plans")).toBeInTheDocument();
    await act(async () => fireEvent.click(within(banner).getByRole("button", { name: "Restart" })));
    expect(call.mock.calls).toEqual([
      ["agent_stop", { agent_id: RESEARCH, mode: "now" }],
      ["agent_start", { agent_id: RESEARCH }],
    ]);
  });
});

describe("SC-005 error", () => {
  it("shows the error banner with the exit detail, Restart, and the start bar", async () => {
    play(planningError);
    open(PLANNING);
    const banner = screen.getAllByRole("alert")[0];
    expect(within(banner).getByText("The process exited unexpectedly")).toBeInTheDocument();
    expect(within(banner).getByText("Exited with code 1")).toBeInTheDocument();
    expect(banner).not.toHaveTextContent("Planning · exited with code 1");
    await act(async () => fireEvent.click(within(banner).getByRole("button", { name: "Restart" })));
    expect(call.mock.calls).toEqual([["agent_start", { agent_id: PLANNING }]]);
    expect(screen.queryByRole("textbox", { name: "Message" })).toBeNull();
  });

  it("shows the event's reason without an exit code line when only the reason is known", () => {
    open(PLANNING);
    emit({ type: "agent.state", payload: { agent_id: PLANNING, state: "error", reason: "Out of memory", exit_code: null } });
    const banner = screen.getAllByRole("alert")[0];
    expect(within(banner).getByText("Out of memory")).toBeInTheDocument();
    expect(banner).not.toHaveTextContent("Exited with code");
  });

  it("shows the exit code under the default title when only the exit code is known", () => {
    open(PLANNING);
    emit({ type: "agent.state", payload: { agent_id: PLANNING, state: "error", reason: null, exit_code: 137 } });
    const banner = screen.getAllByRole("alert")[0];
    expect(within(banner).getByText("The process exited unexpectedly")).toBeInTheDocument();
    expect(within(banner).getByText("Exited with code 137")).toBeInTheDocument();
  });

  it("falls back to the Inbox item when the event gave neither", () => {
    play({
      steps: [
        { event: { ...planningError.steps[0].event, payload: { ...planningError.steps[0].event.payload, reason: null, exit_code: null } } },
        planningError.steps[1],
      ],
    });
    open(PLANNING);
    const banner = screen.getAllByRole("alert")[0];
    expect(within(banner).getByText("The process exited unexpectedly")).toBeInTheDocument();
    expect(within(banner).getByText("Planning · exited with code 1")).toBeInTheDocument();
  });

  it("drops the banner and its status once the agent runs again", () => {
    play(planningError);
    open(PLANNING);
    emit({ type: "agent.state", payload: { agent_id: PLANNING, state: "working", reason: null, exit_code: null } });
    expect(screen.queryByText("Exited with code 1")).toBeNull();
    expect(appStore.getState().mirror?.agentStatus).not.toHaveProperty(PLANNING);
  });
});

describe("SC-005 stopped", () => {
  it("replaces the composer with a Start button", async () => {
    open(PLANNING);
    expect(screen.getByText("This agent isn't running")).toBeInTheDocument();
    expect(screen.queryByRole("textbox", { name: "Message" })).toBeNull();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Start" })));
    expect(call).toHaveBeenCalledWith("agent_start", { agent_id: PLANNING });
  });

  it("shows the composer again once the agent runs", () => {
    open(PLANNING);
    emit({ type: "agent.state", payload: { agent_id: PLANNING, state: "idle", reason: null, exit_code: null } });
    expect(composer()).toBeEnabled();
  });
});

describe("SC-005 detected", () => {
  it("shows the read-only banner and a disabled composer", () => {
    open(SHARED_WEB);
    expect(screen.getByLabelText("Read-only · detected agent")).toBeInTheDocument();
    expect(composer()).toBeDisabled();
    expect(screen.getByRole("button", { name: "Send ⌘↩" })).toBeDisabled();
  });
});

describe("SC-005 stop-confirm", () => {
  it("opens from Stop and stops after this turn", async () => {
    open(BACKEND);
    fireEvent.click(screen.getByRole("button", { name: "Stop ⌘." }));
    const alert = screen.getByRole("alertdialog", { name: "Backend is in the middle of a task" });
    await act(async () => fireEvent.click(within(alert).getByRole("button", { name: "Stop after this turn" })));
    expect(call).toHaveBeenCalledWith("agent_stop", { agent_id: BACKEND, mode: "after_turn" });
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });

  it("opens with ⌘. and stops now", async () => {
    open(BACKEND);
    fireEvent.keyDown(document, { key: ".", metaKey: true });
    const alert = screen.getByRole("alertdialog");
    await act(async () => fireEvent.click(within(alert).getByRole("button", { name: "Stop now" })));
    expect(call).toHaveBeenCalledWith("agent_stop", { agent_id: BACKEND, mode: "now" });
  });

  it("Cancel keeps the agent working", () => {
    open(BACKEND);
    fireEvent.keyDown(document, { key: ".", metaKey: true });
    fireEvent.click(within(screen.getByRole("alertdialog")).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(call).not.toHaveBeenCalled();
  });

  it("⌘. does nothing while the agent is idle", () => {
    play(backendIdle);
    open(BACKEND);
    fireEvent.keyDown(document, { key: ".", metaKey: true });
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });
});

describe("header", () => {
  it("toggles the information panel", () => {
    open(BACKEND);
    const toggle = screen.getByRole("button", { name: "Information panel" });
    expect(toggle).toHaveAttribute("aria-pressed", "true");
    fireEvent.click(toggle);
    expect(uiStore.getState().panelOpen).toBe(false);
    expect(toggle).toHaveAttribute("aria-pressed", "false");
  });

  it("shows a pending stop at once", () => {
    appStore.getState().beginPending(`agent_stop:${BACKEND}`);
    open(BACKEND);
    expect(screen.getByRole("button", { name: "Stop ⌘." })).toBeDisabled();
  });
});
