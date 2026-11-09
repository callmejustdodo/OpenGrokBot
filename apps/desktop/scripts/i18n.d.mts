// Types for scripts/i18n.mjs, so the catalog test in src/i18n can import it.
export declare const enPath: string;
export declare const koPath: string;
export declare function extract(): { messages: Map<string, string>; plurals: Set<string>; problems: string[] };
export declare function nest(messages: Map<string, string>, problems?: string[]): Record<string, unknown>;
export declare function flatten(tree: Record<string, unknown>): Map<string, unknown>;
export declare function checkKorean(
  messages: Map<string, string>,
  plurals: Set<string>,
  ko: Record<string, unknown>,
): { missing: string[]; unused: string[]; placeholderMismatch: string[] };
