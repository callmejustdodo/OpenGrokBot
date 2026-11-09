// Whether a `notify` event shows a macOS notification (SC-019), and its words.
// Pure, so the rule is tested without Tauri.
import type { TFunction } from "i18next";
import type { AgentId } from "../ipc/generated/AgentId";
import type { AttentionTarget } from "../ipc/generated/AttentionTarget";
import type { NotificationSettings } from "../ipc/generated/NotificationSettings";
import type { NotifyEvent } from "../ipc/generated/NotifyEvent";
import type { MirrorState } from "../store/reducer";
import { attentionKindLabel } from "./attention";

export type Notice = { title: string; body: string; target: AttentionTarget };

/** Notifications are off, or paused until a later time. */
export function isMuted(settings: NotificationSettings, now: number): boolean {
  if (!settings.enabled) return true;
  return settings.muted_until !== null && Date.parse(settings.muted_until) > now;
}

/** The facts after the name in a subject such as "Planning · exited with code 1". */
function subjectDetail(subject: string): string {
  const at = subject.indexOf(" · ");
  return at === -1 ? subject : subject.slice(at + 3);
}

/**
 * The notification for `event`, or `null` when it must not alert: formationd did
 * not ask for one, the item is already resolved, or notifications are off or
 * muted. `reasons` holds the last `agent.state` reason per agent. Before the
 * first snapshot the settings are unknown, so formationd's `alert` decides.
 */
export function noticeFor(
  event: NotifyEvent,
  mirror: MirrorState | null,
  reasons: ReadonlyMap<AgentId, string>,
  now: number,
  t: TFunction,
): Notice | null {
  const { item } = event;
  if (!event.alert || item.resolved_at !== null) return null;
  if (mirror && isMuted(mirror.settings.notifications, now)) return null;
  const target = item.target;

  if (item.kind === "approval" && target.kind === "approval") {
    const approval = mirror?.approvals[target.approval_id];
    const title = t("notify.approval.title", "Approval needed");
    if (!approval) return { title, body: item.subject, target };
    const requester =
      mirror?.team?.members.find((m) => m.id === approval.requester)?.display_name ??
      mirror?.agents[approval.agent_id]?.name ??
      approval.agent_id;
    return {
      title,
      body: t("notify.approval.body", "{{requester}} wants to run {{command}}", {
        requester,
        command: approval.summary,
      }),
      target,
    };
  }

  if (item.kind === "agent_error" && target.kind === "agent") {
    const agent = mirror?.agents[target.agent_id]?.name ?? item.subject.split(" · ")[0];
    const reason = reasons.get(target.agent_id) ?? subjectDetail(item.subject);
    return {
      title: t("notify.error.title", "Agent stopped"),
      body: t("notify.error.body", "{{agent}} stopped: {{reason}}", { agent, reason }),
      target,
    };
  }

  return { title: attentionKindLabel(t, item.kind), body: item.subject, target };
}
