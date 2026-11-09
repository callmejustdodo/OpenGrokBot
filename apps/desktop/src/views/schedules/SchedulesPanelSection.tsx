import "./schedules.css";
// SC-007 schedules section of the information panel (mockup `schedPanelCard`):
// each schedule's rule, time, next run and last result, and "Add".
import { useTranslation } from "react-i18next";
import type { AgentId } from "../../ipc/generated/AgentId";
import type { Attention } from "../../ipc/generated/Attention";
import type { Schedule } from "../../ipc/generated/Schedule";
import type { ScheduleId } from "../../ipc/generated/ScheduleId";
import { useAppStore } from "../../store/store";
import { uiStore } from "../../store/ui";
import {
  dayAndTime,
  lastRunState,
  ruleAndTime,
  runText,
  selectAgentSchedules,
  selectScheduleFailures,
  setSheetFocus,
} from "./format";

const NONE: Schedule[] = [];
const NO_FAILURES: Attention[] = [];

function openSheet(agentId: AgentId, schedule: ScheduleId | "new") {
  setSheetFocus(agentId, schedule);
  uiStore.getState().openSheet({ kind: "schedules", agentId });
}

function ScheduleItem({ schedule }: { schedule: Schedule }) {
  const { t, i18n } = useTranslation();
  const lng = i18n.language;
  const failures = useAppStore((s) => (s.mirror ? selectScheduleFailures(s.mirror, schedule.id) : NO_FAILURES));
  const state = lastRunState(schedule, failures);
  const run = schedule.last_run;

  let result: string | null = null;
  if (failures.length > 0) {
    result = t("schedules.panel.failedNotice", "Failed: {{subject}}", { subject: failures[0].subject });
  } else if (run) {
    result = runText(t, run, lng);
  }

  return (
    <li className="sched" data-state={schedule.paused ? "paused" : "active"} data-run={state}>
      <button
        type="button"
        className="sched-open"
        onClick={() => openSheet(schedule.agent_id, schedule.id)}
        aria-label={t("schedules.panel.edit", "Edit schedule: {{prompt}}", { prompt: schedule.prompt })}
      >
        <b>{schedule.prompt}</b>
      </button>
      <small>
        {ruleAndTime(t, schedule, lng)}
        {" · "}
        {schedule.paused ? (
          <span className="pill" data-state="paused">
            {t("schedules.paused", "Paused")}
          </span>
        ) : schedule.next_run_at ? (
          t("schedules.panel.next", "Next: {{when}}", { when: dayAndTime(schedule.next_run_at, lng) })
        ) : (
          t("schedules.panel.noNext", "No next run")
        )}
      </small>
      {result ? (
        <small className="run" data-run={state} role={state === "failed" ? "alert" : undefined}>
          {result}
        </small>
      ) : null}
      {state === "failed" ? (
        <small className="hint">
          {t(
            "schedules.panel.retryHint",
            "It runs again at the next scheduled time. Check what it needs, then edit the schedule if anything changed.",
          )}
        </small>
      ) : null}
    </li>
  );
}

export default function SchedulesPanelSection({ agentId }: { agentId: AgentId }) {
  const { t } = useTranslation();
  const detected = useAppStore((s) => s.mirror?.agents[agentId]?.detected ?? null);
  const schedules = useAppStore((s) => (s.mirror ? selectAgentSchedules(s.mirror, agentId) : NONE));
  // Detected agents have no schedules; the panel explains that (TASK-015).
  if (detected) return null;

  const empty = schedules.length === 0;
  return (
    <section
      className="p-card"
      aria-labelledby="schedules-panel-title"
      data-state={empty ? "empty" : "list"}
    >
      <span className="pr">
        <b id="schedules-panel-title">{t("schedules.title", "Schedules")}</b>
        {empty ? null : (
          <button type="button" className="btn plain" onClick={() => openSheet(agentId, "new")}>
            {t("schedules.add", "Add")}
          </button>
        )}
      </span>
      {empty ? (
        <>
          <small>{t("schedules.panel.empty", "No schedules")}</small>
          <div>
            <button type="button" className="btn" onClick={() => openSheet(agentId, "new")}>
              {t("schedules.add", "Add")}
            </button>
          </div>
        </>
      ) : (
        <ul className="list">
          {schedules.map((s) => (
            <ScheduleItem key={s.id} schedule={s} />
          ))}
        </ul>
      )}
    </section>
  );
}
