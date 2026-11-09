// The preview page (D-050): the whole main window in any M3 state, with a review
// bar like the mockup's for the screen, the state, the theme and the UI language.
import { useEffect, useState } from "react";
import i18n from "../i18n";
import { formatHash, parseHash, type Choice, type Lang, type Theme } from "./address";
import { findState, SCREENS, STATES } from "./catalog";
import Stage from "./Stage";
import "./preview.css";

export default function PreviewApp() {
  const [choice, setChoice] = useState<Choice>(() => parseHash(location.hash));
  const state = findState(choice.id)!;

  useEffect(() => {
    const onHash = () => setChoice(parseHash(location.hash));
    addEventListener("hashchange", onHash);
    return () => removeEventListener("hashchange", onHash);
  }, []);

  useEffect(() => {
    document.documentElement.dataset.theme = choice.theme;
    document.documentElement.lang = choice.lang;
    if (i18n.language !== choice.lang) void i18n.changeLanguage(choice.lang);
    const hash = formatHash(choice);
    if (location.hash !== hash) history.replaceState(null, "", hash);
  }, [choice]);

  const pick = (next: Partial<Choice>) => setChoice((c) => ({ ...c, ...next }));
  const states = STATES.filter((s) => s.screen === state.screen);

  return (
    <div className="preview">
      <header className="review" aria-label="Preview controls, not product UI">
        <strong>Preview</strong>
        <label>
          Screen{" "}
          <select value={state.screen} onChange={(e) => pick({ id: STATES.find((s) => s.screen === e.target.value)!.id })}>
            {SCREENS.map((s) => (
              <option key={s}>{s}</option>
            ))}
          </select>
        </label>
        <label>
          State{" "}
          <select value={state.id} onChange={(e) => pick({ id: e.target.value })}>
            {states.map((s) => (
              <option key={s.id} value={s.id}>
                {s.state}
              </option>
            ))}
          </select>
        </label>
        <label>
          Theme{" "}
          <select value={choice.theme} onChange={(e) => pick({ theme: e.target.value as Theme })}>
            <option value="light">Light</option>
            <option value="dark">Dark</option>
          </select>
        </label>
        <label>
          Language{" "}
          <select value={choice.lang} onChange={(e) => pick({ lang: e.target.value as Lang })}>
            <option value="ko">한국어</option>
            <option value="en">English</option>
          </select>
        </label>
        <span className="note">{state.description}</span>
      </header>
      <div className="preview-stage">
        <Stage key={state.id} state={state} />
      </div>
    </div>
  );
}
