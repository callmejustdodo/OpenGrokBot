// SC-016 request status. Placeholder from the screen skeleton; TASK-026 builds it.
import type { RequestId } from "../../ipc/generated/RequestId";

export default function RequestStatusSheet(_props: { requestId: RequestId }) {
  return <section className="sheet" data-placeholder="TASK-026" aria-label="RequestStatusSheet" />;
}
