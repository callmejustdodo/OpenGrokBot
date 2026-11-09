import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import "../../i18n";
import type { WorkRequest } from "../../ipc/generated/WorkRequest";
import { appStore } from "../../store/store";
import { uiStore, type Sheet } from "../../store/ui";
import SheetHost from "../SheetHost";
import { apply, AUTH, loadWorld, MINSU, MINSU_REQUEST, MY_REQUEST, QA } from "../approval/m3World";

const { call } = vi.hoisted(() => ({ call: vi.fn() }));
vi.mock("../../ipc/transport", () => ({
  tauriTransport: { call: (...args: unknown[]) => call(...args), onEvent: () => () => {}, onStatus: () => () => {} },
}));

const FROM_QUESTION = "01930000-0000-7000-8000-000000000302";

function open(sheet: Sheet) {
  render(<SheetHost />);
  act(() => uiStore.getState().openSheet(sheet));
  return screen.getByRole("dialog");
}

function updateRequest(id: string, change: Partial<WorkRequest>) {
  const request = appStore.getState().mirror!.requests[id] as WorkRequest;
  apply({ type: "request.update", payload: { request: { ...request, ...change } } });
}

beforeEach(() => {
  call.mockReset();
  uiStore.setState({ view: { kind: "home" }, sheet: null });
});

describe("work request compose (SC-016)", () => {
  it("compose: target, messages from the conversation, and the exact prompt", async () => {
    loadWorld();
    const sheet = open({ kind: "work-request", owner: MINSU, agentId: AUTH, fromQuestion: FROM_QUESTION });
    expect(sheet).toHaveTextContent("To: Minsu's Auth agent · accepts work requests");
    expect(sheet).toHaveTextContent("Runs after the current turn; actions follow Minsu's approval rules.");

    const send = within(sheet).getByRole("button", { name: "Send" });
    expect(send).toBeDisabled();

    const include = within(sheet).getByRole("group", { name: "Include messages from the conversation" });
    const boxes = within(include).getAllByRole("checkbox");
    expect(boxes).toHaveLength(4);
    expect(boxes.map((b) => (b as HTMLInputElement).checked)).toEqual([false, false, false, true]);
    expect(include).toHaveTextContent("Me: How does the token refresh work?");

    fireEvent.change(within(sheet).getByRole("textbox", { name: "Request" }), {
      target: { value: "Add token expiry tests, both just before and just after expiry." },
    });
    const prompt =
      "Add token expiry tests, both just before and just after expiry.\n\nContext:\n" +
      "- Auth: Rotation: each time a refresh token is used it's replaced, and the old one stops working at once.";
    expect(within(sheet).getByLabelText("Exact prompt to be delivered").textContent).toBe(prompt);

    fireEvent.click(boxes[3]);
    expect(within(sheet).getByLabelText("Exact prompt to be delivered").textContent).toBe(
      "Add token expiry tests, both just before and just after expiry.",
    );
    fireEvent.click(boxes[3]);

    call.mockResolvedValue({ id: "new-request" });
    await act(async () => fireEvent.click(send));
    expect(call).toHaveBeenCalledWith("request_create", {
      owner: MINSU,
      agent_id: AUTH,
      prompt,
      idempotency_key: expect.stringMatching(/^[0-9a-f-]{36}$/),
      from_question: FROM_QUESTION,
      from_run: null,
    });
    expect(uiStore.getState().sheet).toEqual({ kind: "request-status", requestId: "new-request" });
  });

  it("keeps the idempotency key when sending again after a failure", async () => {
    loadWorld();
    const sheet = open({ kind: "work-request", owner: MINSU, agentId: AUTH, fromQuestion: null });
    expect(within(sheet).queryByRole("checkbox")).toBeNull();
    fireEvent.change(within(sheet).getByRole("textbox", { name: "Request" }), { target: { value: "Run the tests" } });
    call.mockRejectedValueOnce({ code: "rpc", message: "x", rpc_code: 1, data: { code: "peer_offline", message: "x" } });
    await act(async () => fireEvent.click(within(sheet).getByRole("button", { name: "Send" })));
    expect(screen.getByRole("alert")).toHaveTextContent("Minsu's Mac is offline.");
    call.mockResolvedValue({ id: "new-request" });
    await act(async () => fireEvent.click(within(sheet).getByRole("button", { name: "Send" })));
    expect(call.mock.calls[1][1].idempotency_key).toBe(call.mock.calls[0][1].idempotency_key);
  });

  it("not-accepted: QA doesn't accept work requests", () => {
    loadWorld();
    const sheet = open({ kind: "work-request", owner: MINSU, agentId: QA, fromQuestion: null });
    expect(sheet).toHaveTextContent("QA doesn't accept work requests");
    expect(sheet).toHaveTextContent("Minsu turned off work requests for this agent. You can still ask questions.");
    expect(within(sheet).getByRole("button", { name: "Send" })).toBeDisabled();
    expect(within(sheet).queryByRole("textbox")).toBeNull();
  });

  it("not-accepted: switches when formationd refuses the request", async () => {
    loadWorld();
    const sheet = open({ kind: "work-request", owner: MINSU, agentId: AUTH, fromQuestion: null });
    fireEvent.change(within(sheet).getByRole("textbox", { name: "Request" }), { target: { value: "Run the tests" } });
    call.mockRejectedValue({ code: "rpc", message: "x", rpc_code: 1, data: { code: "requests_not_accepted", message: "x" } });
    await act(async () => fireEvent.click(within(sheet).getByRole("button", { name: "Send" })));
    expect(screen.getByRole("dialog")).toHaveTextContent("Auth doesn't accept work requests");
  });
});

