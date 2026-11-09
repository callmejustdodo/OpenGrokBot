// What Formation can do with a detected agent (FR-006), shared by the Connect
// agents sheet (SC-009) and the read-only conversation banner (SC-005 detected).
import type { TFunction } from "i18next";
import type { DetectedAgent } from "../../ipc/generated/DetectedAgent";
import type { DetectedCapabilities } from "../../ipc/generated/DetectedCapabilities";
import type { DetectedHarness } from "../../ipc/generated/DetectedHarness";

/** SC-009 states for one detected agent. */
export type ConnectKind = "connected" | "connectable" | "read-only" | "not-supported";

/**
 * Connectable: Formation can read it and use it for teammates. Read-only: it
 * can only show its state or conversation. Not supported: nothing works.
 */
export function connectKind(agent: DetectedAgent): ConnectKind {
  if (agent.connected_agent) return "connected";
  const c = agent.capabilities;
  if (c.read_conversation.supported && (c.answer_teammates.supported || c.receive_requests.supported)) return "connectable";
  if (c.state.supported || c.read_conversation.supported) return "read-only";
  return "not-supported";
}

export function harnessLabel(harness: DetectedHarness, t: TFunction): string {
  switch (harness) {
    case "claude_code":
      return t("connect.harness.claude_code", "Claude Code");
    case "codex":
      return t("connect.harness.codex", "Codex");
    case "hermes":
      return t("connect.harness.hermes", "Hermes");
    case "open_claw":
      return t("connect.harness.open_claw", "OpenClaw");
  }
}

export type CapabilityRow = { key: keyof DetectedCapabilities; label: string; supported: boolean; reason: string | null };

/** The four capabilities in the mockup's order, each with its label and reason. */
export function capabilityRows(caps: DetectedCapabilities, t: TFunction): CapabilityRow[] {
  const labels: Record<keyof DetectedCapabilities, string> = {
    state: t("connect.capability.state", "Show state"),
    read_conversation: t("connect.capability.read_conversation", "Read the conversation"),
    answer_teammates: t("connect.capability.answer_teammates", "Answer teammates"),
    receive_requests: t("connect.capability.receive_requests", "Send messages, receive requests"),
  };
  return (Object.keys(labels) as (keyof DetectedCapabilities)[]).map((key) => ({
    key,
    label: labels[key],
    supported: caps[key].supported,
    reason: caps[key].supported ? null : (caps[key].reason ?? t("connect.capability.unsupported", "Not supported")),
  }));
}
