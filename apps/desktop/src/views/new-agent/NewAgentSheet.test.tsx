import { act, fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import "../../i18n";
import { appStore } from "../../store/store";
import { uiStore } from "../../store/ui";
import { apiError, emit, loadWorld } from "../my-conversation/world.testing";

// Radix RadioGroup measures its items; jsdom has no ResizeObserver.
globalThis.ResizeObserver ??= class {
  observe() {}
  unobserve() {}
  disconnect() {}
};

const call = vi.fn();
vi.mock("../../ipc/transport", () => ({ tauriTransport: { call: (...a: unknown[]) => call(...a) } }));
const openDialog = vi.fn();
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: (...a: unknown[]) => openDialog(...a) }));
vi.mock("@tauri-apps/api/path", () => ({ homeDir: () => Promise.resolve("/Users/dohyeon") }));

const { default: NewAgentSheet } = await import("./NewAgentSheet");

const CREATED = { id: "new-agent-id" };

beforeEach(() => {
  openDialog.mockReset();
  call.mockReset();
  call.mockResolvedValue(CREATED);
  loadWorld();
  uiStore.setState({ sheet: { kind: "new-agent" } });
});

function fill(name: string, role: string) {
  fireEvent.change(screen.getByRole("textbox", { name: "Name" }), { target: { value: name } });
  fireEvent.change(screen.getByRole("textbox", { name: /^Role/ }), { target: { value: role } });
}

