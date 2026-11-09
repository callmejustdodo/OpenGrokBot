import React from "react";
import ReactDOM from "react-dom/client";
import MenubarPanel from "./MenubarPanel";
import { startStore } from "./store/boot";

startStore();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <MenubarPanel />
  </React.StrictMode>,
);
