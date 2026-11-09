// SC-003 sidebar: top icons, Needs-you line, chat list, Connect agents.
// Skeleton version (a plain agent list that navigates); TASK-015 builds it.
import { useAppStore } from "../../store/store";
import { uiStore } from "../../store/ui";

export default function Sidebar() {
  const agents = useAppStore((s) => s.mirror?.agents);
  return (
    <aside className="sidebar" aria-label="Chats" data-placeholder="TASK-015">
      <ul className="chat-list">
        {Object.values(agents ?? {}).map((a) => (
          <li key={a.id} className="chat-item" data-state={a.state}>
            <button type="button" onClick={() => uiStore.getState().navigate({ kind: "agent", agentId: a.id })}>
              <span className="name">{a.name}</span>
            </button>
          </li>
        ))}
      </ul>
    </aside>
  );
}
