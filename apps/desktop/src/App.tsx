// The main window's frame: sidebar and content. TASK-015 and later fill both;
// this shows formationd's agents so the shell can be checked against the fake.
import { useSnapshot } from "./useSnapshot";

export default function App() {
  const { connected, snapshot } = useSnapshot();
  const agents = snapshot?.agents ?? [];
  return (
    <div className="app">
      <aside className="sidebar" aria-label="Chats">
        <ul className="chat-list">
          {agents.map((a) => (
            <li key={a.id} className="chat-item" data-state={a.state}>
              <span className="name">{a.name}</span>
            </li>
          ))}
        </ul>
      </aside>
      <main className="content" data-connected={connected} />
    </div>
  );
}
