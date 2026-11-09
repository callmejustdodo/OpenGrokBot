//! Owner: C (app).
//!
//! The snapshot a client takes on connect, before applying events in order
//! (frontend architecture, "Data flow"), and the settings it shows.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agent::{Agent, Attention, HarnessStatus, Schedule};
use crate::ask::Question;
use crate::common::{Seq, Timestamp, commands};
use crate::harness::Conversation;
use crate::peer::SharedAgentView;
use crate::policy::{Approval, Rule, ShareSettings};
use crate::request::WorkRequest;
use crate::team::Team;

/// Everything the UI shows, as of event `seq`. Apply events with a higher `seq` in
/// order; on a gap, take a new snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Snapshot {
    pub seq: Seq,
    /// `None` until this Mac creates or joins a team (SC-001).
    pub team: Option<Team>,
    pub agents: Vec<Agent>,
    pub shares: Vec<ShareSettings>,
    pub schedules: Vec<Schedule>,
    /// The recent end of each of my agents' conversations.
    pub conversations: Vec<Conversation>,
    pub teammate_agents: Vec<SharedAgentView>,
    /// Unresolved needs-attention items.
    pub inbox: Vec<Attention>,
    /// Questions I asked and questions asked of me.
    pub questions: Vec<Question>,
    /// Requests I sent and requests to my agents.
    pub requests: Vec<WorkRequest>,
    pub approvals: Vec<Approval>,
    pub rules: Vec<Rule>,
    pub harnesses: Vec<HarnessStatus>,
    pub settings: Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Language {
    Ko,
    En,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct NotificationSettings {
    pub enabled: bool,
    pub muted_until: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Settings {
    /// `None` follows the macOS language (D-033).
    pub language: Option<Language>,
    pub notifications: NotificationSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: None,
            notifications: NotificationSettings {
                enabled: true,
                muted_until: None,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Take a snapshot; events with a higher `seq` follow as `event` notifications.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SnapshotGet {}

/// Replaces all settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SettingsUpdate {
    pub settings: Settings,
}

commands! {
    SnapshotGet => "snapshot" -> Snapshot;
    SettingsUpdate => "settings_update" -> Settings;
}

/// `settings.update`: settings changed from any window or the CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SettingsUpdateEvent {
    pub settings: Settings,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::roundtrip;
    use crate::{agent, ask, harness, peer, policy, request, team};

    #[test]
    fn a_full_snapshot_round_trips() {
        let team = team::samples::team();
        let (me, mate) = (team.members[0].id, team.members[1].id);
        let agent = agent::samples::agent();
        let snapshot = Snapshot {
            seq: Seq(42),
            agents: vec![agent.clone()],
            shares: vec![policy::ShareSettings::private(agent.id)],
            schedules: vec![agent::samples::schedule(agent.id)],
            conversations: vec![harness::samples::conversation(agent.id)],
            teammate_agents: vec![peer::samples::shared_agent(mate)],
            inbox: vec![agent::samples::attention(agent.id)],
            questions: vec![ask::samples::question(mate, me, agent.id)],
            requests: vec![request::samples::work_request(me, agent.id)],
            approvals: vec![policy::samples::approval(agent.id, mate)],
            rules: vec![policy::samples::rule(agent.id, mate)],
            harnesses: vec![agent::samples::harness_status()],
            settings: Settings {
                language: Some(Language::Ko),
                ..Settings::default()
            },
            team: Some(team),
        };
        let json = roundtrip(&snapshot);
        assert_eq!(json["seq"], 42);
        assert_eq!(json["settings"]["language"], "ko");
    }

    #[test]
    fn an_empty_first_run_snapshot_round_trips() {
        let snapshot = Snapshot {
            seq: Seq(0),
            team: None,
            agents: vec![],
            shares: vec![],
            schedules: vec![],
            conversations: vec![],
            teammate_agents: vec![],
            inbox: vec![],
            questions: vec![],
            requests: vec![],
            approvals: vec![],
            rules: vec![],
            harnesses: vec![],
            settings: Settings::default(),
        };
        assert!(roundtrip(&snapshot)["team"].is_null());
    }
}
