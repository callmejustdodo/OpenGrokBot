// One question of an asking conversation (SC-015): my bubble, then what came
// back: the answer, a working line, a routing choice, or a waiting, sent or error card.
import { useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { Answer } from "../../ipc/generated/Answer";
import type { Question } from "../../ipc/generated/Question";
import type { RouteChoice } from "../../ipc/generated/RouteChoice";
import type { SharedAgentView } from "../../ipc/generated/SharedAgentView";
import { formatTime } from "./format";

export type QuestionActions = {
  refresh: (question: Question) => void;
  routeChoose: (question: Question, choice: RouteChoice) => void;
  cancel: (question: Question) => void;
  /** Sends the same text to the same target again (Retry, Ask again). */
  askAgain: (question: Question) => void;
  askOwner: (question: Question) => void;
  requestWork: (question: Question, agent: SharedAgentView) => void;
};

export type QuestionItemProps = {
  question: Question;
  ownerName: string;
  /** The owner's agents currently shared with me. */
  ownerAgents: readonly SharedAgentView[];
  /** No longer shared: past messages stay readable, actions are off. */
  readOnly: boolean;
  pending: boolean;
  actions: QuestionActions;
};

function agentName(question: Question, agentId: string | null, ownerAgents: readonly SharedAgentView[]): string | null {
  if (agentId === null) return null;
  return (
    ownerAgents.find((a) => a.agent_id === agentId)?.name ??
    question.candidates.find((c) => c.agent_id === agentId)?.name ??
    null
  );
}

export default function QuestionItem(props: QuestionItemProps) {
  const { question, ownerName, ownerAgents, readOnly, pending, actions } = props;
  const { t, i18n } = useTranslation();
  const name = agentName(question, question.agent_id, ownerAgents);

  let reply: ReactNode = null;
  switch (question.status) {
    case "received":
      reply = (
        <div className="working" role="status">
          {t("ask.received", "Received · preparing an answer")}
        </div>
      );
      break;
    case "answering":
      reply = (
        <div className="working" role="status">
          {name
            ? t("ask.answeringAgent", "The {{agent}} agent is answering… {{owner}}'s work isn't interrupted", {
                agent: name,
                owner: ownerName,
              })
            : t("ask.answering", "Answering… {{owner}}'s work isn't interrupted", { owner: ownerName })}
        </div>
      );
      break;
    case "unsure":
      reply = (
        <ChoiceCard
          question={question}
          ownerName={ownerName}
          options={[...question.candidates]
            .sort((a, b) => b.score - a.score)
            .map((c) => ({ agentId: c.agent_id, name: c.name, detail: c.reason || c.role }))}
          title={t("ask.unsureTitle", "Not sure which agent should answer")}
          disabled={readOnly || pending}
          onChoose={(choice) => actions.routeChoose(question, choice)}
        />
      );
      break;
    case "waiting_offline":
      reply = (
        <div className="icard" data-state="offline-waiting">
          <div className="h">
            <span>{t("ask.offlineTitle", "{{owner}}'s Mac is offline", { owner: ownerName })}</span>
          </div>
          <p>
            {t(
              "ask.offlineBody",
              "When {{owner}} is back, Formation re-checks sharing and permissions, answers, and notifies you.",
              { owner: ownerName },
            )}
          </p>
          <div className="row-actions">
            <button type="button" className="btn" disabled={pending} onClick={() => actions.cancel(question)}>
              {t("ask.cancelQuestion", "Cancel question")}
            </button>
            <button type="button" className="btn" disabled={readOnly || pending} onClick={() => actions.askAgain(question)}>
              {t("ask.askAgain", "Ask again")}
            </button>
          </div>
        </div>
      );
      break;
    case "sent_to_owner":
      reply = (
        <div className="icard" data-state="sent-to-owner">
          <div className="h">
            <span>{t("ask.sentTitle", "Sent to {{owner}}", { owner: ownerName })}</span>
          </div>
          <p>
            {t("ask.sentBody", "When {{owner}} answers or hands it to an agent, it shows up here.", {
              owner: ownerName,
            })}
          </p>
        </div>
      );
      break;
    case "failed":
      reply = (
        <div className="icard" data-state="error" role="alert">
          <div className="h">
            <span>{t("ask.errorTitle", "Couldn't get an answer")}</span>
          </div>
          <p>{failureText(t, question.error?.code ?? null, ownerName)}</p>
          <button
            type="button"
            className="btn primary"
            disabled={readOnly || pending}
            onClick={() => actions.askAgain(question)}
          >
            {t("ask.retry", "Retry")}
          </button>
        </div>
      );
      break;
    case "canceled":
      reply = <p className="muted small">{t("ask.canceled", "You canceled this question.")}</p>;
      break;
    case "answered":
      if (question.answer) {
        reply = (
          <AnswerBubble
            question={question}
            answer={question.answer}
            ownerName={ownerName}
            ownerAgents={ownerAgents}
            readOnly={readOnly}
            pending={pending}
            actions={actions}
          />
        );
      }
      break;
  }

  return (
    <article className="q" data-state={question.status} aria-label={question.text}>
      <time className="sep-t" dateTime={question.created_at}>
        {formatTime(question.created_at, i18n.language)}
      </time>
      <div className="me">{question.text}</div>
      {reply}
    </article>
  );
}

function failureText(t: ReturnType<typeof useTranslation>["t"], code: string | null, ownerName: string): string {
  switch (code) {
    case "agent_not_shared":
      return t("ask.errorNotShared", "This agent is no longer shared.");
    case "not_member":
      return t("ask.errorNotMember", "You're no longer on {{owner}}'s team.", { owner: ownerName });
    case "rate_limited":
      return t("ask.errorRateLimited", "Too many questions right now. Try again in a moment.");
    case "harness_unavailable":
      return t("ask.errorHarness", "{{owner}}'s agent can't run right now.", { owner: ownerName });
    default:
      return t("ask.errorDropped", "The connection dropped. Your question is saved.");
  }
}

type ChoiceOption = { agentId: string; name: string; detail: string };

function ChoiceCard(props: {
  question: Question;
  ownerName: string;
  title: string;
  options: ChoiceOption[];
  disabled: boolean;
  onChoose: (choice: RouteChoice) => void;
}) {
  const { ownerName, title, options, disabled, onChoose } = props;
  const { t } = useTranslation();
  return (
    <div className="icard choice" role="group" aria-label={title} data-state="unsure">
      <div className="h">
        <span>{title}</span>
      </div>
      <p>{t("ask.unsureBody", "Pick one and it will answer.")}</p>
      {options.map((option, index) => (
        <button
          key={option.agentId}
          type="button"
          className={index === 0 ? "opt best" : "opt"}
          disabled={disabled}
          onClick={() => onChoose({ kind: "agent", agent_id: option.agentId })}
        >
          <span>
            {option.name}
            <small>{option.detail}</small>
          </span>
          {index === 0 ? <span className="tagpill">{t("ask.bestGuess", "Best guess")}</span> : null}
        </button>
      ))}
      <button type="button" className="opt" disabled={disabled} onClick={() => onChoose({ kind: "owner" })}>
        <span>
          {t("ask.askOwnerDirectly", "Ask {{owner}} directly", { owner: ownerName })}
          <small>{t("ask.ownerInbox", "Goes to {{owner}}'s Inbox", { owner: ownerName })}</small>
        </span>
      </button>
    </div>
  );
}

function AnswerBubble(props: {
  question: Question;
  answer: Answer;
  ownerName: string;
  ownerAgents: readonly SharedAgentView[];
  readOnly: boolean;
  pending: boolean;
  actions: QuestionActions;
}) {
  const { question, answer, ownerName, ownerAgents, readOnly, pending, actions } = props;
  const { t, i18n } = useTranslation();
  const [choosing, setChoosing] = useState(false);
  const answeredBy = agentName(question, answer.agent_id, ownerAgents);
  const asOf = formatTime(answer.as_of, i18n.language);
  const others = ownerAgents.filter((a) => a.agent_id !== answer.agent_id);
  const requestAgent = ownerAgents.find((a) => a.agent_id === answer.agent_id && a.accepts_requests) ?? null;
  const off = readOnly || pending;

  return (
    <>
      <div className="ag" data-state={answer.possibly_stale ? "stale" : "answered"}>
        <p>{answer.text}</p>
        <div className="src">
          <span>
            {answer.agent_id === null
              ? t("ask.sourceOwner", "{{owner}} · As of {{time}}", { owner: ownerName, time: asOf })
              : t("ask.source", "{{owner}}'s {{agent}} agent · As of {{time}}", {
                  owner: ownerName,
                  agent: answeredBy ?? t("ask.unknownAgent", "shared"),
                  time: asOf,
                })}
          </span>
          {answer.possibly_stale ? (
            <span className="stale">{t("ask.possiblyStale", "Possibly out of date")}</span>
          ) : null}
        </div>
        {answer.excerpts.length > 0 ? (
          <details className="ex">
            <summary>
              {answer.excerpts.length === 1
                ? t("ask.excerptsOne", "1 supporting excerpt")
                : t("ask.excerptsMany", "{{count}} supporting excerpts", { count: answer.excerpts.length })}
            </summary>
            {answer.excerpts.map((excerpt, index) => (
              <blockquote key={index} data-role={excerpt.role}>
                [{formatTime(excerpt.at, i18n.language)}]{" "}
                {excerpt.role === "user" ? ownerName : t("ask.excerptAgent", "Agent")}: {excerpt.text}
              </blockquote>
            ))}
          </details>
        ) : null}
      </div>
      <div className="acts">
        <button type="button" className="btn plain" disabled={off} onClick={() => actions.refresh(question)}>
          {t("ask.refresh", "Refresh")}
        </button>
        {others.length > 0 ? (
          <button
            type="button"
            className="btn plain"
            disabled={off}
            aria-expanded={choosing}
            onClick={() => setChoosing((c) => !c)}
          >
            {t("ask.askAnotherAgent", "Ask another agent")}
          </button>
        ) : null}
        <button type="button" className="btn plain" disabled={off} onClick={() => actions.askOwner(question)}>
          {t("ask.askOwner", "Ask {{owner}}", { owner: ownerName })}
        </button>
        {requestAgent ? (
          <button
            type="button"
            className="btn plain"
            disabled={readOnly}
            onClick={() => actions.requestWork(question, requestAgent)}
          >
            {t("ask.requestWork", "Request work…")}
          </button>
        ) : null}
      </div>
      {choosing && !off ? (
        <ChoiceCard
          question={question}
          ownerName={ownerName}
          title={t("ask.anotherTitle", "Which agent should answer instead?")}
          options={others.map((a) => ({ agentId: a.agent_id, name: a.name, detail: a.role }))}
          disabled={off}
          onChoose={(choice) => {
            setChoosing(false);
            actions.routeChoose(question, choice);
          }}
        />
      ) : null}
    </>
  );
}
