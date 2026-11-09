// Keeps a window's i18next language on the Settings choice (D-033): Korean,
// English, or the macOS language when none is set.
import i18n, { systemLanguage } from "../../i18n";
import type { Language } from "../../ipc/generated/Language";
import { appStore, type AppStore } from "../../store/store";
import type { Unsubscribe } from "../../store/transport";

export function applyLanguage(language: Language | null): void {
  const next = language ?? systemLanguage();
  if (i18n.language !== next) void i18n.changeLanguage(next);
}

export function followLanguageSetting(store: AppStore = appStore): Unsubscribe {
  let last: Language | null | undefined;
  const check = () => {
    const mirror = store.getState().mirror;
    if (!mirror || mirror.settings.language === last) return;
    last = mirror.settings.language;
    applyLanguage(last);
  };
  check();
  return store.subscribe(check);
}
