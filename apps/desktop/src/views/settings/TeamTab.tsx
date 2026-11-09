// SC-018 "team": the team, its members, my role, and leaving (with a
// confirmation). Leaving has no event that clears the team, so a new
// snapshot is requested once formationd confirms.
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { tauriTransport } from "../../ipc/transport";
import { appStore, useAppStore } from "../../store/store";
import { act } from "../../store/sync";
import Alert, { AlertCancel } from "../../ui/Alert";

const LEAVE_PENDING = "team_leave";

export default function TeamTab() {
  const { t } = useTranslation();
  const team = useAppStore((s) => s.mirror?.team ?? null);
  const connected = useAppStore((s) => s.connected);
  const leaving = useAppStore((s) => LEAVE_PENDING in s.pending);
  const [confirming, setConfirming] = useState(false);
  const [failed, setFailed] = useState(false);

  if (!team) {
    return <p className="muted">{t("settings.team.none", "You're not in a team.")}</p>;
  }

  const me = team.members.find((m) => m.id === team.me);
  const creator = team.members.find((m) => m.role === "creator");

  const leave = () => {
    setConfirming(false);
    setFailed(false);
    act(tauriTransport, LEAVE_PENDING, "team_leave", {}).then(
      () => appStore.getState().requestSnapshot(),
      () => setFailed(true),
    );
  };

  return (
    <>
      <div className="form-row">
        <span>{t("settings.team.team", "Team")}</span>
        <span>
          {creator
            ? t("settings.team.name", "{{name}}'s team", { name: creator.display_name })
            : t("settings.team.unnamed", "Your team")}
        </span>
      </div>
      <div className="form-row">
        <span>{t("settings.team.role", "My role")}</span>
        <span className="muted">
          {me?.role === "creator"
            ? t("settings.team.creator", "Team creator")
            : t("settings.team.member", "Member")}
        </span>
      </div>
      <section aria-labelledby="settings-members">
        <h2 id="settings-members">{t("settings.team.members", "Members")}</h2>
        <ul className="list">
          {team.members.map((m) => (
            <li key={m.id} className="form-row" data-online={m.online}>
              <span>
                {m.display_name} <small className="muted">@{m.github_login}</small>
              </span>
              <span className="muted">
                {m.online ? t("settings.team.online", "Online") : t("settings.team.offline", "Offline")}
              </span>
            </li>
          ))}
        </ul>
      </section>
      <div className="form-row">
        <span />
        <button
          type="button"
          className="btn destructive"
          disabled={leaving || !connected}
          aria-busy={leaving}
          onClick={() => setConfirming(true)}
        >
          {t("settings.team.leave", "Leave team")}
        </button>
      </div>
      {failed ? (
        <p className="err" role="alert">
          {t("settings.team.leaveFailed", "Couldn't leave the team. Try again.")}
        </p>
      ) : null}
      <Alert
        open={confirming}
        onOpenChange={setConfirming}
        title={t("settings.team.leaveTitle", "Leave the team?")}
        description={t(
          "settings.team.leaveBody",
          "Teammates can no longer ask your agents, and you can no longer ask theirs. Your agents keep running.",
        )}
      >
        <button type="button" className="btn destructive" onClick={leave}>
          {t("settings.team.leaveConfirm", "Leave team")}
        </button>
        <AlertCancel className="btn">{t("settings.team.cancel", "Cancel")}</AlertCancel>
      </Alert>
    </>
  );
}
