import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import "../../i18n";
import { uiStore } from "../../store/ui";
import { BACKEND, SHARED_WEB, apiError, loadWorld } from "../../views/my-conversation/world.testing";

const call = vi.fn();
vi.mock("../../ipc/transport", () => ({ tauriTransport: { call: (...a: unknown[]) => call(...a) } }));
const openDialog = vi.fn();
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: (...a: unknown[]) => openDialog(...a) }));
vi.mock("@tauri-apps/api/path", () => ({ homeDir: () => Promise.resolve("/Users/dohyeon") }));

const { default: InfoPanel } = await import("./InfoPanel");

// The M3 world gives Backend one repository, "~/Codes/shared-api".
const SHARED_API = "~/Codes/shared-api";

beforeEach(() => {
  call.mockReset();
  call.mockResolvedValue({});
  openDialog.mockReset();
  loadWorld();
  act(() => uiStore.getState().navigate({ kind: "agent", agentId: BACKEND }));
});

function card() {
  return screen.getByRole("region", { name: "Repositories" });
}

describe("SC-020 repositories", () => {
  it("lists the agent's repositories", () => {
    render(<InfoPanel />);
    expect(within(card()).getByText(SHARED_API)).toBeInTheDocument();
  });

  it("adds picked folders with agent_update and the whole new list, without duplicates", async () => {
    openDialog.mockResolvedValueOnce(["/Users/dohyeon/Codes/web", SHARED_API]);
    render(<InfoPanel />);
    await act(async () => fireEvent.click(within(card()).getByRole("button", { name: "Add folder" })));
    expect(openDialog).toHaveBeenCalledWith({ directory: true, multiple: true });
    expect(call).toHaveBeenCalledWith("agent_update", {
      agent_id: BACKEND,
      name: null,
      role: null,
      avatar: null,
      repositories: [SHARED_API, "/Users/dohyeon/Codes/web"],
    });
  });

  it("does not call agent_update when nothing new was picked", async () => {
    openDialog.mockResolvedValueOnce(null);
    render(<InfoPanel />);
    await act(async () => fireEvent.click(within(card()).getByRole("button", { name: "Add folder" })));
    expect(call).not.toHaveBeenCalled();
  });

  it("removes a repository with agent_update", async () => {
    render(<InfoPanel />);
    await act(async () =>
      fireEvent.click(within(card()).getByRole("button", { name: `Remove ${SHARED_API}` })),
    );
    expect(call).toHaveBeenCalledWith("agent_update", expect.objectContaining({ agent_id: BACKEND, repositories: [] }));
  });

  it("shows an error when agent_update fails", async () => {
    call.mockRejectedValue(apiError("internal"));
    render(<InfoPanel />);
    await act(async () =>
      fireEvent.click(within(card()).getByRole("button", { name: `Remove ${SHARED_API}` })),
    );
    expect(within(card()).getByRole("alert")).toHaveTextContent("Couldn't change the repositories");
  });

  it("has no repositories card for a detected agent", () => {
    act(() => uiStore.getState().navigate({ kind: "agent", agentId: SHARED_WEB }));
    render(<InfoPanel />);
    expect(screen.queryByRole("region", { name: "Repositories" })).toBeNull();
  });
});
