import { describe, expect, it } from "vitest";
import i18n from "../i18n";
import { shortAgo } from "./ago";

const t = i18n.getFixedT("en");
const NOW = Date.parse("2026-11-09T12:00:00Z");
const before = (ms: number) => new Date(NOW - ms).toISOString();

describe("shortAgo", () => {
  it("reads in the panel's short units", () => {
    expect(shortAgo(t, before(20_000), NOW)).toBe("now");
    expect(shortAgo(t, before(3 * 60_000), NOW)).toBe("3m");
    expect(shortAgo(t, before(59 * 60_000 + 59_000), NOW)).toBe("59m");
    expect(shortAgo(t, before(2 * 3_600_000 + 5 * 60_000), NOW)).toBe("2h");
    expect(shortAgo(t, before(4 * 86_400_000), NOW)).toBe("4d");
  });

  it("treats a time slightly in the future as now", () => {
    expect(shortAgo(t, before(-30_000), NOW)).toBe("now");
  });
});
