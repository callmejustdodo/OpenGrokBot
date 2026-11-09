// SC-012 rules: what runs without asking. The always-allowed file reads come
// first, then each saved rule with who and what it covers, its limits, expiry
// and use count; limits can be edited and a rule revoked after a confirm.
import { useEffect, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";
import type { RuleId } from "../../ipc/generated/RuleId";
import { tauriTransport } from "../../ipc/transport";
import { useAppStore } from "../../store/store";
import { act } from "../../store/sync";
import Alert, { AlertCancel } from "../../ui/Alert";
import { errorCode, useNow } from "../approval/model";
import {
  expiryFor,
  expiryText,
  parseMaxUses,
  scopeText,
  selectRules,
  usesText,
  type ExpiryChoice,
  type RuleRow,
} from "./selectors";
import "./rules.css";

type Failure = { message: string; action: "retry" | "dismiss"; retry: () => void };

export default function Rules() {
  const { t } = useTranslation();
  const mirror = useAppStore((s) => s.mirror);
  const pending = useAppStore((s) => s.pending);
  const now = useNow();
  const [editing, setEditing] = useState<RuleId | null>(null);
  const [revoking, setRevoking] = useState<RuleRow | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [failure, setFailure] = useState<Failure | null>(null);
  const title = t("rules.title", "Rules");

  if (!mirror) return <section className="page" aria-label={title} />;
  const rows = selectRules(mirror, now);

  const who = (row: RuleRow) => row.requester ?? t("rules.someone", "A teammate");
  const agentOf = (row: RuleRow) => row.agent ?? t("rules.unknownAgent", "An agent");

  const fail = (error: unknown, retry: () => void, what: "revoke" | "update") => {
    switch (errorCode(error)) {
      case "not_found":
        setFailure({
          message: t("rules.error.notFound", "This rule no longer exists."),
          action: "dismiss",
          retry,
        });
        return;
      case "invalid_params":
        setFailure({
          message: t("rules.error.invalid", "Formation couldn't accept these limits."),
          action: "dismiss",
          retry,
        });
        return;
      case "disconnected":
        setFailure({
          message: t("rules.error.disconnected", "Formation isn't reachable. Try again in a moment."),
          action: "retry",
          retry,
        });
        return;
      default:
        setFailure({
          message:
            what === "revoke"
              ? t("rules.error.revoke", "The rule wasn't revoked. It still applies.")
              : t("rules.error.update", "The new limits weren't saved."),
          action: "retry",
          retry,
        });
    }
  };

  const revoke = (row: RuleRow) => {
    setRevoking(null);
    setFailure(null);
    setNotice(null);
    act(tauriTransport, `rules_revoke:${row.rule.id}`, "rules_revoke", { rule_id: row.rule.id }).then(
      () => setNotice(t("rules.revoked", "Rule revoked")),
      (e: unknown) => fail(e, () => revoke(row), "revoke"),
    );
  };

  const update = (row: RuleRow, expiresAt: string | null, maxUses: number | null) => {
    setFailure(null);
    setNotice(null);
    act(tauriTransport, `rules_update:${row.rule.id}`, "rules_update", {
      rule_id: row.rule.id,
      expires_at: expiresAt,
      max_uses: maxUses,
    }).then(
      () => {
        setEditing(null);
        setNotice(t("rules.updated", "Limits saved"));
      },
      (e: unknown) => fail(e, () => update(row, expiresAt, maxUses), "update"),
    );
  };

  const banners = (
    <>
      {notice ? (
        <p className="banner" role="status">
          {notice}
        </p>
      ) : null}
      {failure ? (
        <div className="banner warn" role="alert">
          <div className="bt">{failure.message}</div>
          {failure.action === "retry" ? (
            <button type="button" className="btn" onClick={failure.retry}>
              {t("rules.tryAgain", "Try again")}
            </button>
          ) : (
            <button type="button" className="btn" onClick={() => setFailure(null)}>
              {t("rules.dismiss", "Dismiss")}
            </button>
          )}
        </div>
      ) : null}
    </>
  );

  if (rows.length === 0) {
    return (
      <section className="page" aria-label={title} data-state="empty">
        <div className="h1row">
          <h1>{title}</h1>
          <span className="spacer" />
        </div>
        {banners}
        <div className="empty">
          <b>{t("rules.empty.title", "No saved rules")}</b>
          {t("rules.empty.body", "Rules appear here when you choose 'Keep allowing similar' while approving.")}
        </div>
      </section>
    );
  }

  return (
    <section className="page" aria-label={title} data-state="default">
      <div className="h1row">
        <h1>{title}</h1>
        <span className="lead">{t("rules.lead", "What runs without asking")}</span>
        <span className="spacer" />
      </div>
      {banners}
      <div className="list always">
        <div className="row">
          <span className="ok" aria-hidden="true">
            ✓
          </span>
          <span className="t">
            <b>{t("rules.always.title", "Always allowed")}</b>
            <small>{t("rules.always.body", "File reads inside an agent's folder and assigned repositories")}</small>
          </span>
        </div>
      </div>
      <ul className="list" aria-label={t("rules.saved", "Saved rules")}>
        {rows.map((row) => {
          const id = row.rule.id;
          const busy = `rules_revoke:${id}` in pending || `rules_update:${id}` in pending;
          const scope = row.rule.scope;
          const heading = `${who(row)} → ${agentOf(row)}`;
          return (
            <li key={id} className="row" data-status={row.status} aria-busy={busy}>
              <span className="t">
                <b>{heading}</b>
                <small className="mono">
                  {scopeText(t, scope)}
                  {scope.kind === "command" ? ` · ${scope.folder}` : null}
                </small>
              </span>
              {row.status === "expired" ? (
                <span className="pill warn">{t("rules.status.expired", "Expired · no longer applies")}</span>
              ) : row.status === "used-up" ? (
                <span className="pill warn">{t("rules.status.usedUp", "Limit reached · no longer applies")}</span>
              ) : null}
              <span className="when">
                {expiryText(t, row.rule.expires_at, now)} · {usesText(t, row.rule)}
              </span>
              <button
                type="button"
                className="btn plain"
                aria-label={t("rules.editLabel", "Edit limits of the rule {{rule}}", { rule: heading })}
                aria-expanded={editing === id}
                disabled={busy}
                onClick={() => setEditing(editing === id ? null : id)}
              >
                {t("rules.edit", "Edit")}
              </button>
              <button
                type="button"
                className="btn plain"
                aria-label={t("rules.revokeLabel", "Revoke the rule {{rule}}", { rule: heading })}
                disabled={busy}
                onClick={() => setRevoking(row)}
              >
                {t("rules.revoke", "Revoke")}
              </button>
              {editing === id ? (
                <LimitsEditor
                  row={row}
                  now={now}
                  busy={busy}
                  onCancel={() => setEditing(null)}
                  onSave={(expiresAt, maxUses) => update(row, expiresAt, maxUses)}
                />
              ) : null}
            </li>
          );
        })}
      </ul>
      <Alert
        open={revoking !== null}
        onOpenChange={(open) => !open && setRevoking(null)}
        title={t("rules.revokeConfirm.title", "Revoke this rule?")}
        description={
          revoking
            ? t(
                "rules.revokeConfirm.body",
                "{{member}}'s requests to {{agent}} will ask you again before running {{scope}}. This applies at once.",
                { member: who(revoking), agent: agentOf(revoking), scope: scopeText(t, revoking.rule.scope) },
              )
            : null
        }
      >
        <button type="button" className="btn destructive" onClick={() => revoking && revoke(revoking)}>
          {t("rules.revoke", "Revoke")}
        </button>
        <AlertCancel asChild>
          <button type="button" className="btn">
            {t("rules.cancel", "Cancel")}
          </button>
        </AlertCancel>
      </Alert>
    </section>
  );
}

function LimitsEditor({
  row,
  now,
  busy,
  onCancel,
  onSave,
}: {
  row: RuleRow;
  now: number;
  busy: boolean;
  onCancel: () => void;
  onSave: (expiresAt: string | null, maxUses: number | null) => void;
}) {
  const { t } = useTranslation();
  const { rule } = row;
  const [expiry, setExpiry] = useState<ExpiryChoice>(rule.expires_at === null ? "never" : "keep");
  const [maxUses, setMaxUses] = useState(rule.max_uses === null ? "" : String(rule.max_uses));
  const first = useRef<HTMLSelectElement>(null);
  const parsed = parseMaxUses(maxUses, rule.uses);
  const ids = `rule-${rule.id}`;

  useEffect(() => first.current?.focus(), []);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!parsed.ok || busy) return;
    onSave(expiryFor(expiry, rule.expires_at, Date.now()), parsed.value);
  };

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Escape") {
      event.preventDefault();
      onCancel();
    }
  };

  return (
    <form
      className="field limits"
      aria-label={t("rules.limits", "Limits")}
      onSubmit={submit}
      onKeyDown={onKeyDown}
    >
      <label htmlFor={`${ids}-expiry`}>{t("rules.expires", "Expires")}</label>
      <select
        id={`${ids}-expiry`}
        ref={first}
        className="select"
        value={expiry}
        onChange={(e) => setExpiry(e.target.value as ExpiryChoice)}
      >
        {rule.expires_at !== null ? (
          <option value="keep">
            {t("rules.keepExpiry", "Keep ({{current}})", { current: expiryText(t, rule.expires_at, now) })}
          </option>
        ) : null}
        <option value="1d">{t("rules.in1d", "In 1 day")}</option>
        <option value="7d">{t("rules.in7d", "In 7 days")}</option>
        <option value="30d">{t("rules.in30d", "In 30 days")}</option>
        <option value="never">{t("rules.noExpiry", "No expiry")}</option>
      </select>
      <label htmlFor={`${ids}-max`}>{t("rules.maxUses", "Use limit")}</label>
      <input
        id={`${ids}-max`}
        className="input"
        inputMode="numeric"
        placeholder={t("rules.noLimit", "No limit")}
        value={maxUses}
        aria-invalid={!parsed.ok}
        aria-describedby={`${ids}-max-hint`}
        onChange={(e) => setMaxUses(e.target.value)}
      />
      <small id={`${ids}-max-hint`} className={parsed.ok ? "muted" : "error"}>
        {parsed.ok
          ? t("rules.maxUsesHint", "Leave blank for no limit. Used {{count}}× so far.", { count: rule.uses })
          : t("rules.maxUsesInvalid", "Enter a whole number above {{count}}, the uses so far.", { count: rule.uses })}
      </small>
      <div className="row-actions">
        <button type="button" className="btn" onClick={onCancel}>
          {t("rules.cancel", "Cancel")}
        </button>
        <button type="submit" className="btn primary" disabled={!parsed.ok || busy}>
          {t("rules.save", "Save limits")}
        </button>
      </div>
    </form>
  );
}
