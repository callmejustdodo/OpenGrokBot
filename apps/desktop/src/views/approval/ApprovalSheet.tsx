// SC-011 approval sheet (keep-similar state: TASK-029). Placeholder from the screen skeleton; TASK-026 builds it.
import type { ApprovalId } from "../../ipc/generated/ApprovalId";

export default function ApprovalSheet(_props: { approvalId: ApprovalId }) {
  return <section className="sheet" data-placeholder="TASK-026" aria-label="ApprovalSheet" />;
}
