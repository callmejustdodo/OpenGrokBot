// SC-020 information panel. Skeleton version: for my agent it hosts the
// sections other tasks own (sharing: TASK-020, schedules: TASK-031). TASK-015
// builds the panel itself (header, instructions card, teammate and person panels).
import { useUi } from "../../store/ui";
import SchedulesPanelSection from "../../views/schedules/SchedulesPanelSection";
import SharingSection from "../../views/sharing/SharingSection";

export default function InfoPanel() {
  const view = useUi((s) => s.view);
  if (view.kind !== "agent") return <aside className="info-panel" data-placeholder="TASK-015" />;
  return (
    <aside className="info-panel" aria-label="Agent information" data-placeholder="TASK-015">
      <SchedulesPanelSection agentId={view.agentId} />
      <SharingSection agentId={view.agentId} />
    </aside>
  );
}
