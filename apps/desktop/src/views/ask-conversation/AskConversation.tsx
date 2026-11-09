// SC-015 asking conversation: a teammate's agent, a person (Formation picks the
// agent), or "new" (choosing whom to ask).
import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { AskTarget } from "../../ipc/generated/AskTarget";
import type { Method, Params } from "../../ipc/generated/commands";
import type { Question } from "../../ipc/generated/Question";
import type { SharedAgentView } from "../../ipc/generated/SharedAgentView";
import { tauriTransport } from "../../ipc/transport";
import { useAppStore } from "../../store/store";
import { act } from "../../store/sync";
import { uiStore, type View } from "../../store/ui";
import Composer from "./Composer";
import { stateLabel } from "./format";
import QuestionItem, { type QuestionActions } from "./QuestionItem";
import {
  errorCode,
  selectAskQuestions,
  selectMember,
  selectSharedAgent,
  targetKey,
  type AskTargetView,
} from "./selectors";

const NO_QUESTIONS: Question[] = [];
const NO_AGENTS: readonly SharedAgentView[] = [];

export default function AskConversation({
  view,
}: {
  view: Extract<View, { kind: "teammate-agent" | "person" | "new-question" }>;
}) {
  if (view.kind === "new-question") return <NewQuestion />;
  return <AskThread key={targetKey(view)} target={view} />;
}

/** The conversation's state as a whole, for `data-state` (the M3 state names). */
function viewState(questions: Question[], unshared: boolean): string {
  if (unshared) return "unshared";
  const last = questions[questions.length - 1];
  if (!last) return "empty";
  switch (last.status) {
    case "answered":
      return last.answer?.possibly_stale ? "stale" : "answered";
    case "waiting_offline":
      return "offline-waiting";
    case "sent_to_owner":
      return "sent-to-owner";
    case "failed":
      return "error";
    default:
      return last.status;
  }
}

