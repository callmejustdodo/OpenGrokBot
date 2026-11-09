import React from "react";
import ReactDOM from "react-dom/client";
import MenubarPanel from "./MenubarPanel";
import "./i18n";
import { tauriTransport } from "./ipc/transport";
import { startNotificationBridge } from "./notifications/bridge";
import { startStore } from "./store/boot";
import { followLanguageSetting } from "./views/settings/language";

startStore();
followLanguageSetting();
// Only this window turns `notify` events into macOS notifications (SC-019).
startNotificationBridge(tauriTransport);

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <MenubarPanel />
  </React.StrictMode>,
);
