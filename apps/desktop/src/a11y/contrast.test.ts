// @vitest-environment node
/// <reference types="node" />
// NFR-009: WCAG 2 contrast of the v2 tokens in styles/tokens.css, light and
// dark. Text colors need 4.5:1 on every opaque surface they appear on; the
// focus ring needs 3:1 (non-text UI).
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

// Read from disk (in Node, not jsdom): Vitest stubs CSS imports, `?raw` included.
const tokens = readFileSync(new URL("../styles/tokens.css", import.meta.url), "utf8");

/** The `--name: #rrggbb` values in the first block that `selector` opens. */
function block(selector: string): Record<string, string> {
  const start = tokens.indexOf(`${selector} {`);
  if (start === -1) throw new Error(`no ${selector} block`);
  const body = tokens.slice(start, tokens.indexOf("}", start));
  return Object.fromEntries(Array.from(body.matchAll(/--([\w-]+):\s*(#[0-9a-f]{6})\b/gi), (m: RegExpMatchArray) => [m[1], m[2]]));
}

function luminance(hex: string): number {
  const [r, g, b] = [1, 3, 5].map((i) => {
    const v = parseInt(hex.slice(i, i + 2), 16) / 255;
    return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

const light = block(":root");
const themes = { light, dark: { ...light, ...block(':root[data-theme="dark"]') } };

const TEXT = ["text", "text2", "text3", "brand", "green", "orange", "yellow", "red"];
const SURFACES = ["win", "side", "card", "bub", "sec"];

describe.each(Object.entries(themes))("%s tokens (NFR-009)", (_, t) => {
  it.each(TEXT)("--%s reaches 4.5:1 on every surface", (fg) => {
    for (const bg of SURFACES) {
      expect({ fg, bg, ratio: contrast(t[fg], t[bg]) >= 4.5 }).toEqual({ fg, bg, ratio: true });
    }
  });

  it("button and bubble text reach 4.5:1 on their fill", () => {
    expect(contrast(t["primary-fg"], t.primary)).toBeGreaterThanOrEqual(4.5);
    expect(contrast(t["me-fg"], t.me)).toBeGreaterThanOrEqual(4.5);
  });

  it("the focus ring reaches 3:1 on the window and cards", () => {
    expect(contrast(t.focus, t.win)).toBeGreaterThanOrEqual(3);
    expect(contrast(t.focus, t.card)).toBeGreaterThanOrEqual(3);
  });
});

describe("the dark block under prefers-color-scheme matches the forced one", () => {
  it("has the same values", () => {
    const media = tokens.slice(tokens.indexOf("@media (prefers-color-scheme: dark)"));
    const forced = block(':root[data-theme="dark"]');
    const auto = Object.fromEntries(
      Array.from(media.slice(0, media.indexOf("}")).matchAll(/--([\w-]+):\s*(#[0-9a-f]{6})\b/gi), (m: RegExpMatchArray) => [m[1], m[2]]),
    );
    expect(auto).toEqual(forced);
  });
});
