// The main window's frame: sidebar and content. TASK-015 and later fill both.
import ConnectionBanner from "./components/ConnectionBanner";
import { useAppStore } from "./store/store";

export default function App() {
  const agents = useAppStore((s) => s.mirror?.agents);
  const connected = useAppStore((s) => s.connected);
  return (
    <div className="app">
      <aside className="sidebar" aria-label="Chats">
        <ul className="chat-list">
          {Object.values(agents ?? {}).map((a) => (
            <li key={a.id} className="chat-item" data-state={a.state}>
              <span className="name">{a.name}</span>
            </li>
          ))}
        </ul>
      </aside>
      <main className="content" data-connected={connected}>
        <ConnectionBanner />
      </main>
    </div>
  );
}
