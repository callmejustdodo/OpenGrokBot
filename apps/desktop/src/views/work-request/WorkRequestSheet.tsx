// SC-016 work request compose (and not-accepted). Placeholder from the screen skeleton; TASK-026 builds it.
import type { AgentId } from "../../ipc/generated/AgentId";
import type { MemberId } from "../../ipc/generated/MemberId";
import type { QuestionId } from "../../ipc/generated/QuestionId";

export default function WorkRequestSheet(_props: { owner: MemberId; agentId: AgentId; fromQuestion: QuestionId | null }) {
  return <section className="sheet" data-placeholder="TASK-026" aria-label="WorkRequestSheet" />;
}
