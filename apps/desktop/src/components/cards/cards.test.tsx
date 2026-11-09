import { act, fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import "../../i18n";
import type { Approval } from "../../ipc/generated/Approval";
import { appStore } from "../../store/store";
import { uiStore } from "../../store/ui";
import { apply, loadWorld, MINSU_APPROVAL, MINSU_REQUEST, PERMISSION } from "../../views/approval/m3World";
import PermissionCard from "./PermissionCard";
import RequestCard from "./RequestCard";

const { call } = vi.hoisted(() => ({ call: vi.fn() }));
vi.mock("../../ipc/transport", () => ({
  tauriTransport: { call: (...args: unknown[]) => call(...args), onEvent: () => () => {}, onStatus: () => () => {} },
}));

beforeEach(() => {
  call.mockReset();
  call.mockResolvedValue({});
  uiStore.setState({ view: { kind: "home" }, sheet: null });
});

describe("RequestCard (SC-005)", () => {
  it("working: Minsu's request runs after this turn", () => {
    loadWorld();
    render(<RequestCard requestId={MINSU_REQUEST} />);
    const card = screen.getByRole("region", { name: "Work request from Minsu" });
    expect(card).toHaveTextContent("Add token expiry tests, both just before and just after expiry.");
    expect(card).toHaveTextContent("Runs after this turn");
    expect(screen.queryByRole("button", { name: "Review" })).toBeNull();
  });

  it("approval: 1 approval waiting, Review opens the approval sheet", () => {
    loadWorld(["approval-raised"]);
    render(<RequestCard requestId={MINSU_REQUEST} />);
    const card = screen.getByRole("region", { name: "Work request from Minsu" });
    expect(card).toHaveTextContent("1 approval waiting");
    expect(card).toHaveTextContent("npm test -- auth/token.spec.ts");
    fireEvent.click(screen.getByRole("button", { name: "Review" }));
    expect(uiStore.getState().sheet).toEqual({ kind: "approval", approvalId: MINSU_APPROVAL });
  });

  it("shows the request's later status", () => {
    loadWorld(["approval-raised", "approval-expired"]);
    render(<RequestCard requestId={MINSU_REQUEST} />);
    expect(screen.getByRole("region")).toHaveTextContent("Expired");
    expect(screen.queryByRole("button", { name: "Review" })).toBeNull();
  });
});

describe("PermissionCard (SC-005)", () => {
  it("approval: a teammate's approval is a compact row with Review", () => {
    loadWorld(["approval-raised"]);
    render(<PermissionCard approvalId={MINSU_APPROVAL} />);
    expect(screen.getByRole("region")).toHaveTextContent("Approval waiting");
    fireEvent.click(screen.getByRole("button", { name: "Review" }));
    expect(uiStore.getState().sheet).toEqual({ kind: "approval", approvalId: MINSU_APPROVAL });
  });

  it("permission: Claude Code's own prompt with its choices", async () => {
    loadWorld(["backend-permission"]);
    render(<PermissionCard approvalId={PERMISSION} />);
    const card = screen.getByRole("region", { name: "Claude Code asks for permission" });
    expect(card).toHaveTextContent("Delete the build folder and rebuild");
    expect(card).toHaveTextContent("rm -rf dist/ && npm run build");
    expect(card).toHaveTextContent("Teammates' requests follow Formation's approval rules.");

    const choices: [string, string][] = [
      ["Allow", "allow_once"],
      ["Allow for this session", "allow_all"],
      ["Deny", "deny"],
    ];
    for (const [name, choice] of choices) {
      await act(async () => fireEvent.click(screen.getByRole("button", { name })));
      expect(call).toHaveBeenLastCalledWith("approval_decide", { approval_id: PERMISSION, choice, rule: null });
    }
  });

  it("disables the choices while the decision is on its way", () => {
    loadWorld(["backend-permission"]);
    act(() => appStore.getState().beginPending(`approval_decide:${PERMISSION}`));
    render(<PermissionCard approvalId={PERMISSION} />);
    expect(screen.getByRole("button", { name: "Allow" })).toBeDisabled();
  });

  it("shows the decision once made", () => {
    loadWorld(["backend-permission"]);
    render(<PermissionCard approvalId={PERMISSION} />);
    const approval = appStore.getState().mirror!.approvals[PERMISSION] as Approval;
    act(() =>
      apply({
        type: "approval.update",
        payload: { approval: { ...approval, status: "allowed", choice: "allow_all", decided_at: "2026-11-09T01:54:00Z" } },
      }),
    );
    expect(screen.getByRole("region")).toHaveTextContent("Allowed for this session");
    expect(screen.queryByRole("button")).toBeNull();
  });
});
