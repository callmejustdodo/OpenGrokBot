import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import "../../i18n";
import { loadWorld } from "../ask-conversation/test-world";
import Welcome from "./Welcome";

const { call } = vi.hoisted(() => ({ call: vi.fn() }));
vi.mock("../../ipc/transport", () => ({
  tauriTransport: { call: (...args: unknown[]) => call(...args), onEvent: () => () => {}, onStatus: () => () => {} },
}));

async function click(element: HTMLElement) {
  await act(async () => {
    fireEvent.click(element);
  });
}

beforeEach(() => {
  call.mockReset();
  call.mockResolvedValue({});
  loadWorld((s) => {
    s.team = null;
  });
});

describe("welcome", () => {
  it("default: harnesses ready, create a team", async () => {
    render(<Welcome />);
    const page = screen.getByRole("region", { name: "Welcome" });
    expect(page).toHaveAttribute("data-state", "default");
    const checks = within(page).getAllByRole("listitem");
    expect(checks.map((c) => c.textContent)).toEqual([
      "Claude Code · Installed, signed in",
      "Codex · Installed, signed in",
    ]);
    await click(within(page).getByRole("button", { name: "Create a team" }));
    expect(call).toHaveBeenCalledWith("team_create", { display_name: null });
  });

  it("harness-missing: setup continues, and Check again refreshes", async () => {
    loadWorld((s) => {
      s.team = null;
      s.harnesses[1] = { ...s.harnesses[1], installed: false, signed_in: null, version: null, path: null };
    });
    render(<Welcome />);
    const page = screen.getByRole("region", { name: "Welcome" });
    expect(page).toHaveAttribute("data-state", "harness-missing");
    expect(within(page).getByText("Codex · Not installed — Codex agents can run after you install it")).toHaveAttribute(
      "data-state",
      "missing",
    );
    expect(within(page).getByRole("button", { name: "Create a team" })).toBeEnabled();
    await click(within(page).getByRole("button", { name: "Check again" }));
    expect(call).toHaveBeenCalledWith("harness_check", {});
  });

  it("joins with a code", async () => {
    render(<Welcome />);
    const input = screen.getByRole("textbox", { name: "Invitation code" });
    expect(screen.getByRole("button", { name: "Join" })).toBeDisabled();
    fireEvent.change(input, { target: { value: " formation://join/7F3K-Q9PX " } });
    await click(screen.getByRole("button", { name: "Join" }));
    expect(call).toHaveBeenCalledWith("team_join", { code: "formation://join/7F3K-Q9PX", display_name: null });
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("invite-expired: explains how to get a new link", async () => {
    call.mockRejectedValue({ code: "rpc", message: "expired", rpc_code: -32000, data: { code: "not_found", message: "expired" } });
    render(<Welcome />);
    const input = screen.getByRole("textbox", { name: "Invitation code" });
    fireEvent.change(input, { target: { value: "formation://join/OLD" } });
    await click(screen.getByRole("button", { name: "Join" }));
    expect(screen.getByRole("region", { name: "Welcome" })).toHaveAttribute("data-state", "invite-expired");
    expect(screen.getByRole("alert")).toHaveTextContent("This link has expired. Ask your teammate for a new one.");
    expect(input).toHaveAttribute("aria-invalid", "true");
    fireEvent.change(input, { target: { value: "formation://join/NEW" } });
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it.each([
    ["rate_limited", "Too many tries. Wait a few minutes, then try again."],
    ["peer_offline", "The team creator's Mac is offline. Try again when it's back."],
  ])("explains a %s join error", async (code, text) => {
    call.mockRejectedValue({ code: "rpc", message: "x", rpc_code: -32000, data: { code, message: "x" } });
    render(<Welcome />);
    fireEvent.change(screen.getByRole("textbox", { name: "Invitation code" }), { target: { value: "code" } });
    await click(screen.getByRole("button", { name: "Join" }));
    expect(screen.getByRole("alert")).toHaveTextContent(text);
  });

  it("says when formationd could not be reached while joining", async () => {
    call.mockRejectedValue({ code: "disconnected", message: "x", rpc_code: null, data: null });
    render(<Welcome />);
    fireEvent.change(screen.getByRole("textbox", { name: "Invitation code" }), { target: { value: "code" } });
    await click(screen.getByRole("button", { name: "Join" }));
    expect(screen.getByRole("alert")).toHaveTextContent("Couldn't reach formationd. Try again.");
    expect(screen.getByRole("region", { name: "Welcome" })).toHaveAttribute("data-state", "default");
  });

  it("turns notifications off and on with settings_update", async () => {
    render(<Welcome />);
    const toggle = screen.getByRole("switch", { name: "Notifications" });
    expect(toggle).toBeChecked();
    await click(toggle);
    expect(call).toHaveBeenCalledWith("settings_update", {
      settings: { language: null, notifications: { enabled: false, muted_until: null } },
    });
  });
});
