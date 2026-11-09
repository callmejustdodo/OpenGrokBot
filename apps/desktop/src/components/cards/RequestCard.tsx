// SC-005 teammate work request delivered to my agent, with its status and,
// while an action waits for approval, "Review" (opens SC-011).
import { useTranslation } from "react-i18next";
import type { RequestId } from "../../ipc/generated/RequestId";
import type { RequestStatus } from "../../ipc/generated/RequestStatus";
import type { MirrorState } from "../../store/reducer";
import { useAppStore } from "../../store/store";
import { uiStore } from "../../store/ui";
import { memberName } from "../../views/approval/model";
import "./cards.css";

function pendingApprovals(state: MirrorState, requestId: RequestId) {
  return Object.values(state.approvals).filter((a) => a.request_id === requestId && a.status === "pending");
}

export default function RequestCard({ requestId }: { requestId: RequestId }) {
  const { t } = useTranslation();
  const request = useAppStore((s) => s.mirror?.requests[requestId]);
  const requester = useAppStore((s) => (s.mirror && request ? memberName(s.mirror, request.requester) : null));
  const waitingCount = useAppStore((s) => (s.mirror ? pendingApprovals(s.mirror, requestId).length : 0));
  const firstWaiting = useAppStore((s) => (s.mirror ? pendingApprovals(s.mirror, requestId)[0] : undefined));

  if (!request) return null;
  const awaiting = request.status === "awaiting_approval" && firstWaiting !== undefined;
  const title = t("requestCard.title", "Work request from {{member}}", {
    member: requester ?? t("requestCard.someone", "a teammate"),
  });

  return (
    <section className="icard" data-state={request.status} aria-label={title}>
      <div className="h">
        <span>{title}</span>
        <span className="tagpill">
          {awaiting
            ? t("requestCard.waiting", {
                count: waitingCount,
                defaultValue_one: "{{count}} approval waiting",
                defaultValue_other: "{{count}} approvals waiting",
              })
            : statusText(t, request.status)}
        </span>
      </div>
      <p>{request.prompt}</p>
      {awaiting ? (
        <div className="row-actions">
          <span className="mono muted">{firstWaiting.summary}</span>
          <span className="spacer" />
          <button
            type="button"
            className="btn primary"
            onClick={() => uiStore.getState().openSheet({ kind: "approval", approvalId: firstWaiting.id })}
          >
            {t("requestCard.review", "Review")}
          </button>
        </div>
      ) : null}
    </section>
  );
}

function statusText(t: ReturnType<typeof useTranslation>["t"], status: RequestStatus): string {
  switch (status) {
    case "received":
      return t("requestCard.status.received", "Received");
    case "awaiting_approval":
      return t("requestCard.status.awaitingApproval", "Waiting for approval");
    case "queued":
      return t("requestCard.status.queued", "Runs after this turn");
    case "running":
      return t("requestCard.status.running", "Running");
    case "completed":
      return t("requestCard.status.completed", "Completed");
    case "denied":
      return t("requestCard.status.denied", "Denied");
    case "failed":
      return t("requestCard.status.failed", "Failed");
    case "canceled":
      return t("requestCard.status.canceled", "Canceled");
    case "expired":
      return t("requestCard.status.expired", "Expired");
  }
}