describe("request status (SC-016)", () => {
  function steps() {
    return within(screen.getByRole("list", { name: "Progress" }))
      .getAllByRole("listitem")
      .map((li) => `${li.className}:${li.getAttribute("data-step")}`);
  }

  it("status: running, with the earlier steps done and no cancel", () => {
    loadWorld(["request-status"]);
    const sheet = open({ kind: "request-status", requestId: MY_REQUEST });
    expect(within(sheet).getByRole("heading", { name: "Request status" })).toBeInTheDocument();
    expect(sheet).toHaveTextContent("Minsu's Auth agent · Add token expiry tests, both just before and just after expiry.");
    expect(steps()).toEqual([
      "done:received",
      "done:awaiting_approval",
      "done:queued",
      "now:running",
      "todo:completed",
    ]);
    expect(sheet.querySelector('[aria-current="step"]')).toHaveTextContent("Running");
    expect(within(sheet).getByRole("button", { name: "Cancel request" })).toBeDisabled();
    expect(sheet).toHaveTextContent("Started requests can't be canceled");
  });

  it("cancels a request that has not started", async () => {
    loadWorld();
    const sheet = open({ kind: "request-status", requestId: MINSU_REQUEST });
    expect(steps()[2]).toBe("now:queued");
    expect(sheet).not.toHaveTextContent("Started requests can't be canceled");
    call.mockResolvedValue({});
    await act(async () => fireEvent.click(within(sheet).getByRole("button", { name: "Cancel request" })));
    expect(call).toHaveBeenCalledWith("request_cancel", { request_id: MINSU_REQUEST });
  });

  it("shows the result when completed", () => {
    loadWorld(["request-status"]);
    updateRequest(MY_REQUEST, { status: "completed", result: "Added two tests; both pass." });
    const sheet = open({ kind: "request-status", requestId: MY_REQUEST });
    expect(steps()).toEqual(["done:received", "done:awaiting_approval", "done:queued", "done:running", "done:completed"]);
    expect(sheet).toHaveTextContent("ResultAdded two tests; both pass.");
  });

  it("ends with denied and its reason", () => {
    loadWorld(["request-status"]);
    updateRequest(MY_REQUEST, { status: "denied", error: { code: "denied", message: "x" } });
    const sheet = open({ kind: "request-status", requestId: MY_REQUEST });
    expect(steps()).toEqual(["done:received", "done:awaiting_approval", "done:denied"]);
    expect(sheet).toHaveTextContent("The owner denied an action this request needed.");
    expect(within(sheet).getByRole("button", { name: "Cancel request" })).toBeDisabled();
  });

  it("ends with canceled", () => {
    loadWorld(["request-status"]);
    updateRequest(MY_REQUEST, { status: "canceled" });
    const sheet = open({ kind: "request-status", requestId: MY_REQUEST });
    expect(steps()).toEqual(["done:received", "done:canceled"]);
    expect(sheet).not.toHaveTextContent("Started requests can't be canceled");
  });

  it("ends with failed or expired", () => {
    loadWorld(["request-status"]);
    updateRequest(MY_REQUEST, { status: "failed", error: { code: "internal", message: "x" } });
    open({ kind: "request-status", requestId: MY_REQUEST });
    expect(steps()).toEqual(["done:received", "done:failed"]);
    act(() => updateRequest(MY_REQUEST, { status: "expired", error: { code: "approval_expired", message: "x" } }));
    expect(steps()).toEqual(["done:received", "done:awaiting_approval", "done:expired"]);
  });
});
