// The preview page's address keeps its choice, so a link opens a screen and state
// directly: `preview.html#SC-005/working?theme=dark&lang=en`. A screen alone
// (`#SC-012`) opens its first state.
import { findState, STATES } from "./catalog";

export type Theme = "light" | "dark";
export type Lang = "ko" | "en";
export type Choice = { id: string; theme: Theme; lang: Lang };

export function parseHash(hash: string): Choice {
  const [path, query = ""] = hash.replace(/^#/, "").split("?");
  const params = new URLSearchParams(query);
  const id = findState(path) ? path : (STATES.find((s) => s.screen === path)?.id ?? STATES[0].id);
  return {
    id,
    theme: params.get("theme") === "dark" ? "dark" : "light",
    lang: params.get("lang") === "en" ? "en" : "ko",
  };
}

export function formatHash({ id, theme, lang }: Choice): string {
  return `#${id}?theme=${theme}&lang=${lang}`;
}
