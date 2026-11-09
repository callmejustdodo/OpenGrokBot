// SC-020 information panel, beside the conversation: my agent (details, the
// Instructions & skills card, and the schedules and sharing sections other tasks
// own), a teammate's agent (nothing private), or a person (their shared agents).
// The collapse button hides it; the conversation header's panel button brings it back.
import { useTranslation } from "react-i18next";
import type { AgentId } from "../../ipc/generated/AgentId";
import type { MemberId } from "../../ipc/generated/MemberId";
import { useAppStore } from "../../store/store";
import { uiStore, useUi } from "../../store/ui";
import { harnessLabel as detectedHarnessLabel } from "../../views/connect/capabilities";
import SchedulesPanelSection from "../../views/schedules/SchedulesPanelSection";
import SharingSection from "../../views/sharing/SharingSection";
import AgentAvatar, { PersonAvatar } from "../sidebar/AgentAvatar";
import { harnessLabel, stateLabel } from "../sidebar/labels";

function when(at: string | null): string {
  return at ? new Date(at).toLocaleString([], { dateStyle: "medium", timeStyle: "short" }) : "";
}

function CollapseButton() {
  const { t } = useTranslation();
  return (
    <button
      type="button"
      className="ticon collapse"
      aria-label={t("panel.collapse", "Hide information panel")}
      title={t("panel.collapse", "Hide information panel")}
      onClick={() => uiStore.getState().setPanelOpen(false)}
    />
  );
}

function MyAgentPanel({ agentId }: { agentId: AgentId }) {
  const { t } = useTranslation();
  const agent = useAppStore((s) => s.mirror?.agents[agentId]);
  const mirror = useAppStore((s) => s.mirror);
  if (!agent || !mirror) return null;
  const me = mirror.team?.me;
  const teammateQuestions = Object.values(mirror.questions)
    .filter((q) => q.target.kind === "agent" && q.target.member_id === me && q.target.agent_id === agentId && q.asker !== me)
    .sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at))
    .slice(0, 3);

  return (
    <aside className="panel" aria-label={t("panel.agent", "Agent information")} data-kind="my-agent">
      <CollapseButton />
      <div className="p-head">
        <AgentAvatar avatar={agent.avatar} size={56} />
        <span>
          <b>{agent.name}</b>
          <small>{agent.role}</small>
        </span>
      </div>
      <div className="p-card">
        <dl className="kv">
          <dt>{t("panel.state", "State")}</dt>
          <dd>
            <span className="status" data-state={agent.state}>
              {stateLabel(t, agent.state)}
            </span>
          </dd>
          <dt>{t("panel.lastActivity", "Last activity")}</dt>
          <dd>{when(agent.last_activity_at)}</dd>
          <dt>{t("panel.harness", "Harness")}</dt>
          <dd>{agent.detected ? detectedHarnessLabel(agent.detected.harness, t) : harnessLabel(t, agent.harness)}</dd>
          <dt>{t("panel.folder", "Folder")}</dt>
          <dd>
            <span className="mono">{agent.folder_path}</span>
          </dd>
        </dl>
      </div>
      {agent.detected ? (
        <div className="p-card">
          <small>
            {t(
              "panel.detectedNote",
              "Detected agent. Instructions, schedules, and sharing are for agents Formation created.",
            )}
          </small>
        </div>
      ) : (
        <>
          <button
            type="button"
            className="p-card"
            onClick={() => uiStore.getState().openSheet({ kind: "instructions", agentId })}
          >
            <span className="pr">
              <b>{t("panel.instructions", "Instructions & skills")}</b>
            </span>
            <small>AGENTS.md</small>
          </button>
          <SchedulesPanelSection agentId={agentId} />
          <SharingSection agentId={agentId} />
          {teammateQuestions.length > 0 ? (
            <>
              <div className="p-lbl">{t("panel.teammateQuestions", "Teammates' questions")}</div>
              {teammateQuestions.map((q) => (
                <div key={q.id} className="p-card">
                  <small>
                    {mirror.team?.members.find((m) => m.id === q.asker)?.display_name ?? ""} · “{q.text}”
                  </small>
                </div>
              ))}
            </>
          ) : null}
        </>
      )}
    </aside>
  );
}

