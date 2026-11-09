// The main window: sidebar, content, information panel, and sheets.
import ConnectionBanner from "./components/ConnectionBanner";
import InfoPanel from "./components/info-panel/InfoPanel";
import Sidebar from "./components/sidebar/Sidebar";
import { useAppStore } from "./store/store";
import { useUi } from "./store/ui";
import SheetHost from "./views/SheetHost";
import ViewRouter from "./views/ViewRouter";

export default function App() {
  const connected = useAppStore((s) => s.connected);
  const panelOpen = useUi((s) => s.panelOpen);
  return (
    <div className="app">
      <Sidebar />
      <main className="content" data-connected={connected}>
        <ConnectionBanner />
        <ViewRouter />
      </main>
      {panelOpen ? <InfoPanel /> : null}
      <SheetHost />
    </div>
  );
}
