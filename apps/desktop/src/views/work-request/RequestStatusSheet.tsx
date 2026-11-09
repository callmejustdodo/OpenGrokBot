// SC-016 request status: a timeline from the request's status, the result or
// error, and "Cancel request" until it starts. The contract keeps only
// `updated_at`, so only the current step shows a time.
import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { RequestId } from "../../ipc/generated/RequestId";
import type { RequestStatus } from "../../ipc/generated/RequestStatus";
import { tauriTransport } from "../../ipc/transport";
import { useAppStore } from "../../store/store";
import { act } from "../../store/sync";
import { uiStore } from "../../store/ui";
import Sheet from "../../ui/Sheet";
import { agentName, errorCode, memberName } from "../approval/model";
import "./work-request.css";

const FLOW: readonly RequestStatus[] = ["received", "awaiting_approval", "queued", "running", "completed"];
const CANCELABLE: readonly RequestStatus[] = ["received", "awaiting_approval", "queued"];
const STARTED: readonly RequestStatus[] = ["running", "completed", "failed"];

/** The steps shown for a status; an ending other than completed replaces the rest of the flow. */
function stepsFor(status: RequestStatus): RequestStatus[] {
  switch (status) {
    case "denied":
    case "expired":
      return ["received", "awaiting_approval", status];
    case "canceled":
    case "failed":
      return ["received", status];
    default:
      return [...FLOW];
  }
}

export default function RequestStatusSheet({ requestId }: { requestId: RequestId }) {
  const { t, i18n } = useTranslation();
  const request = useAppStore((s) => s.mirror?.requests[requestId]);
  const owner = useAppStore((s) => (s.mirror && request ? memberName(s.mirror, request.owner) : null));
  const agent = useAppStore((s) => (s.mirror && request ? agentName(s.mirror, request.owner, request.agent_id) : null));
  const pendingKey = `request_cancel:${requestId}`;
  const canceling = useAppStore((s) => pendingKey in s.pending);
  const [error, setError] = useState<string | null>(null);

  const close = () => uiStore.getState().closeSheet();
  const onOpenChange = (open: boolean) => {
    if (!open) close();
  };
  const title = t("requestStatus.title", "Request status");
  const closeButton = (
    <button type="button" className="btn primary" onClick={close}>
      {t("common.close", "Close")}
    </button>
  );

  if (!request) {
    return (
      <Sheet open onOpenChange={onOpenChange} title={title} footer={closeButton}>
        <p>{t("requestStatus.missing", "This request is no longer available.")}</p>
      </Sheet>
    );
  }

  const steps = stepsFor(request.status);
  const current = steps.indexOf(request.status);
  const ended = !FLOW.slice(0, 4).includes(request.status);
  const cancelable = CANCELABLE.includes(request.status);
  const time = new Intl.DateTimeFormat(i18n.language, { hour: "numeric", minute: "2-digit" }).format(
    new Date(request.updated_at),
  );
  const firstLine = request.prompt.split("\n")[0];

  const cancel = async () => {
    setError(null);
    try {
      await act(tauriTransport, pendingKey, "request_cancel", { request_id: request.id });
    } catch (e) {
      setError(
        errorCode(e) === "conflict"
          ? t("requestStatus.error.started", "The request already started, so it can't be canceled.")
          : t("requestStatus.error.other", "The request wasn't canceled. Try again."),
      );
    }
  };

  return (
    <Sheet
      open
      onOpenChange={onOpenChange}
      title={title}
      lead={t("requestStatus.lead", "{{owner}}'s {{agent}} agent · {{prompt}}", {
        owner: owner ?? t("requestStatus.someone", "A teammate"),
        agent: agent ?? t("requestStatus.unknownAgent", "unknown"),
        prompt: firstLine,
      })}
      footer={
        <>
          {STARTED.includes(request.status) ? (
            <span className="muted small spacer">
              {t("requestStatus.cantCancel", "Started requests can't be canceled")}
            </span>
          ) : null}
          <button type="button" className="btn" disabled={!cancelable || canceling} onClick={() => void cancel()}>
            {t("requestStatus.cancel", "Cancel request")}
          </button>
          {closeButton}
        </>
      }
    >
      <ol className="timeline" data-state={request.status} aria-label={t("requestStatus.timeline", "Progress")}>
        {steps.map((step, index) => {
          const state = index < current || (index === current && ended) ? "done" : index === current ? "now" : "todo";
          return (
            <li key={step} className={state} data-step={step} aria-current={index === current ? "step" : undefined}>
              <span className="mark" aria-hidden="true" />
              <span>{stepText(t, step)}</span>
              {index === current ? <span className="muted small">{time}</span> : null}
            </li>
          );
        })}
      </ol>

      {request.result ? (
        <div className="field">
          <span>{t("requestStatus.result", "Result")}</span>
          <p>{request.result}</p>
        </div>
      ) : null}

      {request.error ? (
        <div className="banner warn" role="status">
          <div className="bt">{failureText(t, request.error.code)}</div>
        </div>
      ) : null}

      {error ? (
        <div className="banner warn" role="alert">
          <div className="bt">{error}</div>
        </div>
      ) : null}
    </Sheet>
  );
}

type T = ReturnType<typeof useTranslation>["t"];

function stepText(t: T, step: RequestStatus): string {
  switch (step) {
    case "received":
      return t("requestStatus.step.received", "Received");
    case "awaiting_approval":
      return t("requestStatus.step.awaitingApproval", "Waiting for approval");
    case "queued":
      return t("requestStatus.step.queued", "Queued · waiting for the current turn");
    case "running":
      return t("requestStatus.step.running", "Running");
    case "completed":
      return t("requestStatus.step.completed", "Completed");
    case "denied":
      return t("requestStatus.step.denied", "Denied");
    case "failed":
      return t("requestStatus.step.failed", "Failed");
    case "canceled":
      return t("requestStatus.step.canceled", "Canceled");
    case "expired":
      return t("requestStatus.step.expired", "Expired");
  }
}

function failureText(t: T, code: string): string {
  switch (code) {
    case "denied":
      return t("requestStatus.failure.denied", "The owner denied an action this request needed.");
    case "approval_expired":
      return t("requestStatus.failure.expired", "An approval got no answer for 24 hours, so the request expired.");
    case "requests_not_accepted":
      return t("requestStatus.failure.notAccepted", "This agent stopped accepting work requests.");
    case "agent_not_shared":
      return t("requestStatus.failure.notShared", "This agent is no longer shared with you.");
    case "peer_offline":
      return t("requestStatus.failure.offline", "The owner's Mac is offline.");
    case "harness_unavailable":
      return t("requestStatus.failure.harness", "The agent couldn't start.");
    default:
      return t("requestStatus.failure.other", "Something went wrong while running the request.");
  }
}
