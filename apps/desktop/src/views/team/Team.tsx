// SC-013 team: members with online state and shared-agent counts, an expiring
// invitation, and Remove for the team creator.
import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { Invitation } from "../../ipc/generated/Invitation";
import type { Member } from "../../ipc/generated/Member";
import { tauriTransport } from "../../ipc/transport";
import Alert, { AlertCancel } from "../../ui/Alert";
import { useAppStore } from "../../store/store";
import { act } from "../../store/sync";
import { uiStore } from "../../store/ui";
import { formatTime } from "../ask-conversation/format";

export default function Team() {
  const { t, i18n } = useTranslation();
  const team = useAppStore((s) => s.mirror?.team ?? null);
  const teammateAgents = useAppStore((s) => s.mirror?.teammate_agents);
  const inviting = useAppStore((s) => "team:invite" in s.pending);
  const pending = useAppStore((s) => s.pending);
  const [created, setCreated] = useState<Invitation | null>(null);
  const [inviteFailed, setInviteFailed] = useState(false);
  const [removing, setRemoving] = useState<Member | null>(null);
  const [copied, setCopied] = useState(false);

  if (!team) return <section className="page" aria-label={t("team.label", "Team")} />;

  const me = team.members.find((m) => m.id === team.me) ?? null;
  const creator = team.members.find((m) => m.role === "creator") ?? null;
  const iAmCreator = me?.role === "creator";
  const onlyMe = team.members.length <= 1;
  const invitation = created ?? (team.invitation && team.invitation.used_by === null ? team.invitation : null);
  const state = invitation ? "invite" : onlyMe ? "only-me" : "default";

  const invite = () => {
    setInviteFailed(false);
    setCopied(false);
    act(tauriTransport, "team:invite", "team_invite", {}).then(setCreated, () => setInviteFailed(true));
  };

  const copy = (code: string) => {
    void navigator.clipboard?.writeText(code).then(
      () => setCopied(true),
      () => setCopied(false),
    );
  };

  const remove = (member: Member) => {
    setRemoving(null);
    act(tauriTransport, `team:remove:${member.id}`, "team_remove", { member_id: member.id }).catch(() => {});
  };

  const sharedCount = (member: Member) => (teammateAgents?.[member.id] ?? []).length;
  const subtitle = (member: Member) => {
    if (member.id === team.me) {
      return member.role === "creator" ? t("team.meCreator", "Me · team creator") : t("team.me", "Me");
    }
    const count = sharedCount(member);
    if (count === 0) return t("team.noShared", "No shared agents");
    if (count === 1) return t("team.sharedOne", "1 shared agent");
    return t("team.sharedMany", "{{count}} shared agents", { count });
  };

  return (
    <section className="page" aria-label={t("team.label", "Team")} data-state={state}>
      <div className="h1row">
        <h1>{t("team.title", "Team")}</h1>
        <span className="lead">
          {t("team.lead", "{{creator}}'s team · {{count}} people", {
            creator: creator?.display_name ?? "",
            count: team.members.length,
          })}
        </span>
        <span className="spacer" />
        <button type="button" className="btn primary" disabled={inviting} onClick={invite}>
          {t("team.invite", "Invite")}
        </button>
      </div>
      {inviteFailed ? (
        <div className="banner warn" role="alert">
          <div className="bt">
            <b>{t("team.inviteFailed", "Couldn't create an invitation")}</b>
          </div>
          <button type="button" className="btn" onClick={invite}>
            {t("team.tryAgain", "Try again")}
          </button>
        </div>
      ) : null}
      {invitation ? (
        <div className="list invite" role="region" aria-label={t("team.invitationCreated", "Invitation link created")}>
          <b>{t("team.invitationCreated", "Invitation link created")}</b>
          <div className="row-actions">
            <span className="code">{invitation.link}</span>
            <button type="button" className="btn" onClick={() => copy(invitation.link)}>
              {copied ? t("team.copied", "Copied") : t("team.copy", "Copy")}
            </button>
          </div>
          <p className="muted small">
            {t("team.expires", "Expires {{time}}. Send it over Slack or chat.", {
              time: formatTime(invitation.expires_at, i18n.language),
            })}
          </p>
        </div>
      ) : null}
      {onlyMe ? (
        <div className="banner info" role="status">
          <div className="bt">
            <b>{t("team.onlyMeTitle", "It's just you so far")}</b>
            {t("team.onlyMeBody", "Invite teammates to ask each other's agents.")}
          </div>
        </div>
      ) : null}
      <ul className="list" aria-label={t("team.members", "Members")}>
        {team.members.map((member) => (
          <li key={member.id} className="row" data-online={member.online}>
            <span className="t">
              <b>{member.display_name}</b>
              <small>{subtitle(member)}</small>
            </span>
            <span className={member.online ? "online" : "online off"}>
              {member.online ? t("team.online", "Online") : t("team.offline", "Offline")}
            </span>
            {member.id !== team.me ? (
              <>
                <button
                  type="button"
                  className="btn plain"
                  aria-label={t("team.viewAgentsOf", "View {{name}}'s agents", { name: member.display_name })}
                  onClick={() => uiStore.getState().navigate({ kind: "member", memberId: member.id })}
                >
                  {t("team.viewAgents", "View agents")}
                </button>
                {iAmCreator ? (
                  <button
                    type="button"
                    className="btn plain"
                    aria-label={t("team.removeMember", "Remove {{name}}", { name: member.display_name })}
                    disabled={`team:remove:${member.id}` in pending}
                    onClick={() => setRemoving(member)}
                  >
                    {t("team.remove", "Remove")}
                  </button>
                ) : null}
              </>
            ) : null}
          </li>
        ))}
      </ul>
      <Alert
        open={removing !== null}
        onOpenChange={(open) => !open && setRemoving(null)}
        title={t("team.removeTitle", "Remove {{name}}?", { name: removing?.display_name ?? "" })}
        description={t(
          "team.removeBody",
          "{{name}} loses access at once: no more questions or requests, and their agents leave your lists.",
          { name: removing?.display_name ?? "" },
        )}
      >
        <button type="button" className="btn destructive" onClick={() => removing && remove(removing)}>
          {t("team.removeConfirm", "Remove")}
        </button>
        <AlertCancel asChild>
          <button type="button" className="btn">
            {t("team.cancel", "Cancel")}
          </button>
        </AlertCancel>
      </Alert>
    </section>
  );
}
