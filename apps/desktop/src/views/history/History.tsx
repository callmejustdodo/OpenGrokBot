// SC-017 history: this Mac's questions, requests, approval decisions and results,
// with filters and delete (FR-013). History is not mirrored; it loads with
// `history_list` each time the page opens.
import * as ToggleGroup from "@radix-ui/react-toggle-group";
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import "../team/team.css";
import "./history.css";
import type { Activity } from "../../ipc/generated/Activity";
import type { ActivityId } from "../../ipc/generated/ActivityId";
import type { CallError } from "../../ipc/client";
import { tauriTransport } from "../../ipc/transport";
import { useAppStore } from "../../store/store";
import { act } from "../../store/sync";
import Alert, { AlertCancel } from "../../ui/Alert";
import {
  actorOf,
  deletableQuestion,
  humanize,
  memberName,
  statusOf,
  subjectOf,
  TAG_OF,
  type HistoryFilter,
  type HistoryTag,
} from "./describe";

/** How many entries one page of history asks for. */
const HISTORY_LIMIT = 200;

type Load =
  | { status: "loading" }
  | { status: "ready"; items: Activity[] }
  | { status: "error"; reason: string | null };

const FILTERS: HistoryFilter[] = ["all", "question", "request", "approval", "result"];

function errorReason(error: unknown): string | null {
  const e = error as Partial<CallError> | null;
  return e?.data?.code ?? e?.code ?? null;
}

function newestFirst(a: Activity, b: Activity): number {
  return Date.parse(b.at) - Date.parse(a.at);
}

const timeFormat = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
});

