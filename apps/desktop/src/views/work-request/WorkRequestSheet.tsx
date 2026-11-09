// SC-016 work request compose, and the not-accepted state when the agent
// does not take work requests.
import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { AgentId } from "../../ipc/generated/AgentId";
import type { MemberId } from "../../ipc/generated/MemberId";
import type { QuestionId } from "../../ipc/generated/QuestionId";
import { tauriTransport } from "../../ipc/transport";
import type { MirrorState } from "../../store/reducer";
import { selectThreadQuestions } from "../../store/selectors";
import { useAppStore } from "../../store/store";
import { act } from "../../store/sync";
import { uiStore } from "../../store/ui";
import Sheet from "../../ui/Sheet";
import { errorCode, memberName } from "../approval/model";
import "./work-request.css";

/** One message of the side conversation that can go with the request. */
type ThreadMessage = { id: string; who: string; text: string };

const NO_MESSAGES: ThreadMessage[] = [];
const messagesCache = new WeakMap<MirrorState, Map<string, ThreadMessage[]>>();

/** The thread's questions and answers, oldest first, labeled with who wrote them. */
function threadMessages(state: MirrorState, questionId: QuestionId, me: string, agent: string): ThreadMessage[] {
  const question = state.questions[questionId];
  if (!question) return NO_MESSAGES;
  let byKey = messagesCache.get(state);
  if (!byKey) {
    byKey = new Map();
    messagesCache.set(state, byKey);
  }
  const key = `${question.thread_id}|${me}|${agent}`;
  const cached = byKey.get(key);
  if (cached) return cached;
  const messages: ThreadMessage[] = [];
  for (const q of selectThreadQuestions(state, question.thread_id)) {
    const asker = q.asker === state.team?.me ? me : (memberName(state, q.asker) ?? q.asker);
    messages.push({ id: `${q.id}:question`, who: asker, text: q.text });
    if (q.answer) messages.push({ id: `${q.id}:answer`, who: agent, text: q.answer.text });
  }
  byKey.set(key, messages);
  return messages;
}

/** The exact prompt delivered: the request, then the chosen messages as context. */
function buildPrompt(text: string, context: readonly ThreadMessage[]): string {
  const request = text.trim();
  if (context.length === 0) return request;
  return `${request}\n\nContext:\n${context.map((m) => `- ${m.who}: ${m.text}`).join("\n")}`;
}

