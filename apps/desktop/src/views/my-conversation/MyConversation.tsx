import "./my-conversation.css";
// SC-005 conversation with my agent. States: working, idle, queued, permission,
// approval (cards owned by TASK-026), stalled, error, stopped, detected
// (read-only, banner owned by TASK-033), stop-confirm.
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import AgentAvatar from "../../components/sidebar/AgentAvatar";
import { stateLabel, stepLine } from "../../components/sidebar/labels";
import type { AgentId } from "../../ipc/generated/AgentId";
import type { ConversationItem } from "../../ipc/generated/ConversationItem";
import type { StopMode } from "../../ipc/generated/StopMode";
import Alert, { AlertAction, AlertCancel } from "../../ui/Alert";
import { useAppStore } from "../../store/store";
import { uiStore, useUi } from "../../store/ui";
import DetectedBanner from "../connect/DetectedBanner";
import { errorText, pendingKey, restart, send, start, stop } from "./actions";
import Messages from "./Messages";
import QueuedChip from "./QueuedChip";

type StepItem = Extract<ConversationItem, { kind: "step" }>;

function lastStep(items: readonly ConversationItem[]): StepItem | undefined {
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i];
    if (item.kind === "step") return item;
  }
  return undefined;
}

function minutesSince(at: string | null): number {
  if (!at) return 0;
  return Math.max(1, Math.round((Date.now() - Date.parse(at)) / 60000));
}

