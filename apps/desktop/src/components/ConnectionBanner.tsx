// Shown while formationd is unreachable (frontend architecture, "Reconnect").
// The connection retries by itself; the banner says so and goes away on return.
import { useAppStore } from "../store/store";

export default function ConnectionBanner() {
  const connected = useAppStore((s) => s.connected);
  const everConnected = useAppStore((s) => s.mirror !== null);
  if (connected) return null;
  return (
    <div className="banner warn" role="status" data-first-run={!everConnected}>
      <b>{everConnected ? "Lost the connection to formationd" : "Connecting to formationd…"}</b>
      <span>Retrying automatically. Agents keep running in formationd.</span>
    </div>
  );
}
