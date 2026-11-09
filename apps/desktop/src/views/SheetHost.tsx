// Shows the open sheet over the main window. Each sheet component renders its
// own `ui/Sheet` and closes with `uiStore.getState().closeSheet()`.
import { useUi } from "../store/ui";
import ApprovalSheet from "./approval/ApprovalSheet";
import ConnectSheet from "./connect/ConnectSheet";
import InstructionsSheet from "./instructions/InstructionsSheet";
import NewAgentSheet from "./new-agent/NewAgentSheet";
import SchedulesSheet from "./schedules/SchedulesSheet";
import SharePreviewSheet from "./sharing/SharePreviewSheet";
import RequestStatusSheet from "./work-request/RequestStatusSheet";
import WorkRequestSheet from "./work-request/WorkRequestSheet";

export default function SheetHost() {
  const sheet = useUi((s) => s.sheet);
  if (!sheet) return null;
  switch (sheet.kind) {
    case "new-agent":
      return <NewAgentSheet />;
    case "instructions":
      return <InstructionsSheet agentId={sheet.agentId} />;
    case "schedules":
      return <SchedulesSheet agentId={sheet.agentId} />;
    case "share-preview":
      return <SharePreviewSheet agentId={sheet.agentId} />;
    case "connect":
      return <ConnectSheet />;
    case "approval":
      return <ApprovalSheet approvalId={sheet.approvalId} />;
    case "work-request":
      return <WorkRequestSheet owner={sheet.owner} agentId={sheet.agentId} fromQuestion={sheet.fromQuestion} />;
    case "request-status":
      return <RequestStatusSheet requestId={sheet.requestId} />;
  }
}
