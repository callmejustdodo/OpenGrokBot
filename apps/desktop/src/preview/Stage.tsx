// The main window in one preview state. Mount it with `key={state.id}`: the
// state is loaded into the stores once per mount, so component state starts fresh.
import { useEffect, useState } from "react";
import App from "../App";
import { loadState, runStep, type PreviewState } from "./catalog";

export default function Stage({ state }: { state: PreviewState }) {
  // Loaded before the first render, so the app never draws the previous state.
  useState(() => loadState(state));
  useEffect(() => {
    const step = state.ui.step;
    if (!step) return;
    const timer = setTimeout(() => runStep(step), 0);
    return () => clearTimeout(timer);
  }, [state]);
  return <App />;
}
