// SC-020 information panel, beside the conversation: my agent (details, the
// compact Instructions & skills, Schedules and Sharing cards, and teammates'
// questions), a teammate's agent (nothing private), or a person (their shared agents).
// The collapse button hides it; the conversation header's panel button brings it back.
import { useState } from "react";
import { useTranslation } from "react-i18next";
import "./info-panel.css";
import type { AgentId } from "../../ipc/generated/AgentId";
import type { MemberId } from "../../ipc/generated/MemberId";
import { useAppStore } from "../../store/store";
import { uiStore, useUi } from "../../store/ui";
import { harnessLabel as detectedHarnessLabel } from "../../views/connect/capabilities";
import { dayAndTime, selectAgentSchedules, setSheetFocus } from "../../views/schedules/format";
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

/** A compact card: title, one-line summary, and a › chevron. */
function LinkCard({
  title,
  summary,
  onClick,
  expanded,
  controls,
}: {
  title: string;
  summary: string;
  onClick: () => void;
  expanded?: boolean;
  controls?: string;
}) {
  return (
    <button
      type="button"
      className="p-card p-link"
      aria-expanded={expanded}
      aria-controls={controls}
      data-expanded={expanded}
      onClick={onClick}
    >
      <span className="pr">
        <b>{title}</b>
        <span className="chev" aria-hidden="true" />
      </span>
      <small>{summary}</small>
    </button>
  );
}

const NO_SCHEDULES: ReturnType<typeof selectAgentSchedules> = [];

function SchedulesCard({ agentId }: { agentId: AgentId }) {
  const { t, i18n } = useTranslation();
  const schedules = useAppStore((s) => (s.mirror ? selectAgentSchedules(s.mirror, agentId) : NO_SCHEDULES));
  const next = schedules
    .filter((s) => !s.paused && s.next_run_at)
    .map((s) => s.next_run_at as string)
    .sort()[0];
  const summary =
    schedules.length === 0
      ? t("schedules.panel.empty", "No schedules")
      : [
          t("panel.scheduleCount", {
            count: schedules.length,
            defaultValue_one: "{{count}} schedule",
            defaultValue_other: "{{count}} schedules",
          }),
          next ? t("schedules.panel.next", "Next: {{when}}", { when: dayAndTime(next, i18n.language) }) : null,
        ]
          .filter(Boolean)
          .join(" · ");
  return (
    <LinkCard
      title={t("schedules.title", "Schedules")}
      summary={summary}
      onClick={() => {
        setSheetFocus(agentId, null);
        uiStore.getState().openSheet({ kind: "schedules", agentId });
      }}
    />
  );
}

function SharingCard({ agentId }: { agentId: AgentId }) {
  const { t } = useTranslation();
  const share = useAppStore((s) => s.mirror?.shares[agentId] ?? null);
  const [open, setOpen] = useState(false);
  const controlsId = `sharing-${agentId}`;
  const summary = share?.shared
    ? [
        t("panel.sharing.shared", "Shared with team"),
        share.visibility === "answer"
          ? t("sharing.answerOnly", "Answer only")
          : t("sharing.answerExcerpts", "Answer and excerpts"),
        share.accepts_requests ? t("panel.sharing.requests", "Accepts work requests") : null,
      ]
        .filter(Boolean)
        .join(" · ")
    : t("panel.sharing.private", "Private");
  return (
    <>
      <LinkCard
        title={t("sharing.title", "Sharing")}
        summary={summary}
        expanded={open}
        controls={controlsId}
        onClick={() => setOpen((o) => !o)}
      />
      <div id={controlsId} hidden={!open}>
        {open ? <SharingSection agentId={agentId} /> : null}
      </div>
    </>
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
              "Detected agent. Instructions, schedules, and sharing are for agents OpenGrokBot created.",
            )}
          </small>
        </div>
      ) : (
        <>
          <LinkCard
            title={t("panel.instructions", "Instructions & skills")}
            summary="AGENTS.md"
            onClick={() => uiStore.getState().openSheet({ kind: "instructions", agentId })}
          />
          <SchedulesCard agentId={agentId} />
          <SharingCard agentId={agentId} />
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
          {t("panel.personNote", "When you ask a person, OpenGrokBot picks which shared agent answers.")}
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
