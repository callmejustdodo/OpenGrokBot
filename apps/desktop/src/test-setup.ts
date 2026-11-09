import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach, beforeEach } from "vitest";
import i18n from "./i18n";

// Tests assert the English copy whatever the machine's language is.
beforeEach(async () => {
  await i18n.changeLanguage("en");
});

// Without vitest globals, Testing Library does not clean up by itself.
afterEach(() => cleanup());
