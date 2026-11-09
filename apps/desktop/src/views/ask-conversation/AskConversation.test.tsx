import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import authAnswering from "../../../../../tools/fake-formationd/fixtures/scripts/auth-answering.json";
import authReceived from "../../../../../tools/fake-formationd/fixtures/scripts/auth-received.json";
import authStale from "../../../../../tools/fake-formationd/fixtures/scripts/auth-stale.json";
import authUnshared from "../../../../../tools/fake-formationd/fixtures/scripts/auth-unshared.json";
import personSent from "../../../../../tools/fake-formationd/fixtures/scripts/person-sent.json";
import questionFailed from "../../../../../tools/fake-formationd/fixtures/scripts/question-failed.json";
import "../../i18n";
import { appStore } from "../../store/store";
import { uiStore, type View } from "../../store/ui";
import ViewRouter from "../ViewRouter";
import { AUTH, DESIGN, FRONTEND, JIWOO, MINSU, SEOYEON, loadWorld, play } from "./test-world";

const { call } = vi.hoisted(() => ({ call: vi.fn() }));
vi.mock("../../ipc/transport", () => ({
  tauriTransport: { call: (...args: unknown[]) => call(...args), onEvent: () => () => {}, onStatus: () => () => {} },
}));

const AUTH_VIEW: View = { kind: "teammate-agent", owner: MINSU, agentId: AUTH };
const THREAD = "01930000-0000-7000-8000-000000000301";

/** Clicks and lets the command settle, so its pending state ends. */
async function click(element: HTMLElement) {
  await act(async () => {
    fireEvent.click(element);
  });
}

function open(view: View) {
  render(<ViewRouter />);
  act(() => uiStore.getState().navigate(view));
  return screen.getByRole("region", { name: view.kind === "person" ? "Minsu" : /·/ });
}

beforeEach(() => {
  call.mockReset();
  call.mockResolvedValue({});
  loadWorld();
});

