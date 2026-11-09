// SC-005 message list: my bubbles on the right, the agent's on the left, steps
// grouped into rows collapsed by default (a ▸ button expands them), cards for teammate requests and permission
// prompts, notices, and a date separator whenever the day changes.
import type { TFunction } from "i18next";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import i18n from "../../i18n";
import PermissionCard from "../../components/cards/PermissionCard";
import RequestCard from "../../components/cards/RequestCard";
import { stepLine } from "../../components/sidebar/labels";
import type { AgentId } from "../../ipc/generated/AgentId";
import type { ConversationItem } from "../../ipc/generated/ConversationItem";
import type { MirrorState } from "../../store/reducer";
import { useAppStore } from "../../store/store";

const NO_APPROVALS: MirrorState["approvals"] = {};

type StepItem = Extract<ConversationItem, { kind: "step" }>;
type Block = { kind: "item"; item: ConversationItem } | { kind: "steps"; id: string; steps: StepItem[] };

function blocks(items: readonly ConversationItem[]): Block[] {
  const out: Block[] = [];
  for (const item of items) {
    const last = out[out.length - 1];
    if (item.kind === "step") {
      if (last?.kind === "steps") last.steps.push(item);
      else out.push({ kind: "steps", id: item.id, steps: [item] });
    } else {
      out.push({ kind: "item", item });
    }
  }
  return out;
}

function firstAt(block: Block): string {
  return block.kind === "item" ? block.item.at : block.steps[0].at;
}

/** "4 steps · 3 files edited, 1 command run" */
function stepSummary(t: TFunction, steps: StepItem[]): string {
  const edits = steps.filter((s) => s.step.kind === "edit").length;
  const commands = steps.filter((s) => s.step.kind === "command").length;
  const parts: string[] = [];
  if (edits > 0) parts.push(t("conversation.steps.edits", { count: edits, defaultValue_one: "{{count}} file edited", defaultValue_other: "{{count}} files edited" }));
  if (commands > 0) parts.push(t("conversation.steps.commands", { count: commands, defaultValue_one: "{{count}} command run", defaultValue_other: "{{count}} commands run" }));
  const head = t("conversation.steps.count", { count: steps.length, defaultValue_one: "{{count}} step", defaultValue_other: "{{count}} steps" });
  return parts.length > 0 ? `${head} · ${parts.join(", ")}` : head;
}

function StepGroup({ id, steps }: { id: string; steps: StepItem[] }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const running = steps.some((s) => s.step.status === "running");
  const listId = `steps-${id}`;
  return (
    <div className="steps" data-open={open} data-running={running}>
      <button type="button" className="steps-toggle" aria-expanded={open} aria-controls={listId} onClick={() => setOpen((o) => !o)}>
        <span className="chev" aria-hidden="true" />
        {stepSummary(t, steps)}
      </button>
      <div id={listId} role="list" hidden={!open}>
        {steps.map(({ id: stepId, step }) => (
          <div key={stepId} role="listitem" className="step" data-kind={step.kind} data-status={step.status}>
            <span className="mono">{stepLine(t, step)}</span>
            {step.detail ? <pre className="detail">{step.detail}</pre> : null}
          </div>
        ))}
      </div>
    </div>
  );
}

function Item({ item }: { item: ConversationItem }) {
  const { t } = useTranslation();
  switch (item.kind) {
    case "user_message":
      return (
        <div className="me" data-author={item.author.kind}>
          {item.author.kind === "schedule" ? (
            <small className="tag">{t("conversation.scheduled", "Scheduled")}</small>
          ) : null}
          {item.text}
        </div>
      );
    case "assistant_text":
      return <div className="ag">{item.text}</div>;
    case "request":
      return <RequestCard requestId={item.request_id} />;
    case "approval":
      return <PermissionCard approvalId={item.approval_id} />;
    case "notice":
      return (
        <div className="notice" data-level={item.level} role={item.level === "error" ? "alert" : undefined}>
          {item.text}
        </div>
      );
    case "step":
      return null;
  }
}

/** Blocks with a date separator before the first block of each day. */
function withSeparators(items: readonly ConversationItem[]): { block: Block; separator: string | null }[] {
  let previousDay = "";
  return blocks(items).map((block) => {
    const at = firstAt(block);
    const day = new Date(at).toDateString();
    const separator = day !== previousDay ? at : null;
    previousDay = day;
    return { block, separator };
  });
}

/**
 * A teammate's approval belongs to a request whose card in this conversation
 * already shows "N approvals waiting" with Review (SC-005 approval), so its own
 * approval item would repeat that Review; only approvals without such a card show.
 */
function withoutCoveredApprovals(
  items: readonly ConversationItem[],
  approvals: MirrorState["approvals"],
): readonly ConversationItem[] {
  const requests = new Set(items.flatMap((i) => (i.kind === "request" ? [i.request_id] : [])));
  if (requests.size === 0) return items;
  return items.filter((i) => {
    if (i.kind !== "approval") return true;
    const requestId = approvals[i.approval_id]?.request_id;
    return !(requestId && requests.has(requestId));
  });
}

/**
 * The gate raises the owner's own permission prompts as approvals, but no
 * harness adds an approval item for them, so each of this agent's owner
 * approvals without an item joins the conversation at the time it was raised
 * (FR-022, proposed default 11): a pending one always, a decided one while that
 * time is within the loaded items, so its result stays where it was asked.
 */
function withOwnerApprovals(
  items: readonly ConversationItem[],
  approvals: MirrorState["approvals"],
  agentId: AgentId,
): readonly ConversationItem[] {
  const listed = new Set(items.flatMap((i) => (i.kind === "approval" ? [i.approval_id] : [])));
  const since = items.length > 0 ? Date.parse(items[0].at) : Infinity;
  const extra = Object.values(approvals)
    .filter(
      (a) =>
        a.agent_id === agentId &&
        a.request_id === null &&
        !listed.has(a.id) &&
        (a.status === "pending" || Date.parse(a.created_at) >= since),
    )
    .sort((a, b) => Date.parse(a.created_at) - Date.parse(b.created_at))
    .map((a): ConversationItem => ({ kind: "approval", id: `approval-${a.id}`, at: a.created_at, approval_id: a.id }));
  if (extra.length === 0) return items;
  const out: ConversationItem[] = [];
  let next = 0;
  for (const item of items) {
    while (next < extra.length && Date.parse(extra[next].at) < Date.parse(item.at)) out.push(extra[next++]);
    out.push(item);
  }
  return out.concat(extra.slice(next));
}

export default function Messages({ agentId, items }: { agentId: AgentId; items: readonly ConversationItem[] }) {
  const approvals = useAppStore((s) => s.mirror?.approvals ?? NO_APPROVALS);
  return (
    <>
      {withSeparators(withoutCoveredApprovals(withOwnerApprovals(items, approvals, agentId), approvals)).map(({ block, separator }) => (
        <div key={block.kind === "item" ? block.item.id : block.id} className="block">
          {separator ? (
            <div className="sep-t">
              <time dateTime={separator}>
                {new Date(separator).toLocaleString(i18n.language, { dateStyle: "medium", timeStyle: "short" })}
              </time>
            </div>
          ) : null}
          {block.kind === "steps" ? <StepGroup id={block.id} steps={block.steps} /> : <Item item={block.item} />}
        </div>
      ))}
    </>
  );
}
