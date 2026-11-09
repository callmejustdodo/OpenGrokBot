// SC-005 permission/approval card in my conversation. Placeholder from the screen skeleton; TASK-026 builds it.
import type { ApprovalId } from "../../ipc/generated/ApprovalId";

export default function PermissionCard(_props: { approvalId: ApprovalId }) {
  return <section className="icard" data-placeholder="TASK-026" aria-label="PermissionCard" />;
}
