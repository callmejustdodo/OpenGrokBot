//! Owner: C (app) for the envelope. Each payload lives with its domain owner.
//!
//! formationd sends every event as a JSON-RPC notification named
//! [`EVENT_NOTIFICATION`](crate::common::EVENT_NOTIFICATION) whose params is an
//! [`EventEnvelope`].

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agent::{
    AgentStateEvent, AgentUpdateEvent, HarnessUpdateEvent, NotifyEvent, ScheduleUpdateEvent,
};
use crate::ask::QuestionUpdateEvent;
use crate::common::{Seq, Timestamp};
use crate::harness::{AgentQueueEvent, AgentStreamEvent};
use crate::peer::PresenceEvent;
use crate::policy::{ApprovalEvent, RuleUpdateEvent, ShareUpdateEvent};
use crate::request::RequestUpdateEvent;
use crate::snapshot::SettingsUpdateEvent;
use crate::team::TeamUpdateEvent;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EventEnvelope {
    pub seq: Seq,
    pub at: Timestamp,
    pub event: Event,
}

/// Every event, as `{ "type": "<name>", "payload": { … } }`. Each slice of the
/// [`Snapshot`](crate::snapshot::Snapshot) has an event that keeps it current.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", content = "payload")]
#[ts(export)]
pub enum Event {
    // A: agent runtime
    #[serde(rename = "agent.state")]
    AgentState(AgentStateEvent),
    #[serde(rename = "agent.stream")]
    AgentStream(AgentStreamEvent),
    #[serde(rename = "agent.queue")]
    AgentQueue(AgentQueueEvent),
    #[serde(rename = "agent.update")]
    AgentUpdate(AgentUpdateEvent),
    #[serde(rename = "schedule.update")]
    ScheduleUpdate(ScheduleUpdateEvent),
    #[serde(rename = "notify")]
    Notify(NotifyEvent),
    #[serde(rename = "harness.update")]
    HarnessUpdate(HarnessUpdateEvent),

    // B: collaboration core
    #[serde(rename = "approval.raised")]
    ApprovalRaised(ApprovalEvent),
    #[serde(rename = "approval.update")]
    ApprovalUpdate(ApprovalEvent),
    #[serde(rename = "question.update")]
    QuestionUpdate(QuestionUpdateEvent),
    #[serde(rename = "request.update")]
    RequestUpdate(RequestUpdateEvent),
    #[serde(rename = "share.update")]
    ShareUpdate(ShareUpdateEvent),
    #[serde(rename = "rule.update")]
    RuleUpdate(RuleUpdateEvent),
    #[serde(rename = "team.update")]
    TeamUpdate(TeamUpdateEvent),
    #[serde(rename = "peer.presence")]
    PeerPresence(PresenceEvent),

    // C: app
    #[serde(rename = "settings.update")]
    SettingsUpdate(SettingsUpdateEvent),
}

impl Event {
    pub fn name(&self) -> &'static str {
        match self {
            Self::AgentState(_) => "agent.state",
            Self::AgentStream(_) => "agent.stream",
            Self::AgentQueue(_) => "agent.queue",
            Self::AgentUpdate(_) => "agent.update",
            Self::ScheduleUpdate(_) => "schedule.update",
            Self::Notify(_) => "notify",
            Self::HarnessUpdate(_) => "harness.update",
            Self::ApprovalRaised(_) => "approval.raised",
            Self::ApprovalUpdate(_) => "approval.update",
            Self::QuestionUpdate(_) => "question.update",
            Self::RequestUpdate(_) => "request.update",
            Self::ShareUpdate(_) => "share.update",
            Self::RuleUpdate(_) => "rule.update",
            Self::TeamUpdate(_) => "team.update",
            Self::PeerPresence(_) => "peer.presence",
            Self::SettingsUpdate(_) => "settings.update",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentState, samples as agent_samples};
    use crate::common::{AgentId, MemberId};
    use crate::harness::StreamDelta;
    use crate::testutil::{roundtrip, t0};
    use crate::{ask, peer, policy, request, team};

    fn every_event() -> Vec<Event> {
        let agent = agent_samples::agent();
        let team = team::samples::team();
        let (me, mate) = (team.members[0].id, team.members[1].id);
        let id = agent.id;
        vec![
            Event::AgentState(AgentStateEvent {
                agent_id: id,
                state: AgentState::Error,
                reason: Some("claude exited".into()),
                exit_code: Some(1),
            }),
            Event::AgentStream(AgentStreamEvent {
                agent_id: id,
                stream_seq: 1,
                delta: StreamDelta::TurnStarted {
                    message_id: crate::common::MessageId::generate(),
                },
            }),
            Event::AgentQueue(AgentQueueEvent {
                agent_id: id,
                items: vec![request::samples::queue_item(id)],
            }),
            Event::AgentUpdate(AgentUpdateEvent::Upsert {
                agent: Box::new(agent.clone()),
            }),
            Event::ScheduleUpdate(ScheduleUpdateEvent::Upsert {
                schedule: agent_samples::schedule(id),
            }),
            Event::Notify(NotifyEvent {
                item: agent_samples::attention(id),
                alert: false,
            }),
            Event::HarnessUpdate(HarnessUpdateEvent {
                harnesses: vec![agent_samples::harness_status()],
            }),
            Event::ApprovalRaised(ApprovalEvent {
                approval: policy::samples::approval(id, mate),
            }),
            Event::ApprovalUpdate(ApprovalEvent {
                approval: policy::samples::approval(id, mate),
            }),
            Event::QuestionUpdate(QuestionUpdateEvent::Upsert {
                question: Box::new(ask::samples::question(mate, me, id)),
            }),
            Event::QuestionUpdate(QuestionUpdateEvent::Removed {
                question_id: crate::common::QuestionId::generate(),
            }),
            Event::RequestUpdate(RequestUpdateEvent {
                request: request::samples::work_request(me, id),
            }),
            Event::ShareUpdate(ShareUpdateEvent {
                share: policy::ShareSettings::private(AgentId::generate()),
            }),
            Event::RuleUpdate(RuleUpdateEvent {
                rule: policy::samples::rule(id, MemberId::generate()),
            }),
            Event::TeamUpdate(TeamUpdateEvent { team }),
            Event::PeerPresence(PresenceEvent {
                member_id: mate,
                online: true,
                last_seen_at: None,
                agents: vec![peer::samples::shared_agent(mate)],
            }),
            Event::SettingsUpdate(SettingsUpdateEvent {
                settings: crate::snapshot::Settings::default(),
            }),
        ]
    }

    #[test]
    fn every_event_round_trips_under_its_name() {
        let events = every_event();
        for (i, event) in events.into_iter().enumerate() {
            let name = event.name();
            let json = roundtrip(&EventEnvelope {
                seq: Seq(i as u64 + 1),
                at: t0(),
                event,
            });
            assert_eq!(json["event"]["type"], name);
            assert!(json["event"]["payload"].is_object());
        }
    }

    #[test]
    fn event_names_are_unique() {
        let mut names: Vec<&str> = every_event().iter().map(Event::name).collect();
        // One sample per variant (question.update has two).
        assert_eq!(names.len(), 17);
        names.dedup();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
    }
}
