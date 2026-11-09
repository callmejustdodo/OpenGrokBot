// The preview page's entry (preview.html, development only). The Tauri mock is
// installed before anything calls `@tauri-apps/api`; the store is loaded by the
// preview itself, so `startStore` and the main window's notifications are not used.
import React from "react";
import ReactDOM from "react-dom/client";
import "../i18n";
import "../styles/index.css";
import PreviewApp from "./PreviewApp";
import { installTauriMock } from "./tauriMock";

installTauriMock();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <PreviewApp />
  </React.StrictMode>,
);
