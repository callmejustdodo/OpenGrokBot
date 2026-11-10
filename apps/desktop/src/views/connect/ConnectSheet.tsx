import "./connect.css";
// SC-009 Connect agents sheet: finds agents running outside Formation on this
// Mac (`agents_detect`), shows what Formation can do with each, and connects
// the chosen one (`agent_connect`), which then opens its read-only
// conversation (SC-005 detected). The owner can also search one folder, which
// lists its sessions however old; the list is grouped by folder.
import { useEffect, useState } from "react";
import { pickFolders, tildePath, useHomeDir } from "../../components/folders/folders";
import { useTranslation } from "react-i18next";
import type { CallError } from "../../ipc/client";
import type { DetectedAgent } from "../../ipc/generated/DetectedAgent";
import { tauriTransport } from "../../ipc/transport";
import { useAppStore } from "../../store/store";
import { act } from "../../store/sync";
import { uiStore } from "../../store/ui";
import Sheet from "../../ui/Sheet";
import { capabilityRows, connectKind, harnessLabel } from "./capabilities";

/** The sidebar's detecting state (SC-003) reads this pending key too. */
const DETECT_KEY = "agents_detect";
const connectKey = (key: string) => `agent_connect:${key}`;

function errorCode(error: unknown): string | null {
  return (error as Partial<CallError> | null)?.data?.code ?? null;
}

