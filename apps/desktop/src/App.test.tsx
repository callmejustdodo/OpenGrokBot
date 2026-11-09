import { act, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import m3 from "../../../tools/fake-formationd/fixtures/worlds/m3.json";
import type { Snapshot } from "./ipc/generated/Snapshot";

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
const { appStore, connectStore } = await import("./store/store");
const { tauriTransport } = await import("./ipc/transport");

const snapshot = m3 as unknown as Snapshot;

function formationd(connected: boolean, answer: Snapshot = snapshot) {
  invoke.mockImplementation((cmd: string, args?: { method: string }) => {
    if (cmd === "formationd_status") return Promise.resolve({ connected, socket: "/s" });
    if (cmd === "formationd_call" && args?.method === "snapshot") return Promise.resolve(answer);
    return Promise.reject(new Error(cmd));
  });
}

let stop: () => void = () => {};
beforeEach(() => {
  stop();
  invoke.mockReset();
  listeners.clear();
  appStore.setState({ mirror: null, connected: false, stale: true, pending: {} });
});

describe("windows over the store", () => {
  it("shows formationd's agents once connected", async () => {
    formationd(true);
    stop = connectStore(tauriTransport);
    render(<App />);
    expect(await screen.findByText("Backend")).toBeInTheDocument();
    expect(screen.getByText("Research").closest("li")).toHaveAttribute("data-state", "stalled");
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("shows the banner while formationd is unreachable, then the data", async () => {
    formationd(false);
    stop = connectStore(tauriTransport);
    render(<MenubarPanel />);
    render(<App />);
    expect(await screen.findByRole("status")).toHaveTextContent("Connecting to formationd");
    formationd(true);
    await act(async () => {
      listeners.get("formationd://status")?.({ payload: { connected: true, socket: "/s" } });
    });
    expect(await screen.findAllByText("Backend")).toHaveLength(2);
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("asks to restart formationd when its contract version differs", async () => {
    formationd(true, { ...snapshot, daemon: { version: "0.0.9", protocol: 0 } });
    stop = connectStore(tauriTransport);
    render(<App />);
    await screen.findByText("Backend");
    const banner = screen.getByRole("status");
    expect(banner).toHaveTextContent("formationd needs restarting");
    expect(banner).toHaveTextContent("contract version 0; this app needs version 1");
  });

  it("applies events after the snapshot", async () => {
    formationd(true);
    stop = connectStore(tauriTransport);
    render(<App />);
    await screen.findByText("Backend");
    await act(async () => {
      listeners.get("formationd://event")?.({
        payload: {
          seq: 1,
          at: "2026-11-09T01:54:00Z",
          event: {
            type: "agent.state",
            payload: { agent_id: "01930000-0000-7000-8000-000000000101", state: "idle", reason: null, exit_code: null },
          },
        },
      });
    });
    expect(screen.getByText("Backend").closest("li")).toHaveAttribute("data-state", "idle");
  });
});
