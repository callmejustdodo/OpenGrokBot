// SC-001 welcome and setup (no team yet): harness check, create or join a team,
// notifications. Once a team exists, `team.update` takes the window to the main view.
import * as Switch from "@radix-ui/react-switch";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import "./welcome.css";
import type { HarnessStatus } from "../../ipc/generated/HarnessStatus";
import { tauriTransport } from "../../ipc/transport";
import { useAppStore } from "../../store/store";
import { act } from "../../store/sync";
import { harnessLabel } from "../ask-conversation/format";
import { errorCode } from "../ask-conversation/selectors";

const NO_HARNESSES: readonly HarnessStatus[] = [];

type JoinError = "expired" | "rate-limited" | "creator-offline" | "unreachable" | null;

export default function Welcome() {
  const { t } = useTranslation();
  const harnesses = useAppStore((s) => s.mirror?.harnesses ?? NO_HARNESSES);
  const settings = useAppStore((s) => s.mirror?.settings ?? null);
  const pending = useAppStore((s) => s.pending);
  const [code, setCode] = useState("");
  const [joinError, setJoinError] = useState<JoinError>(null);
  const [createFailed, setCreateFailed] = useState(false);

  const missing = harnesses.some((h) => !h.installed || h.signed_in === false);
  const busy = "team:create" in pending || "team:join" in pending;
  const state = joinError === "expired" ? "invite-expired" : missing ? "harness-missing" : "default";

  const check = () => void act(tauriTransport, "harness:check", "harness_check", {}).catch(() => {});

  const create = () => {
    setCreateFailed(false);
    act(tauriTransport, "team:create", "team_create", { display_name: null }).catch(() => setCreateFailed(true));
  };

  const join = () => {
    const trimmed = code.trim();
    if (!trimmed) return;
    setJoinError(null);
    act(tauriTransport, "team:join", "team_join", { code: trimmed, display_name: null }).catch((error: unknown) => {
      // Unknown, used, or expired codes answer `not_found` (tech spec, "Team and roster").
      switch (errorCode(error)) {
        case "not_found":
        case "invalid_params":
          return setJoinError("expired");
        case "rate_limited":
          return setJoinError("rate-limited");
        case "peer_offline":
          return setJoinError("creator-offline");
        default:
          return setJoinError("unreachable");
      }
    });
  };

  const setNotifications = (enabled: boolean) => {
    if (!settings) return;
    void act(tauriTransport, "settings:notifications", "settings_update", {
      settings: { ...settings, notifications: { ...settings.notifications, enabled } },
    }).catch(() => {});
  };

  return (
    <section className="page welcome" aria-label={t("welcome.label", "Welcome")} data-state={state}>
      <div className="wbody">
        <h2>{t("welcome.title", "Welcome to OpenGrokBot")}</h2>
        <p className="muted">
          {t("welcome.lead", "Teammates ask your agents directly, and you run your agents in one place.")}
        </p>

        <section className="wstep" aria-labelledby="welcome-harnesses">
          <span className="n">1</span>
          <div>
            <b id="welcome-harnesses">{t("welcome.harnesses", "Check harnesses")}</b>
            <ul className="checks">
              {harnesses.map((h) => (
                <li key={h.harness} className="check" data-state={harnessState(h)}>
                  {harnessLabel(t, h.harness)} · {harnessText(t, h)}
                </li>
              ))}
            </ul>
            <button type="button" className="btn" disabled={"harness:check" in pending} onClick={check}>
              {t("welcome.checkAgain", "Check again")}
            </button>
          </div>
        </section>

        <section className="wstep" aria-labelledby="welcome-team">
          <span className="n">2</span>
          <div>
            <b id="welcome-team">{t("welcome.team", "Team")}</b>
            <div className="row-actions">
              <button type="button" className="btn primary" disabled={busy} onClick={create}>
                {t("welcome.createTeam", "Create a team")}
              </button>
            </div>
            {createFailed ? (
              <span className="err" role="alert">
                {t("welcome.createFailed", "Couldn't create the team. Try again.")}
              </span>
            ) : null}
            <form
              className="row-actions"
              onSubmit={(e) => {
                e.preventDefault();
                join();
              }}
            >
              <input
                className={joinError === "expired" ? "input invalid" : "input"}
                aria-label={t("welcome.invitationCode", "Invitation code")}
                aria-invalid={joinError === "expired"}
                aria-describedby={joinError ? "welcome-join-error" : undefined}
                placeholder="formation://join/…"
                value={code}
                onChange={(e) => {
                  setCode(e.target.value);
                  setJoinError(null);
                }}
              />
              <button type="submit" className="btn" disabled={busy || code.trim() === ""}>
                {t("welcome.join", "Join")}
              </button>
            </form>
            {joinError ? (
              <span className="err" id="welcome-join-error" role="alert">
                {joinErrorText(t, joinError)}
              </span>
            ) : null}
          </div>
        </section>

        <section className="wstep" aria-labelledby="welcome-notifications">
          <span className="n">3</span>
          <div className="form-row">
            <span>
              <b id="welcome-notifications">{t("welcome.notifications", "Notifications")}</b>
              <small>{t("welcome.notificationsNote", "Only when you're needed: approvals, errors, stalls.")}</small>
            </span>
            <Switch.Root
              className="switch"
              aria-labelledby="welcome-notifications"
              checked={settings?.notifications.enabled ?? false}
              disabled={!settings || "settings:notifications" in pending}
              onCheckedChange={setNotifications}
            >
              <Switch.Thumb />
            </Switch.Root>
          </div>
        </section>
      </div>
    </section>
  );
}

function joinErrorText(t: ReturnType<typeof useTranslation>["t"], error: Exclude<JoinError, null>): string {
  switch (error) {
    case "expired":
      return t("welcome.inviteExpired", "This link has expired. Ask your teammate for a new one.");
    case "rate-limited":
      return t("welcome.joinRateLimited", "Too many tries. Wait a few minutes, then try again.");
    case "creator-offline":
      return t("welcome.creatorOffline", "The team creator's Mac is offline. Try again when it's back.");
    case "unreachable":
      return t("welcome.joinUnreachable", "Couldn't reach formationd. Try again.");
  }
}

function harnessState(h: HarnessStatus): string {
  if (!h.installed) return "missing";
  if (h.signed_in === false) return "signed-out";
  return "ok";
}

function harnessText(t: ReturnType<typeof useTranslation>["t"], h: HarnessStatus): string {
  const name = harnessLabel(t, h.harness);
  if (!h.installed) {
    return t("welcome.notInstalled", "Not installed — {{name}} agents can run after you install it", { name });
  }
  if (h.signed_in === false) {
    return t("welcome.signedOut", "Not signed in — {{name}} agents can run after you sign in", { name });
  }
  if (h.signed_in === null) return t("welcome.installed", "Installed");
  return t("welcome.ready", "Installed, signed in");
}