function TeammateAgentPanel({ owner, agentId }: { owner: MemberId; agentId: AgentId }) {
  const { t } = useTranslation();
  const agent = useAppStore((s) => s.mirror?.teammate_agents[owner]?.find((a) => a.agent_id === agentId));
  const ownerName = useAppStore((s) => s.mirror?.team?.members.find((m) => m.id === owner)?.display_name ?? "");
  if (!agent) {
    return (
      <aside className="panel" aria-label={t("panel.agent", "Agent information")} data-kind="teammate-agent">
        <CollapseButton />
        <div className="p-card">
          <small>{t("panel.notShared", "{{owner}} no longer shares this agent.", { owner: ownerName })}</small>
        </div>
      </aside>
    );
  }
  return (
    <aside className="panel" aria-label={t("panel.agent", "Agent information")} data-kind="teammate-agent">
      <CollapseButton />
      <div className="p-head">
        <AgentAvatar avatar={agent.avatar} size={56} ownerInitial={ownerName.slice(0, 1).toUpperCase()} />
        <span>
          <b>{agent.name}</b>
          <small>{t("panel.ownersAgent", "{{owner}}'s agent", { owner: ownerName })}</small>
        </span>
      </div>
      <div className="p-card">
        <dl className="kv">
          <dt>{t("panel.role", "Role")}</dt>
          <dd>{agent.role}</dd>
          <dt>{t("panel.harness", "Harness")}</dt>
          <dd>{harnessLabel(t, agent.harness)}</dd>
          <dt>{t("panel.state", "State")}</dt>
          <dd>
            <span className="status" data-state={agent.state}>
              {stateLabel(t, agent.state)}
            </span>
          </dd>
          <dt>{t("panel.lastActivity", "Last activity")}</dt>
          <dd>{when(agent.last_activity_at)}</dd>
        </dl>
      </div>
      <div className="p-card" data-accepts-requests={agent.accepts_requests}>
        <span className="pr">
          <b>{t("panel.workRequests", "Work requests")}</b>
        </span>
        <small>
          {agent.accepts_requests
            ? t("panel.requestsAccepted", "Accepted. Actions follow {{owner}}'s approval rules.", { owner: ownerName })
            : t("panel.requestsNotAccepted", "Not accepted")}
        </small>
        {agent.accepts_requests ? (
          <button
            type="button"
            className="btn primary"
            onClick={() => uiStore.getState().openSheet({ kind: "work-request", owner, agentId, fromQuestion: null })}
          >
            {t("panel.requestWork", "Request work…")}
          </button>
        ) : null}
      </div>
    </aside>
  );
}

function PersonPanel({ memberId }: { memberId: MemberId }) {
  const { t } = useTranslation();
  const member = useAppStore((s) => s.mirror?.team?.members.find((m) => m.id === memberId));
  const agents = useAppStore((s) => s.mirror?.teammate_agents[memberId]);
  if (!member) return null;
  return (
    <aside className="panel" aria-label={t("panel.person", "Teammate information")} data-kind="person">
      <CollapseButton />
      <div className="p-head">
        <PersonAvatar name={member.display_name} size={56} />
        <span>
          <b>{member.display_name}</b>
          <small data-online={member.online}>
            {member.online ? t("sidebar.online", "Online") : t("sidebar.offline", "Offline")}
          </small>
        </span>
      </div>
      <div className="p-card">
        <small>
          {t("panel.personNote", "When you ask a person, Formation picks which shared agent answers.")}
        </small>
      </div>
      <div className="p-lbl">{t("panel.sharedAgents", "Shared agents")}</div>
      {agents && agents.length > 0 ? (
        agents.map((a) => (
          <button
            key={a.agent_id}
            type="button"
            className="p-card"
            onClick={() => uiStore.getState().navigate({ kind: "teammate-agent", owner: memberId, agentId: a.agent_id })}
          >
            <span className="pr">
              <AgentAvatar avatar={a.avatar} size={26} />
              <b>{a.name}</b>
              <span className="status" data-state={a.state}>
                {stateLabel(t, a.state)}
              </span>
            </span>
          </button>
        ))
      ) : (
        <div className="p-card">
          <small>{t("panel.noSharedAgents", "No shared agents")}</small>
        </div>
      )}
    </aside>
  );
}

export default function InfoPanel() {
  const view = useUi((s) => s.view);
  switch (view.kind) {
    case "agent":
      return <MyAgentPanel agentId={view.agentId} />;
    case "teammate-agent":
      return <TeammateAgentPanel owner={view.owner} agentId={view.agentId} />;
    case "person":
      return <PersonPanel memberId={view.memberId} />;
    default:
      return null;
  }
}
