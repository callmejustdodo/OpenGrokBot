import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./i18n";
import "./notifications/main-window";
import { startStore } from "./store/boot";

startStore();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
