// SC-008 sharing section of the information panel: share switch (asks before
// turning off), visibility, work requests, and a preview of what teammates see.
import * as Switch from "@radix-ui/react-switch";
import * as ToggleGroup from "@radix-ui/react-toggle-group";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { AgentId } from "../../ipc/generated/AgentId";
import type { ShareUpdate } from "../../ipc/generated/ShareUpdate";
import type { Visibility } from "../../ipc/generated/Visibility";
import { tauriTransport } from "../../ipc/transport";
import Alert, { AlertCancel } from "../../ui/Alert";
import { useAppStore } from "../../store/store";
import { act } from "../../store/sync";
import { uiStore } from "../../store/ui";

export default function SharingSection({ agentId }: { agentId: AgentId }) {
  const { t } = useTranslation();
  const share = useAppStore((s) => s.mirror?.shares[agentId] ?? null);
  const agentName = useAppStore((s) => s.mirror?.agents[agentId]?.name ?? "");
  const key = `share:${agentId}`;
  const pending = useAppStore((s) => key in s.pending);
  const [confirming, setConfirming] = useState(false);
  const [failed, setFailed] = useState<ShareUpdate | null>(null);

  // The default is private (D-026).
  const shared = share?.shared ?? false;
  const visibility: Visibility = share?.visibility ?? "answer_excerpts";
  const acceptsRequests = share?.accepts_requests ?? false;

  const update = (change: Partial<Omit<ShareUpdate, "agent_id">>) => {
    const params: ShareUpdate = { agent_id: agentId, shared: null, visibility: null, accepts_requests: null, ...change };
    setFailed(null);
    act(tauriTransport, key, "share_update", params).catch(() => setFailed(params));
  };

  return (
    <section className="p-card" aria-label={t("sharing.title", "Sharing")} data-state={shared ? "shared" : "private"}>
      <span className="pr">
        <b>{t("sharing.title", "Sharing")}</b>
      </span>
      <div className="form-row">
        <span id={`share-${agentId}`}>
          {t("sharing.shareWithTeam", "Share with team")}
          <small>{t("sharing.offByDefault", "Off by default for new agents")}</small>
        </span>
        <Switch.Root
          className="switch"
          aria-labelledby={`share-${agentId}`}
          checked={shared}
          disabled={pending}
          onCheckedChange={(on) => (on ? update({ shared: true }) : setConfirming(true))}
        >
          <Switch.Thumb />
        </Switch.Root>
      </div>
      {shared ? (
        <>
          <div className="form-row column">
            <span id={`visibility-${agentId}`}>
              {t("sharing.visibility", "Visibility")}
              <small>{t("sharing.visibilityNote", "Same for every teammate")}</small>
            </span>
            <ToggleGroup.Root
              type="single"
              className="seg"
              aria-labelledby={`visibility-${agentId}`}
              value={visibility}
              disabled={pending}
              onValueChange={(value) => {
                if (value && value !== visibility) update({ visibility: value as Visibility });
              }}
            >
              <ToggleGroup.Item value="answer">{t("sharing.answerOnly", "Answer only")}</ToggleGroup.Item>
              <ToggleGroup.Item value="answer_excerpts">
                {t("sharing.answerExcerpts", "Answer and excerpts")}
              </ToggleGroup.Item>
            </ToggleGroup.Root>
          </div>
          <div className="form-row">
            <span id={`requests-${agentId}`}>
              {t("sharing.acceptRequests", "Accept work requests")}
              <small>{t("sharing.acceptRequestsNote", "Actions follow your rules")}</small>
            </span>
            <Switch.Root
              className="switch"
              aria-labelledby={`requests-${agentId}`}
              checked={acceptsRequests}
              disabled={pending}
              onCheckedChange={(on) => update({ accepts_requests: on })}
            >
              <Switch.Thumb />
            </Switch.Root>
          </div>
          <button
            type="button"
            className="btn"
            onClick={() => uiStore.getState().openSheet({ kind: "share-preview", agentId })}
          >
            {t("sharing.preview", "Preview what teammates see")}
          </button>
        </>
      ) : (
        <small>{t("sharing.privateNote", "Hidden from teammates' lists and routing.")}</small>
      )}
      {failed ? (
        <div className="banner warn" role="alert">
          <div className="bt">
            <b>{t("sharing.failed", "Couldn't change sharing")}</b>
          </div>
          <button type="button" className="btn" onClick={() => update(failed)}>
            {t("sharing.tryAgain", "Try again")}
          </button>
        </div>
      ) : null}
      <Alert
        open={confirming}
        onOpenChange={setConfirming}
        title={t("sharing.unshareTitle", "Stop sharing {{agent}}?", { agent: agentName })}
        description={t(
          "sharing.unshareBody",
          "New questions and requests stop now. Pending requests are canceled. A running task continues unless you stop it.",
        )}
      >
        <button
          type="button"
          className="btn destructive"
          onClick={() => {
            setConfirming(false);
            update({ shared: false });
          }}
        >
          {t("sharing.stopSharing", "Stop sharing")}
        </button>
        <AlertCancel asChild>
          <button type="button" className="btn">
            {t("sharing.cancel", "Cancel")}
          </button>
        </AlertCancel>
      </Alert>
    </section>
  );
}