describe("SC-004 default", () => {
  it("creates an agent with the chosen harness and avatar, then opens its conversation", async () => {
    render(<NewAgentSheet />);
    const dialog = screen.getByRole("dialog", { name: "New agent" });
    expect(dialog.querySelector("form")).toHaveAttribute("data-state", "default");
    const create = screen.getByRole("button", { name: "Create" });
    expect(create).toBeDisabled();
    fill("English support", "Draft replies to English customer emails");
    fireEvent.click(screen.getByRole("radio", { name: /^Codex/ }));
    fireEvent.click(screen.getByRole("button", { name: "rocket" }));
    fireEvent.click(screen.getByRole("button", { name: "#3a6df0" }));
    expect(screen.getByRole("button", { name: "rocket" })).toHaveAttribute("aria-pressed", "true");
    fireEvent.change(screen.getByRole("textbox", { name: /Starting instructions/ }), {
      target: { value: "Be polite." },
    });
    await act(async () => fireEvent.click(create));
    expect(call).toHaveBeenCalledWith("agent_create", {
      name: "English support",
      role: "Draft replies to English customer emails",
      harness: "codex",
      avatar: { shape: "rocket", color: "#3a6df0" },
      folder: null,
      repositories: [],
      instructions: "Be polite.",
    });
    expect(uiStore.getState().view).toEqual({ kind: "agent", agentId: "new-agent-id" });
    expect(uiStore.getState().sheet).toBeNull();
  });

  it("shows both harnesses as ready from the mirror", () => {
    render(<NewAgentSheet />);
    expect(screen.getByRole("radio", { name: "Claude Code · Installed · signed in" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(screen.getByRole("radio", { name: "Codex · Installed · signed in" })).toBeInTheDocument();
    expect(screen.queryByRole("note")).toBeNull();
  });

  it("disables Create while the call is pending", () => {
    render(<NewAgentSheet />);
    fill("A", "B");
    act(() => appStore.getState().beginPending("agent_create"));
    expect(screen.getByRole("button", { name: "Create" })).toBeDisabled();
  });

  it("Cancel closes the sheet", () => {
    render(<NewAgentSheet />);
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(uiStore.getState().sheet).toBeNull();
  });
});

describe("SC-004 harness-not-ready", () => {
  it("shows the problem and still allows creating (US-002)", async () => {
    emit({
      type: "harness.update",
      payload: {
        harnesses: [
          { harness: "claude_code", installed: true, signed_in: true, version: "2", path: "/c", problem: null },
          { harness: "codex", installed: true, signed_in: false, version: "0.98.0", path: "/x", problem: "Sign in with `codex login`" },
        ],
      },
    });
    render(<NewAgentSheet />);
    fireEvent.click(screen.getByRole("radio", { name: /^Codex · Sign in with/ }));
    expect(screen.getByRole("dialog").querySelector("form")).toHaveAttribute("data-state", "harness-not-ready");
    expect(screen.getByRole("note")).toHaveTextContent("can't start until the harness is ready");
    fill("Ops", "Operations");
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Create" })));
    expect(call).toHaveBeenCalledWith("agent_create", expect.objectContaining({ harness: "codex" }));
  });
});

describe("SC-004 name-taken", () => {
  it("marks the name when agent_create answers conflict, until it changes", async () => {
    call.mockRejectedValue(apiError("conflict"));
    render(<NewAgentSheet />);
    fill("Marketing", "Content");
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Create" })));
    const name = screen.getByRole("textbox", { name: "Name" });
    expect(name).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByRole("alert")).toHaveTextContent("An agent with this name already exists.");
    expect(screen.getByRole("button", { name: "Create" })).toBeDisabled();
    expect(uiStore.getState().sheet).toEqual({ kind: "new-agent" });
    fireEvent.change(name, { target: { value: "Marketing 2" } });
    expect(name).toHaveAttribute("aria-invalid", "false");
    expect(screen.getByRole("button", { name: "Create" })).toBeEnabled();
  });

  it("shows a general error for other failures", async () => {
    call.mockRejectedValue(apiError("internal"));
    render(<NewAgentSheet />);
    fill("X", "Y");
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Create" })));
    expect(screen.getByRole("alert")).toHaveTextContent("Couldn't create the agent");
  });
});

describe("SC-004 folders", () => {
  it("adds repositories with the folder picker, skips duplicates, removes one, and sends the rest", async () => {
    render(<NewAgentSheet />);
    fill("API", "Backend API development");
    openDialog.mockResolvedValueOnce(["/Users/dohyeon/Codes/api", "/Users/dohyeon/Codes/web"]);
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Add folder" })));
    expect(openDialog).toHaveBeenCalledWith({ directory: true, multiple: true });
    openDialog.mockResolvedValueOnce(["/Users/dohyeon/Codes/web", "/opt/shared"]);
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Add folder" })));
    expect(screen.getByText("~/Codes/api")).toBeInTheDocument();
    expect(screen.getAllByText("~/Codes/web")).toHaveLength(1);
    expect(screen.getByText("/opt/shared")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Remove ~/Codes/web" }));
    expect(screen.queryByText("~/Codes/web")).toBeNull();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Create" })));
    expect(call).toHaveBeenCalledWith(
      "agent_create",
      expect.objectContaining({ folder: null, repositories: ["/Users/dohyeon/Codes/api", "/opt/shared"] }),
    );
  });

  it("keeps the list when the picker is canceled", async () => {
    render(<NewAgentSheet />);
    fill("A", "B");
    openDialog.mockResolvedValueOnce(null);
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Add folder" })));
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Create" })));
    expect(call).toHaveBeenCalledWith("agent_create", expect.objectContaining({ repositories: [] }));
  });

  it("sends an existing folder picked as the agent folder, and can go back to a new one", async () => {
    render(<NewAgentSheet />);
    fill("API", "Backend API development");
    openDialog.mockResolvedValueOnce("/Users/dohyeon/Codes/api");
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Use an existing folder" })));
    expect(openDialog).toHaveBeenCalledWith({ directory: true, multiple: false });
    expect(screen.getByText("~/Codes/api")).toBeInTheDocument();
    expect(screen.getByText(/AGENTS\.md and CLAUDE\.md, are kept/)).toBeInTheDocument();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Create" })));
    expect(call).toHaveBeenLastCalledWith(
      "agent_create",
      expect.objectContaining({ folder: "/Users/dohyeon/Codes/api", repositories: [] }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Use a new folder" }));
    expect(screen.getByText("A new folder named after the agent")).toBeInTheDocument();
  });

  it("says when the chosen folder is already another agent's", async () => {
    call.mockRejectedValue({
      code: "rpc",
      message: "conflict",
      rpc_code: -32000,
      data: { code: "conflict", message: "the folder is already another agent's folder" },
    });
    render(<NewAgentSheet />);
    fill("API", "Backend API development");
    openDialog.mockResolvedValueOnce("/Users/dohyeon/Formation/Agents/backend");
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Use an existing folder" })));
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Create" })));
    expect(screen.getByRole("alert")).toHaveTextContent("This folder is already another agent's folder.");
    expect(screen.getByRole("textbox", { name: "Name" })).toHaveAttribute("aria-invalid", "false");
  });

  it("shows an error when the picker fails", async () => {
    render(<NewAgentSheet />);
    openDialog.mockRejectedValueOnce(new Error("no dialog"));
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Add folder" })));
    expect(screen.getByRole("alert")).toHaveTextContent("Couldn't open the folder picker");
  });
});
