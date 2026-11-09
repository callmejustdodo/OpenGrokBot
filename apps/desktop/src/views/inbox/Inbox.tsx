// SC-010 Inbox: approvals waiting, questions sent to me (TASK-038's
// QuestionsForMe), and alerts; "Nothing needs you" when empty.
import { useTranslation } from "react-i18next";
import type { Attention } from "../../ipc/generated/Attention";
import type { MirrorState } from "../../store/reducer";
import { useAppStore } from "../../store/store";
import { uiStore } from "../../store/ui";
import { actionText, agentName, agoText, memberName, useNow } from "../approval/model";
import QuestionsForMe from "./QuestionsForMe";
import { selectInbox, type InboxApproval } from "./selectors";

export default function Inbox() {
  const { t } = useTranslation();
  const mirror = useAppStore((s) => s.mirror);
  const now = useNow();
  const title = t("inbox.title", "Inbox");

  if (!mirror) return <section className="page" aria-label={title} />;
  const groups = selectInbox(mirror);

  return (
    <section className="page" aria-label={title} data-state={groups.count === 0 ? "empty" : "default"}>
      <header className="page-head">
        <h1>{title}</h1>
        {groups.count > 0 ? (
          <span className="muted">
            {t("inbox.count", {
              count: groups.count,
              defaultValue_one: "{{count}} item",
              defaultValue_other: "{{count}} items",
            })}
          </span>
        ) : null}
      </header>

      {groups.count === 0 ? (
        <div className="empty">
          <b>{t("inbox.empty.title", "Nothing needs you")}</b>
          {t("inbox.empty.body", "Approvals, questions, and work requests collect here.")}
        </div>
      ) : null}

      {groups.approvals.length > 0 ? (
        <>
          <h2 className="grp" id="inbox-approvals">
            {t("inbox.approvals", "Approvals waiting")}
          </h2>
          <div className="list" role="list" aria-labelledby="inbox-approvals">
            {groups.approvals.map((item) => (
              <ApprovalRow key={item.approval.id} item={item} mirror={mirror} now={now} />
            ))}
          </div>
        </>
      ) : null}

      {/* TASK-038's group brings its own heading and renders nothing when empty. */}
      <QuestionsForMe />

      {groups.alerts.length > 0 ? (
        <>
          <h2 className="grp" id="inbox-alerts">
            {t("inbox.alerts", "Alerts")}
          </h2>
          <div className="list" role="list" aria-labelledby="inbox-alerts">
            {groups.alerts.map((item) => (
              <AlertRow key={item.id} item={item} mirror={mirror} now={now} />
            ))}
          </div>
        </>
      ) : null}
    </section>
  );
}

function ApprovalRow({ item, mirror, now }: { item: InboxApproval; mirror: MirrorState; now: number }) {
  const { t } = useTranslation();
  const { approval } = item;
  const agent = agentName(mirror, null, approval.agent_id) ?? t("inbox.unknownAgent", "An agent");
  // The owner's own permission prompts are answered in the conversation, not the sheet.
  const own = approval.request_id === null;
  const open = () => {
    if (own) uiStore.getState().navigate({ kind: "agent", agentId: approval.agent_id });
    else uiStore.getState().openSheet({ kind: "approval", approvalId: approval.id });
  };
  const who = own ? agent : `${memberName(mirror, approval.requester) ?? t("inbox.someone", "A teammate")} → ${agent}`;
  return (
    <div role="listitem">
      <button type="button" className="row" data-state={own ? "permission" : "approval"} onClick={open}>
        <span className="t">
          <b>{who}</b> · {own ? t("inbox.permission", "Asks for permission") : actionText(t, approval.call.tool_name)}
          <small className="mono">{approval.summary}</small>
        </span>
        <span className="when">{agoText(t, item.at, now)}</span>
      </button>
    </div>
  );
}

function AlertRow({ item, mirror, now }: { item: Attention; mirror: MirrorState; now: number }) {
  const { t } = useTranslation();
  const target = item.target;
  const open = () => {
    const ui = uiStore.getState();
    switch (target.kind) {
      case "agent":
        ui.navigate({ kind: "agent", agentId: target.agent_id });
        break;
      case "request":
        ui.openSheet({ kind: "request-status", requestId: target.request_id });
        break;
      case "schedule": {
        const schedule = mirror.schedules[target.schedule_id];
        if (schedule) ui.navigate({ kind: "agent", agentId: schedule.agent_id });
        break;
      }
      case "approval":
        ui.openSheet({ kind: "approval", approvalId: target.approval_id });
        break;
      case "question":
        break;
    }
  };
  return (
    <div role="listitem">
      <button type="button" className="row" data-state={item.kind} onClick={open}>
        <span className="t">
          <span className="tag">{alertKindText(t, item.kind)}</span> <b>{item.subject}</b>
        </span>
        <span className="when">{agoText(t, item.at, now)}</span>
      </button>
    </div>
  );
}

function alertKindText(t: ReturnType<typeof useTranslation>["t"], kind: Attention["kind"]): string {
  switch (kind) {
    case "agent_error":
      return t("inbox.alert.error", "Error");
    case "agent_stalled":
      return t("inbox.alert.stalled", "Stalled");
    case "schedule_failed":
      return t("inbox.alert.scheduleFailed", "Schedule");
    case "request_finished":
      return t("inbox.alert.requestFinished", "Request finished");
    default:
      return "";
  }
}
