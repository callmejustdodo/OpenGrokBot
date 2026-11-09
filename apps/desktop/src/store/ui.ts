// Where the main window is: the view in the content area, the open sheet, and
// whether the information panel shows (frontend architecture, "Navigation").
// Not mirrored from formationd; each window keeps its own.
import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";
import type { AgentId } from "../ipc/generated/AgentId";
import type { ApprovalId } from "../ipc/generated/ApprovalId";
import type { MemberId } from "../ipc/generated/MemberId";
import type { QuestionId } from "../ipc/generated/QuestionId";
import type { RequestId } from "../ipc/generated/RequestId";

export type View =
  | { kind: "home" }
  /** SC-005: a conversation with one of my agents. */
  | { kind: "agent"; agentId: AgentId }
  /** SC-015: asking a teammate's agent. */
  | { kind: "teammate-agent"; owner: MemberId; agentId: AgentId }
  /** SC-015: asking a person; Formation picks the agent. */
  | { kind: "person"; memberId: MemberId }
  /** SC-015 "new": choosing whom to ask. */
  | { kind: "new-question" }
  | { kind: "inbox" }
  | { kind: "team" }
  | { kind: "member"; memberId: MemberId }
  | { kind: "history" }
  | { kind: "rules" };

export type Sheet =
  | { kind: "new-agent" }
  | { kind: "instructions"; agentId: AgentId }
  | { kind: "schedules"; agentId: AgentId }
  | { kind: "share-preview"; agentId: AgentId }
  | { kind: "connect" }
  | { kind: "approval"; approvalId: ApprovalId }
  | { kind: "work-request"; owner: MemberId; agentId: AgentId; fromQuestion: QuestionId | null }
  | { kind: "request-status"; requestId: RequestId };

export type UiState = {
  view: View;
  sheet: Sheet | null;
  panelOpen: boolean;
  navigate: (view: View) => void;
  openSheet: (sheet: Sheet) => void;
  closeSheet: () => void;
  setPanelOpen: (open: boolean) => void;
};

export function createUiStore() {
  return createStore<UiState>()((set) => ({
    view: { kind: "home" },
    sheet: null,
    panelOpen: true,
    navigate: (view) => set({ view }),
    openSheet: (sheet) => set({ sheet }),
    closeSheet: () => set({ sheet: null }),
    setPanelOpen: (panelOpen) => set({ panelOpen }),
  }));
}

export const uiStore = createUiStore();

export function useUi<T>(selector: (state: UiState) => T): T {
  return useStore(uiStore, selector);
}