function AskThread({ target }: { target: AskTargetView }) {
  const { t } = useTranslation();
  const key = targetKey(target);
  const ownerId = target.kind === "person" ? target.memberId : target.owner;
  const questions = useAppStore((s) => (s.mirror ? selectAskQuestions(s.mirror, target) : NO_QUESTIONS));
  const owner = useAppStore((s) => (s.mirror ? selectMember(s.mirror, ownerId) : null));
  const ownerAgents = useAppStore((s) => s.mirror?.teammate_agents[ownerId] ?? NO_AGENTS);
  const agent = useAppStore((s) =>
    s.mirror && target.kind === "teammate-agent" ? selectSharedAgent(s.mirror, target.owner, target.agentId) : null,
  );
  const pending = useAppStore((s) => s.pending);
  // `ask` answered `agent_not_shared` before the mirror caught up.
  const [refused, setRefused] = useState(false);
  const [failure, setFailure] = useState<{ retry: () => void } | null>(null);

  const ownerName = owner?.display_name ?? t("ask.unknownMember", "Your teammate");
  const unshared = target.kind === "teammate-agent" && (agent === null || refused);
  const askKey = `ask:${key}`;
  const askTarget: AskTarget =
    target.kind === "person"
      ? { kind: "person", member_id: target.memberId }
      : { kind: "agent", member_id: target.owner, agent_id: target.agentId };

  function run<M extends Method>(pendingKey: string, method: M, params: Params<M>): Promise<boolean> {
    setFailure(null);
    return act(tauriTransport, pendingKey, method, params).then(
      () => true,
      (error: unknown) => {
        if (errorCode(error) === "agent_not_shared") setRefused(true);
        else setFailure({ retry: () => void run(pendingKey, method, params) });
        return false;
      },
    );
  }

  const ask = (text: string, threadId: string | null) =>
    run(askKey, "ask", { target: askTarget, text, thread_id: threadId, from_run: null });

  const questionKey = (q: Question) => `question:${q.id}`;
  const actions: QuestionActions = {
    refresh: (q) => void run(questionKey(q), "question_refresh", { question_id: q.id }),
    routeChoose: (q, choice) => void run(questionKey(q), "question_route_choose", { question_id: q.id, choice }),
    cancel: (q) => void run(questionKey(q), "question_cancel", { question_id: q.id }),
    askAgain: (q) => void run(questionKey(q), "ask", { target: q.target, text: q.text, thread_id: q.thread_id, from_run: null }),
    askOwner: (q) => {
      const person: AskTarget = { kind: "person", member_id: q.target.member_id };
      void run(`ask:person/${q.target.member_id}`, "ask", { target: person, text: q.text, thread_id: null, from_run: null }).then(
        (sent) => {
          if (sent) uiStore.getState().navigate({ kind: "person", memberId: q.target.member_id });
        },
      );
    },
    requestWork: (q, a) =>
      uiStore.getState().openSheet({ kind: "work-request", owner: a.owner, agentId: a.agent_id, fromQuestion: q.id }),
  };

  const last = questions[questions.length - 1];
  const threadId = last ? last.thread_id : null;
  const agentName = agent?.name ?? t("ask.unsharedAgent", "Agent");
  const title =
    target.kind === "person" ? ownerName : t("ask.agentTitle", "{{agent}} · {{owner}}", { agent: agentName, owner: ownerName });

  return (
    <section className="conversation" aria-label={title} data-state={viewState(questions, unshared)}>
      <div className="c-head">
        {target.kind === "person" ? (
          <span className="pill">
            <b>{ownerName}</b>
            <span className="st">{t("ask.personNote", "Asking a person · Formation picks the agent")}</span>
          </span>
        ) : (
          <span className="pill">
            <b>{agentName}</b>
            <span className="st">
              {ownerName}
              {agent ? (
                <>
                  {" · "}
                  <span className="status" data-state={agent.state}>
                    {stateLabel(t, agent.state)}
                  </span>
                </>
              ) : null}
            </span>
          </span>
        )}
        <div className="right">
          {agent && agent.accepts_requests && !unshared ? (
            <button
              type="button"
              className="btn"
              onClick={() =>
                uiStore
                  .getState()
                  .openSheet({ kind: "work-request", owner: agent.owner, agentId: agent.agent_id, fromQuestion: null })
              }
            >
              {t("ask.requestWork", "Request work…")}
            </button>
          ) : null}
        </div>
      </div>
      {unshared ? (
        <div className="banner warn" role="status">
          <div className="bt">
            <b>{t("ask.unsharedTitle", "This agent is no longer shared")}</b>
            {t("ask.unsharedBody", "{{owner}} turned sharing off. You can read past messages but can't ask new ones.", {
              owner: ownerName,
            })}
          </div>
          <button
            type="button"
            className="btn"
            onClick={() => uiStore.getState().navigate({ kind: "person", memberId: ownerId })}
          >
            {t("ask.askOwner", "Ask {{owner}}", { owner: ownerName })}
          </button>
        </div>
      ) : null}
      {failure ? (
        <div className="banner warn" role="alert">
          <div className="bt">
            <b>{t("ask.actionFailed", "That didn't go through")}</b>
          </div>
          <button type="button" className="btn" onClick={failure.retry}>
            {t("ask.tryAgain", "Try again")}
          </button>
        </div>
      ) : null}
      <div className="msgs">
        {questions.map((q) => (
          <QuestionItem
            key={q.id}
            question={q}
            ownerName={ownerName}
            ownerAgents={ownerAgents}
            readOnly={unshared}
            pending={questionKey(q) in pending}
            actions={actions}
          />
        ))}
      </div>
      <Composer
        disabled={unshared}
        pending={askKey in pending}
        placeholder={
          unshared
            ? t("ask.composerUnshared", "Sharing is off")
            : target.kind === "person"
              ? t("ask.composerPerson", "Ask {{owner}}", { owner: ownerName })
              : t("ask.composerAgent", "Ask {{agent}}", { agent: agentName })
        }
        onSend={(text) => ask(text, threadId)}
      />
    </section>
  );
}

/** SC-015 "new": choose a person (Formation picks the agent) or a shared agent. */
function NewQuestion() {
  const { t } = useTranslation();
  const team = useAppStore((s) => s.mirror?.team ?? null);
  const teammateAgents = useAppStore((s) => s.mirror?.teammate_agents);
  const people = team ? team.members.filter((m) => m.id !== team.me) : [];
  const nameOf = (id: string) => people.find((m) => m.id === id)?.display_name ?? "";
  const agents = Object.values(teammateAgents ?? {}).flat();
  const navigate = uiStore.getState().navigate;

  return (
    <section className="conversation" aria-label={t("ask.newTitle", "New question")} data-state="new">
      <div className="c-head">
        <span className="pill">{t("ask.newTitle", "New question")}</span>
      </div>
      <div className="msgs">
        <div className="empty">
          <b>{t("ask.whoTitle", "Who do you want to ask?")}</b>
          <p>{t("ask.whoBody", "Pick a person and Formation picks the agent, or pick an agent directly.")}</p>
          <div className="row-actions" role="group" aria-label={t("ask.people", "People")}>
            {people.map((m) => (
              <button
                key={m.id}
                type="button"
                className="btn"
                data-online={m.online}
                onClick={() => navigate({ kind: "person", memberId: m.id })}
              >
                {m.display_name}
              </button>
            ))}
          </div>
          <div className="row-actions" role="group" aria-label={t("ask.sharedAgents", "Shared agents")}>
            {agents.map((a) => (
              <button
                key={`${a.owner}/${a.agent_id}`}
                type="button"
                className="btn"
                onClick={() => navigate({ kind: "teammate-agent", owner: a.owner, agentId: a.agent_id })}
              >
                {t("ask.agentChoice", "{{owner}} · {{agent}}", { owner: nameOf(a.owner), agent: a.name })}
              </button>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}
