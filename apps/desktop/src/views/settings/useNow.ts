import { useEffect, useState } from "react";

/**
 * The current time, refreshed whenever the window gains focus: enough for
 * "paused until" checks in windows that stay open for the whole session.
 */
export function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const refresh = () => setNow(Date.now());
    window.addEventListener("focus", refresh);
    return () => window.removeEventListener("focus", refresh);
  }, []);
  return now;
}
