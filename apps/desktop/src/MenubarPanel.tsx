// The menu bar panel (SC-002): agents with their state and a way back to the
// main window. TASK-035 completes it (needs-attention list, all-clear, offline).
import { showMainWindow } from "./ipc/client";
import { useAppStore } from "./store/store";

export default function MenubarPanel() {
  const agents = useAppStore((s) => s.mirror?.agents);
  const connected = useAppStore((s) => s.connected);
  return (
    <div className="menubar" data-connected={connected}>
      <ul className="agents">
        {Object.values(agents ?? {}).map((a) => (
          <li key={a.id} data-state={a.state}>
            {a.name}
          </li>
        ))}
      </ul>
      <button type="button" className="open" onClick={() => void showMainWindow()}>
        Open Formation
      </button>
    </div>
  );
}