export default function History() {
  const { t } = useTranslation();
  const mirror = useAppStore((s) => s.mirror);
  const deleting = useAppStore((s) =>
    Object.keys(s.pending).some((key) => key === "history_delete" || key.startsWith("question_delete:")),
  );
  const [load, setLoad] = useState<Load>({ status: "loading" });
  const [attempt, setAttempt] = useState(0);
  const [filter, setFilter] = useState<HistoryFilter>("all");
  const [search, setSearch] = useState("");
  const [selected, setSelected] = useState<ReadonlySet<ActivityId>>(new Set());
  /** The entries the delete confirmation is about; `null` when it is closed. */
  const [confirming, setConfirming] = useState<ActivityId[] | null>(null);
  const [deleteError, setDeleteError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    act(tauriTransport, "history_list", "history_list", {
      agent_id: null,
      member_id: null,
      before: null,
      limit: HISTORY_LIMIT,
    }).then(
      (items) => live && setLoad({ status: "ready", items: [...items].sort(newestFirst) }),
      (error: unknown) => live && setLoad({ status: "error", reason: errorReason(error) }),
    );
    return () => {
      live = false;
    };
  }, [attempt]);

  const tagLabel = (tag: HistoryTag) =>
    ({
      question: t("history.tag.question", "Question"),
      request: t("history.tag.request", "Request"),
      approval: t("history.tag.approval", "Approval"),
      result: t("history.tag.result", "Result"),
      agent: t("history.tag.agent", "Agent"),
    })[tag];
  const filterLabel = (f: HistoryFilter) =>
    ({
      all: t("history.filter.all", "All"),
      question: t("history.filter.question", "Questions"),
      request: t("history.filter.request", "Requests"),
      approval: t("history.filter.approval", "Approvals"),
      result: t("history.filter.result", "Results"),
    })[f];
  const meLabel = t("history.me", "Me");

  const rows = useMemo(() => {
    if (load.status !== "ready" || !mirror) return [];
    return load.items.map((activity) => {
      const actor = actorOf(mirror, activity);
      const status = statusOf(mirror, activity);
      return {
        activity,
        tag: TAG_OF[activity.kind],
        who: actor === null ? meLabel : (memberName(mirror, actor) ?? actor),
        whom: subjectOf(mirror, activity),
        status: status ? t(`history.status.${status}`, humanize(status)) : null,
        question: deletableQuestion(mirror, activity),
      };
    });
  }, [load, mirror, meLabel, t]);

  const query = search.trim().toLowerCase();
  const visible = rows.filter((row) => {
    if (filter !== "all" && row.tag !== filter) return false;
    if (!query) return true;
    return [row.activity.summary, row.who, row.whom, row.status, tagLabel(row.tag)].some((text) =>
      text?.toLowerCase().includes(query),
    );
  });

  const toggle = (id: ActivityId) =>
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const openConfirm = (ids: ActivityId[]) => {
    setDeleteError(null);
    setConfirming(ids);
  };

  const confirmRows = confirming ? rows.filter((row) => confirming.includes(row.activity.id)) : [];
  const confirmQuestions = [...new Set(confirmRows.flatMap((row) => (row.question ? [row.question.id] : [])))];

  const remove = async (withQuestions: boolean) => {
    if (!confirming) return;
    const ids = confirming;
    try {
      if (withQuestions) {
        for (const questionId of confirmQuestions) {
          await act(tauriTransport, `question_delete:${questionId}`, "question_delete", { question_id: questionId });
        }
      }
      await act(tauriTransport, "history_delete", "history_delete", { ids });
      setLoad((current) =>
        current.status === "ready"
          ? { status: "ready", items: current.items.filter((item) => !ids.includes(item.id)) }
          : current,
      );
      setSelected((current) => new Set([...current].filter((id) => !ids.includes(id))));
      setConfirming(null);
    } catch (error) {
      setDeleteError(errorReason(error));
    }
  };

  const filters = (
    <div className="history-filters">
      <ToggleGroup.Root
        type="single"
        className="seg"
        value={filter}
        onValueChange={(value) => value && setFilter(value as HistoryFilter)}
        aria-label={t("history.filterLabel", "Show")}
      >
        {FILTERS.map((f) => (
          <ToggleGroup.Item key={f} value={f}>
            {filterLabel(f)}
          </ToggleGroup.Item>
        ))}
      </ToggleGroup.Root>
      <input
        type="search"
        className="input"
        value={search}
        onChange={(e) => setSearch(e.target.value)}
        placeholder={t("history.searchPlaceholder", "Search")}
        aria-label={t("history.search", "Search history")}
      />
    </div>
  );

  let body;
  if (load.status === "loading") {
    body = (
      <p role="status" className="muted">
        {t("history.loading", "Loading history…")}
      </p>
    );
  } else if (load.status === "error") {
    body = (
      <div className="banner" role="alert">
        <div className="bt">
          <b>{t("history.error.title", "Couldn't load history")}</b>
          {load.reason === "disconnected"
            ? t("history.error.disconnected", "Formation isn't reachable right now. Try again in a moment.")
            : t("history.error.other", "Something went wrong while loading. Try again.")}
        </div>
        <button
          type="button"
          className="btn"
          onClick={() => {
            setLoad({ status: "loading" });
            setAttempt((n) => n + 1);
          }}
        >
          {t("history.retry", "Try again")}
        </button>
      </div>
    );
  } else if (load.items.length === 0) {
    body = (
      <div className="empty">
        <b>{t("history.empty.title", "No history yet")}</b>
        {t("history.empty.body", "Questions, requests, approvals and results collect here.")}
      </div>
    );
  } else if (visible.length === 0) {
    body = (
      <div className="empty">
        <b>{t("history.noMatches", "Nothing matches this filter")}</b>
      </div>
    );
  } else {
    body = (
      <ul className="list">
        {visible.map(({ activity, tag, who, whom, status }) => (
          <li key={activity.id} className="row" data-kind={activity.kind} data-selected={selected.has(activity.id)}>
            <input
              type="checkbox"
              checked={selected.has(activity.id)}
              onChange={() => toggle(activity.id)}
              aria-label={t("history.select", "Select {{summary}}", { summary: activity.summary })}
            />
            <time className="when" dateTime={activity.at}>
              {timeFormat.format(new Date(activity.at))}
            </time>
            <span className="tag">{tagLabel(tag)}</span>
            <span className="t">
              <b>{activity.summary}</b>
              <small>
                {whom ? `${who} → ${whom}` : who}
                {status ? ` · ${status}` : ""}
              </small>
            </span>
            <button
              type="button"
              className="ticon"
              onClick={() => openConfirm([activity.id])}
              aria-label={t("history.deleteOne", "Delete {{summary}}", { summary: activity.summary })}
            >
              ×
            </button>
          </li>
        ))}
      </ul>
    );
  }


  return (
    <section className="page" aria-label={t("history.title", "History")} data-state={load.status}>
      <div className="h1row">
        <h1>{t("history.title", "History")}</h1>
        <span className="spacer" />
        {selected.size > 0 ? (
          <button type="button" className="btn destructive" onClick={() => openConfirm([...selected])}>
            {t("history.deleteSelected", "Delete {{count}} selected", { count: selected.size })}
          </button>
        ) : null}
        {filters}
      </div>
      {body}
      <Alert
        open={confirming !== null}
        onOpenChange={(open) => !open && !deleting && setConfirming(null)}
        title={
          confirmRows.length === 1
            ? t("history.confirm.oneTitle", "Delete this entry?")
            : t("history.confirm.manyTitle", "Delete {{count}} entries?", { count: confirmRows.length })
        }
        description={t(
          "history.confirm.body",
          "They're removed from this Mac's history only; teammates keep their own copies. This can't be undone.",
        )}
      >
        {deleteError ? (
          <p role="alert">
            {deleteError === "not_found"
              ? t("history.confirm.notFound", "Some of these were already deleted.")
              : t("history.confirm.failed", "Couldn't delete. Try again.")}
          </p>
        ) : null}
        <button type="button" className="btn destructive" disabled={deleting} onClick={() => void remove(false)}>
          {t("history.confirm.delete", "Delete")}
        </button>
        {confirmQuestions.length > 0 ? (
          <button type="button" className="btn destructive" disabled={deleting} onClick={() => void remove(true)}>
            {t("history.confirm.deleteQuestions", "Delete the question and answer too")}
          </button>
        ) : null}
        <AlertCancel asChild>
          <button type="button" className="btn" disabled={deleting}>
            {t("history.confirm.cancel", "Cancel")}
          </button>
        </AlertCancel>
      </Alert>
    </section>
  );
}
