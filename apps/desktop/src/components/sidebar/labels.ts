// Localized words shared by the sidebar, the information panel and my
// conversation: agent states, harnesses, and a conversation's latest line.
import type { TFunction } from "i18next";
import type { AgentState } from "../../ipc/generated/AgentState";
import type { ConversationItem } from "../../ipc/generated/ConversationItem";
import type { HarnessKind } from "../../ipc/generated/HarnessKind";
import type { Step } from "../../ipc/generated/Step";

export function stateLabel(t: TFunction, state: AgentState): string {
  switch (state) {
    case "working":
      return t("agent.state.working", "Working");
    case "idle":
      return t("agent.state.idle", "Idle");
    case "waiting_for_approval":
      return t("agent.state.waiting_for_approval", "Waiting for approval");
    case "stalled":
      return t("agent.state.stalled", "Stalled");
    case "error":
      return t("agent.state.error", "Error");
    case "stopped":
      return t("agent.state.stopped", "Stopped");
    case "unknown":
      return t("agent.state.unknown", "Unknown");
  }
}

export function harnessLabel(t: TFunction, harness: HarnessKind): string {
  switch (harness) {
    case "claude_code":
      return "Claude Code";
    case "codex":
      return "Codex";
    case "detected":
      return t("agent.harness.detected", "Detected");
  }
}

/** One step as a short line, for example "Editing auth/token.ts". */
export function stepLine(t: TFunction, step: Step): string {
  const target = step.target;
  switch (step.kind) {
    case "edit":
      return t("agent.step.edit", "Editing {{target}}", { target });
    case "command":
      return t("agent.step.command", "Running {{target}}", { target });
    case "read":
      return t("agent.step.read", "Reading {{target}}", { target });
    case "web":
      return t("agent.step.web", "Opening {{target}}", { target });
    case "tool":
      return t("agent.step.tool", "Using {{tool}} on {{target}}", { tool: step.tool_name, target });
  }
}

/** A conversation item as the sidebar's second line. */
export function itemLine(t: TFunction, item: ConversationItem): string {
  switch (item.kind) {
    case "user_message":
    case "assistant_text":
    case "notice":
      return item.text;
    case "step":
      return stepLine(t, item.step);
    case "approval":
      return t("agent.state.waiting_for_approval", "Waiting for approval");
    case "request":
      return t("sidebar.latest.request", "Work request from a teammate");
  }
}
