// The menu bar panel (SC-002): a one-line summary, what needs me, my agents
// with their state, and ways into the app. States: default (something needs
// me), all-clear, and offline (formationd unreachable, this Mac off the
// tailnet, or every teammate offline; local agents are still listed). Rows show how long ago each item
// came in, and the window tells the tray icon whether to show its dot.
import { invoke } from "@tauri-apps/api/core";
import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { showMainWindow } from "./ipc/client";
import type { AttentionTarget } from "./ipc/generated/AttentionTarget";
import {
  agentStateLabel,
  attentionKindLabel,
  offlineReason,
  selectNeedsYouItems,
  type NeedsYouItem,
  type PanelState,
} from "./notifications/attention";
import { shortAgo, useMinuteClock } from "./notifications/ago";
import { isMuted } from "./notifications/decide";
import { setTrayAttention } from "./notifications/tray";
import { useAppStore } from "./store/store";
import { useSettingsUpdate } from "./views/settings/useSettingsUpdate";
import "./menubar-panel.css";

/** How long "Pause alerts" mutes notifications. */
const PAUSE_MS = 60 * 60 * 1000;
const NO_ITEMS: NeedsYouItem[] = [];

function focusItem(target: AttentionTarget): Promise<void> {
  return invoke("focus_item", { target });
}

export default function MenubarPanel() {
  const { t } = useTranslation();
  const mirror = useAppStore((s) => s.mirror);
  const connected = useAppStore((s) => s.connected);
  const { settings, pending: pausing, failed, update } = useSettingsUpdate();

  const needs = mirror ? selectNeedsYouItems(mirror) : NO_ITEMS;
  // Connected detected agents are not listed here, as in the approved mockup.
  const agents = Object.values(mirror?.agents ?? {}).filter((a) => a.detected === null);
  const offline = offlineReason(connected, mirror);
  const state: PanelState = offline ? "offline" : needs.length > 0 ? "default" : "all-clear";
  const now = useMinuteClock();
  const attention = needs.length > 0;
  useEffect(() => {
    void setTrayAttention(attention);
  }, [attention]);
  const paused = settings.notifications.enabled && isMuted(settings.notifications, now);

  let summary: string;
  if (offline === "formationd") {
    summary = t("menubar.offline.formationd", "Can't reach formationd · retrying. Agents keep running.");
  } else if (offline === "tailnet") {
    summary = t("menubar.offline.tailnet", "Not on the tailnet · local agents keep running");
  } else if (offline === "team") {
    summary = t("menubar.offline.team", "Can't reach the team · local agents keep running");
  } else if (needs.length > 0) {
    summary = t("menubar.summary", "{{agents}} agents · {{needs}} need you", {
      agents: agents.length,
      needs: needs.length,
    });
  } else {
    summary = t("menubar.allClear", "Everything is running");
  }

  const togglePause = () => {
    const muted_until = paused ? null : new Date(Date.now() + PAUSE_MS).toISOString();
    void update({ ...settings, notifications: { ...settings.notifications, muted_until } });
  };

  return (
    <main
      className="mb-panel"
      aria-label={t("menubar.label", "OpenGrokBot menu bar panel")}
      data-state={state}
      data-offline={offline ?? undefined}
    >
      <div className="head">
        <b>OpenGrokBot</b>
        <p className="muted small">{summary}</p>
      </div>

      {needs.length > 0 ? (
        <section className="sec" aria-labelledby="mb-needs">
          <h2 id="mb-needs">{t("menubar.needsYou", "Needs you")}</h2>
          <ul className="list">
            {needs.map((item) => (
              <li key={item.key} data-kind={item.kind}>
                <button type="button" className="mb-row" onClick={() => void focusItem(item.target)}>
                  <span>{item.subject}</span>
                  <span className="r">{attentionKindLabel(t, item.kind)}</span>
                  <time className="ago" dateTime={item.at}>
                    {shortAgo(t, item.at, now)}
                  </time>
                </button>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      <section className="sec" aria-labelledby="mb-agents">
        <h2 id="mb-agents">{t("menubar.myAgents", "My agents")}</h2>
        <ul className="list">
          {agents.map((a) => (
            <li key={a.id} data-state={a.state}>
              <button
                type="button"
                className="mb-row"
                onClick={() => void focusItem({ kind: "agent", agent_id: a.id })}
              >
                <span className="name">{a.name}</span>
                <span className="r">{agentStateLabel(t, a.state)}</span>
              </button>
            </li>
          ))}
        </ul>
      </section>

      <div className="sec actions">
        <button type="button" className="btn primary" onClick={() => void showMainWindow()}>
          {t("menubar.open", "Open OpenGrokBot")}
        </button>
        {mirror && connected ? (
          <button
            type="button"
            className="btn"
            disabled={pausing}
            aria-busy={pausing}
            onClick={togglePause}
          >
            {paused ? t("menubar.resumeAlerts", "Resume alerts") : t("menubar.pauseAlerts", "Pause alerts 1 hr")}
          </button>
        ) : null}
        <button type="button" className="btn" onClick={() => void invoke("show_settings_window")}>
          {t("menubar.settings", "Settings…")}
        </button>
        <button type="button" className="btn" onClick={() => void invoke("quit_app")}>
          {t("menubar.quit", "Quit OpenGrokBot")}
        </button>
        {failed ? (
          <p className="err" role="alert">
            {t("menubar.pauseFailed", "Couldn't change alerts. Try again.")}
          </p>
        ) : null}
      </div>
    </main>
  );
}
