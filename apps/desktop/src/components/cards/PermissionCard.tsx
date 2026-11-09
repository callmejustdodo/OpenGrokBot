// SC-005 approval item in my conversation. A teammate's request shows a compact
// row whose Review opens SC-011; the harness's own permission prompt for my
// work (no request) shows its choices in place.
import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { ApprovalChoice } from "../../ipc/generated/ApprovalChoice";
import type { ApprovalId } from "../../ipc/generated/ApprovalId";
import type { ApprovalStatus } from "../../ipc/generated/ApprovalStatus";
import type { HarnessKind } from "../../ipc/generated/HarnessKind";
import { tauriTransport } from "../../ipc/transport";
import { useAppStore } from "../../store/store";
import { act } from "../../store/sync";
import { uiStore } from "../../store/ui";
import { errorCode } from "../../views/approval/model";

export default function PermissionCard({ approvalId }: { approvalId: ApprovalId }) {
  const { t } = useTranslation();
  const approval = useAppStore((s) => s.mirror?.approvals[approvalId]);
  const harness = useAppStore((s) => (approval ? s.mirror?.agents[approval.agent_id]?.harness : undefined));
  const pendingKey = `approval_decide:${approvalId}`;
  const deciding = useAppStore((s) => pendingKey in s.pending);
  const [error, setError] = useState<string | null>(null);

  if (!approval) return null;
  const waiting = approval.status === "pending";

  if (approval.request_id !== null) {
    return (
      <section className="icard" data-state={approval.status} aria-label={t("permission.requestApproval", "Approval")}>
        <div className="row-actions">
          <span>
            {waiting
              ? t("permission.waiting", "Approval waiting")
              : decisionText(t, approval.status, approval.choice, false)}
          </span>
          <span className="mono muted">{approval.summary}</span>
          <span className="spacer" />
          {waiting ? (
            <button
              type="button"
              className="btn primary"
              onClick={() => uiStore.getState().openSheet({ kind: "approval", approvalId: approval.id })}
            >
              {t("permission.review", "Review")}
            </button>
          ) : null}
        </div>
      </section>
    );
  }

  const decide = async (choice: ApprovalChoice) => {
    setError(null);
    try {
      await act(tauriTransport, pendingKey, "approval_decide", { approval_id: approval.id, choice, rule: null });
    } catch (e) {
      setError(
        errorCode(e) === "approval_expired"
          ? t("permission.error.expired", "This permission request expired.")
          : t("permission.error.other", "Your choice wasn't saved. Try again."),
      );
    }
  };

  const title = t("permission.title", "{{harness}} asks for permission", { harness: harnessName(t, harness) });
  return (
    <section className="icard" data-state={approval.status} aria-label={title}>
      <div className="h">
        <span>{title}</span>
      </div>
      {approval.purpose ? <p>{approval.purpose}</p> : null}
      <pre className="code">
        <code>{approval.summary}</code>
      </pre>
      {waiting ? (
        <div className="row-actions" aria-busy={deciding}>
          <button type="button" className="btn primary" disabled={deciding} onClick={() => void decide("allow_once")}>
            {t("permission.allow", "Allow")}
          </button>
          <button type="button" className="btn" disabled={deciding} onClick={() => void decide("allow_all")}>
            {t("permission.allowSession", "Allow for this session")}
          </button>
          <button type="button" className="btn destructive" disabled={deciding} onClick={() => void decide("deny")}>
            {t("permission.deny", "Deny")}
          </button>
        </div>
      ) : (
        <p className="decision">{decisionText(t, approval.status, approval.choice, true)}</p>
      )}
      {error ? (
        <p className="small" role="alert">
          {error}
        </p>
      ) : null}
      <p className="small">
        {t(
          "permission.note",
          "These are {{harness}}'s own choices. Teammates' requests follow OpenGrokBot's approval rules.",
          { harness: harnessName(t, harness) },
        )}
      </p>
    </section>
  );
}

function harnessName(t: ReturnType<typeof useTranslation>["t"], harness: HarnessKind | undefined): string {
  switch (harness) {
    case "codex":
      return t("harness.codex", "Codex");
    case "detected":
      return t("harness.agent", "The agent");
    default:
      return t("harness.claudeCode", "Claude Code");
  }
}

function decisionText(
  t: ReturnType<typeof useTranslation>["t"],
  status: ApprovalStatus,
  choice: ApprovalChoice | null,
  own: boolean,
): string {
  switch (status) {
    case "denied":
      return t("permission.decided.denied", "Denied");
    case "expired":
      return t("permission.decided.expired", "Expired without an answer");
    default:
      if (choice === "allow_all") {
        return own
          ? t("permission.decided.session", "Allowed for this session")
          : t("permission.decided.all", "Allowed all for this requester and agent");
      }
      if (choice === "keep_allowing_similar") return t("permission.decided.similar", "Allowed · similar commands keep running");
      return own ? t("permission.decided.allowed", "Allowed") : t("permission.decided.once", "Allowed once");
  }
}
