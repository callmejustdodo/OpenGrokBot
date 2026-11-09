import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import "../../i18n";
import { appStore } from "../../store/store";
import { uiStore } from "../../store/ui";
import { BACKEND, MINSU, loadWorld } from "../ask-conversation/test-world";
import SharePreviewSheet from "./SharePreviewSheet";
import SharingSection from "./SharingSection";

const { call } = vi.hoisted(() => ({ call: vi.fn() }));
vi.mock("../../ipc/transport", () => ({
  tauriTransport: { call: (...args: unknown[]) => call(...args), onEvent: () => () => {}, onStatus: () => () => {} },
}));

const MARKETING = "01930000-0000-7000-8000-000000000102";

async function click(element: HTMLElement) {
  await act(async () => {
    fireEvent.click(element);
  });
}

beforeEach(() => {
  call.mockReset();
  call.mockResolvedValue({});
  loadWorld();
});

describe("sharing section", () => {
  it("shared: shows visibility, work requests, and preview", async () => {
    render(<SharingSection agentId={BACKEND} />);
    const section = screen.getByRole("region", { name: "Sharing" });
    expect(section).toHaveAttribute("data-state", "shared");
    expect(within(section).getByRole("switch", { name: /Share with team/ })).toBeChecked();
    expect(within(section).getByRole("radio", { name: "Answer and excerpts" })).toBeChecked();
    expect(within(section).getByRole("switch", { name: /Accept work requests/ })).toBeChecked();

    await click(within(section).getByRole("radio", { name: "Answer only" }));
    expect(call).toHaveBeenLastCalledWith("share_update", {
      agent_id: BACKEND,
      shared: null,
      visibility: "answer",
      accepts_requests: null,
    });
    await click(within(section).getByRole("switch", { name: /Accept work requests/ }));
    expect(call).toHaveBeenLastCalledWith("share_update", {
      agent_id: BACKEND,
      shared: null,
      visibility: null,
      accepts_requests: false,
    });
    fireEvent.click(within(section).getByRole("button", { name: "Preview what teammates see" }));
    expect(uiStore.getState().sheet).toEqual({ kind: "share-preview", agentId: BACKEND });
  });

  it("unshare-confirm: asks before turning sharing off", async () => {
    render(<SharingSection agentId={BACKEND} />);
    const toggle = screen.getByRole("switch", { name: /Share with team/ });
    fireEvent.click(toggle);
    let alert = screen.getByRole("alertdialog", { name: "Stop sharing Backend?" });
    expect(alert).toHaveTextContent("Pending requests are canceled. A running task continues unless you stop it.");
    fireEvent.click(within(alert).getByRole("button", { name: "Cancel" }));
    expect(call).not.toHaveBeenCalled();
    expect(toggle).toBeChecked();

    fireEvent.click(toggle);
    alert = screen.getByRole("alertdialog", { name: "Stop sharing Backend?" });
    await click(within(alert).getByRole("button", { name: "Stop sharing" }));
    expect(call).toHaveBeenLastCalledWith("share_update", {
      agent_id: BACKEND,
      shared: false,
      visibility: null,
      accepts_requests: null,
    });
  });

  it("private: hides the options and shares without asking", async () => {
    render(<SharingSection agentId={MARKETING} />);
    const section = screen.getByRole("region", { name: "Sharing" });
    expect(section).toHaveAttribute("data-state", "private");
    expect(within(section).getByText("Hidden from teammates' lists and routing.")).toBeInTheDocument();
    expect(within(section).queryByRole("radio")).toBeNull();
    await click(within(section).getByRole("switch", { name: /Share with team/ }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(call).toHaveBeenLastCalledWith("share_update", {
      agent_id: MARKETING,
      shared: true,
      visibility: null,
      accepts_requests: null,
    });
  });

  it("follows share.update events", () => {
    render(<SharingSection agentId={MARKETING} />);
    act(() => {
      const seq = appStore.getState().mirror!.seq + 1;
      appStore.getState().receive({
        seq,
        at: "2026-11-09T02:00:00Z",
        event: {
          type: "share.update",
          payload: { share: { agent_id: MARKETING, shared: true, visibility: "answer", accepts_requests: false } },
        },
      });
    });
    expect(screen.getByRole("region", { name: "Sharing" })).toHaveAttribute("data-state", "shared");
    expect(screen.getByRole("radio", { name: "Answer only" })).toBeChecked();
  });

  it("offers Try again when a change fails", async () => {
    call.mockRejectedValueOnce({ code: "disconnected", message: "x", rpc_code: null, data: null });
    render(<SharingSection agentId={BACKEND} />);
    await click(screen.getByRole("switch", { name: /Accept work requests/ }));
    const banner = screen.getByRole("alert");
    await click(within(banner).getByRole("button", { name: "Try again" }));
    expect(call).toHaveBeenCalledTimes(2);
    expect(call).toHaveBeenLastCalledWith("share_update", {
      agent_id: BACKEND,
      shared: null,
      visibility: null,
      accepts_requests: false,
    });
  });
});

describe("share preview", () => {
  const preview = {
    view: {
      owner: MINSU,
      agent_id: BACKEND,
      slug: "backend",
      name: "Backend",
      role: "API developer",
      harness: "claude_code",
      avatar: { shape: "server", color: "#000" },
      state: "working",
      last_activity_at: null,
      accepts_requests: true,
    },
    visibility: "answer_excerpts",
  };

  it("preview: shows what teammates would see", async () => {
    call.mockResolvedValue(preview);
    uiStore.getState().openSheet({ kind: "share-preview", agentId: BACKEND });
    await act(async () => {
      render(<SharePreviewSheet agentId={BACKEND} />);
    });
    expect(call).toHaveBeenCalledWith("share_preview", { agent_id: BACKEND });
    const sheet = screen.getByRole("dialog", { name: "What teammates see" });
    expect(within(sheet).getByText("Backend")).toBeInTheDocument();
    expect(within(sheet).getByText("API developer")).toBeInTheDocument();
    expect(within(sheet).getByText("Accepted")).toBeInTheDocument();
    expect(within(sheet).getByText(/the conversation passages it used/)).toBeInTheDocument();
    fireEvent.click(within(sheet).getByRole("button", { name: "Close" }));
    expect(uiStore.getState().sheet).toBeNull();
  });

  it("retries when the preview fails", async () => {
    call.mockRejectedValueOnce({ code: "timeout", message: "x", rpc_code: null, data: null });
    call.mockResolvedValueOnce({ ...preview, visibility: "answer" });
    await act(async () => {
      render(<SharePreviewSheet agentId={BACKEND} />);
    });
    await click(within(screen.getByRole("alert")).getByRole("button", { name: "Try again" }));
    expect(screen.getByText(/No conversation passages/)).toBeInTheDocument();
  });
});
