// SC-018 "notifications": whether needs-attention events alert, and pausing
// them for a while (`muted_until`).
import * as Switch from "@radix-ui/react-switch";
import { useTranslation } from "react-i18next";
import { isMuted } from "../../notifications/decide";
import { useNow } from "./useNow";
import { useSettingsUpdate } from "./useSettingsUpdate";

const HOUR_MS = 60 * 60 * 1000;

/** 9 AM tomorrow, local time. */
function tomorrowMorning(now: number): Date {
  const date = new Date(now);
  date.setDate(date.getDate() + 1);
  date.setHours(9, 0, 0, 0);
  return date;
}

export default function NotificationsTab() {
  const { t, i18n } = useTranslation();
  const { settings, disabled, failed, update } = useSettingsUpdate();
  const now = useNow();
  const { enabled, muted_until } = settings.notifications;
  const paused = enabled && isMuted(settings.notifications, now);

  const setNotifications = (next: Partial<typeof settings.notifications>) =>
    void update({ ...settings, notifications: { ...settings.notifications, ...next } });
  const pauseUntil = (until: Date) => setNotifications({ muted_until: until.toISOString() });

  return (
    <>
      <div className="form-row">
        <label htmlFor="settings-notify">{t("settings.notifications.enabled", "Notify me when I'm needed")}</label>
        <Switch.Root
          id="settings-notify"
          className="switch"
          checked={enabled}
          disabled={disabled}
          onCheckedChange={(checked) => setNotifications({ enabled: checked })}
        >
          <Switch.Thumb className="track" />
        </Switch.Root>
      </div>
      <p className="muted small">
        {t("settings.notifications.which", "Only when you're needed: approvals, errors, stalls, failed schedules.")}
      </p>
      {enabled ? (
        <div className="form-row" data-state={paused ? "paused" : "active"}>
          {paused && muted_until ? (
            <>
              <span>
                {t("settings.notifications.pausedUntil", "Paused until {{time}}", {
                  time: new Date(muted_until).toLocaleString(i18n.language, {
                    month: "short",
                    day: "numeric",
                    hour: "numeric",
                    minute: "2-digit",
                  }),
                })}
              </span>
              <button
                type="button"
                className="btn"
                disabled={disabled}
                onClick={() => setNotifications({ muted_until: null })}
              >
                {t("settings.notifications.resume", "Resume")}
              </button>
            </>
          ) : (
            <>
              <span>{t("settings.notifications.pause", "Pause notifications")}</span>
              <span className="row-actions">
                <button
                  type="button"
                  className="btn"
                  disabled={disabled}
                  onClick={() => pauseUntil(new Date(Date.now() + HOUR_MS))}
                >
                  {t("settings.notifications.pauseHour", "For 1 hour")}
                </button>
                <button
                  type="button"
                  className="btn"
                  disabled={disabled}
                  onClick={() => pauseUntil(tomorrowMorning(Date.now()))}
                >
                  {t("settings.notifications.pauseTomorrow", "Until tomorrow")}
                </button>
              </span>
            </>
          )}
        </div>
      ) : null}
      {failed ? (
        <p className="err" role="alert">
          {t("settings.saveFailed", "Couldn't save the change. Try again.")}
        </p>
      ) : null}
    </>
  );
}