export default function WorkRequestSheet({
  owner,
  agentId,
  fromQuestion,
}: {
  owner: MemberId;
  agentId: AgentId;
  fromQuestion: QuestionId | null;
}) {
  const { t } = useTranslation();
  const ownerName = useAppStore((s) => (s.mirror ? memberName(s.mirror, owner) : null)) ?? t("workRequest.someone", "A teammate");
  const agent = useAppStore((s) => s.mirror?.teammate_agents[owner]?.find((a) => a.agent_id === agentId));
  const agentLabel = agent?.name ?? t("workRequest.unknownAgent", "this agent");
  const meLabel = t("workRequest.me", "Me");
  const messages = useAppStore((s) =>
    s.mirror && fromQuestion ? threadMessages(s.mirror, fromQuestion, meLabel, agentLabel) : NO_MESSAGES,
  );
  const [text, setText] = useState("");
  const [chosen, setChosen] = useState<ReadonlySet<string> | null>(null);
  const [idempotencyKey] = useState(() => crypto.randomUUID());
  const [error, setError] = useState<string | null>(null);
  const [refused, setRefused] = useState(false);
  const pendingKey = `request_create:${idempotencyKey}`;
  const sending = useAppStore((s) => pendingKey in s.pending);

  const close = () => uiStore.getState().closeSheet();
  const onOpenChange = (open: boolean) => {
    if (!open) close();
  };
  // By default the latest answer goes with the request.
  const selected = chosen ?? new Set(messages.filter((m) => m.id.endsWith(":answer")).slice(-1).map((m) => m.id));
  const context = messages.filter((m) => selected.has(m.id));
  const prompt = buildPrompt(text, context);
  const accepts = agent?.accepts_requests === true && !refused;

  const toggle = (id: string) => {
    const next = new Set(selected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    setChosen(next);
  };

  const send = async () => {
    setError(null);
    try {
      const request = await act(tauriTransport, pendingKey, "request_create", {
        owner,
        agent_id: agentId,
        prompt,
        idempotency_key: idempotencyKey,
        from_question: fromQuestion,
        from_run: null,
      });
      uiStore.getState().openSheet({ kind: "request-status", requestId: request.id });
    } catch (e) {
      switch (errorCode(e)) {
        case "requests_not_accepted":
          setRefused(true);
          break;
        case "agent_not_shared":
          setError(t("workRequest.error.notShared", "{{agent}} is no longer shared with you.", { agent: agentLabel }));
          break;
        case "peer_offline":
          setError(t("workRequest.error.offline", "{{owner}}'s Mac is offline. Try again when they're back.", { owner: ownerName }));
          break;
        case "rate_limited":
          setError(t("workRequest.error.rateLimited", "Too many requests right now. Try again in a moment."));
          break;
        default:
          setError(t("workRequest.error.other", "The request wasn't sent. Try again."));
      }
    }
  };

  const title = t("workRequest.title", "Request work");

  if (!accepts) {
    return (
      <Sheet
        open
        onOpenChange={onOpenChange}
        title={title}
        footer={
          <>
            <button type="button" className="btn" onClick={close}>
              {t("common.close", "Close")}
            </button>
            <button type="button" className="btn primary" disabled>
              {t("workRequest.send", "Send")}
            </button>
          </>
        }
      >
        <div className="banner" role="status" data-state="not-accepted">
          <div className="bt">
            <b>
              {agent
                ? t("workRequest.notAccepted.title", "{{agent}} doesn't accept work requests", { agent: agentLabel })
                : t("workRequest.notShared.title", "This agent isn't shared with you")}
            </b>
            {agent
              ? t(
                  "workRequest.notAccepted.body",
                  "{{owner}} turned off work requests for this agent. You can still ask questions.",
                  { owner: ownerName },
                )
              : t("workRequest.notShared.body", "{{owner}} no longer shares it.", { owner: ownerName })}
          </div>
        </div>
      </Sheet>
    );
  }

  return (
    <Sheet
      open
      onOpenChange={onOpenChange}
      title={title}
      lead={t("workRequest.to", "To: {{owner}}'s {{agent}} agent · accepts work requests", {
        owner: ownerName,
        agent: agentLabel,
      })}
      footer={
        <>
          <button type="button" className="btn" onClick={close}>
            {t("common.cancel", "Cancel")}
          </button>
          <button
            type="button"
            className="btn primary"
            disabled={sending || text.trim() === ""}
            onClick={() => void send()}
          >
            {t("workRequest.send", "Send")}
          </button>
        </>
      }
    >
      <div data-state="compose" aria-busy={sending}>
        <label className="field">
          <span>{t("workRequest.request", "Request")}</span>
          <textarea className="textarea" value={text} onChange={(e) => setText(e.target.value)} />
        </label>

        {messages.length > 0 ? (
          <fieldset className="field">
            <legend>{t("workRequest.include", "Include messages from the conversation")}</legend>
            {messages.map((m) => (
              <label key={m.id} className="check">
                <input type="checkbox" checked={selected.has(m.id)} onChange={() => toggle(m.id)} /> {m.who}: {m.text}
              </label>
            ))}
          </fieldset>
        ) : null}

        <div className="field">
          <span id="work-request-preview">{t("workRequest.preview", "Exact prompt to be delivered")}</span>
          <pre className="code" aria-labelledby="work-request-preview">
            {prompt}
          </pre>
          <small>
            {t("workRequest.note", "Runs after the current turn; actions follow {{owner}}'s approval rules.", {
              owner: ownerName,
            })}
          </small>
        </div>

        {error ? (
          <div className="banner warn" role="alert">
            <div className="bt">{error}</div>
          </div>
        ) : null}
      </div>
    </Sheet>
  );
}
