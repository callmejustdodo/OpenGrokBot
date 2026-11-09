use std::sync::Mutex;

use formation_proto::agent::{Activity, Agent, Attention, AttentionTarget, Schedule, ScheduleRun};
use formation_proto::common::{
    ActivityId, AgentId, ApiError, ErrorCode, MemberId, ScheduleId, Timestamp,
};
use formation_proto::harness::SessionInfo;
use formation_proto::snapshot::Settings;

use crate::ports::{
    ActivityRepo, AgentRepo, AttentionRepo, Result, ScheduleRepo, SessionRepo, SettingsRepo,
};

/// Thread-safe storage for A's repositories. Deleted agents retain tombstones.
#[derive(Default)]
pub struct MemoryStore {
    data: Mutex<Data>,
}

#[derive(Default)]
struct Data {
    agents: Vec<(Agent, Option<Timestamp>)>,
    sessions: Vec<SessionInfo>,
    schedules: Vec<Schedule>,
    runs: Vec<ScheduleRun>,
    attention: Vec<Attention>,
    activity: Vec<Activity>,
    settings: Settings,
}

impl AgentRepo for MemoryStore {
    fn agents(&self) -> Result<Vec<Agent>> {
        Ok(self
            .data
            .lock()
            .unwrap()
            .agents
            .iter()
            .filter(|(_, deleted)| deleted.is_none())
            .map(|(a, _)| a.clone())
            .collect())
    }
    fn agent(&self, id: AgentId) -> Result<Option<Agent>> {
        Ok(self.agents()?.into_iter().find(|a| a.id == id))
    }
    fn agent_by_slug(&self, slug: &str) -> Result<Option<Agent>> {
        Ok(self.agents()?.into_iter().find(|a| a.slug == slug))
    }
    fn insert_agent(&self, agent: &Agent) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        if data
            .agents
            .iter()
            .any(|(a, deleted)| a.id == agent.id || (deleted.is_none() && a.slug == agent.slug))
        {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "agent ID or slug is taken",
            ));
        }
        data.agents.push((agent.clone(), None));
        Ok(())
    }
    fn update_agent(&self, agent: &Agent) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        if !data
            .agents
            .iter()
            .any(|(a, deleted)| a.id == agent.id && deleted.is_none())
        {
            return Err(ApiError::new(ErrorCode::NotFound, "agent not found"));
        }
        if data
            .agents
            .iter()
            .any(|(a, deleted)| deleted.is_none() && a.id != agent.id && a.slug == agent.slug)
        {
            return Err(ApiError::new(ErrorCode::Conflict, "slug is taken"));
        }
        let (stored, _) = data
            .agents
            .iter_mut()
            .find(|(a, deleted)| a.id == agent.id && deleted.is_none())
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "agent not found"))?;
        *stored = agent.clone();
        Ok(())
    }
    fn delete_agent(&self, id: AgentId, at: Timestamp) -> Result<()> {
        if let Some((_, deleted)) = self
            .data
            .lock()
            .unwrap()
            .agents
            .iter_mut()
            .find(|(a, deleted)| a.id == id && deleted.is_none())
        {
            *deleted = Some(at);
        }
        Ok(())
    }
}

impl SessionRepo for MemoryStore {
    fn main_session(&self, agent_id: AgentId) -> Result<Option<SessionInfo>> {
        Ok(self
            .data
            .lock()
            .unwrap()
            .sessions
            .iter()
            .find(|s| s.agent_id == agent_id)
            .cloned())
    }
    fn put_main_session(&self, session: &SessionInfo) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        data.sessions.retain(|s| s.agent_id != session.agent_id);
        data.sessions.push(session.clone());
        Ok(())
    }
}

