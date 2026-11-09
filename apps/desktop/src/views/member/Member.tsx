// SC-014 member's agents: a teammate's online state and shared agents, reached
// from Team. Asking an agent or the person opens SC-015, which adds it to the chat list.
import { useTranslation } from "react-i18next";
import "../team/team.css";
import type { MemberId } from "../../ipc/generated/MemberId";
import type { SharedAgentView } from "../../ipc/generated/SharedAgentView";
import { useAppStore } from "../../store/store";
import { uiStore } from "../../store/ui";
import { harnessLabel, stateLabel } from "../ask-conversation/format";
import { selectMember } from "../ask-conversation/selectors";

const NO_AGENTS: readonly SharedAgentView[] = [];

export default function Member({ memberId }: { memberId: MemberId }) {
  const { t } = useTranslation();
  const member = useAppStore((s) => (s.mirror ? selectMember(s.mirror, memberId) : null));
  const agents = useAppStore((s) => s.mirror?.teammate_agents[memberId] ?? NO_AGENTS);
  const ui = uiStore.getState();

  if (!member) {
    return (
      <section className="page" aria-label={t("member.label", "Member")} data-state="missing">
        <div className="empty">
          <b>{t("member.missing", "This person is no longer on the team")}</b>
        </div>
      </section>
    );
  }

  const name = member.display_name;
  const state = agents.length === 0 ? "no-shared" : member.online ? "online" : "offline";

  return (
    <section className="page" aria-label={t("member.label", "Member")} data-state={state}>
      <div className="h1row">
        <h1>{name}</h1>
        <span className="lead">
          <span className={member.online ? "online" : "online off"}>
            {member.online ? t("member.online", "Online") : t("member.offline", "Offline")}
          </span>
        </span>
        <span className="spacer" />
        {agents.length > 0 ? (
          <button
            type="button"
            className="btn primary"
            onClick={() => ui.navigate({ kind: "person", memberId: member.id })}
          >
            {t("member.askPerson", "Ask {{name}}", { name })}
          </button>
        ) : null}
      </div>
      {!member.online ? (
        <div className="banner warn" role="status">
          <div className="bt">
            <b>{t("member.offlineTitle", "{{name}}'s Mac is offline", { name })}</b>
            {t("member.offlineBody", "Questions you send are answered when {{name}} is back.", { name })}
          </div>
        </div>
      ) : null}
      {agents.length === 0 ? (
        <div className="empty">
          <b>{t("member.noShared", "{{name}} hasn't shared any agents yet", { name })}</b>
        </div>
      ) : (
        <ul className="list" aria-label={t("member.sharedAgents", "Shared agents")}>
          {agents.map((a) => (
            <li key={a.agent_id} className="row" data-state={a.state}>
              <span className="t">
                <b>{a.name}</b> <span className="tag">{harnessLabel(t, a.harness)}</span>
                <small>{a.role}</small>
              </span>
              <span className="status" data-state={a.state}>
                {stateLabel(t, a.state)}
              </span>
              <button
                type="button"
                className="btn plain"
                aria-label={t("member.askAgent", "Ask {{agent}}", { agent: a.name })}
                onClick={() => ui.navigate({ kind: "teammate-agent", owner: a.owner, agentId: a.agent_id })}
              >
                {t("member.ask", "Ask")}
              </button>
              {a.accepts_requests ? (
                <button
                  type="button"
                  className="btn plain"
                  aria-label={t("member.requestWorkAgent", "Request work from {{agent}}", { agent: a.name })}
                  onClick={() =>
                    ui.openSheet({ kind: "work-request", owner: a.owner, agentId: a.agent_id, fromQuestion: null })
                  }
                >
                  {t("member.requestWork", "Request work")}
                </button>
              ) : (
                <span className="muted small">{t("member.noRequests", "No requests")}</span>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
