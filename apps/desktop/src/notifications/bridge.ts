// Turns `notify` events into macOS notifications (SC-019). Started by the menu
// bar panel's window only: that window is created at launch, is never closed,
// and holds the mirror and the language, so each item alerts exactly once.
// `src-tauri` shows the notification and handles its click.
import { invoke } from "@tauri-apps/api/core";
import i18n from "../i18n";
import type { AgentId } from "../ipc/generated/AgentId";
import { appStore, type AppStore } from "../store/store";
import type { Transport, Unsubscribe } from "../store/transport";
import { noticeFor, type Notice } from "./decide";

function showNotice(notice: Notice): Promise<void> {
  return invoke("show_notification", { ...notice });
}

export function startNotificationBridge(
  transport: Transport,
  store: AppStore = appStore,
  show: (notice: Notice) => Promise<void> = showNotice,
  now: () => number = Date.now,
): Unsubscribe {
  const reasons = new Map<AgentId, string>();
  return transport.onEvent(({ event }) => {
    if (event.type === "agent.state") {
      if (event.payload.reason) reasons.set(event.payload.agent_id, event.payload.reason);
      else reasons.delete(event.payload.agent_id);
      return;
    }
    if (event.type !== "notify") return;
    const notice = noticeFor(event.payload, store.getState().mirror, reasons, now(), i18n.getFixedT(i18n.language));
    if (notice) show(notice).catch(() => {});
  });
}
