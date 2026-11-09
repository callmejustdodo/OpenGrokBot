//! Owner: B. Who is online (FR-016, US-018: the asker sees the owner is offline
//! before sending).
//!
//! A teammate's Mac counts as online while this Mac's `WS /v1/events` connection
//! to it is open. The owner's Mac sends a [`hello`] as the first message of every
//! connection and again whenever its shared agents change; this Mac marks the
//! teammate offline, with the time it was last heard from, when the connection
//! closes or cannot open. [`PresenceBook`] keeps the last state per teammate and
//! returns an event only when something the app shows changed.

use std::collections::HashMap;

use formation_proto::common::{MemberId, Timestamp};
use formation_proto::peer::{PresenceEvent, SharedAgentView};

/// The presence message a Mac sends about itself: online, with the caller's view
/// of its shared agents.
pub fn hello(me: MemberId, agents: Vec<SharedAgentView>, now: Timestamp) -> PresenceEvent {
    PresenceEvent {
        member_id: me,
        online: true,
        last_seen_at: Some(now),
        agents,
    }
}

#[derive(Debug, Default)]
pub struct PresenceBook {
    members: HashMap<MemberId, PresenceEvent>,
}

impl PresenceBook {
    /// Applies a presence message that arrived on `from`'s connection, stamped
    /// `at` (this Mac's clock). A Mac speaks only for itself, so a message about
    /// another member is dropped. Returns the event to publish when the member
    /// came online or their agents changed.
    pub fn received(
        &mut self,
        from: MemberId,
        presence: PresenceEvent,
        at: Timestamp,
    ) -> Option<PresenceEvent> {
        if presence.member_id != from {
            return None;
        }
        let next = PresenceEvent {
            member_id: from,
            online: presence.online,
            last_seen_at: Some(at),
            agents: presence.agents,
        };
        let changed = self
            .members
            .get(&from)
            .is_none_or(|last| last.online != next.online || last.agents != next.agents);
        self.members.insert(from, next.clone());
        changed.then_some(next)
    }

    /// Something else arrived from `from` at `at`: they are still there.
    pub fn heard(&mut self, from: MemberId, at: Timestamp) {
        if let Some(last) = self.members.get_mut(&from)
            && last.online
        {
            last.last_seen_at = Some(at);
        }
    }

    /// The connection to `member` closed or could not open. Returns the event to
    /// publish the first time: the member went offline, or was never reached.
    pub fn lost(&mut self, member: MemberId) -> Option<PresenceEvent> {
        match self.members.get_mut(&member) {
            Some(last) if last.online => {
                last.online = false;
                Some(last.clone())
            }
            Some(_) => None,
            None => {
                let never = PresenceEvent {
                    member_id: member,
                    online: false,
                    last_seen_at: None,
                    agents: Vec::new(),
                };
                self.members.insert(member, never.clone());
                Some(never)
            }
        }
    }

    pub fn is_online(&self, member: MemberId) -> bool {
        self.members.get(&member).is_some_and(|p| p.online)
    }

    pub fn get(&self, member: MemberId) -> Option<&PresenceEvent> {
        self.members.get(&member)
    }

    pub fn all(&self) -> Vec<PresenceEvent> {
        self.members.values().cloned().collect()
    }

    /// The member left the team.
    pub fn forget(&mut self, member: MemberId) {
        self.members.remove(&member);
    }
}

#[cfg(test)]
mod tests {
    use formation_proto::agent::{AgentState, Avatar, HarnessKind};
    use formation_proto::common::AgentId;

    use super::*;
    use crate::peer::outbox::now;

    fn agent(owner: MemberId) -> SharedAgentView {
        SharedAgentView {
            owner,
            agent_id: AgentId::generate(),
            slug: "api".into(),
            name: "API".into(),
            role: "Backend developer".into(),
            harness: HarnessKind::ClaudeCode,
            avatar: Avatar {
                shape: "square".into(),
                color: "green".into(),
            },
            state: AgentState::Idle,
            last_activity_at: None,
            accepts_requests: false,
        }
    }

    #[test]
    fn online_offline_and_agent_changes_each_publish_once() {
        let mut book = PresenceBook::default();
        let mate = MemberId::generate();
        let (t1, t2) = (now(), now() + secs(5));

        let first = hello(mate, vec![agent(mate)], t1);
        let event = book.received(mate, first.clone(), t1).expect("came online");
        assert!(event.online);
        assert!(book.is_online(mate));
        assert_eq!(book.received(mate, first.clone(), t2), None, "nothing new");
        book.heard(mate, t2);
        assert_eq!(book.get(mate).unwrap().last_seen_at, Some(t2));

        let more = hello(mate, vec![agent(mate), agent(mate)], t2);
        assert_eq!(book.received(mate, more, t2).unwrap().agents.len(), 2);

        let gone = book.lost(mate).expect("went offline");
        assert!(!gone.online);
        assert_eq!(gone.last_seen_at, Some(t2));
        assert_eq!(gone.agents.len(), 2, "last seen agents stay");
        assert_eq!(book.lost(mate), None, "already offline");
    }

    #[test]
    fn a_teammate_never_reached_is_reported_offline_once() {
        let mut book = PresenceBook::default();
        let mate = MemberId::generate();
        let event = book.lost(mate).unwrap();
        assert!(!event.online);
        assert_eq!(event.last_seen_at, None);
        assert_eq!(book.lost(mate), None);
        book.forget(mate);
        assert!(book.all().is_empty());
    }

    #[test]
    fn a_mac_cannot_speak_for_another_member() {
        let mut book = PresenceBook::default();
        let (mate, other) = (MemberId::generate(), MemberId::generate());
        assert_eq!(
            book.received(mate, hello(other, vec![], now()), now()),
            None
        );
        assert!(!book.is_online(other));
    }

    fn secs(seconds: u64) -> std::time::Duration {
        std::time::Duration::from_secs(seconds)
    }
}
