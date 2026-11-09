//! Schedule commands and a serial, clock-injectable runner (TASK-030).
//! Calendar rules use the host's local timezone. Every-hours rules start at the
//! next local time-of-day on creation, then advance by elapsed hours.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Datelike, Days, Duration, Local, TimeZone, Utc};
use formation_proto::{
    agent::*,
    common::{ApiError, ErrorCode, MessageId, RunId, ScheduleId, ScheduleRunId, Timestamp},
    events::Event,
    harness::TurnOutcome,
    policy::Caller,
    request::{QueueItem, QueueSource},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::ports::{
    CommandCall, CommandHandler, EventSink, ForkRun, ForkTools, Harness, Notifier, Result, Store,
};

pub struct Schedules {
    store: Arc<dyn Store>,
    harness: Arc<dyn Harness>,
    events: Arc<dyn EventSink>,
    notifier: Arc<dyn Notifier>,
    operations: Mutex<()>,
    ticks: Mutex<()>,
}

impl Schedules {
    pub fn new(
        store: Arc<dyn Store>,
        harness: Arc<dyn Harness>,
        events: Arc<dyn EventSink>,
        notifier: Arc<dyn Notifier>,
    ) -> Self {
        Self {
            store,
            harness,
            events,
            notifier,
            operations: Mutex::new(()),
            ticks: Mutex::new(()),
        }
    }

    async fn stored<T: Send + 'static>(
        &self,
        action: impl FnOnce(&dyn Store, &dyn EventSink) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let _guard = self.operations.lock().await;
        let store = self.store.clone();
        let events = self.events.clone();
        tokio::task::spawn_blocking(move || action(store.as_ref(), events.as_ref()))
            .await
            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
    }

    /// Runs due work once, even when many slots were missed. Busy agents remain
    /// due until a later tick. Only one tick runs at a time on this instance.
    pub async fn tick(&self, mut now: Timestamp) -> Result<()> {
        let _guard = self.ticks.lock().await;
        let schedules = self.stored(|store, _| store.schedules(None)).await?;
        for candidate in schedules {
            let prepared = self
                .stored(move |store, events| {
                    let Some(mut schedule) = store.schedule(candidate.id)? else {
                        return Ok(None);
                    };
                    let Some(planned) = schedule.next_run_at else {
                        return Ok(None);
                    };
                    if schedule.paused || planned > now {
                        return Ok(None);
                    }
                    let agent = store
                        .agent(schedule.agent_id)?
                        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "agent not found"))?;
                    if !matches!(
                        agent.state,
                        AgentState::Idle | AgentState::Stopped | AgentState::Error
                    ) {
                        return Ok(None);
                    }
                    let resume = store
                        .main_session(agent.id)?
                        .map(|session| session.session_id);
                    let run = ScheduleRun {
                        id: ScheduleRunId::generate(),
                        schedule_id: schedule.id,
                        planned_at: planned,
                        started_at: Some(now),
                        finished_at: None,
                        result: None,
                    };
                    schedule.last_run = Some(run.clone());
                    store.put_schedule_run(&run)?;
                    save(store, events, &schedule)?;
                    Ok(Some((schedule, agent, resume, run)))
                })
                .await?;
            let Some((schedule, agent, resume, mut run)) = prepared else {
                continue;
            };
            let planned = run.planned_at;
            let started = std::time::Instant::now();
            let result = async {
                let run_id = RunId::generate();
                let item = QueueItem {
                    id: MessageId::generate(),
                    agent_id: agent.id,
                    source: QueueSource::Schedule {
                        schedule_id: schedule.id,
                    },
                    text: schedule.prompt.clone(),
                    queued_at: now,
                };
                if let Some(session) = self.harness.session_info(agent.id).await? {
                    return self
                        .harness
                        .run_fork(
                            agent.id,
                            ForkRun {
                                run_id,
                                from: session.session_id,
                                item,
                                tools: ForkTools::Gated,
                            },
                        )
                        .await
                        .map(|fork| fork.result);
                }
                self.harness.start(&agent, resume).await?;
                self.harness.run_turn(agent.id, run_id, item).await
            }
            .await;
            let failed = !matches!(result, Ok(ref turn) if turn.outcome == TurnOutcome::Completed);
            // One polling period of lateness distinguishes a missed slot from
            // ordinary polling jitter.
            run.result = Some(if failed {
                ScheduleRunResult::Failed
            } else if now - planned >= Duration::seconds(30) {
                ScheduleRunResult::MissedRan
            } else {
                ScheduleRunResult::Completed
            });
            let finished = now + Duration::from_std(started.elapsed()).expect("turn duration");
            run.finished_at = Some(finished);
            now = finished;
            let schedule_id = schedule.id;
            self.stored(move |store, events| {
                if let Some(mut current) = store.schedule(schedule_id)? {
                    // Commands remain available during the turn; preserve edits and deletion.
                    if !current.paused && current.next_run_at.is_some_and(|due| due <= finished) {
                        current.next_run_at = Some(next_after(&current, finished)?);
                    }
                    current.last_run = Some(run.clone());
                    store.put_schedule_run(&run)?;
                    save(store, events, &current)?;
                }
                Ok(())
            })
            .await?;
            if failed {
                self.notifier
                    .raise(
                        AttentionKind::ScheduleFailed,
                        AttentionTarget::Schedule {
                            schedule_id: schedule.id,
                        },
                        agent.name,
                    )
                    .await?;
            }
        }
        Ok(())
    }

    /// Run until cancelled by the caller; errors are returned to the supervisor.
    pub async fn run(&self) -> Result<()> {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            self.tick(Utc::now()).await?;
        }
    }
}

