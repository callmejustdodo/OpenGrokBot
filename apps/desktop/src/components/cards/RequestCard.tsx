// SC-005 teammate request card in my conversation. Placeholder from the screen skeleton; TASK-026 builds it.
import type { RequestId } from "../../ipc/generated/RequestId";

export default function RequestCard(_props: { requestId: RequestId }) {
  return <section className="icard" data-placeholder="TASK-026" aria-label="RequestCard" />;
}
