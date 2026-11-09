// SC-008 sharing section of the information panel. Placeholder from the screen skeleton; TASK-020 builds it.
import type { AgentId } from "../../ipc/generated/AgentId";

export default function SharingSection(_props: { agentId: AgentId }) {
  return <section className="p-card" data-placeholder="TASK-020" aria-label="SharingSection" />;
}
