// SC-008 share preview sheet. Placeholder from the screen skeleton; TASK-020 builds it.
import type { AgentId } from "../../ipc/generated/AgentId";

export default function SharePreviewSheet(_props: { agentId: AgentId }) {
  return <section className="sheet" data-placeholder="TASK-020" aria-label="SharePreviewSheet" />;
}
