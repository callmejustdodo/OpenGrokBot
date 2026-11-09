// @vitest-environment node
// The copy catalog stays in step with the code (TASK-040): en.json is what the
// `t()` calls say, and ko.json translates exactly those keys.
import { describe, expect, it } from "vitest";
import { checkKorean, extract, nest } from "../../scripts/i18n.mjs";
import en from "./en.json";
import ko from "./ko.json";

describe("i18n catalog", () => {
  const { messages, plurals, problems } = extract();

  it("reads every t() call", () => {
    expect(problems).toEqual([]);
  });

  it("en.json matches the English defaults in the code (run `pnpm i18n:extract`)", () => {
    expect(en).toEqual(nest(messages));
  });

  it("ko.json translates every key, keeps the placeholders, and has no unused keys", () => {
    expect(checkKorean(messages, plurals, ko)).toEqual({ missing: [], unused: [], placeholderMismatch: [] });
  });
});