describe("asking a teammate's agent", () => {
  it("answered: shows each question with its answer, source, and excerpts", () => {
    const region = open(AUTH_VIEW);
    expect(region).toHaveAttribute("data-state", "answered");
    expect(region).toHaveAccessibleName("Auth · Minsu");
    expect(within(region).getByText("How far is the login work?")).toHaveClass("me");
    expect(within(region).getByText(/The login API is done/)).toBeInTheDocument();
    expect(within(region).getAllByText(/^Minsu's Auth agent · As of /)).toHaveLength(2);
    expect(within(region).getByText("2 supporting excerpts")).toBeInTheDocument();
    expect(within(region).getByText("1 supporting excerpt")).toBeInTheDocument();
    expect(within(region).queryByText("Possibly out of date")).toBeNull();
    // Pill: agent, owner, and state.
    expect(within(region).getByText("Working")).toHaveAttribute("data-state", "working");
  });

  it("hides excerpts when the answer has none (visibility: answer only)", () => {
    loadWorld((s) => {
      for (const q of s.questions) if (q.answer) q.answer.excerpts = [];
    });
    const region = open(AUTH_VIEW);
    expect(within(region).queryByText(/supporting excerpt/)).toBeNull();
    expect(region.querySelector("details")).toBeNull();
  });

  it("answer actions: refresh, ask Minsu, ask another agent, request work", async () => {
    const region = open(AUTH_VIEW);
    const [firstActs] = region.querySelectorAll(".acts");
    const acts = within(firstActs as HTMLElement);

    await click(acts.getByRole("button", { name: "Refresh" }));
    expect(call).toHaveBeenLastCalledWith("question_refresh", { question_id: THREAD });

    fireEvent.click(acts.getByRole("button", { name: "Request work…" }));
    expect(uiStore.getState().sheet).toEqual({ kind: "work-request", owner: MINSU, agentId: AUTH, fromQuestion: THREAD });

    await click(acts.getByRole("button", { name: "Ask another agent" }));
    const choice = within(region).getByRole("group", { name: "Which agent should answer instead?" });
    await click(within(choice).getByRole("button", { name: /Frontend/ }));
    expect(call).toHaveBeenLastCalledWith("question_route_choose", {
      question_id: THREAD,
      choice: { kind: "agent", agent_id: FRONTEND },
    });

    await act(async () => {
      fireEvent.click(acts.getByRole("button", { name: "Ask Minsu" }));
    });
    expect(call).toHaveBeenLastCalledWith("ask", {
      target: { kind: "person", member_id: MINSU },
      text: "How far is the login work?",
      thread_id: null,
      from_run: null,
    });
    expect(uiStore.getState().view).toEqual({ kind: "person", memberId: MINSU });
  });

  it("opens Request work… from the header when the agent accepts requests", () => {
    const region = open(AUTH_VIEW);
    const head = region.querySelector(".c-head") as HTMLElement;
    fireEvent.click(within(head).getByRole("button", { name: "Request work…" }));
    expect(uiStore.getState().sheet).toEqual({ kind: "work-request", owner: MINSU, agentId: AUTH, fromQuestion: null });
  });

  it("sends a follow-up in the thread with ⌘↩ and shows the pending state", async () => {
    let resolve: (value: unknown) => void = () => {};
    call.mockReturnValue(new Promise((r) => (resolve = r)));
    const region = open(AUTH_VIEW);
    const input = within(region).getByRole("textbox", { name: "Question" });
    const send = within(region).getByRole("button", { name: "Send ⌘↩" });
    expect(send).toBeDisabled();

    fireEvent.change(input, { target: { value: "  Any blockers?  " } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(call).not.toHaveBeenCalled();
    fireEvent.keyDown(input, { key: "Enter", metaKey: true });
    expect(call).toHaveBeenCalledWith("ask", {
      target: { kind: "agent", member_id: MINSU, agent_id: AUTH },
      text: "Any blockers?",
      thread_id: THREAD,
      from_run: null,
    });
    expect(send).toBeDisabled();
    expect(region.querySelector(".composer")).toHaveAttribute("data-state", "pending");

    await act(async () => resolve({}));
    expect(input).toHaveValue("");
    expect(region.querySelector(".composer")).not.toHaveAttribute("data-state");
  });

  it("received: shows that the question arrived", () => {
    play(authReceived);
    const region = open(AUTH_VIEW);
    expect(region).toHaveAttribute("data-state", "received");
    expect(within(region).getByRole("status")).toHaveTextContent("Received · preparing an answer");
  });

  it("answering: says the agent answers without interrupting the owner", () => {
    play(authAnswering);
    const region = open(AUTH_VIEW);
    expect(region).toHaveAttribute("data-state", "answering");
    expect(within(region).getByRole("status")).toHaveTextContent(
      "The Auth agent is answering… Minsu's work isn't interrupted",
    );
  });

  it("stale: marks a possibly out-of-date answer", () => {
    play(authStale);
    const region = open(AUTH_VIEW);
    expect(region).toHaveAttribute("data-state", "stale");
    expect(within(region).getByText("Possibly out of date")).toBeInTheDocument();
  });

  it("unshared: warns, disables the composer, keeps past messages readable", () => {
    play(authUnshared);
    const region = open(AUTH_VIEW);
    expect(region).toHaveAttribute("data-state", "unshared");
    expect(within(region).getByText("This agent is no longer shared")).toBeInTheDocument();
    expect(within(region).getByRole("textbox", { name: "Question" })).toBeDisabled();
    expect(within(region).getByRole("textbox", { name: "Question" })).toHaveAttribute("placeholder", "Sharing is off");
    expect(within(region).getByText("How far is the login work?")).toBeInTheDocument();
    expect(within(region).getByText(/The login API is done/)).toBeInTheDocument();
    const head = region.querySelector(".c-head") as HTMLElement;
    expect(within(head).queryByRole("button", { name: "Request work…" })).toBeNull();
    for (const refresh of within(region).getAllByRole("button", { name: "Refresh" })) expect(refresh).toBeDisabled();
  });

  it("unshared: also when ask is refused with agent_not_shared", async () => {
    call.mockRejectedValue({ code: "rpc", message: "x", rpc_code: 1, data: { code: "agent_not_shared", message: "x" } });
    const region = open(AUTH_VIEW);
    const input = within(region).getByRole("textbox", { name: "Question" });
    fireEvent.change(input, { target: { value: "Still there?" } });
    await act(async () => {
      fireEvent.keyDown(input, { key: "Enter", metaKey: true });
    });
    expect(region).toHaveAttribute("data-state", "unshared");
    expect(input).toBeDisabled();
    expect(input).toHaveValue("Still there?");
  });

  it("error: shows the failure and retries with the same text", () => {
    play(questionFailed);
    const region = open(AUTH_VIEW);
    expect(region).toHaveAttribute("data-state", "error");
    const card = within(region).getByRole("alert");
    expect(card).toHaveTextContent("Couldn't get an answer");
    expect(card).toHaveTextContent("The connection dropped. Your question is saved.");
    fireEvent.click(within(card).getByRole("button", { name: "Retry" }));
    expect(call).toHaveBeenLastCalledWith("ask", {
      target: { kind: "agent", member_id: MINSU, agent_id: AUTH },
      text: "How far is the login work?",
      thread_id: THREAD,
      from_run: null,
    });
  });

  it("shows a banner with Try again when an action fails", async () => {
    call.mockRejectedValueOnce({ code: "disconnected", message: "x", rpc_code: null, data: null });
    const region = open(AUTH_VIEW);
    await act(async () => {
      fireEvent.click(within(region).getAllByRole("button", { name: "Refresh" })[0]);
    });
    const banner = within(region).getByRole("alert");
    expect(banner).toHaveTextContent("That didn't go through");
    await act(async () => {
      fireEvent.click(within(banner).getByRole("button", { name: "Try again" }));
    });
    expect(call).toHaveBeenCalledTimes(2);
    expect(within(region).queryByRole("alert")).toBeNull();
  });

  it("offline-waiting: offers Cancel question and Ask again", async () => {
    render(<ViewRouter />);
    act(() => uiStore.getState().navigate({ kind: "teammate-agent", owner: JIWOO, agentId: DESIGN }));
    const region = screen.getByRole("region", { name: "Design · Jiwoo" });
    expect(region).toHaveAttribute("data-state", "offline-waiting");
    expect(within(region).getByText("Jiwoo's Mac is offline")).toBeInTheDocument();
    await click(within(region).getByRole("button", { name: "Cancel question" }));
    expect(call).toHaveBeenLastCalledWith("question_cancel", { question_id: "01930000-0000-7000-8000-000000000304" });
    fireEvent.click(within(region).getByRole("button", { name: "Ask again" }));
    expect(call).toHaveBeenLastCalledWith("ask", {
      target: { kind: "agent", member_id: JIWOO, agent_id: DESIGN },
      text: "Are the design mockups updated?",
      thread_id: "01930000-0000-7000-8000-000000000304",
      from_run: null,
    });
  });
});

describe("asking a person", () => {
  const PERSON: View = { kind: "person", memberId: MINSU };
  const UNSURE = "01930000-0000-7000-8000-000000000303";

  it("unsure: lists candidates best guess first, plus Ask Minsu directly", async () => {
    const region = open(PERSON);
    expect(region).toHaveAttribute("data-state", "unsure");
    expect(within(region).getByText("Asking a person · Formation picks the agent")).toBeInTheDocument();
    const card = within(region).getByRole("group", { name: "Not sure which agent should answer" });
    const options = within(card).getAllByRole("button");
    expect(options.map((o) => o.textContent)).toEqual([
      "AuthRecently worked on the deploy scriptBest guess",
      "FrontendOwns web deploy settings",
      "QAPre-release tests",
      "Ask Minsu directlyGoes to Minsu's Inbox",
    ]);
    expect(options[0]).toHaveClass("best");

    await click(options[0]);
    expect(call).toHaveBeenLastCalledWith("question_route_choose", {
      question_id: UNSURE,
      choice: { kind: "agent", agent_id: AUTH },
    });
    fireEvent.click(options[3]);
    expect(call).toHaveBeenLastCalledWith("question_route_choose", { question_id: UNSURE, choice: { kind: "owner" } });
  });

  it("sent-to-owner: says the question went to Minsu", () => {
    play(personSent);
    const region = open(PERSON);
    expect(region).toHaveAttribute("data-state", "sent-to-owner");
    expect(within(region).getByText("Sent to Minsu")).toBeInTheDocument();
  });

  it("asks the person with the composer", () => {
    const region = open(PERSON);
    const input = within(region).getByRole("textbox", { name: "Question" });
    expect(input).toHaveAttribute("placeholder", "Ask Minsu");
    fireEvent.change(input, { target: { value: "Who owns deploys?" } });
    fireEvent.click(within(region).getByRole("button", { name: "Send ⌘↩" }));
    expect(call).toHaveBeenLastCalledWith("ask", {
      target: { kind: "person", member_id: MINSU },
      text: "Who owns deploys?",
      thread_id: UNSURE,
      from_run: null,
    });
  });
});

describe("new question", () => {
  it("new: picks a person or a shared agent", () => {
    render(<ViewRouter />);
    act(() => uiStore.getState().navigate({ kind: "new-question" }));
    const region = screen.getByRole("region", { name: "New question" });
    expect(region).toHaveAttribute("data-state", "new");
    expect(within(region).getByText("Who do you want to ask?")).toBeInTheDocument();
    const people = within(region).getByRole("group", { name: "People" });
    expect(within(people).getAllByRole("button").map((b) => b.textContent)).toEqual(["Minsu", "Jiwoo", "Seoyeon"]);
    const agents = within(region).getByRole("group", { name: "Shared agents" });
    expect(within(agents).getAllByRole("button").map((b) => b.textContent)).toEqual([
      "Minsu · Auth",
      "Minsu · Frontend",
      "Minsu · QA",
      "Jiwoo · Design",
    ]);

    fireEvent.click(within(people).getByRole("button", { name: "Seoyeon" }));
    expect(uiStore.getState().view).toEqual({ kind: "person", memberId: SEOYEON });
    act(() => uiStore.getState().navigate({ kind: "new-question" }));
    fireEvent.click(within(screen.getByRole("group", { name: "Shared agents" })).getByRole("button", { name: "Minsu · Auth" }));
    expect(uiStore.getState().view).toEqual(AUTH_VIEW);
    expect(appStore.getState().mirror).not.toBeNull();
  });
});