export default function ConnectSheet() {
  const { t, i18n } = useTranslation();
  const [agents, setAgents] = useState<DetectedAgent[] | null>(null);
  const [detectFailed, setDetectFailed] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const [selected, setSelected] = useState<string | null>(null);
  const [connectError, setConnectError] = useState<{ key: string; code: string | null } | null>(null);
  // The folder being searched; null lists the recent sessions.
  const [folder, setFolder] = useState<string | null>(null);
  const [pickFailed, setPickFailed] = useState(false);
  const home = useHomeDir();

  useEffect(() => {
    let live = true;
    act(tauriTransport, DETECT_KEY, "agents_detect", folder ? { folder } : {})
      .then((found) => {
        if (!live) return;
        setAgents(found);
        setSelected((key) => key ?? (found.find((a) => !a.connected_agent) ?? found[0])?.key ?? null);
      })
      .catch(() => live && setDetectFailed(true));
    return () => {
      live = false;
    };
  }, [attempt, folder]);

  const search = (next: string | null) => {
    setAgents(null);
    setDetectFailed(false);
    setSelected(null);
    setFolder(next);
  };
  const pick = () => {
    setPickFailed(false);
    pickFolders(false).then(
      ([chosen]) => chosen && search(chosen),
      () => setPickFailed(true),
    );
  };

  const close = () => uiStore.getState().closeSheet();
  const open = (agentId: string) => {
    uiStore.getState().navigate({ kind: "agent", agentId });
    close();
  };
  const connect = (agent: DetectedAgent) => {
    setConnectError(null);
    act(tauriTransport, connectKey(agent.key), "agent_connect", folder ? { key: agent.key, folder } : { key: agent.key })
      .then((connected) => open(connected.id))
      .catch((error: unknown) => setConnectError({ key: agent.key, code: errorCode(error) }));
  };

  const current = agents?.find((a) => a.key === selected) ?? null;
  const kind = current ? connectKind(current) : null;
  const connecting = useAppStore((s) => (current ? connectKey(current.key) in s.pending : false));
  const when = (at: string | null) =>
    at
      ? new Date(at).toLocaleString(i18n.language, { dateStyle: "medium", timeStyle: "short" })
      : t("connect.noActivity", "No activity yet");

  let body;
  if (!agents && !detectFailed) {
    body = (
      <p role="status" data-state="detecting">
        {t("connect.detecting", "Looking for agents running on this Mac…")}
      </p>
    );
  } else if (!agents) {
    body = (
      <div role="alert" data-state="detect-error">
        <p>{t("connect.detectFailed", "Couldn't look for agents. Check that formationd is running.")}</p>
        <button type="button" onClick={() => {
            setDetectFailed(false);
            setAttempt((n) => n + 1);
          }}>
          {t("connect.retry", "Try again")}
        </button>
      </div>
    );
  } else if (agents && agents.length === 0) {
    body = (
      <p className="empty" data-state="empty">
        {folder
          ? t("connect.noneInFolder", "No Claude Code or Codex sessions ran in this folder.")
          : t("connect.none", "No agents are running outside OpenGrokBot on this Mac.")}
      </p>
    );
  } else if (agents) {
    // Grouped by folder, in the order each folder first appears.
    const folders = [...new Set(agents.map((a) => a.folder))];
    body = (
      <div className="connect-body">
        <ul className="list" aria-label={t("connect.found", "Detected agents")}>
          {folders.flatMap((f) => [
            <li key={`folder:${f}`} role="presentation" className="connect-group mono">
              {tildePath(f, home)}
            </li>,
            ...agents.filter((a) => a.folder === f).map((a) => (
            <li key={a.key} data-state={connectKind(a)}>
              <button type="button" className="row" aria-pressed={a.key === selected} onClick={() => setSelected(a.key)}>
                <span className="t">
                  <b>{a.name}</b>
                  <small>
                    {harnessLabel(a.harness, t)}
                    {a.connected_agent ? ` · ${t("connect.connected", "Connected")}` : ""}
                  </small>
                </span>
              </button>
            </li>
            )),
          ])}
        </ul>
        {current && kind ? (
          <section aria-label={current.name} data-state={kind}>
            <dl>
              <dt>{t("connect.harnessLabel", "Harness")}</dt>
              <dd>{harnessLabel(current.harness, t)}</dd>
              <dt>{t("connect.path", "Folder")}</dt>
              <dd className="mono">{current.path}</dd>
              <dt>{t("connect.lastActivity", "Last activity")}</dt>
              <dd>{when(current.last_activity_at)}</dd>
            </dl>
            {kind === "read-only" ? (
              <p className="p-card">
                {t(
                  "connect.readOnly",
                  "Read-only: OpenGrokBot can show this agent's activity but can't use it for teammates.",
                )}
              </p>
            ) : null}
            {kind === "not-supported" ? (
              <p className="p-card">{t("connect.notSupported", "OpenGrokBot can't connect this agent yet.")}</p>
            ) : null}
            <ul className="list" aria-label={t("connect.capabilities", "What OpenGrokBot can do")}>
              {capabilityRows(current.capabilities, t).map((c) => (
                <li key={c.key} className="row" data-supported={c.supported}>
                  <span className={c.supported ? "ok" : "muted"}>
                    {c.supported ? t("connect.works", "Works") : t("connect.doesNotWork", "Doesn't work")}
                  </span>
                  <span className="t">
                    {c.label}
                    {c.reason ? <small>{c.reason}</small> : null}
                  </span>
                </li>
              ))}
            </ul>
            <p className="muted small">
              {t("connect.noChange", "OpenGrokBot won't change this agent's own configuration.")}
            </p>
            {connectError?.key === current.key ? (
              <p role="alert" data-state="connect-error">
                {connectError.code === "harness_unsupported"
                  ? t("connect.error.harness_unsupported", "OpenGrokBot doesn't support this harness yet.")
                  : t("connect.error.other", "Couldn't connect this agent. Try again.")}
              </p>
            ) : null}
          </section>
        ) : null}
      </div>
    );
  }

  const connectedId = current?.connected_agent ?? null;
  const primary = !current ? null : connectedId ? (
    <button type="button" className="primary" onClick={() => open(connectedId)}>
      {t("connect.open", "Open")}
    </button>
  ) : (
    <button
      type="button"
      className="primary"
      disabled={kind === "not-supported" || connecting}
      aria-busy={connecting}
      onClick={() => connect(current)}
    >
      {connecting ? t("connect.connecting", "Connecting…") : t("connect.connect", "Connect")}
    </button>
  );

  return (
    <Sheet
      open
      onOpenChange={(isOpen) => (isOpen ? undefined : close())}
      title={t("connect.title", "Connect agents")}
      lead={t(
        "connect.lead",
        "Agents found running outside OpenGrokBot on this Mac. Connecting adds them to your list, private.",
      )}
      footer={
        <>
          <button type="button" onClick={close}>
            {t("connect.cancel", "Cancel")}
          </button>
          {primary}
        </>
      }
    >
      <div className="connect-folder">
        {folder ? (
          <>
            <span className="muted small">
              {t("connect.searching", "Sessions in {{folder}}", { folder: tildePath(folder, home) })}
            </span>
            <button type="button" onClick={() => search(null)}>
              {t("connect.recent", "Back to recent")}
            </button>
          </>
        ) : null}
        <button type="button" onClick={pick}>
          {t("connect.findInFolder", "Find in a folder…")}
        </button>
        {pickFailed ? (
          <span role="alert" className="small">
            {t("connect.pickFailed", "Couldn't open the folder picker. Try again.")}
          </span>
        ) : null}
      </div>
      {body}
    </Sheet>
  );
}
