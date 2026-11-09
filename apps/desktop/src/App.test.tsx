import { act, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

// The Tauri bridge, replaced by an in-memory formationd.
const invoke = vi.fn();
const listeners = new Map<string, (e: { payload: unknown }) => void>();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: { payload: unknown }) => void) => {
    listeners.set(name, cb);
    return Promise.resolve(() => listeners.delete(name));
  },
}));

const { default: App } = await import("./App");
const { default: MenubarPanel } = await import("./MenubarPanel");

const snapshot = {
  agents: [
    { id: "a1", name: "Backend", state: "working" },
    { id: "a2", name: "Research", state: "stalled" },
  ],
};

beforeEach(() => {
  invoke.mockReset();
  listeners.clear();
});

describe("shell windows", () => {
  it("shows formationd's agents once connected", async () => {
    invoke.mockImplementation((cmd: string, args?: { method: string }) => {
      if (cmd === "formationd_status") return Promise.resolve({ connected: true, socket: "/s" });
      if (cmd === "formationd_call" && args?.method === "snapshot") return Promise.resolve(snapshot);
      return Promise.reject(new Error(cmd));
    });
    render(<App />);
    expect(await screen.findByText("Backend")).toBeInTheDocument();
    expect(screen.getByText("Research").closest("li")).toHaveAttribute("data-state", "stalled");
    expect(screen.getByRole("main")).toHaveAttribute("data-connected", "true");
  });

  it("waits while formationd is unreachable and loads when it connects", async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "formationd_status") return Promise.resolve({ connected: false, socket: "/s" });
      if (cmd === "formationd_call") return Promise.resolve(snapshot);
      return Promise.reject(new Error(cmd));
    });
    render(<MenubarPanel />);
    await act(async () => {});
    expect(screen.queryByText("Backend")).toBeNull();
    await act(async () => {
      listeners.get("formationd://status")?.({ payload: { connected: true, socket: "/s" } });
    });
    expect(await screen.findByText("Backend")).toBeInTheDocument();
  });
});