impl ScheduleRepo for MemoryStore {
    fn schedules(&self, agent_id: Option<AgentId>) -> Result<Vec<Schedule>> {
        Ok(self
            .data
            .lock()
            .unwrap()
            .schedules
            .iter()
            .filter(|s| agent_id.is_none_or(|id| id == s.agent_id))
            .cloned()
            .collect())
    }
    fn schedule(&self, id: ScheduleId) -> Result<Option<Schedule>> {
        Ok(self
            .data
            .lock()
            .unwrap()
            .schedules
            .iter()
            .find(|s| s.id == id)
            .cloned())
    }
    fn put_schedule(&self, schedule: &Schedule) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        data.schedules.retain(|s| s.id != schedule.id);
        data.schedules.push(schedule.clone());
        Ok(())
    }
    fn delete_schedule(&self, id: ScheduleId) -> Result<()> {
        self.data.lock().unwrap().schedules.retain(|s| s.id != id);
        Ok(())
    }
    fn put_schedule_run(&self, run: &ScheduleRun) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        data.runs.retain(|r| r.id != run.id);
        data.runs.push(run.clone());
        Ok(())
    }
    fn schedule_runs(
        &self,
        schedule_id: ScheduleId,
        before: Option<Timestamp>,
        limit: u32,
    ) -> Result<Vec<ScheduleRun>> {
        let mut runs: Vec<_> = self
            .data
            .lock()
            .unwrap()
            .runs
            .iter()
            .filter(|r| r.schedule_id == schedule_id && before.is_none_or(|t| r.planned_at < t))
            .cloned()
            .collect();
        runs.sort_by_key(|r| std::cmp::Reverse(r.planned_at));
        runs.truncate(limit as usize);
        Ok(runs)
    }
}

impl AttentionRepo for MemoryStore {
    fn open_attention(&self) -> Result<Vec<Attention>> {
        let mut items: Vec<_> = self
            .data
            .lock()
            .unwrap()
            .attention
            .iter()
            .filter(|a| a.resolved_at.is_none())
            .cloned()
            .collect();
        items.sort_by_key(|a| a.at);
        Ok(items)
    }
    fn put_attention(&self, item: &Attention) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        data.attention.retain(|a| a.id != item.id);
        data.attention.push(item.clone());
        Ok(())
    }
    fn resolve_attention(&self, target: &AttentionTarget, at: Timestamp) -> Result<Vec<Attention>> {
        let mut data = self.data.lock().unwrap();
        let mut resolved = Vec::new();
        for item in &mut data.attention {
            if item.target == *target && item.resolved_at.is_none() {
                item.resolved_at = Some(at);
                resolved.push(item.clone());
            }
        }
        resolved.sort_by_key(|a| a.at);
        Ok(resolved)
    }
}

impl ActivityRepo for MemoryStore {
    fn record_activity(&self, activity: &Activity) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        data.activity.retain(|a| a.id != activity.id);
        data.activity.push(activity.clone());
        Ok(())
    }
    fn activity(
        &self,
        agent_id: Option<AgentId>,
        member_id: Option<MemberId>,
        before: Option<Timestamp>,
        limit: u32,
    ) -> Result<Vec<Activity>> {
        let mut items: Vec<_> = self
            .data
            .lock()
            .unwrap()
            .activity
            .iter()
            .filter(|a| {
                agent_id.is_none_or(|id| a.agent_id == Some(id))
                    && member_id.is_none_or(|id| a.actor == Some(id))
                    && before.is_none_or(|t| a.at < t)
            })
            .cloned()
            .collect();
        items.sort_by_key(|a| std::cmp::Reverse(a.at));
        items.truncate(limit as usize);
        Ok(items)
    }
    fn delete_activity(&self, ids: &[ActivityId]) -> Result<()> {
        self.data
            .lock()
            .unwrap()
            .activity
            .retain(|a| !ids.contains(&a.id));
        Ok(())
    }
}

impl SettingsRepo for MemoryStore {
    fn settings(&self) -> Result<Settings> {
        Ok(self.data.lock().unwrap().settings.clone())
    }
    fn put_settings(&self, settings: &Settings) -> Result<()> {
        self.data.lock().unwrap().settings = settings.clone();
        Ok(())
    }
}
