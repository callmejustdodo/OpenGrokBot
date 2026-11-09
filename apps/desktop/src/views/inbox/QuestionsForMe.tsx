// SC-010 "Questions sent to me" (US-010): questions Formation could not route,
// which the asker chose to send to me. I answer them myself or hand them to one
// of my shared agents. Renders nothing when there are none; the Inbox shows its
// own empty state.
import { useMemo, useState, type KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";
import type { AgentId } from "../../ipc/generated/AgentId";
import type { Question } from "../../ipc/generated/Question";
import type { CallError } from "../../ipc/client";
import { tauriTransport } from "../../ipc/transport";
import type { MirrorState } from "../../store/reducer";
import { useAppStore } from "../../store/store";
import { act } from "../../store/sync";

/**
 * Questions asked of me in person and still waiting for me, oldest first: each
 * has an unresolved "question for you" Inbox item, so resolving the item
 * (answered elsewhere, canceled) removes the row too.
 */
function questionsForMe(mirror: MirrorState): Question[] {
  const me = mirror.team?.me;
  if (!me) return [];
  return Object.values(mirror.inbox)
    .filter((item) => item.kind === "question_for_you")
    .sort((a, b) => Date.parse(a.at) - Date.parse(b.at))
    .flatMap((item) => {
      const question = item.target.kind === "question" ? mirror.questions[item.target.question_id] : undefined;
      return question &&
        question.status === "sent_to_owner" &&
        question.target.kind === "person" &&
        question.target.member_id === me
        ? [question]
        : [];
    });
}

function errorCode(error: unknown): string | null {
  const e = error as Partial<CallError> | null;
  return e?.data?.code ?? e?.code ?? null;
}

type Mode = { kind: "idle" } | { kind: "answer"; text: string } | { kind: "handoff"; agentId: AgentId | "" };

function QuestionRow({ question, asker, agents }: {
  question: Question;
  asker: string;
  agents: { id: AgentId; name: string }[];
}) {
  const { t } = useTranslation();
  const answering = useAppStore((s) => `question_answer:${question.id}` in s.pending);
  const handingOff = useAppStore((s) => `question_handoff:${question.id}` in s.pending);
  const [mode, setMode] = useState<Mode>({ kind: "idle" });
  const [error, setError] = useState<string | null>(null);
  const busy = answering || handingOff;

  const run = async (call: () => Promise<unknown>) => {
    setError(null);
    try {
      await call();
      setMode({ kind: "idle" });
    } catch (e) {
      setError(errorCode(e));
    }
  };

  const sendAnswer = () => {
    if (mode.kind !== "answer" || !mode.text.trim() || busy) return;
    void run(() =>
      act(tauriTransport, `question_answer:${question.id}`, "question_answer", {
        question_id: question.id,
        text: mode.text.trim(),
      }),
    );
  };

  const handOff = () => {
    if (mode.kind !== "handoff" || !mode.agentId || busy) return;
    const agentId = mode.agentId;
    void run(() =>
      act(tauriTransport, `question_handoff:${question.id}`, "question_handoff", {
        question_id: question.id,
        agent_id: agentId,
      }),
    );
  };

  const onAnswerKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && e.metaKey) {
      e.preventDefault();
      sendAnswer();
    } else if (e.key === "Escape") {
      e.preventDefault();
      setMode({ kind: "idle" });
    }
  };

  return (
    <li className="row" data-state={busy ? "pending" : mode.kind}>
      <span className="t">
        <b>{asker}</b> · “{question.text}”
        <small>
          {t("inbox.questions.why", "Formation wasn't sure which agent should answer; {{asker}} chose to ask you", {
            asker,
          })}
        </small>
      </span>
      {mode.kind === "idle" ? (
        <>
          <button type="button" className="btn" onClick={() => setMode({ kind: "answer", text: "" })}>
            {t("inbox.questions.answer", "Answer")}
          </button>
          <button
            type="button"
            className="btn"
            disabled={agents.length === 0}
            title={agents.length === 0 ? t("inbox.questions.noAgents", "None of your agents is shared") : undefined}
            onClick={() => setMode({ kind: "handoff", agentId: agents.length === 1 ? agents[0].id : "" })}
          >
            {t("inbox.questions.handOff", "Hand to agent")}
          </button>
        </>
      ) : null}
      {mode.kind === "answer" ? (
        <form
          className="composer"
          onSubmit={(e) => {
            e.preventDefault();
            sendAnswer();
          }}
        >
          <textarea
            autoFocus
            value={mode.text}
            onChange={(e) => setMode({ kind: "answer", text: e.target.value })}
            onKeyDown={onAnswerKey}
            aria-label={t("inbox.questions.answerTo", "Answer to {{asker}}", { asker })}
            disabled={busy}
          />
          <button type="button" className="btn" onClick={() => setMode({ kind: "idle" })} disabled={busy}>
            {t("inbox.questions.cancel", "Cancel")}
          </button>
          <button type="submit" className="btn primary" disabled={busy || !mode.text.trim()}>
            {answering ? t("inbox.questions.sending", "Sending…") : t("inbox.questions.send", "Send answer")}
          </button>
        </form>
      ) : null}
      {mode.kind === "handoff" ? (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            handOff();
          }}
        >
          <select
            autoFocus
            value={mode.agentId}
            onChange={(e) => setMode({ kind: "handoff", agentId: e.target.value })}
            aria-label={t("inbox.questions.pickAgent", "Agent to answer")}
            disabled={busy}
          >
            <option value="" disabled>
              {t("inbox.questions.pickAgentPlaceholder", "Choose an agent")}
            </option>
            {agents.map((agent) => (
              <option key={agent.id} value={agent.id}>
                {agent.name}
              </option>
            ))}
          </select>
          <button type="button" className="btn" onClick={() => setMode({ kind: "idle" })} disabled={busy}>
            {t("inbox.questions.cancel", "Cancel")}
          </button>
          <button type="submit" className="btn primary" disabled={busy || !mode.agentId}>
            {handingOff ? t("inbox.questions.handingOff", "Handing off…") : t("inbox.questions.handOffConfirm", "Hand off")}
          </button>
        </form>
      ) : null}
      {error ? (
        <p role="alert" className="small">
          {error === "not_found"
            ? t("inbox.questions.gone", "This question is no longer waiting for you.")
            : error === "agent_not_shared"
              ? t("inbox.questions.notShared", "That agent isn't shared any more. Pick another.")
              : t("inbox.questions.failed", "Couldn't send. Try again.")}
        </p>
      ) : null}
    </li>
  );
}

export default function QuestionsForMe() {
  const { t } = useTranslation();
  const mirror = useAppStore((s) => s.mirror);
  const questions = useMemo(() => (mirror ? questionsForMe(mirror) : []), [mirror]);
  const agents = useMemo(
    () =>
      mirror
        ? Object.values(mirror.agents)
            .filter((agent) => mirror.shares[agent.id]?.shared)
            .map((agent) => ({ id: agent.id, name: agent.name }))
        : [],
    [mirror],
  );

  if (!mirror || questions.length === 0) return null;

  const nameOf = (id: string) => mirror.team?.members.find((m) => m.id === id)?.display_name ?? id;

  return (
    <section aria-labelledby="questions-for-me">
      <h2 className="grp" id="questions-for-me">
        {t("inbox.questions.title", "Questions sent to me")}
      </h2>
      <ul className="list">
        {questions.map((question) => (
          <QuestionRow key={question.id} question={question} asker={nameOf(question.asker)} agents={agents} />
        ))}
      </ul>
    </section>
  );
}
