// SC-008 preview: what teammates would see of my agent, from `share_preview`.
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import "./sharing.css";
import type { AgentId } from "../../ipc/generated/AgentId";
import type { SharePreviewResult } from "../../ipc/generated/SharePreviewResult";
import { tauriTransport } from "../../ipc/transport";
import Sheet from "../../ui/Sheet";
import { useAppStore } from "../../store/store";
import { act } from "../../store/sync";
import { uiStore } from "../../store/ui";
import { formatTime, harnessLabel, stateLabel } from "../ask-conversation/format";

type Load = { kind: "loading" } | { kind: "loaded"; preview: SharePreviewResult } | { kind: "failed" };

export default function SharePreviewSheet({ agentId }: { agentId: AgentId }) {
  const { t, i18n } = useTranslation();
  const ownerName = useAppStore((s) => {
    const team = s.mirror?.team;
    return team?.members.find((m) => m.id === team.me)?.display_name ?? "";
  });
  const [load, setLoad] = useState<Load>({ kind: "loading" });

  const request = useCallback(() => {
    act(tauriTransport, `share:preview:${agentId}`, "share_preview", { agent_id: agentId }).then(
      (preview) => setLoad({ kind: "loaded", preview }),
      () => setLoad({ kind: "failed" }),
    );
  }, [agentId]);

  useEffect(request, [request]);

  const retry = () => {
    setLoad({ kind: "loading" });
    request();
  };

  const close = () => uiStore.getState().closeSheet();

  let body;
  if (load.kind === "loading") {
    body = (
      <p className="muted" role="status">
        {t("sharing.previewLoading", "Loading the preview…")}
      </p>
    );
  } else if (load.kind === "failed") {
    body = (
      <div className="banner warn" role="alert">
        <div className="bt">
          <b>{t("sharing.previewFailed", "Couldn't load the preview")}</b>
        </div>
        <button type="button" className="btn" onClick={retry}>
          {t("sharing.tryAgain", "Try again")}
        </button>
      </div>
    );
  } else {
    const { view, visibility } = load.preview;
    body = (
      <>
        <div className="p-card" data-state={view.state}>
          <span className="pr">
            <b>{view.name}</b>
            <span className="tag">{harnessLabel(t, view.harness)}</span>
          </span>
          <dl className="kv">
            <dt>{t("sharing.previewOwner", "Owner")}</dt>
            <dd>{ownerName}</dd>
            <dt>{t("sharing.previewRole", "Role")}</dt>
            <dd>{view.role}</dd>
            <dt>{t("sharing.previewState", "State")}</dt>
            <dd>{stateLabel(t, view.state)}</dd>
            <dt>{t("sharing.previewLastActivity", "Last activity")}</dt>
            <dd>{view.last_activity_at ? formatTime(view.last_activity_at, i18n.language) : "—"}</dd>
            <dt>{t("sharing.previewRequests", "Work requests")}</dt>
            <dd>
              {view.accepts_requests
                ? t("sharing.previewRequestsOn", "Accepted")
                : t("sharing.previewRequestsOff", "Not accepted")}
            </dd>
          </dl>
        </div>
        <div className="p-card" data-visibility={visibility}>
          <span className="pr">
            <b>{t("sharing.previewAnswers", "What answers include")}</b>
          </span>
          <small>
            {visibility === "answer_excerpts"
              ? t("sharing.previewExcerpts", "The answer, the agent and an as-of time, and the conversation passages it used.")
              : t("sharing.previewAnswerOnly", "The answer, the agent and an as-of time. No conversation passages.")}
          </small>
        </div>
        <p className="muted small">
          {t(
            "sharing.previewMasking",
            "Anything that looks like a password or key is masked. Masking is a backup, not a guarantee.",
          )}
        </p>
      </>
    );
  }

  return (
    <Sheet
      open
      onOpenChange={(open) => !open && close()}
      title={t("sharing.previewTitle", "What teammates see")}
      footer={
        <button type="button" className="btn primary" onClick={close}>
          {t("sharing.close", "Close")}
        </button>
      }
    >
      {body}
    </Sheet>
  );
}
