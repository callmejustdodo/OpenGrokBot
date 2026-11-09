// SC-005 conversation with my agent. Placeholder from the screen skeleton; TASK-015 builds it.
import type { AgentId } from "../../ipc/generated/AgentId";

export default function MyConversation(_props: { agentId: AgentId }) {
  return <section className="conversation" data-placeholder="TASK-015" aria-label="MyConversation" />;
}
