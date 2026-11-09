import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "../../i18n";
import { uiStore } from "../../store/ui";
import SheetHost from "../SheetHost";
import { loadWorld, M3_NOW, MINSU_APPROVAL, type ScriptName } from "./m3World";

const { call } = vi.hoisted(() => ({ call: vi.fn() }));
vi.mock("../../ipc/transport", () => ({
  tauriTransport: { call: (...args: unknown[]) => call(...args), onEvent: () => () => {}, onStatus: () => () => {} },
}));

function openApproval(scripts: ScriptName[] = ["approval-raised"]) {
  loadWorld(scripts);
  render(<SheetHost />);
  act(() => uiStore.getState().openSheet({ kind: "approval", approvalId: MINSU_APPROVAL }));
  return screen.getByRole("dialog");
}

function choice(name: RegExp) {
  return screen.getByRole("button", { name });
}

function pressEscape() {
  fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
}

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["Date"], now: M3_NOW });
  call.mockReset();
  call.mockResolvedValue({});
  uiStore.setState({ view: { kind: "inbox" }, sheet: null });
});

afterEach(() => vi.useRealTimers());

describe("approval sheet (SC-011)", () => {
  it("default: shows who asked, the target agent, purpose, the exact command and folder, and time left", () => {
    const sheet = openApproval();
    expect(within(sheet).getByRole("heading", { name: "Approval needed" })).toBeInTheDocument();
    expect(sheet).toHaveTextContent("23 h 57 min left");
    expect(sheet).toHaveTextContent("Minsu · sent by Minsu's agent via CLI");
    expect(sheet).toHaveTextContent("Backend");
    expect(sheet).toHaveTextContent("Run tests while handling the request to add token expiry tests");
    expect(within(sheet).getByText("npm test -- auth/token.spec.ts").closest("pre")).toHaveClass("code");
    expect(sheet).toHaveTextContent("~/Codes/shared-api");
    expect(sheet.querySelector("[data-state]")).toHaveAttribute("data-state", "default");
    for (const name of [/Allow once/, /Keep allowing similar commands/, /Allow all for Minsu · Backend/, /^Deny/]) {
      expect(choice(name)).toBeEnabled();
    }
  });

  it("allows once and closes", async () => {
    openApproval();
    await act(async () => fireEvent.click(choice(/Allow once/)));
    expect(call).toHaveBeenCalledWith("approval_decide", {
      approval_id: MINSU_APPROVAL,
      choice: "allow_once",
      rule: null,
    });
    expect(uiStore.getState().sheet).toBeNull();
  });

  it("allows all for this requester and agent", async () => {
    openApproval();
    await act(async () => fireEvent.click(choice(/Allow all for Minsu · Backend/)));
    expect(call).toHaveBeenCalledWith("approval_decide", {
      approval_id: MINSU_APPROVAL,
      choice: "allow_all",
      rule: { scope: { kind: "all" }, expires_at: null, max_uses: null },
    });
  });

  it("binds no choice to Return", () => {
    openApproval();
    for (const name of [/Allow once/, /Keep allowing similar commands/, /Allow all for Minsu · Backend/, /^Deny/]) {
      const button = choice(name);
      button.focus();
      // `false` means the default action (activating the button) was prevented.
      expect(fireEvent.keyDown(button, { key: "Enter" })).toBe(false);
    }
    expect(call).not.toHaveBeenCalled();
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(uiStore.getState().sheet).not.toBeNull();
  });

  it("deny-confirm: Esc asks before denying instead of closing", async () => {
    openApproval();
    pressEscape();
    const alert = screen.getByRole("alertdialog", { name: "Deny this action?" });
    expect(alert).toHaveTextContent("Minsu will be told it was denied. The agent won't run this command.");
    expect(uiStore.getState().sheet).not.toBeNull();

    fireEvent.click(within(alert).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(call).not.toHaveBeenCalled();
    expect(uiStore.getState().sheet).not.toBeNull();

    pressEscape();
    const again = screen.getByRole("alertdialog");
    expect(fireEvent.keyDown(within(again).getByRole("button", { name: "Deny" }), { key: "Enter" })).toBe(false);
    expect(call).not.toHaveBeenCalled();
    await act(async () => fireEvent.click(within(again).getByRole("button", { name: "Deny" })));
    expect(call).toHaveBeenCalledWith("approval_decide", { approval_id: MINSU_APPROVAL, choice: "deny", rule: null });
    expect(uiStore.getState().sheet).toBeNull();
  });

  it("deny-confirm: the Deny choice asks first", () => {
    openApproval();
    fireEvent.click(choice(/^Deny/));
    expect(screen.getByRole("alertdialog", { name: "Deny this action?" })).toBeInTheDocument();
    expect(call).not.toHaveBeenCalled();
  });

  it("expiring: under an hour left", () => {
    const sheet = openApproval(["approval-raised", "approval-expiring"]);
    expect(sheet).toHaveTextContent("Expires in 40 min");
    expect(sheet.querySelector("[data-state]")).toHaveAttribute("data-state", "expiring");
    expect(choice(/Allow once/)).toBeEnabled();
  });

  it("expired: a banner and Close only; Esc just closes", () => {
    const sheet = openApproval(["approval-raised", "approval-expired"]);
    expect(sheet).toHaveTextContent("This request expired");
    expect(sheet).toHaveTextContent("No answer for 24 hours; Minsu was told.");
    expect(within(sheet).getAllByRole("button").map((b) => b.textContent)).toEqual(["Close"]);
    pressEscape();
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(uiStore.getState().sheet).toBeNull();
  });

  it("shows an error and stays open when the decision fails", async () => {
    call.mockRejectedValue({ code: "rpc", message: "x", rpc_code: 1, data: { code: "approval_expired", message: "x" } });
    openApproval();
    await act(async () => fireEvent.click(choice(/Allow once/)));
    expect(screen.getByRole("alert")).toHaveTextContent("This request expired before your choice arrived.");
    expect(uiStore.getState().sheet).not.toBeNull();
  });
});

describe("keep-similar state (TASK-029)", () => {
  it("shows the proposed rule and goes back", () => {
    const sheet = openApproval();
    fireEvent.click(choice(/Keep allowing similar commands/));
    expect(sheet.querySelector("[data-state]")).toHaveAttribute("data-state", "keep-similar");
    expect(sheet).toHaveTextContent("Rule to save");
    expect(sheet).toHaveTextContent("RequesterMinsu");
    expect(sheet).toHaveTextContent("AgentBackend");
    expect(sheet).toHaveTextContent("Programnpm");
    expect(screen.getByLabelText("Arguments")).toHaveValue("test -- auth/*");
    expect(sheet).toHaveTextContent("Folder~/Codes/shared-api");
    expect(screen.getByLabelText("Expires")).toHaveDisplayValue("In 7 days");
    expect(sheet).toHaveTextContent("Never allows chaining (&&, ;) or other folders.");
    expect(screen.queryByRole("button", { name: /Allow once/ })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(choice(/Allow once/)).toBeInTheDocument();
  });

  it("saves the edited rule and allows", async () => {
    openApproval();
    fireEvent.click(choice(/Keep allowing similar commands/));
    fireEvent.change(screen.getByLabelText("Arguments"), { target: { value: "test -- auth/token*" } });
    fireEvent.change(screen.getByLabelText("Expires"), { target: { value: "never" } });
    const save = screen.getByRole("button", { name: "Save rule and allow" });
    expect(fireEvent.keyDown(save, { key: "Enter" })).toBe(false);
    expect(call).not.toHaveBeenCalled();
    await act(async () => fireEvent.click(save));
    expect(call).toHaveBeenCalledWith("approval_decide", {
      approval_id: MINSU_APPROVAL,
      choice: "keep_allowing_similar",
      rule: {
        scope: { kind: "command", program: "npm", arg_pattern: "test -- auth/token*", folder: "~/Codes/shared-api" },
        expires_at: null,
        max_uses: null,
      },
    });
    expect(uiStore.getState().sheet).toBeNull();
  });

  it("keeps the proposed expiry by default", async () => {
    openApproval();
    fireEvent.click(choice(/Keep allowing similar commands/));
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Save rule and allow" })));
    expect(call).toHaveBeenCalledWith("approval_decide", {
      approval_id: MINSU_APPROVAL,
      choice: "keep_allowing_similar",
      rule: {
        scope: { kind: "command", program: "npm", arg_pattern: "test -- auth/*", folder: "~/Codes/shared-api" },
        expires_at: "2026-11-16T01:53:00Z",
        max_uses: null,
      },
    });
  });

  it("refuses chained or empty arguments", () => {
    openApproval();
    fireEvent.click(choice(/Keep allowing similar commands/));
    const args = screen.getByLabelText("Arguments");
    fireEvent.change(args, { target: { value: "test && rm -rf /" } });
    expect(screen.getByRole("button", { name: "Save rule and allow" })).toBeDisabled();
    fireEvent.change(args, { target: { value: "  " } });
    expect(screen.getByRole("button", { name: "Save rule and allow" })).toBeDisabled();
  });

  it("Esc still asks before denying", () => {
    openApproval();
    fireEvent.click(choice(/Keep allowing similar commands/));
    pressEscape();
    expect(screen.getByRole("alertdialog", { name: "Deny this action?" })).toBeInTheDocument();
    expect(uiStore.getState().sheet).not.toBeNull();
  });
});