#[async_trait]
impl CommandHandler for Schedules {
    fn methods(&self) -> Vec<&'static str> {
        vec![
            "schedule_list",
            "schedule_create",
            "schedule_update",
            "schedule_delete",
            "schedule_run_list",
        ]
    }

    async fn handle(&self, call: CommandCall) -> Result<Value> {
        if call.caller != Caller::Owner {
            return Err(ApiError::new(
                ErrorCode::Denied,
                "schedule management requires the owner",
            ));
        }
        self.stored(move |store, events| command(store, events, call, Utc::now()))
            .await
    }
}

fn command(
    store: &dyn Store,
    events: &dyn EventSink,
    call: CommandCall,
    now: Timestamp,
) -> Result<Value> {
    match call.method.as_str() {
        "schedule_list" => {
            let args: ScheduleList = decode(call.params)?;
            encode(store.schedules(args.agent_id)?)
        }
        "schedule_create" => {
            let args: ScheduleCreate = decode(call.params)?;
            let agent = store
                .agent(args.agent_id)?
                .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "agent not found"))?;
            if agent.harness == HarnessKind::Detected {
                return Err(ApiError::new(
                    ErrorCode::HarnessUnsupported,
                    "detected agents cannot run scheduled main-session turns",
                ));
            }
            let mut schedule = Schedule {
                id: ScheduleId::generate(),
                agent_id: args.agent_id,
                prompt: args.prompt,
                rule: args.rule,
                time: args.time,
                paused: false,
                next_run_at: None,
                last_run: None,
            };
            validate(&schedule)?;
            schedule.next_run_at = Some(next_after(&schedule, now)?);
            save(store, events, &schedule)?;
            encode(schedule)
        }
        "schedule_update" => {
            let args: ScheduleUpdate = decode(call.params)?;
            let mut schedule = get(store, args.schedule_id)?;
            let reschedule = args.rule.is_some()
                || args.time.is_some()
                || args.paused == Some(false) && schedule.paused;
            if let Some(prompt) = args.prompt {
                schedule.prompt = prompt;
            }
            if let Some(rule) = args.rule {
                schedule.rule = rule;
            }
            if let Some(time) = args.time {
                schedule.time = time;
            }
            if let Some(paused) = args.paused {
                schedule.paused = paused;
            }
            validate(&schedule)?;
            if schedule.paused {
                schedule.next_run_at = None;
            } else if reschedule {
                schedule.next_run_at = None;
                schedule.next_run_at = Some(next_after(&schedule, now)?);
            }
            save(store, events, &schedule)?;
            encode(schedule)
        }
        "schedule_delete" => {
            let args: ScheduleDelete = decode(call.params)?;
            get(store, args.schedule_id)?;
            store.delete_schedule(args.schedule_id)?;
            events.publish(Event::ScheduleUpdate(ScheduleUpdateEvent::Removed {
                schedule_id: args.schedule_id,
            }));
            encode(())
        }
        "schedule_run_list" => {
            let args: ScheduleRunList = decode(call.params)?;
            get(store, args.schedule_id)?;
            encode(store.schedule_runs(args.schedule_id, args.before, args.limit)?)
        }
        _ => Err(ApiError::new(
            ErrorCode::InvalidParams,
            "unknown schedule command",
        )),
    }
}

