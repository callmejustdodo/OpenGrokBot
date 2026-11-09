import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./i18n";
import "./styles/index.css";
import "./notifications/main-window";
import { startStore } from "./store/boot";
import { followLanguageSetting } from "./views/settings/language";

startStore();
// Follow the Settings language choice, or macOS when none is set (D-033).
followLanguageSetting();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
