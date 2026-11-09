// Words and times for SC-007: a schedule's rule, its local time, and its last run.
import type { TFunction } from "i18next";
import type { AgentId } from "../../ipc/generated/AgentId";
import type { Attention } from "../../ipc/generated/Attention";
import type { Schedule } from "../../ipc/generated/Schedule";
import type { ScheduleId } from "../../ipc/generated/ScheduleId";
import type { ScheduleRule } from "../../ipc/generated/ScheduleRule";
import type { ScheduleRun } from "../../ipc/generated/ScheduleRun";
import type { Weekday } from "../../ipc/generated/Weekday";
import type { MirrorState } from "../../store/reducer";

export const WEEKDAYS: readonly Weekday[] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

export function weekdayName(t: TFunction, day: Weekday): string {
  switch (day) {
    case "mon":
      return t("schedules.day.mon", "Monday");
    case "tue":
      return t("schedules.day.tue", "Tuesday");
    case "wed":
      return t("schedules.day.wed", "Wednesday");
    case "thu":
      return t("schedules.day.thu", "Thursday");
    case "fri":
      return t("schedules.day.fri", "Friday");
    case "sat":
      return t("schedules.day.sat", "Saturday");
    case "sun":
      return t("schedules.day.sun", "Sunday");
  }
}

/** "Daily", "Weekdays", "Weekly on Monday", "Every 4 hours". */
export function ruleText(t: TFunction, rule: ScheduleRule): string {
  switch (rule.kind) {
    case "daily":
      return t("schedules.rule.daily", "Daily");
    case "weekdays":
      return t("schedules.rule.weekdays", "Weekdays");
    case "weekly":
      return t("schedules.rule.weeklyOn", "Weekly on {{day}}", { day: weekdayName(t, rule.weekday) });
    case "every_hours":
      return t("schedules.rule.everyHours", {
        count: rule.hours,
        defaultValue_one: "Every hour",
        defaultValue_other: "Every {{count}} hours",
      });
  }
}

/** "09:00:00" (formationd's local time of day) → "09:00", the `<input type="time">` value. */
export function timeInputValue(time: string): string {
  return time.slice(0, 5);
}

/** "09:00" → "09:00:00". */
export function timeParam(value: string): string {
  return value.length === 5 ? `${value}:00` : value;
}

/** A local time of day ("09:00:00") in the UI language, e.g. "9:00 AM". */
export function timeOfDay(time: string, language: string): string {
  const [hours = 0, minutes = 0] = time.split(":").map(Number);
  const date = new Date(2000, 0, 1, hours, minutes);
  return new Intl.DateTimeFormat(language, { hour: "numeric", minute: "2-digit" }).format(date);
}

/** A timestamp's clock time, e.g. "9:47 AM". */
export function clock(iso: string, language: string): string {
  return new Intl.DateTimeFormat(language, { hour: "numeric", minute: "2-digit" }).format(new Date(iso));
}

/** A timestamp with its day, e.g. "Nov 10, 9:00 AM". */
export function dayAndTime(iso: string, language: string): string {
  return new Intl.DateTimeFormat(language, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  }).format(new Date(iso));
}

/** "Weekdays · 9:00 AM", or "Every 4 hours · from 9:00 AM". */
export function ruleAndTime(t: TFunction, schedule: Schedule, language: string): string {
  const time = timeOfDay(schedule.time, language);
  const at =
    schedule.rule.kind === "every_hours"
      ? t("schedules.fromTime", "from {{time}}", { time })
      : time;
  return `${ruleText(t, schedule.rule)} · ${at}`;
}

/** One run in words, by its result. */
export function runText(t: TFunction, run: ScheduleRun, language: string): string {
  switch (run.result) {
    case "completed":
      return t("schedules.run.completed", "{{when}} completed", {
        when: dayAndTime(run.finished_at ?? run.planned_at, language),
      });
    case "failed":
      return t("schedules.run.failed", "{{when}} failed", { when: dayAndTime(run.planned_at, language) });
    case "missed_ran":
      return t("schedules.run.missed", "Missed → ran on wake at {{time}}", {
        time: clock(run.started_at ?? run.finished_at ?? run.planned_at, language),
      });
    case null:
      return run.started_at
        ? t("schedules.run.running", "Running since {{time}}", { time: clock(run.started_at, language) })
        : t("schedules.run.waiting", "Waiting to run");
  }
}

/** The run state a schedule shows: a `schedule_failed` alert counts as failed. */
export function lastRunState(schedule: Schedule, failures: readonly Attention[]): string {
  if (failures.length > 0) return "failed";
  const result = schedule.last_run?.result;
  if (result === "missed_ran") return "missed";
  if (result === "failed") return "failed";
  if (result === "completed") return "completed";
  if (schedule.last_run) return "running";
  return "none";
}

const agentCache = new WeakMap<MirrorState, Map<AgentId, Schedule[]>>();
const failureCache = new WeakMap<MirrorState, Map<ScheduleId, Attention[]>>();

/** An agent's schedules, by time of day then prompt. Cached per mirror. */
export function selectAgentSchedules(state: MirrorState, agentId: AgentId): Schedule[] {
  let byAgent = agentCache.get(state);
  if (!byAgent) {
    byAgent = new Map();
    agentCache.set(state, byAgent);
  }
  const cached = byAgent.get(agentId);
  if (cached) return cached;
  const list = Object.values(state.schedules)
    .filter((s) => s.agent_id === agentId)
    .sort((a, b) => a.time.localeCompare(b.time) || a.prompt.localeCompare(b.prompt));
  byAgent.set(agentId, list);
  return list;
}

/** Unresolved `schedule_failed` alerts for one schedule, newest first. Cached per mirror. */
export function selectScheduleFailures(state: MirrorState, scheduleId: ScheduleId): Attention[] {
  let bySchedule = failureCache.get(state);
  if (!bySchedule) {
    bySchedule = new Map();
    failureCache.set(state, bySchedule);
  }
  const cached = bySchedule.get(scheduleId);
  if (cached) return cached;
  const list = Object.values(state.inbox)
    .filter(
      (i) => i.kind === "schedule_failed" && i.target.kind === "schedule" && i.target.schedule_id === scheduleId,
    )
    .sort((a, b) => Date.parse(b.at) - Date.parse(a.at));
  bySchedule.set(scheduleId, list);
  return list;
}

/** Which schedule the sheet should open on: set by the panel just before it opens the sheet. */
let sheetFocus: { agentId: AgentId; schedule: ScheduleId | "new" } | null = null;

export function setSheetFocus(agentId: AgentId, schedule: ScheduleId | "new" | null): void {
  sheetFocus = schedule === null ? null : { agentId, schedule };
}

export function peekSheetFocus(agentId: AgentId): ScheduleId | "new" | null {
  return sheetFocus?.agentId === agentId ? sheetFocus.schedule : null;
}
