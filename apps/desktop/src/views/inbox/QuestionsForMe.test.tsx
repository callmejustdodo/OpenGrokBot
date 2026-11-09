import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import approvalRaised from "../../../../../tools/fake-formationd/fixtures/scripts/approval-raised.json";
import inboxCleared from "../../../../../tools/fake-formationd/fixtures/scripts/inbox-cleared.json";
import m3 from "../../../../../tools/fake-formationd/fixtures/worlds/m3.json";
import "../../i18n";
import type { EventEnvelope } from "../../ipc/generated/EventEnvelope";
import type { Snapshot } from "../../ipc/generated/Snapshot";
import { appStore } from "../../store/store";

const call = vi.fn();
vi.mock("../../ipc/transport", () => ({ tauriTransport: { call: (...a: unknown[]) => call(...a) } }));

const { default: QuestionsForMe } = await import("./QuestionsForMe");

const QUESTION = "01930000-0000-7000-8000-000000000305";
const BACKEND = "01930000-0000-7000-8000-000000000101";
const MARKETING = "01930000-0000-7000-8000-000000000102";
const snapshot = m3 as unknown as Snapshot;

type Script = { steps: { event: EventEnvelope["event"] }[] };

/** Applies a fixture script's events after the snapshot, numbering `seq` and per-agent `stream_seq`. */
function play(script: Script) {
  for (const step of script.steps) {
    const state = appStore.getState();
    const event = structuredClone(step.event) as EventEnvelope["event"];
    if (event.type === "agent.stream") {
      event.payload.stream_seq = (state.mirror?.conversations[event.payload.agent_id]?.stream_seq ?? 0) + 1;
    }
    act(() => state.receive({ seq: (state.mirror?.seq ?? 0) + 1, at: "2026-11-09T01:53:00Z", event }));
  }
}

beforeEach(() => {
  call.mockReset();
  appStore.setState({ mirror: null, connected: true, stale: false, pending: {} });
  appStore.getState().loadSnapshot(snapshot);
});

describe("Questions sent to me (SC-010)", () => {
  it("lists the question with asker, text and why (default)", () => {
    play(approvalRaised as unknown as Script);
    render(<QuestionsForMe />);
    const group = screen.getByRole("region", { name: "Questions sent to me" });
    const row = within(group).getByRole("listitem");
    expect(row).toHaveTextContent("Jiwoo");
    expect(row).toHaveTextContent("When will the payment refactor be done?");
    expect(row).toHaveTextContent("OpenGrokBot wasn't sure which agent should answer; Jiwoo chose to ask you");
    expect(within(row).getByRole("button", { name: "Answer" })).toBeEnabled();
    expect(within(row).getByRole("button", { name: "Hand to agent" })).toBeEnabled();
  });

  it("renders nothing when no question waits for me (empty)", () => {
    play(inboxCleared as unknown as Script);
    const { container } = render(<QuestionsForMe />);
    expect(container).toBeEmptyDOMElement();
  });

  it("answers inline with question_answer and shows the pending state", async () => {
    let reply: (q: unknown) => void = () => {};
    call.mockImplementation(() => new Promise((resolve) => (reply = resolve)));
    render(<QuestionsForMe />);
    fireEvent.click(screen.getByRole("button", { name: "Answer" }));
    const box = screen.getByRole("textbox", { name: "Answer to Jiwoo" });
    expect(screen.getByRole("button", { name: "Send answer" })).toBeDisabled();
    fireEvent.change(box, { target: { value: "  End of next week.  " } });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true });
    expect(call).toHaveBeenCalledWith("question_answer", { question_id: QUESTION, text: "End of next week." });
    expect(screen.getByRole("button", { name: "Sending…" })).toBeDisabled();
    expect(screen.getByRole("listitem")).toHaveAttribute("data-state", "pending");
    await act(async () => reply({}));
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(screen.getByRole("button", { name: "Answer" })).toBeInTheDocument();
  });

  it("Escape closes the answer box without sending", () => {
    render(<QuestionsForMe />);
    fireEvent.click(screen.getByRole("button", { name: "Answer" }));
    fireEvent.keyDown(screen.getByRole("textbox"), { key: "Escape" });
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(call).not.toHaveBeenCalled();
  });

  it("hands the question to one of my shared agents with question_handoff", async () => {
    call.mockResolvedValue({});
    render(<QuestionsForMe />);
    fireEvent.click(screen.getByRole("button", { name: "Hand to agent" }));
    const picker = screen.getByRole("combobox", { name: "Agent to answer" });
    const names = within(picker)
      .getAllByRole("option")
      .map((o) => o.textContent);
    // Only Backend is shared in the M3 world; private agents are not offered.
    expect(names).toEqual(["Choose an agent", "Backend"]);
    expect(picker).toHaveValue(BACKEND);
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Hand off" })));
    expect(call).toHaveBeenCalledWith("question_handoff", { question_id: QUESTION, agent_id: BACKEND });
    expect(screen.queryByRole("combobox")).toBeNull();
  });

  it("asks to pick when several agents are shared, and shows an error from the call", async () => {
    const mirror = appStore.getState().mirror!;
    appStore.setState({
      mirror: { ...mirror, shares: { ...mirror.shares, [MARKETING]: { ...mirror.shares[MARKETING], shared: true } } },
    });
    call.mockRejectedValue({ code: "rpc", message: "x", rpc_code: 1, data: { code: "agent_not_shared", message: "x" } });
    render(<QuestionsForMe />);
    fireEvent.click(screen.getByRole("button", { name: "Hand to agent" }));
    const handOff = screen.getByRole("button", { name: "Hand off" });
    expect(handOff).toBeDisabled();
    fireEvent.change(screen.getByRole("combobox"), { target: { value: MARKETING } });
    await act(async () => fireEvent.click(handOff));
    expect(call).toHaveBeenCalledWith("question_handoff", { question_id: QUESTION, agent_id: MARKETING });
    expect(screen.getByRole("alert")).toHaveTextContent("That agent isn't shared any more");
  });

  it("disables Hand to agent when none of my agents is shared", () => {
    const mirror = appStore.getState().mirror!;
    appStore.setState({
      mirror: { ...mirror, shares: { ...mirror.shares, [BACKEND]: { ...mirror.shares[BACKEND], shared: false } } },
    });
    render(<QuestionsForMe />);
    expect(screen.getByRole("button", { name: "Hand to agent" })).toBeDisabled();
  });
});
