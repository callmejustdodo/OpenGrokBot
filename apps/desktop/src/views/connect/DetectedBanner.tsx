// SC-005 'detected' read-only banner for a connected detected agent. Placeholder from the screen skeleton; TASK-033 builds it.
import type { AgentId } from "../../ipc/generated/AgentId";

export default function DetectedBanner(_props: { agentId: AgentId }) {
  return <section className="banner" data-placeholder="TASK-033" aria-label="DetectedBanner" />;
}