export default function MyConversation({ agentId }: { agentId: AgentId }) {
  const { t } = useTranslation();
  const agent = useAppStore((s) => s.mirror?.agents[agentId]);
  const conversation = useAppStore((s) => s.mirror?.conversations[agentId]);
  const status = useAppStore((s) => s.mirror?.agentStatus[agentId]);
  // Only when the `agent.state` event gave neither a reason nor an exit code.
  const errorItem = useAppStore((s) =>
    Object.values(s.mirror?.inbox ?? {}).find(
      (i) => i.kind === "agent_error" && i.target.kind === "agent" && i.target.agent_id === agentId,
    ),
  );
  const sending = useAppStore((s) => pendingKey.send(agentId) in s.pending);
  const stopping = useAppStore((s) => pendingKey.stop(agentId) in s.pending);
  const starting = useAppStore((s) => pendingKey.start(agentId) in s.pending);
  const panelOpen = useUi((s) => s.panelOpen);
  const [text, setText] = useState("");
  const [confirmStop, setConfirmStop] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const state = agent?.state;
  const detected = agent ? agent.detected !== null : false;
  const working = state === "working" || state === "waiting_for_approval";
  const notRunning = state === "stopped" || state === "error";

  // ⌘. asks before stopping a working agent.
  useEffect(() => {
    if (!working || detected) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey && e.key === ".") {
        e.preventDefault();
        setConfirmStop(true);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [working, detected]);

  if (!agent) return <section className="conversation" aria-label={t("conversation.label", "Conversation")} />;

  const items = conversation?.items ?? [];
  const queue = (conversation?.queue ?? []).filter((q) => q.source.kind === "owner");
  const step = lastStep(items);

  const run = (call: () => Promise<unknown>, after?: () => void) => {
    setError(null);
    call().then(
      () => after?.(),
      (e: unknown) => setError(errorText(t, e)),
    );
  };
  const submit = () => {
    const message = text.trim();
    if (!message || sending) return;
    run(() => send(agentId, message), () => setText(""));
  };
  const stopWith = (mode: StopMode) => run(() => stop(agentId, mode));

  return (
    <section className="conversation" aria-label={t("conversation.labelWith", "Conversation with {{name}}", { name: agent.name })} data-state={state}>
      <div className="c-head" data-tauri-drag-region>
        <span className="pill">
          <AgentAvatar avatar={agent.avatar} size={28} working={agent.state === "working"} />
          <span className="nm" role="heading" aria-level={1}>
            {agent.name}
          </span>
          <span className="st status" data-state={state}>
            {stateLabel(t, agent.state)}
          </span>
        </span>
        <div className="right">
          <button
            type="button"
            className="ticon"
            aria-label={t("conversation.panel", "Information panel")}
            aria-pressed={panelOpen}
            onClick={() => uiStore.getState().setPanelOpen(!panelOpen)}
          />
        </div>
      </div>

      {detected ? <DetectedBanner agentId={agentId} /> : null}
      {state === "stalled" ? (
        <div className="banner stall" role="status">
          <div className="bt">
            <b>
              {t("conversation.stalled.title", {
                count: minutesSince(agent.last_activity_at),
                defaultValue_one: "No activity for {{count}} minute",
                defaultValue_other: "No activity for {{count}} minutes",
              })}
            </b>
            {step ? (
              <span>{t("conversation.stalled.lastStep", "Last step: {{step}}", { step: stepLine(t, step.step) })}</span>
            ) : null}
          </div>
          <button type="button" className="btn primary" disabled={starting || stopping} onClick={() => run(() => restart(agentId, true))}>
            {t("conversation.restart", "Restart")}
          </button>
        </div>
      ) : null}
      {state === "error" ? (
        <div className="banner err" role="alert">
          <div className="bt">
            <b>{status?.reason ?? t("conversation.error.title", "The process exited unexpectedly")}</b>
            {status && status.exit_code !== null ? (
              <span className="mono">
                {t("conversation.error.exitCode", "Exited with code {{code}}", { code: status.exit_code })}
              </span>
            ) : !status && errorItem ? (
              <span className="mono">{errorItem.subject}</span>
            ) : null}
          </div>
          <button type="button" className="btn primary" disabled={starting} onClick={() => run(() => restart(agentId, false))}>
            {t("conversation.restart", "Restart")}
          </button>
        </div>
      ) : null}

      <div className="msgs" role="log" aria-label={t("conversation.messages", "Messages")}>
        <Messages agentId={agentId} items={items} />
        {working ? (
          <div className="working" role="status">
            <span className="dots" aria-hidden="true">
              <i />
              <i />
              <i />
            </span>
            {state === "waiting_for_approval"
              ? t("conversation.waitingApproval", "Waiting for approval")
              : step && step.step.status === "running"
                ? t("conversation.workingOn", "Working · {{step}}", { step: stepLine(t, step.step) })
                : t("conversation.working", "Working")}
          </div>
        ) : null}
      </div>

      {queue.map((item) => (
        <QueuedChip key={item.id} item={item} turnRunning={working} />
      ))}

      {error ? (
        <p className="err" role="alert">
          {error}
        </p>
      ) : null}

      {notRunning && !detected ? (
        <div className="stopped-bar" data-state={state}>
          <span>{t("conversation.notRunning", "This agent isn't running")}</span>
          <button type="button" className="btn primary" disabled={starting} onClick={() => run(() => start(agentId))}>
            {t("conversation.start", "Start")}
          </button>
        </div>
      ) : (
        <div className={detected ? "composer off" : "composer"}>
          <textarea
            aria-label={t("conversation.message", "Message")}
            placeholder={
              detected
                ? t("conversation.detectedPlaceholder", "OpenGrokBot can't send messages to this session")
                : t("conversation.placeholder", "Message {{name}}", { name: agent.name })
            }
            disabled={detected}
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && e.metaKey) {
                e.preventDefault();
                submit();
              }
            }}
          />
          {working && !detected ? (
            <button
              type="button"
              className="cbtn stop"
              aria-label={t("conversation.stop", "Stop ⌘.")}
              title={t("conversation.stop", "Stop ⌘.")}
              disabled={stopping}
              onClick={() => setConfirmStop(true)}
            />
          ) : null}
          <button
            type="button"
            className="cbtn go"
            aria-label={t("conversation.send", "Send ⌘↩")}
            title={t("conversation.send", "Send ⌘↩")}
            disabled={detected || sending || !text.trim()}
            onClick={submit}
          />
        </div>
      )}
      <div className="c-note">{t("conversation.private", "Teammates never see this conversation")}</div>

      <Alert
        open={confirmStop}
        onOpenChange={setConfirmStop}
        title={t("conversation.stopConfirm.title", "{{name}} is in the middle of a task", { name: agent.name })}
        description={t("conversation.stopConfirm.body", "Stopping now may leave the current change half done.")}
      >
        <AlertAction asChild>
          <button type="button" className="btn primary" onClick={() => stopWith("after_turn")}>
            {t("conversation.stopConfirm.afterTurn", "Stop after this turn")}
          </button>
        </AlertAction>
        <AlertAction asChild>
          <button type="button" className="btn destructive" onClick={() => stopWith("now")}>
            {t("conversation.stopConfirm.now", "Stop now")}
          </button>
        </AlertAction>
        <AlertCancel asChild>
          <button type="button" className="btn">
            {t("conversation.stopConfirm.cancel", "Cancel")}
          </button>
        </AlertCancel>
      </Alert>
    </section>
  );
}
