// i18next setup (D-033): Korean and English, following the macOS language
// unless Settings chooses one. Code calls `t("view.key", "English default")`,
// so a missing translation shows the English text. `pnpm i18n:extract` writes
// en.json from those defaults; ko.json holds the Korean (TASK-040).
import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import en from "./en.json";
import ko from "./ko.json";

export function systemLanguage(): "ko" | "en" {
  return typeof navigator !== "undefined" && navigator.language.toLowerCase().startsWith("ko") ? "ko" : "en";
}

void i18n.use(initReactI18next).init({
  resources: { en: { translation: en }, ko: { translation: ko } },
  lng: systemLanguage(),
  fallbackLng: "en",
  interpolation: { escapeValue: false },
  returnNull: false,
});

export default i18n;
