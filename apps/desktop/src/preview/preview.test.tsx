import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import statesJson from "../../../../tools/fake-formationd/fixtures/states.json";
import "../i18n";
import i18n from "../i18n";
import { appStore } from "../store/store";
import { uiStore } from "../store/ui";
import { findState, prepare, STATES } from "./catalog";
import { formatHash, parseHash } from "./address";
import Stage from "./Stage";
import { installTauriMock } from "./tauriMock";

// Radix measures some controls; jsdom has no ResizeObserver.
globalThis.ResizeObserver ??= class {
  observe() {}
  unobserve() {}
  disconnect() {}
};

beforeAll(async () => {
  installTauriMock();
  await i18n.changeLanguage("en");
});

afterEach(() => vi.restoreAllMocks());

describe("preview catalog", () => {
  it("has every state in states.json", () => {
    for (const id of Object.keys(statesJson)) expect(findState(id), id).toBeDefined();
  });

  it("fills placeholders and moves fixture times to now", () => {
    const now = Date.parse("2026-12-01T00:00:00Z");
    const shift = now - Date.parse("2026-11-09T01:53:00Z");
    expect(prepare({ at: "{{now}}", id: "{{id:q}}", same: "{{id:q}}", t: "2026-11-09T01:50:00Z" }, shift)).toEqual({
      at: "2026-12-01T00:00:00Z",
      id: expect.stringMatching(/^01930000-/),
      same: expect.any(String),
      t: "2026-11-30T23:57:00Z",
    });
  });

  it("opens a state from the address", () => {
    expect(parseHash("#SC-011/expired?theme=dark&lang=en")).toEqual({ id: "SC-011/expired", theme: "dark", lang: "en" });
    expect(parseHash("#SC-012").id).toBe("SC-012/default");
    expect(formatHash({ id: "SC-005/idle", theme: "light", lang: "ko" })).toBe("#SC-005/idle?theme=light&lang=ko");
  });
});

describe.each(STATES.map((s) => [s.id, s] as const))("preview state %s", (_id, state) => {
  it("loads and renders without throwing", async () => {
    const errors = vi.spyOn(console, "error").mockImplementation(() => {});
    render(<Stage state={state} />);
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(appStore.getState().mirror).not.toBeNull();
    expect(appStore.getState().stale).toBe(false);
    expect(uiStore.getState().view).toEqual(state.ui.view ?? { kind: "home" });
    if (state.ui.sheet) expect(screen.getAllByRole("dialog", { hidden: true }).length).toBeGreaterThan(0);
    // The page did the state's click: a confirmation, or the sheet's keep-similar view.
    if (state.ui.step === "keep-similar") expect(document.querySelector('[data-state="keep-similar"]')).not.toBeNull();
    else if (state.ui.step) expect(screen.getByRole("alertdialog")).toBeInTheDocument();
    expect(errors).not.toHaveBeenCalled();
  });
});
