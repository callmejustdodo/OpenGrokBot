// SC-011 approval sheet: default, expiring, expired, deny-confirm, and
// keep-similar (the keep-similar state is part of TASK-029). No choice is bound
// to Return; Esc asks before denying.
import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { ApprovalChoice } from "../../ipc/generated/ApprovalChoice";
import type { ApprovalId } from "../../ipc/generated/ApprovalId";
import type { RuleDraft } from "../../ipc/generated/RuleDraft";
import { tauriTransport } from "../../ipc/transport";
import { useAppStore } from "../../store/store";
import { act } from "../../store/sync";
import { uiStore } from "../../store/ui";
import Alert, { AlertCancel } from "../../ui/Alert";
import Sheet from "../../ui/Sheet";
import { agentName, approvalFolder, blockReturn, errorCode, memberName, timeLeft, timeLeftText, useNow } from "./model";
import "./approval.css";

type Expiry = "proposed" | "never";

const CHAINING = /&&|\|\||;|\|/;

export default function ApprovalSheet({ approvalId }: { approvalId: ApprovalId }) {
  const { t } = useTranslation();
  const approval = useAppStore((s) => s.mirror?.approvals[approvalId]);
  const requester = useAppStore((s) => (s.mirror && approval ? memberName(s.mirror, approval.requester) : null));
  const agent = useAppStore((s) => (s.mirror && approval ? agentName(s.mirror, null, approval.agent_id) : null));
  const folder = useAppStore((s) => (s.mirror && approval ? approvalFolder(s.mirror, approval) : null));
  const pendingKey = `approval_decide:${approvalId}`;
  const deciding = useAppStore((s) => pendingKey in s.pending);
  const now = useNow();
  const [view, setView] = useState<"choose" | "keep-similar">("choose");
  const [denyOpen, setDenyOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [args, setArgs] = useState<string | null>(null);
  const [expiry, setExpiry] = useState<Expiry>("proposed");

  const close = () => uiStore.getState().closeSheet();
  const onOpenChange = (open: boolean) => {
    if (!open) close();
  };

  if (!approval) {
    return (
      <Sheet
        open
        onOpenChange={onOpenChange}
        title={t("approval.title", "Approval needed")}
        footer={
          <button type="button" className="btn primary" onClick={close}>
            {t("common.close", "Close")}
          </button>
        }
      >
        <p>{t("approval.missing", "This approval is no longer available.")}</p>
      </Sheet>
    );
  }

  const left = timeLeft(approval.expires_at, now);
  const expired = approval.status === "expired" || (approval.status === "pending" && left.kind === "expired");
  const decidable = approval.status === "pending" && !expired;
  const who = requester ?? t("approval.someone", "A teammate");
  const agentLabel = agent ?? t("approval.unknownAgent", "This agent");
  const proposed = approval.proposed_rule?.scope.kind === "command" ? approval.proposed_rule : null;
  const proposedScope = proposed?.scope.kind === "command" ? proposed.scope : null;
  const argPattern = args ?? proposedScope?.arg_pattern ?? "";
  const argsInvalid = argPattern.trim() === "" || CHAINING.test(argPattern);

  const decide = async (choice: ApprovalChoice, rule: RuleDraft | null) => {
    setError(null);
    try {
      await act(tauriTransport, pendingKey, "approval_decide", { approval_id: approval.id, choice, rule });
      close();
    } catch (e) {
      switch (errorCode(e)) {
        case "approval_expired":
          setError(t("approval.error.expired", "This request expired before your choice arrived."));
          break;
        case "conflict":
          setError(t("approval.error.conflict", "This approval was already decided."));
          break;
        case "invalid_params":
          setError(t("approval.error.invalid", "Formation couldn't accept this choice. Allow once or deny instead."));
          break;
        case "disconnected":
          setError(t("approval.error.disconnected", "Formation isn't reachable. Try again in a moment."));
          break;
        default:
          setError(t("approval.error.other", "Your choice wasn't saved. Try again."));
      }
    }
  };

  const saveRule = () => {
    if (!proposed || !proposedScope || argsInvalid) return;
    void decide("keep_allowing_similar", {
      scope: { ...proposedScope, arg_pattern: argPattern.trim() },
      expires_at: expiry === "never" ? null : proposed.expires_at,
      max_uses: proposed.max_uses,
    });
  };

  const onEscape = (event: KeyboardEvent) => {
    if (!decidable) return;
    event.preventDefault();
    setDenyOpen(true);
  };

  const proposedExpiryText = (() => {
    if (!proposed?.expires_at) return t("approval.rule.noExpiry", "No expiry");
    const days = Math.max(1, Math.round((Date.parse(proposed.expires_at) - now) / 86_400_000));
    return t("approval.rule.inDays", "In {{count}} days", { count: days });
  })();

  const lead = decidable || expired ? timeLeftText(t, expired ? { kind: "expired" } : left) : decidedText(t, approval.choice, approval.status);
  const state = expired ? "expired" : !decidable ? approval.status : view === "keep-similar" ? "keep-similar" : left.kind === "expiring" ? "expiring" : "default";

  let footer;
  if (!decidable) {
    footer = (
      <button type="button" className="btn primary" onClick={close}>
        {t("common.close", "Close")}
      </button>
    );
  } else if (view === "keep-similar") {
    footer = (
      <>
        <button type="button" className="btn" onClick={() => setView("choose")} onKeyDown={blockReturn}>
          {t("approval.back", "Back")}
        </button>
        <button
          type="button"
          className="btn primary"
          disabled={deciding || argsInvalid}
          onClick={saveRule}
          onKeyDown={blockReturn}
        >
          {t("approval.saveRule", "Save rule and allow")}
        </button>
      </>
    );
  }

  return (
    <>
      <Sheet open onOpenChange={onOpenChange} title={t("approval.title", "Approval needed")} lead={lead} onEscape={onEscape} footer={footer}>
        <div data-state={state} aria-busy={deciding}>
          <div className="list">
            <dl className="kv">
              <dt>{t("approval.requestedBy", "Requested by")}</dt>
              <dd>
                {who}
                {approval.requester_agent !== null
                  ? ` · ${t("approval.viaAgent", "sent by {{member}}'s agent via CLI", { member: who })}`
                  : null}
              </dd>
              <dt>{t("approval.targetAgent", "Target agent")}</dt>
              <dd>{agentLabel}</dd>
              {approval.purpose ? (
                <>
                  <dt>{t("approval.purpose", "Purpose")}</dt>
                  <dd>{approval.purpose}</dd>
                </>
              ) : null}
              <dt>{t("approval.command", "Command")}</dt>
              <dd>
                <pre className="code">
                  <code>{approval.summary}</code>
                </pre>
                {folder ? <span className="mono muted">{folder}</span> : null}
              </dd>
            </dl>
          </div>

          {expired ? (
            <div className="banner" role="status">
              <div className="bt">
                <b>{t("approval.expired.title", "This request expired")}</b>
                {t("approval.expired.body", "No answer for 24 hours; {{member}} was told.", { member: who })}
              </div>
            </div>
          ) : null}

          {error ? (
            <div className="banner warn" role="alert">
              <div className="bt">{error}</div>
            </div>
          ) : null}

          {decidable && view === "keep-similar" && proposed && proposedScope ? (
            <div className="field">
              <span>{t("approval.rule.title", "Rule to save")}</span>
              <div className="list">
                <dl className="kv">
                  <dt>{t("approval.rule.requester", "Requester")}</dt>
                  <dd>{who}</dd>
                  <dt>{t("approval.rule.agent", "Agent")}</dt>
                  <dd>{agentLabel}</dd>
                  <dt>{t("approval.rule.program", "Program")}</dt>
                  <dd className="mono">{proposedScope.program}</dd>
                  <dt>
                    <label htmlFor="approval-rule-args">{t("approval.rule.arguments", "Arguments")}</label>
                  </dt>
                  <dd>
                    <input
                      id="approval-rule-args"
                      className="input mono"
                      value={argPattern}
                      aria-invalid={argsInvalid}
                      onChange={(e) => setArgs(e.target.value)}
                      onKeyDown={blockReturn}
                    />
                  </dd>
                  <dt>{t("approval.rule.folder", "Folder")}</dt>
                  <dd className="mono">{proposedScope.folder}</dd>
                  <dt>
                    <label htmlFor="approval-rule-expiry">{t("approval.rule.expires", "Expires")}</label>
                  </dt>
                  <dd>
                    <select
                      id="approval-rule-expiry"
                      className="select"
                      value={expiry}
                      onChange={(e) => setExpiry(e.target.value as Expiry)}
                    >
                      {proposed.expires_at ? <option value="proposed">{proposedExpiryText}</option> : null}
                      <option value="never">{t("approval.rule.noExpiry", "No expiry")}</option>
                    </select>
                  </dd>
                </dl>
                <p className="muted small">
                  {t("approval.rule.note", "Never allows chaining (&&, ;) or other folders.")}
                </p>
              </div>
            </div>
          ) : null}

          {decidable && view === "choose" ? (
            <>
              <div className="field" role="group" aria-labelledby="approval-choices">
                <span id="approval-choices">{t("approval.choice", "Your choice")}</span>
                <div className="choice-list">
                  <button
                    type="button"
                    className="btn"
                    disabled={deciding}
                    onClick={() => void decide("allow_once", null)}
                    onKeyDown={blockReturn}
                  >
                    <span>{t("approval.allowOnce", "Allow once")}</span>
                    <small>{t("approval.allowOnceHint", "This command only")}</small>
                  </button>
                  <button
                    type="button"
                    className="btn"
                    disabled={deciding || !proposed}
                    onClick={() => {
                      if (!proposed?.expires_at) setExpiry("never");
                      setView("keep-similar");
                    }}
                    onKeyDown={blockReturn}
                  >
                    <span>{t("approval.keepSimilar", "Keep allowing similar commands…")}</span>
                    <small>
                      {proposed
                        ? t("approval.keepSimilarHint", "Review the rule first")
                        : t("approval.keepSimilarNone", "No rule fits this action")}
                    </small>
                  </button>
                  <button
                    type="button"
                    className="btn"
                    disabled={deciding}
                    onClick={() => void decide("allow_all", { scope: { kind: "all" }, expires_at: null, max_uses: null })}
                    onKeyDown={blockReturn}
                  >
                    <span>
                      {t("approval.allowAll", "Allow all for {{member}} · {{agent}}", { member: who, agent: agentLabel })}
                    </span>
                    <small>{t("approval.allowAllHint", "This requester and agent")}</small>
                  </button>
                  <button
                    type="button"
                    className="btn destructive"
                    disabled={deciding}
                    onClick={() => setDenyOpen(true)}
                    onKeyDown={blockReturn}
                  >
                    <span>{t("approval.deny", "Deny")}</span>
                    <small>Esc</small>
                  </button>
                </div>
              </div>
              <p className="muted small">{t("approval.noReturn", "Return doesn't pick any choice, to prevent accidents.")}</p>
            </>
          ) : null}
        </div>
      </Sheet>
      <Alert
        open={denyOpen}
        onOpenChange={setDenyOpen}
        title={t("approval.denyConfirm.title", "Deny this action?")}
        description={t(
          "approval.denyConfirm.body",
          "{{member}} will be told it was denied. The agent won't run this command.",
          { member: who },
        )}
      >
        <button
          type="button"
          className="btn destructive"
          onClick={() => {
            setDenyOpen(false);
            void decide("deny", null);
          }}
          onKeyDown={blockReturn}
        >
          {t("approval.deny", "Deny")}
        </button>
        <AlertCancel className="btn">{t("common.cancel", "Cancel")}</AlertCancel>
      </Alert>
    </>
  );
}

function decidedText(
  t: ReturnType<typeof useTranslation>["t"],
  choice: ApprovalChoice | null,
  status: string,
): string {
  if (status === "denied") return t("approval.decided.denied", "Denied");
  switch (choice) {
    case "keep_allowing_similar":
      return t("approval.decided.similar", "Allowed · similar commands keep running");
    case "allow_all":
      return t("approval.decided.all", "Allowed all for this requester and agent");
    default:
      return t("approval.decided.once", "Allowed once");
  }
}
