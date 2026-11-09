// Commands from the conversation with my agent, each with a pending key so the
// UI shows progress at once. The confirming events update the mirror.
import type { TFunction } from "i18next";
import type { CallError } from "../../ipc/client";
import type { AgentId } from "../../ipc/generated/AgentId";
import type { MessageId } from "../../ipc/generated/MessageId";
import type { StopMode } from "../../ipc/generated/StopMode";
import { tauriTransport } from "../../ipc/transport";
import { act } from "../../store/sync";

export const pendingKey = {
  send: (agentId: AgentId) => `agent_message_send:${agentId}`,
  stop: (agentId: AgentId) => `agent_stop:${agentId}`,
  start: (agentId: AgentId) => `agent_start:${agentId}`,
  queue: (messageId: MessageId) => `agent_message:${messageId}`,
};

export function send(agentId: AgentId, text: string) {
  return act(tauriTransport, pendingKey.send(agentId), "agent_message_send", { agent_id: agentId, text });
}

export function stop(agentId: AgentId, mode: StopMode) {
  return act(tauriTransport, pendingKey.stop(agentId), "agent_stop", { agent_id: agentId, mode });
}

export function start(agentId: AgentId) {
  return act(tauriTransport, pendingKey.start(agentId), "agent_start", { agent_id: agentId });
}

/** A stalled agent's process is still there: stop it now, then start it again. */
export async function restart(agentId: AgentId, running: boolean) {
  if (running) await stop(agentId, "now");
  return start(agentId);
}

export function steer(agentId: AgentId, messageId: MessageId) {
  return act(tauriTransport, pendingKey.queue(messageId), "agent_message_steer", {
    agent_id: agentId,
    message_id: messageId,
  });
}

export function edit(agentId: AgentId, messageId: MessageId, text: string) {
  return act(tauriTransport, pendingKey.queue(messageId), "agent_message_edit", {
    agent_id: agentId,
    message_id: messageId,
    text,
  });
}

export function withdraw(agentId: AgentId, messageId: MessageId) {
  return act(tauriTransport, pendingKey.queue(messageId), "agent_message_withdraw", {
    agent_id: agentId,
    message_id: messageId,
  });
}

/** Localized copy for a failed command, chosen by formationd's error code. */
export function errorText(t: TFunction, error: unknown): string {
  const code = (error as Partial<CallError> | null)?.data?.code;
  switch (code) {
    case "conflict":
      return t("conversation.error.conflict", "That changed in the meantime. Try again.");
    case "harness_unsupported":
      return t("conversation.error.harnessUnsupported", "This agent's harness can't do that.");
    case "harness_unavailable":
      return t("conversation.error.harnessUnavailable", "The harness isn't ready. Check it in Settings.");
    case "denied":
      return t("conversation.error.denied", "That isn't allowed.");
    default:
      if ((error as Partial<CallError> | null)?.code === "disconnected")
        return t("conversation.error.disconnected", "formationd isn't reachable. Try again when it's back.");
      return t("conversation.error.generic", "Something went wrong. Try again.");
  }
}
