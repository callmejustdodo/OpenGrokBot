import { act, fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import "../../i18n";
import type { Agent } from "../../ipc/generated/Agent";
import type { AgentFiles } from "../../ipc/generated/AgentFiles";
import { appStore } from "../../store/store";
import { uiStore } from "../../store/ui";
import { BACKEND, apiError, emit, loadWorld } from "../my-conversation/world.testing";

const call = vi.fn();
vi.mock("../../ipc/transport", () => ({ tauriTransport: { call: (...a: unknown[]) => call(...a) } }));

const { default: InstructionsSheet } = await import("./InstructionsSheet");

const ORIGINAL = "# Backend agent\n\n- Run tests for every change";

function files(revision: string, content: string): AgentFiles {
  return {
    agent_id: BACKEND,
    revision,
    files: [
      { path: "AGENTS.md", content },
      { path: ".claude/skills/api-style/SKILL.md", content: "x" },
      { path: ".claude/skills/test-first/SKILL.md", content: "y" },
    ],
  };
}

let disk = files("rev-backend-1", ORIGINAL);

beforeEach(() => {
  disk = files("rev-backend-1", ORIGINAL);
  call.mockReset();
  call.mockImplementation((method: string, params: { content?: string }) => {
    if (method === "agent_files_read") return Promise.resolve(disk);
    if (method === "agent_files_write") return Promise.resolve(files("rev-backend-2", params.content ?? ""));
    return Promise.reject(new Error(method));
  });
  loadWorld();
  uiStore.setState({ sheet: { kind: "instructions", agentId: BACKEND } });
});

/** The agent's `files_revision` changes, as formationd reports edits made elsewhere. */
function changeOnDisk(revision: string, content: string) {
  disk = files(revision, content);
  const agent = appStore.getState().mirror?.agents[BACKEND] as Agent;
  emit({ type: "agent.update", payload: { change: "upsert", agent: { ...agent, files_revision: revision } } });
}

async function openSheet() {
  render(<InstructionsSheet agentId={BACKEND} />);
  const editor = screen.getByRole("textbox", { name: "AGENTS.md" });
  await screen.findByDisplayValue(/Backend agent/);
  return editor;
}

function state() {
  return screen.getByRole("dialog").querySelector(".instructions");
}

describe("SC-006 default", () => {
  it("reads the agent's files on open and shows AGENTS.md and the skills", async () => {
    await openSheet();
    expect(call).toHaveBeenCalledWith("agent_files_read", { agent_id: BACKEND });
    expect(screen.getByRole("dialog", { name: "Instructions & skills · Backend" })).toBeInTheDocument();
    expect(screen.getByText("Saved · takes effect from the next session")).toBeInTheDocument();
    expect(screen.getByText("2 skills")).toBeInTheDocument();
    expect(screen.getByText(".claude/skills/api-style/SKILL.md")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Save/ })).toBeDisabled();
    expect(state()).toHaveAttribute("data-state", "default");
  });

  it("takes changes made elsewhere when nothing is unsaved", async () => {
    await openSheet();
    await act(async () => changeOnDisk("rev-backend-9", "# Edited in vim"));
    expect(await screen.findByDisplayValue("# Edited in vim")).toBeInTheDocument();
    expect(state()).toHaveAttribute("data-state", "default");
  });
});

describe("SC-006 unsaved", () => {
  it("marks unsaved edits and saves them with ⌘S against the revision it read", async () => {
    const editor = await openSheet();
    fireEvent.change(editor, { target: { value: `${ORIGINAL}\n- Never log tokens` } });
    expect(screen.getByText("Unsaved changes · ⌘S")).toBeInTheDocument();
    expect(state()).toHaveAttribute("data-state", "unsaved");
    await act(async () => fireEvent.keyDown(editor, { key: "s", metaKey: true }));
    expect(call).toHaveBeenCalledWith("agent_files_write", {
      agent_id: BACKEND,
      path: "AGENTS.md",
      content: `${ORIGINAL}\n- Never log tokens`,
      base_revision: "rev-backend-1",
    });
    expect(screen.getByText("Saved · takes effect from the next session")).toBeInTheDocument();
  });

  it("Save button saves too, and Close closes", async () => {
    const editor = await openSheet();
    fireEvent.change(editor, { target: { value: "new" } });
    await act(async () => fireEvent.click(screen.getByRole("button", { name: /Save/ })));
    expect(call).toHaveBeenCalledWith("agent_files_write", expect.objectContaining({ content: "new" }));
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(uiStore.getState().sheet).toBeNull();
  });
});

describe("SC-006 changed-on-disk", () => {
  it("offers Reload when the files change while I edit", async () => {
    const editor = await openSheet();
    fireEvent.change(editor, { target: { value: "my edit" } });
    await act(async () => changeOnDisk("rev-backend-9", "# Edited in vim"));
    expect(state()).toHaveAttribute("data-state", "changed-on-disk");
    expect(screen.getByRole("alert")).toHaveTextContent("The file changed in the folder");
    expect(editor).toHaveValue("my edit");
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Reload" })));
    expect(editor).toHaveValue("# Edited in vim");
    expect(state()).toHaveAttribute("data-state", "default");
  });

  it("Keep my edits saves over the new revision", async () => {
    const editor = await openSheet();
    fireEvent.change(editor, { target: { value: "my edit" } });
    await act(async () => changeOnDisk("rev-backend-9", "# Edited in vim"));
    fireEvent.click(screen.getByRole("button", { name: "Keep my edits" }));
    expect(state()).toHaveAttribute("data-state", "unsaved");
    await act(async () => fireEvent.keyDown(editor, { key: "s", metaKey: true }));
    expect(call).toHaveBeenCalledWith("agent_files_write", expect.objectContaining({ base_revision: "rev-backend-9" }));
  });

  it("shows it when the save answers conflict", async () => {
    const editor = await openSheet();
    call.mockImplementation((method: string) =>
      method === "agent_files_write" ? Promise.reject(apiError("conflict")) : Promise.resolve(disk),
    );
    fireEvent.change(editor, { target: { value: "my edit" } });
    await act(async () => fireEvent.keyDown(editor, { key: "s", metaKey: true }));
    expect(state()).toHaveAttribute("data-state", "changed-on-disk");
  });
});
