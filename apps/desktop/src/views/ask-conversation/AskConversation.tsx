// SC-015 asking conversation (agent, person, or new). Placeholder from the screen skeleton; TASK-020 builds it.
import type { View } from "../../store/ui";

export default function AskConversation(_props: { view: Extract<View, { kind: "teammate-agent" | "person" | "new-question" }> }) {
  return <section className="conversation" data-placeholder="TASK-020" aria-label="AskConversation" />;
}
