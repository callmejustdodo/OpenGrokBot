// What the Rules page (SC-012) shows for each saved rule: who and what it
// covers, its limits, and whether it still applies.
import type { TFunction } from "i18next";
import type { Rule } from "../../ipc/generated/Rule";
import type { RuleScope } from "../../ipc/generated/RuleScope";
import type { MirrorState } from "../../store/reducer";
import { agentName, memberName } from "../approval/model";

/** `used-up`: the use limit is reached. Revoked rules leave the page. */
export type RuleStatus = "active" | "expired" | "used-up";

export type RuleRow = {
  rule: Rule;
  status: RuleStatus;
  requester: string | null;
  agent: string | null;
};

const DAY_MS = 86_400_000;
const HOUR_MS = 3_600_000;

export function ruleStatus(rule: Rule, now: number): RuleStatus | "revoked" {
  if (rule.revoked_at !== null) return "revoked";
  if (rule.expires_at !== null && Date.parse(rule.expires_at) <= now) return "expired";
  if (rule.max_uses !== null && rule.uses >= rule.max_uses) return "used-up";
  return "active";
}

/** Rules that are not revoked: the ones still applying first, then newest first. */
export function selectRules(state: MirrorState, now: number): RuleRow[] {
  const rows: RuleRow[] = [];
  for (const rule of Object.values(state.rules)) {
    const status = ruleStatus(rule, now);
    if (status === "revoked") continue;
    rows.push({
      rule,
      status,
      requester: memberName(state, rule.requester),
      agent: agentName(state, null, rule.agent_id),
    });
  }
  return rows.sort((a, b) => {
    const live = Number(b.status === "active") - Number(a.status === "active");
    return live !== 0 ? live : Date.parse(b.rule.created_at) - Date.parse(a.rule.created_at);
  });
}

/** What the rule allows, without the folder: "npm test -- auth/*". */
export function scopeText(t: TFunction, scope: RuleScope): string {
  if (scope.kind === "all") return t("rules.scope.all", "Every action");
  return scope.arg_pattern ? `${scope.program} ${scope.arg_pattern}` : scope.program;
}

export function expiryText(t: TFunction, expiresAt: string | null, now: number): string {
  if (expiresAt === null) return t("rules.noExpiry", "No expiry");
  const ms = Date.parse(expiresAt) - now;
  if (ms <= 0) return t("rules.expired", "Expired");
  if (ms < DAY_MS) {
    return t("rules.hoursLeft", {
      count: Math.ceil(ms / HOUR_MS),
      defaultValue_one: "{{count}} hour left",
      defaultValue_other: "{{count}} hours left",
    });
  }
  return t("rules.daysLeft", {
    count: Math.floor(ms / DAY_MS),
    defaultValue_one: "{{count}} day left",
    defaultValue_other: "{{count}} days left",
  });
}

export function usesText(t: TFunction, rule: Rule): string {
  if (rule.max_uses === null) return t("rules.used", "used {{count}}×", { count: rule.uses });
  return t("rules.usedOf", "used {{count}} of {{max}}", { count: rule.uses, max: rule.max_uses });
}

export type ExpiryChoice = "keep" | "1d" | "7d" | "30d" | "never";

/** The `expires_at` an edit sends for a choice. */
export function expiryFor(choice: ExpiryChoice, current: string | null, now: number): string | null {
  switch (choice) {
    case "keep":
      return current;
    case "never":
      return null;
    case "1d":
      return new Date(now + DAY_MS).toISOString();
    case "7d":
      return new Date(now + 7 * DAY_MS).toISOString();
    case "30d":
      return new Date(now + 30 * DAY_MS).toISOString();
  }
}

/**
 * The `max_uses` an edit sends: blank removes the limit. A limit at or below the
 * uses so far would stop the rule at once, so it is refused.
 */
export function parseMaxUses(text: string, uses: number): { ok: true; value: number | null } | { ok: false } {
  const trimmed = text.trim();
  if (trimmed === "") return { ok: true, value: null };
  if (!/^\d+$/.test(trimmed)) return { ok: false };
  const value = Number(trimmed);
  return value > uses && Number.isSafeInteger(value) ? { ok: true, value } : { ok: false };
}