fn get(store: &dyn Store, id: ScheduleId) -> Result<Schedule> {
    store
        .schedule(id)?
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "schedule not found"))
}
fn save(store: &dyn Store, events: &dyn EventSink, schedule: &Schedule) -> Result<()> {
    store.put_schedule(schedule)?;
    events.publish(Event::ScheduleUpdate(ScheduleUpdateEvent::Upsert {
        schedule: schedule.clone(),
    }));
    Ok(())
}
fn validate(schedule: &Schedule) -> Result<()> {
    if schedule.prompt.trim().is_empty()
        || matches!(schedule.rule, ScheduleRule::EveryHours { hours: 0 })
    {
        return Err(ApiError::new(
            ErrorCode::InvalidParams,
            "prompt and a positive interval are required",
        ));
    }
    Ok(())
}
fn decode<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value)
        .map_err(|e| ApiError::new(ErrorCode::InvalidParams, e.to_string()))
}
fn encode(value: impl Serialize) -> Result<Value> {
    serde_json::to_value(value).map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))
}

fn next_after(schedule: &Schedule, after: Timestamp) -> Result<Timestamp> {
    if let ScheduleRule::EveryHours { hours } = schedule.rule
        && let Some(anchor) = schedule.next_run_at
    {
        let interval = Duration::hours(i64::from(hours));
        let steps = (after - anchor)
            .num_seconds()
            .div_euclid(interval.num_seconds())
            + 1;
        return Ok(anchor + Duration::seconds(interval.num_seconds() * steps));
    }
    let today = after.with_timezone(&Local).date_naive();
    for offset in 0..=7 {
        let date = today
            .checked_add_days(Days::new(offset))
            .expect("calendar date");
        let matches = match schedule.rule {
            ScheduleRule::Daily | ScheduleRule::EveryHours { .. } => true,
            ScheduleRule::Weekdays => date.weekday().number_from_monday() <= 5,
            ScheduleRule::Weekly { weekday } => {
                date.weekday().number_from_monday()
                    == match weekday {
                        Weekday::Mon => 1,
                        Weekday::Tue => 2,
                        Weekday::Wed => 3,
                        Weekday::Thu => 4,
                        Weekday::Fri => 5,
                        Weekday::Sat => 6,
                        Weekday::Sun => 7,
                    }
            }
        };
        if matches
            && let Some(next) = Local
                .from_local_datetime(&date.and_time(schedule.time))
                .earliest()
        {
            let next = next.with_timezone(&Utc);
            if next > after {
                return Ok(next);
            }
        }
    }
    Err(ApiError::new(
        ErrorCode::InvalidParams,
        "no next local schedule time",
    ))
}
