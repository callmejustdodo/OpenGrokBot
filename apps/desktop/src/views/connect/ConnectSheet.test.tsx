import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import reactions from "../../../../../tools/fake-formationd/fixtures/reactions.json";
import m3 from "../../../../../tools/fake-formationd/fixtures/worlds/m3.json";
import "../../i18n";
import type { DetectedAgent } from "../../ipc/generated/DetectedAgent";
import type { Snapshot } from "../../ipc/generated/Snapshot";
import { appStore } from "../../store/store";
import { uiStore } from "../../store/ui";

const call = vi.fn();
vi.mock("../../ipc/transport", () => ({ tauriTransport: { call: (...a: unknown[]) => call(...a) } }));

const { default: ConnectSheet } = await import("./ConnectSheet");
const { default: DetectedBanner } = await import("./DetectedBanner");

// fake-formationd's answer: claude · shared-web (connected), Hermes, OpenClaw.
const detected = reactions.agents_detect[0].result as DetectedAgent[];
const [shared, hermes] = detected;
const SHARED_WEB = "01930000-0000-7000-8000-000000000106";
const BACKEND = "01930000-0000-7000-8000-000000000101";

/** A formationd that answers `agents_detect` with `agents` (resolved at once unless held). */
function formationd(agents: DetectedAgent[], hold = false) {
  let release: () => void = () => {};
  call.mockImplementation((method: string, params: { key?: string }) => {
    if (method === "agents_detect")
      return hold ? new Promise((resolve) => (release = () => resolve(agents))) : Promise.resolve(agents);
    if (method === "agent_connect") return Promise.resolve({ ...m3.agents[5], id: `agent-for-${params.key}` });
    return Promise.reject(new Error(method));
  });
  return () => release();
}

async function openSheet() {
  render(<ConnectSheet />);
  return screen.findByRole("list", { name: "Detected agents" });
}

beforeEach(() => {
  call.mockReset();
  appStore.setState({ mirror: null, connected: true, stale: false, pending: {} });
  appStore.getState().loadSnapshot(m3 as unknown as Snapshot);
  uiStore.setState({ view: { kind: "home" }, sheet: { kind: "connect" } });
});

