// SC-007 schedule sheet (mockup `schedules`): an agent's schedules, and a form
// to add or edit one (task, repeat, time, paused) with its recent runs.
import * as Switch from "@radix-ui/react-switch";
import * as ToggleGroup from "@radix-ui/react-toggle-group";
import { useEffect, useId, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import type { CallError } from "../../ipc/client";
import type { AgentId } from "../../ipc/generated/AgentId";
import type { Schedule } from "../../ipc/generated/Schedule";
import type { ScheduleId } from "../../ipc/generated/ScheduleId";
import type { ScheduleRule } from "../../ipc/generated/ScheduleRule";
import type { ScheduleRun } from "../../ipc/generated/ScheduleRun";
import type { Weekday } from "../../ipc/generated/Weekday";
import { tauriTransport } from "../../ipc/transport";
import { useAppStore } from "../../store/store";
import { act } from "../../store/sync";
import { uiStore } from "../../store/ui";
import Alert, { AlertCancel } from "../../ui/Alert";
import Sheet from "../../ui/Sheet";
import {
  WEEKDAYS,
  dayAndTime,
  peekSheetFocus,
  ruleAndTime,
  runText,
  selectAgentSchedules,
  setSheetFocus,
  timeInputValue,
  timeParam,
  weekdayName,
} from "./format";

const NONE: Schedule[] = [];
/** How many recent runs the sheet shows. */
const RUN_LIMIT = 10;

type RuleKind = ScheduleRule["kind"];

function close() {
  uiStore.getState().closeSheet();
}

function errorText(t: TFunction, error: unknown): string {
  const e = error as Partial<CallError>;
  if (e.code === "disconnected" || e.code === "timeout") {
    return t("schedules.error.disconnected", "Formation isn't reachable right now. Your changes are still here; try again.");
  }
  switch (e.data?.code) {
    case "invalid_params":
      return t("schedules.error.invalid", "Check the task, repeat and time, then save again.");
    case "not_found":
      return t("schedules.error.notFound", "This schedule no longer exists.");
    default:
      return t("schedules.error.generic", "Couldn't save the schedule. Try again.");
  }
}

function sameRule(a: ScheduleRule, b: ScheduleRule): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

function Runs({ schedule }: { schedule: Schedule }) {
  const { t, i18n } = useTranslation();
  const [runs, setRuns] = useState<{ status: "loading" | "ready" | "error"; items: ScheduleRun[] }>({
    status: "loading",
    items: [],
  });
  const lastRunId = schedule.last_run?.id ?? null;

  useEffect(() => {
    let live = true;
    tauriTransport
      .call("schedule_run_list", { schedule_id: schedule.id, before: null, limit: RUN_LIMIT })
      .then(
        (items) => live && setRuns({ status: "ready", items }),
        () => live && setRuns((r) => ({ status: "error", items: r.items })),
      );
    return () => {
      live = false;
    };
  }, [schedule.id, lastRunId]);

  return (
    <div className="field" data-state={runs.status}>
      <span id={`runs-${schedule.id}`}>{t("schedules.sheet.runs", "Recent runs")}</span>
      {runs.status === "loading" && runs.items.length === 0 ? (
        <p className="muted small">{t("schedules.sheet.runsLoading", "Loading runs…")}</p>
      ) : runs.status === "error" ? (
        <p className="muted small" role="alert">
          {t("schedules.sheet.runsError", "Couldn't load the run history.")}
        </p>
      ) : runs.items.length === 0 ? (
        <p className="muted small">{t("schedules.sheet.noRuns", "No runs yet")}</p>
      ) : (
        <ul className="list" aria-labelledby={`runs-${schedule.id}`}>
          {runs.items.map((run) => (
            <li key={run.id} className="row" data-run={run.result ?? "running"}>
              {run.result === "missed_ran"
                ? `${dayAndTime(run.planned_at, i18n.language)} · ${runText(t, run, i18n.language)}`
                : runText(t, run, i18n.language)}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function ScheduleForm({
  agentId,
  schedule,
  onBack,
}: {
  agentId: AgentId;
  /** `null` for a new schedule. */
  schedule: Schedule | null;
  /** Back to the list; `null` when there is no list to go back to. */
  onBack: (() => void) | null;
}) {
  const { t } = useTranslation();
  const id = useId();
  const promptRef = useRef<HTMLTextAreaElement>(null);
  const rule = schedule?.rule;
  const [prompt, setPrompt] = useState(schedule?.prompt ?? "");
  const [kind, setKind] = useState<RuleKind>(rule?.kind ?? "daily");
  const [weekday, setWeekday] = useState<Weekday>(rule?.kind === "weekly" ? rule.weekday : "mon");
  const [hours, setHours] = useState(rule?.kind === "every_hours" ? String(rule.hours) : "4");
  const [time, setTime] = useState(schedule ? timeInputValue(schedule.time) : "09:00");
  const [paused, setPaused] = useState(schedule?.paused ?? false);
  const [invalid, setInvalid] = useState<"prompt" | "hours" | "time" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);

  const saveKey = schedule ? `schedule.save:${schedule.id}` : `schedule.create:${agentId}`;
  const deleteKey = `schedule.delete:${schedule?.id ?? ""}`;
  const saving = useAppStore((s) => saveKey in s.pending);
  const deleting = useAppStore((s) => deleteKey in s.pending);

  const hoursNumber = Number(hours);
  const buildRule = (): ScheduleRule => {
    switch (kind) {
      case "daily":
        return { kind: "daily" };
      case "weekdays":
        return { kind: "weekdays" };
      case "weekly":
        return { kind: "weekly", weekday };
      case "every_hours":
        return { kind: "every_hours", hours: hoursNumber };
    }
  };

  const save = async () => {
    if (saving) return;
    const text = prompt.trim();
    if (!text) {
      setInvalid("prompt");
      promptRef.current?.focus();
      return;
    }
    if (kind === "every_hours" && !(Number.isInteger(hoursNumber) && hoursNumber >= 1 && hoursNumber <= 24)) {
      setInvalid("hours");
      return;
    }
    if (!/^\d{2}:\d{2}$/.test(time)) {
      setInvalid("time");
      return;
    }
    setInvalid(null);
    setError(null);
    const nextRule = buildRule();
    try {
      if (schedule) {
        await act(tauriTransport, saveKey, "schedule_update", {
          schedule_id: schedule.id,
          prompt: text === schedule.prompt ? null : text,
          rule: sameRule(nextRule, schedule.rule) ? null : nextRule,
          time: timeParam(time) === schedule.time ? null : timeParam(time),
          paused: paused === schedule.paused ? null : paused,
        });
      } else {
        await act(tauriTransport, saveKey, "schedule_create", {
          agent_id: agentId,
          prompt: text,
          rule: nextRule,
          time: timeParam(time),
        });
      }
      close();
    } catch (e) {
      setError(errorText(t, e));
    }
  };

  const remove = async () => {
    if (!schedule || deleting) return;
    setError(null);
    try {
      await act(tauriTransport, deleteKey, "schedule_delete", { schedule_id: schedule.id });
      setConfirmDelete(false);
      close();
    } catch (e) {
      setConfirmDelete(false);
      setError(errorText(t, e));
    }
  };

  const onSubmit = (e: FormEvent) => {
    e.preventDefault();
    void save();
  };
  // ⌘S saves (frontend architecture, "Menus and shortcuts").
  const onKeyDown = (e: KeyboardEvent) => {
    if (e.metaKey && e.key.toLowerCase() === "s") {
      e.preventDefault();
      void save();
    }
  };

  return (
    <form
      noValidate
      onSubmit={onSubmit}
      onKeyDown={onKeyDown}
      aria-label={schedule ? t("schedules.sheet.editForm", "Edit schedule") : t("schedules.sheet.newForm", "New schedule")}
      data-state={saving ? "saving" : schedule ? "edit" : "new"}
    >
      {onBack ? (
        <button type="button" className="btn plain" onClick={onBack}>
          {t("schedules.sheet.back", "All schedules")}
        </button>
      ) : null}
      {error ? (
        <div className="banner error" role="alert">
          <div className="bt">{error}</div>
          <button type="button" className="btn" onClick={() => void save()}>
            {t("schedules.sheet.retry", "Try again")}
          </button>
        </div>
      ) : null}

      <div className="field">
        <label htmlFor={`${id}-prompt`}>{t("schedules.sheet.task", "Task")}</label>
        <textarea
          id={`${id}-prompt`}
          ref={promptRef}
          className={invalid === "prompt" ? "textarea invalid" : "textarea"}
          value={prompt}
          aria-required
          aria-invalid={invalid === "prompt"}
          aria-describedby={invalid === "prompt" ? `${id}-prompt-err` : undefined}
          onChange={(e) => setPrompt(e.target.value)}
        />
        {invalid === "prompt" ? (
          <span className="err" id={`${id}-prompt-err`}>
            {t("schedules.sheet.taskRequired", "Write what the agent should do.")}
          </span>
        ) : null}
      </div>

      <div className="field">
        <span id={`${id}-repeat`}>{t("schedules.sheet.repeat", "Repeat")}</span>
        <ToggleGroup.Root
          type="single"
          className="seg"
          aria-labelledby={`${id}-repeat`}
          value={kind}
          onValueChange={(v) => v && setKind(v as RuleKind)}
        >
          <ToggleGroup.Item value="daily">{t("schedules.rule.daily", "Daily")}</ToggleGroup.Item>
          <ToggleGroup.Item value="weekdays">{t("schedules.rule.weekdays", "Weekdays")}</ToggleGroup.Item>
          <ToggleGroup.Item value="weekly">{t("schedules.rule.weekly", "Weekly")}</ToggleGroup.Item>
          <ToggleGroup.Item value="every_hours">{t("schedules.rule.everyN", "Every N hours")}</ToggleGroup.Item>
        </ToggleGroup.Root>
      </div>

      {kind === "weekly" ? (
        <div className="field">
          <span id={`${id}-day`}>{t("schedules.sheet.day", "Day")}</span>
          <ToggleGroup.Root
            type="single"
            className="seg"
            aria-labelledby={`${id}-day`}
            value={weekday}
            onValueChange={(v) => v && setWeekday(v as Weekday)}
          >
            {WEEKDAYS.map((d) => (
              <ToggleGroup.Item key={d} value={d}>
                {weekdayName(t, d)}
              </ToggleGroup.Item>
            ))}
          </ToggleGroup.Root>
        </div>
      ) : null}

      {kind === "every_hours" ? (
        <div className="field">
          <label htmlFor={`${id}-hours`}>{t("schedules.sheet.hours", "Hours between runs")}</label>
          <input
            id={`${id}-hours`}
            className={invalid === "hours" ? "input invalid" : "input"}
            type="number"
            min={1}
            max={24}
            step={1}
            value={hours}
            aria-invalid={invalid === "hours"}
            aria-describedby={invalid === "hours" ? `${id}-hours-err` : undefined}
            onChange={(e) => setHours(e.target.value)}
          />
          {invalid === "hours" ? (
            <span className="err" id={`${id}-hours-err`}>
              {t("schedules.sheet.hoursRange", "Use a whole number from 1 to 24.")}
            </span>
          ) : null}
        </div>
      ) : null}

      <div className="field">
        <label htmlFor={`${id}-time`}>
          {kind === "every_hours" ? t("schedules.sheet.startTime", "Starting at") : t("schedules.sheet.time", "Time")}
        </label>
        <input
          id={`${id}-time`}
          className={invalid === "time" ? "input invalid" : "input"}
          type="time"
          value={time}
          aria-required
          aria-invalid={invalid === "time"}
          aria-describedby={invalid === "time" ? `${id}-time-err` : undefined}
          onChange={(e) => setTime(e.target.value)}
        />
        {invalid === "time" ? (
          <span className="err" id={`${id}-time-err`}>
            {t("schedules.sheet.timeRequired", "Choose a time.")}
          </span>
        ) : null}
      </div>

      {schedule ? (
        <div className="form-row">
          <label htmlFor={`${id}-paused`}>{t("schedules.paused", "Paused")}</label>
          <Switch.Root id={`${id}-paused`} className="switch" checked={paused} onCheckedChange={setPaused}>
            <Switch.Thumb />
          </Switch.Root>
        </div>
      ) : null}

      {schedule ? <Runs schedule={schedule} /> : null}

      <div className="sheet-foot">
        {schedule ? (
          <button type="button" className="btn destructive" onClick={() => setConfirmDelete(true)} disabled={deleting}>
            {t("schedules.sheet.delete", "Delete")}
          </button>
        ) : null}
        <span className="spacer" />
        <button type="button" className="btn" onClick={close}>
          {t("schedules.sheet.cancel", "Cancel")}
        </button>
        <button type="submit" className="btn primary" disabled={saving} aria-keyshortcuts="Meta+S">
          {saving ? t("schedules.sheet.saving", "Saving…") : t("schedules.sheet.save", "Save")}
        </button>
      </div>

      {schedule ? (
        <Alert
          open={confirmDelete}
          onOpenChange={setConfirmDelete}
          title={t("schedules.delete.title", "Delete this schedule?")}
          description={t("schedules.delete.body", "It won't run again. Past runs stay in the agent's conversation.")}
        >
          <button type="button" className="btn destructive" onClick={() => void remove()} disabled={deleting}>
            {deleting ? t("schedules.delete.deleting", "Deleting…") : t("schedules.delete.confirm", "Delete schedule")}
          </button>
          <AlertCancel asChild>
            <button type="button" className="btn">
              {t("schedules.sheet.cancel", "Cancel")}
            </button>
          </AlertCancel>
        </Alert>
      ) : null}
    </form>
  );
}

export default function SchedulesSheet({ agentId }: { agentId: AgentId }) {
  const { t, i18n } = useTranslation();
  const agentName = useAppStore((s) => s.mirror?.agents[agentId]?.name ?? "");
  const schedules = useAppStore((s) => (s.mirror ? selectAgentSchedules(s.mirror, agentId) : NONE));
  const [selected, setSelected] = useState<ScheduleId | "new" | null>(
    () => peekSheetFocus(agentId) ?? (schedules.length === 0 ? "new" : null),
  );
  // The panel's focus is used once.
  useEffect(() => setSheetFocus(agentId, null), [agentId]);

  const current = selected && selected !== "new" ? (schedules.find((s) => s.id === selected) ?? null) : null;
  // A schedule removed while open falls back to the list (or to a new one when none are left).
  const mode = selected === "new" || (selected !== null && !current && schedules.length === 0) ? "new" : current ? "edit" : "list";
  const back = schedules.length > 0 ? () => setSelected(null) : null;

  return (
    <Sheet
      open
      onOpenChange={(open) => !open && close()}
      title={t("schedules.sheet.title", "Schedules · {{agent}}", { agent: agentName })}
      lead={t("schedules.sheet.lead", "Runs in a new session at the set time. Approval rules still apply.")}
      footer={
        mode === "list" ? (
          <button type="button" className="btn" onClick={close}>
            {t("schedules.sheet.close", "Close")}
          </button>
        ) : undefined
      }
    >
      {mode === "list" ? (
        <>
          <ul className="list" aria-label={t("schedules.title", "Schedules")}>
            {schedules.map((s) => (
              <li key={s.id}>
                <button
                  type="button"
                  className="row"
                  data-state={s.paused ? "paused" : "active"}
                  onClick={() => setSelected(s.id)}
                >
                  <span className="t">
                    <b>{s.prompt}</b>
                    <small>
                      {ruleAndTime(t, s, i18n.language)}
                      {s.paused ? ` · ${t("schedules.paused", "Paused")}` : ""}
                    </small>
                  </span>
                </button>
              </li>
            ))}
          </ul>
          <button type="button" className="btn" onClick={() => setSelected("new")}>
            {t("schedules.sheet.new", "New schedule")}
          </button>
        </>
      ) : (
        <ScheduleForm
          key={mode === "edit" && current ? current.id : "new"}
          agentId={agentId}
          schedule={mode === "edit" ? current : null}
          onBack={back}
        />
      )}
    </Sheet>
  );
}
