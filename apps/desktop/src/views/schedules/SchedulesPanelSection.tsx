// SC-007 schedules section of the information panel. Placeholder from the screen skeleton; TASK-031 builds it.
import type { AgentId } from "../../ipc/generated/AgentId";

export default function SchedulesPanelSection(_props: { agentId: AgentId }) {
  return <section className="p-card" data-placeholder="TASK-031" aria-label="SchedulesPanelSection" />;
}