describe("Connect agents sheet (SC-009)", () => {
  it("shows detecting under the agents_detect pending key until formationd answers", async () => {
    const release = formationd(detected, true);
    render(<ConnectSheet />);
    expect(screen.getByRole("status")).toHaveTextContent("Looking for agents");
    expect(appStore.getState().pending).toHaveProperty("agents_detect");
    expect(call).toHaveBeenCalledWith("agents_detect", {});
    await act(async () => release());
    expect(appStore.getState().pending).not.toHaveProperty("agents_detect");
    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.getAllByRole("listitem").map((li) => li.getAttribute("data-state")).slice(0, 3)).toEqual([
      "connected",
      "read-only",
      "not-supported",
    ]);
  });

  it("connectable: lists harness, path, last activity and capabilities, then connects and opens it", async () => {
    const connectable = { ...shared, key: "claude_code:~/Codes/other", connected_agent: null };
    formationd([connectable]);
    await openSheet();
    const detail = screen.getByRole("region", { name: "claude · shared-web" });
    expect(detail).toHaveAttribute("data-state", "connectable");
    expect(within(detail).getByText("Claude Code")).toBeInTheDocument();
    expect(within(detail).getByText("~/Codes/shared-web")).toBeInTheDocument();
    expect(within(detail).getByText(/2026/)).toBeInTheDocument();
    const caps = within(detail).getByRole("list", { name: "What OpenGrokBot can do" });
    expect(within(caps).getAllByRole("listitem").map((li) => li.getAttribute("data-supported"))).toEqual([
      "true",
      "true",
      "true",
      "false",
    ]);
    expect(caps).toHaveTextContent("Started in Terminal, so OpenGrokBot can't send it messages");

    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    expect(call).toHaveBeenLastCalledWith("agent_connect", { key: "claude_code:~/Codes/other" });
    expect(appStore.getState().pending).toHaveProperty(["agent_connect:claude_code:~/Codes/other"]);
    await act(async () => {});
    expect(uiStore.getState().view).toEqual({ kind: "agent", agentId: "agent-for-claude_code:~/Codes/other" });
    expect(uiStore.getState().sheet).toBeNull();
  });

  it("read-only: explains with the capabilities' reasons and can still connect", async () => {
    formationd(detected);
    await openSheet();
    const detail = screen.getByRole("region", { name: "Hermes" });
    expect(detail).toHaveAttribute("data-state", "read-only");
    expect(detail).toHaveTextContent("Read-only");
    expect(detail).toHaveTextContent("Hermes sessions can't be forked yet");
    expect(screen.getByRole("button", { name: "Connect" })).toBeEnabled();
  });

  it("not-supported: Connect is disabled and each reason shows", async () => {
    formationd(detected);
    await openSheet();
    fireEvent.click(screen.getByRole("button", { name: /OpenClaw/ }));
    const detail = screen.getByRole("region", { name: "OpenClaw" });
    expect(detail).toHaveAttribute("data-state", "not-supported");
    expect(detail).toHaveTextContent("OpenClaw reports no session state");
    expect(detail).toHaveTextContent("No activity yet");
    expect(screen.getByRole("button", { name: "Connect" })).toBeDisabled();
  });

  it("already connected: shows Connected and Open goes to the agent", async () => {
    formationd(detected);
    await openSheet();
    const row = screen.getByRole("button", { name: /claude · shared-web/ });
    expect(row).toHaveTextContent("Connected");
    fireEvent.click(row);
    expect(screen.queryByRole("button", { name: "Connect" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Open" }));
    expect(uiStore.getState().view).toEqual({ kind: "agent", agentId: SHARED_WEB });
    expect(uiStore.getState().sheet).toBeNull();
  });

  it("shows harness_unsupported inline when connecting fails", async () => {
    formationd([hermes]);
    call.mockImplementation((method: string) =>
      method === "agents_detect"
        ? Promise.resolve([hermes])
        : Promise.reject({ code: "rpc", message: "x", rpc_code: -32000, data: { code: "harness_unsupported", message: "x" } }),
    );
    await openSheet();
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("OpenGrokBot doesn't support this harness yet.");
    expect(uiStore.getState().sheet).toEqual({ kind: "connect" });
  });

  it("shows a retry when detection fails, and an empty state when nothing is found", async () => {
    call.mockRejectedValueOnce({ code: "disconnected", message: "x", rpc_code: null, data: null });
    render(<ConnectSheet />);
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't look for agents");
    formationd([]);
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByText("No agents are running outside OpenGrokBot on this Mac.")).toBeInTheDocument();
  });

  it("closes on Cancel and Esc", async () => {
    formationd(detected);
    await openSheet();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(uiStore.getState().sheet).toBeNull();
    uiStore.setState({ sheet: { kind: "connect" } });
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(uiStore.getState().sheet).toBeNull();
  });
});

describe("Detected banner (SC-005 detected)", () => {
  it("explains a read-only detected agent and lists what works", () => {
    render(<DetectedBanner agentId={SHARED_WEB} />);
    const banner = screen.getByRole("region", { name: "Read-only · detected agent" });
    expect(banner).toHaveTextContent("This Claude Code session in ~/Codes/shared-web was started outside OpenGrokBot");
    expect(banner).toHaveTextContent("can't send it messages");
    const items = within(banner).getAllByRole("listitem");
    expect(items.map((li) => li.getAttribute("data-supported"))).toEqual(["true", "true", "true", "false"]);
    expect(items[0]).toHaveTextContent("Show state: works");
    expect(items[3]).toHaveTextContent("Started in Terminal, so OpenGrokBot can't send it messages");
  });

  it("renders nothing for an agent OpenGrokBot created", () => {
    const { container } = render(<DetectedBanner agentId={BACKEND} />);
    expect(container).toBeEmptyDOMElement();
  });
});
