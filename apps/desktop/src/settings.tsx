import React from "react";
import ReactDOM from "react-dom/client";
import "./i18n";
import { startStore } from "./store/boot";
import Settings from "./views/settings/Settings";
import { followLanguageSetting } from "./views/settings/language";

startStore();
followLanguageSetting();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <Settings />
  </React.StrictMode>,
);
