// Compact "3m"-style ages for the menu bar panel's rows (SC-002), and a clock
// that ticks each minute so they stay current while the panel is open.
import type { TFunction } from "i18next";
import { useEffect, useState } from "react";

const MINUTE = 60_000;

/** How long ago `at` was, in the panel's short form: "now", "3m", "2h", "4d". */
export function shortAgo(t: TFunction, at: string, now: number): string {
  const minutes = Math.max(0, Math.floor((now - Date.parse(at)) / MINUTE));
  if (minutes < 1) return t("menubar.ago.now", "now");
  if (minutes < 60) return t("menubar.ago.minutes", "{{count}}m", { count: minutes });
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return t("menubar.ago.hours", "{{count}}h", { count: hours });
  return t("menubar.ago.days", "{{count}}d", { count: Math.floor(hours / 24) });
}

/** The current time, refreshed every minute and whenever the window gains focus. */
export function useMinuteClock(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const refresh = () => setNow(Date.now());
    const timer = setInterval(refresh, MINUTE);
    window.addEventListener("focus", refresh);
    return () => {
      clearInterval(timer);
      window.removeEventListener("focus", refresh);
    };
  }, []);
  return now;
}
