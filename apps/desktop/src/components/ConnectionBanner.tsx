// Shown while formationd is unreachable (frontend architecture, "Reconnect").
// The connection retries by itself; the banner says so and goes away on return.
// Also shown while connected to a formationd built from another contract version.
import { useTranslation } from "react-i18next";
import { PROTOCOL_VERSION } from "../ipc/generated/commands";
import { useAppStore } from "../store/store";

export default function ConnectionBanner() {
  const connected = useAppStore((s) => s.connected);
  const everConnected = useAppStore((s) => s.mirror !== null);
  const daemonProtocol = useAppStore((s) => s.mirror?.daemon.protocol);
  const { t } = useTranslation();
  if (connected) {
    if (daemonProtocol === undefined || daemonProtocol === PROTOCOL_VERSION) return null;
    return (
      <div className="banner warn" role="status" data-protocol-mismatch>
        <b>{t("connection.restartNeeded", "formationd needs restarting")}</b>
        <span>
          {t(
            "connection.protocolMismatch",
            "It speaks contract version {{daemon}}; this app needs version {{app}}. Restart formationd to update it.",
            { daemon: daemonProtocol, app: PROTOCOL_VERSION },
          )}
        </span>
      </div>
    );
  }
  return (
    <div className="banner warn" role="status" data-first-run={!everConnected}>
      <b>{everConnected ? "Lost the connection to formationd" : "Connecting to formationd…"}</b>
      <span>Retrying automatically. Agents keep running in formationd.</span>
    </div>
  );
}
