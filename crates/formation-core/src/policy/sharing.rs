//! Sharing an agent with the team (FR-009, US-009, D-025, D-026).
//!
//! Every agent is private until its owner shares it. A private agent is never
//! listed, routed to, or answered from (backend architecture, "Where each rule
//! is enforced"); everything a teammate can see of this Mac's agents goes through
//! [`visible_agents`] or [`shared_agents`]. Once shared, teammates get the answer
//! and the excerpts it used (`answer_excerpts`) unless the owner chooses the
//! answer only; work requests stay off until the owner turns them on.

use formation_proto::agent::Agent;
use formation_proto::common::{AgentId, ApiError, ErrorCode, MemberId};
use formation_proto::peer::SharedAgentView;
use formation_proto::policy::{SharePreviewResult, ShareSettings, ShareUpdate, Visibility};

use crate::ports::{AgentRepo, Result, ShareRepo};

/// Applies the fields an update sets; `None` keeps the current value.
pub fn apply_update(current: &ShareSettings, update: &ShareUpdate) -> ShareSettings {
    ShareSettings {
        agent_id: current.agent_id,
        shared: update.shared.unwrap_or(current.shared),
        visibility: update.visibility.unwrap_or(current.visibility),
        accepts_requests: update.accepts_requests.unwrap_or(current.accepts_requests),
    }
}

/// One stored change of an agent's sharing, so the caller can publish
/// `share.update` and act on what stopped (US-009).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareChange {
    pub before: ShareSettings,
    pub after: ShareSettings,
}

impl ShareChange {
    pub fn changed(&self) -> bool {
        self.before != self.after
    }

    /// The agent went private: new questions and requests are refused from now on
    /// and pending ones must be canceled. A running task continues unless the owner
    /// stops it (FR-009).
    pub fn unshared(&self) -> bool {
        self.before.shared && !self.after.shared
    }

    /// Teammates could send work requests before and cannot now, because the agent
    /// was unshared or requests were turned off; pending requests must be canceled.
    pub fn requests_stopped(&self) -> bool {
        accepts_requests(&self.before) && !accepts_requests(&self.after)
    }
}

/// Whether teammates may send this agent work requests: shared and turned on.
pub fn accepts_requests(share: &ShareSettings) -> bool {
    share.shared && share.accepts_requests
}

/// `share_update`: applies and stores the update. `not_found` for an agent this Mac
/// does not have.
pub fn update_share<A, S>(agents: &A, shares: &S, update: &ShareUpdate) -> Result<ShareChange>
where
    A: AgentRepo + ?Sized,
    S: ShareRepo + ?Sized,
{
    require_agent(agents, update.agent_id)?;
    let before = shares.share(update.agent_id)?;
    let after = apply_update(&before, update);
    if after != before {
        shares.put_share(&after)?;
    }
    Ok(ShareChange { before, after })
}

/// `share_preview`: what a teammate would see of the agent with its current
/// settings, shown before sharing, so it is built even while the agent is private.
/// Answers follow the returned visibility: with `answer`, excerpts are dropped
/// ([`crate::ask::answer`]); masking applies either way (NFR-003).
pub fn share_preview<A, S>(
    owner: MemberId,
    agents: &A,
    shares: &S,
    agent_id: AgentId,
) -> Result<SharePreviewResult>
where
    A: AgentRepo + ?Sized,
    S: ShareRepo + ?Sized,
{
    let agent = require_agent(agents, agent_id)?;
    let share = shares.share(agent_id)?;
    Ok(SharePreviewResult {
        view: teammate_view(owner, &agent, &share),
        visibility: share.visibility,
    })
}

/// The teammate's view of a shared agent; `None` for a private one.
pub fn shared_view(
    owner: MemberId,
    agent: &Agent,
    share: &ShareSettings,
) -> Option<SharedAgentView> {
    (share.agent_id == agent.id && share.shared).then(|| teammate_view(owner, agent, share))
}

/// The caller's view of this Mac's agents (`GET /v1/agents`, `teammate_agents`):
/// shared agents only, in the order given. An agent without stored settings is
/// private.
pub fn visible_agents(
    owner: MemberId,
    agents: &[Agent],
    shares: &[ShareSettings],
) -> Vec<SharedAgentView> {
    agents
        .iter()
        .filter_map(|agent| shared_view(owner, agent, &share_of(agent.id, shares)))
        .collect()
}

/// The agents routing may consider and answer from, with their settings: shared
/// agents only (FR-010).
pub fn shared_agents(agents: &[Agent], shares: &[ShareSettings]) -> Vec<(Agent, ShareSettings)> {
    agents
        .iter()
        .map(|agent| (agent, share_of(agent.id, shares)))
        .filter(|(_, share)| share.shared)
        .map(|(agent, share)| (agent.clone(), share))
        .collect()
}

/// `agent_not_shared` unless the agent is shared (tech spec, ask pipeline step 2).
pub fn require_shared(share: &ShareSettings) -> Result<()> {
    if share.shared {
        Ok(())
    } else {
        Err(ApiError::new(
            ErrorCode::AgentNotShared,
            "the agent is not shared",
        ))
    }
}

/// `requests_not_accepted` unless the agent is shared and accepts work requests.
pub fn require_requests(share: &ShareSettings) -> Result<()> {
    require_shared(share)?;
    if share.accepts_requests {
        Ok(())
    } else {
        Err(ApiError::new(
            ErrorCode::RequestsNotAccepted,
            "the agent does not accept work requests",
        ))
    }
}

/// Whether teammates may see the excerpts an answer used (D-025).
pub fn shows_excerpts(visibility: Visibility) -> bool {
    matches!(visibility, Visibility::AnswerExcerpts)
}

fn teammate_view(owner: MemberId, agent: &Agent, share: &ShareSettings) -> SharedAgentView {
    SharedAgentView {
        owner,
        agent_id: agent.id,
        slug: agent.slug.clone(),
        name: agent.name.clone(),
        role: agent.role.clone(),
        harness: agent.harness,
        avatar: agent.avatar.clone(),
        state: agent.state,
        last_activity_at: agent.last_activity_at,
        accepts_requests: share.accepts_requests,
    }
}

fn share_of(agent_id: AgentId, shares: &[ShareSettings]) -> ShareSettings {
    shares
        .iter()
        .find(|share| share.agent_id == agent_id)
        .cloned()
        .unwrap_or_else(|| ShareSettings::private(agent_id))
}

fn require_agent<A: AgentRepo + ?Sized>(agents: &A, agent_id: AgentId) -> Result<Agent> {
    agents
        .agent(agent_id)?
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "no such agent"))
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use formation_proto::agent::{AgentState, Avatar, HarnessKind};
    use formation_proto::common::Timestamp;

    use super::*;

    pub fn agent(slug: &str, role: &str) -> Agent {
        Agent {
            id: AgentId::generate(),
            slug: slug.into(),
            name: slug.into(),
            role: role.into(),
            harness: HarnessKind::ClaudeCode,
            folder_path: format!("/agents/{slug}").into(),
            repositories: vec![],
            avatar: Avatar {
                shape: "circle".into(),
                color: "blue".into(),
            },
            state: AgentState::Idle,
            files_revision: "1".into(),
            created_at: Timestamp::default(),
            last_activity_at: None,
            detected: None,
            running_requests: vec![],
        }
    }

    /// `ShareRepo` in memory, for this module's tests until the testkit's
    /// in-memory `CollabStore` (TASK-042) lands.
    #[derive(Default)]
    pub struct Shares(pub Mutex<HashMap<AgentId, ShareSettings>>);

    impl ShareRepo for Shares {
        fn share(&self, agent_id: AgentId) -> Result<ShareSettings> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(&agent_id)
                .cloned()
                .unwrap_or_else(|| ShareSettings::private(agent_id)))
        }
        fn shares(&self) -> Result<Vec<ShareSettings>> {
            Ok(self.0.lock().unwrap().values().cloned().collect())
        }
        fn put_share(&self, share: &ShareSettings) -> Result<()> {
            self.0.lock().unwrap().insert(share.agent_id, share.clone());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{Shares, agent};
    use super::*;
    use crate::testkit::MemoryStore;

    fn update(agent_id: AgentId) -> ShareUpdate {
        ShareUpdate {
            agent_id,
            shared: None,
            visibility: None,
            accepts_requests: None,
        }
    }

    fn store_with(agents: &[&Agent]) -> MemoryStore {
        let store = MemoryStore::default();
        for agent in agents {
            store.insert_agent(agent).unwrap();
        }
        store
    }

    #[test]
    fn a_new_agent_is_private_with_excerpts_and_no_requests() {
        let shares = Shares::default();
        let id = AgentId::generate();
        let share = shares.share(id).unwrap();
        assert!(!share.shared);
        assert_eq!(share.visibility, Visibility::AnswerExcerpts);
        assert!(!accepts_requests(&share));
        assert_eq!(
            require_shared(&share).unwrap_err().code,
            ErrorCode::AgentNotShared
        );
    }

    #[test]
    fn an_update_changes_only_the_fields_it_sets() {
        let current = ShareSettings::private(AgentId::generate());
        let shared = apply_update(
            &current,
            &ShareUpdate {
                shared: Some(true),
                ..update(current.agent_id)
            },
        );
        assert!(shared.shared);
        assert_eq!(shared.visibility, Visibility::AnswerExcerpts);
        assert!(!shared.accepts_requests);
        assert_eq!(apply_update(&shared, &update(shared.agent_id)), shared);
    }

    #[test]
    fn unsharing_stops_questions_and_requests() {
        let api = agent("api", "API developer");
        let store = store_with(&[&api]);
        let shares = Shares::default();
        let on = update_share(
            &store,
            &shares,
            &ShareUpdate {
                shared: Some(true),
                accepts_requests: Some(true),
                ..update(api.id)
            },
        )
        .unwrap();
        assert!(on.changed() && !on.unshared() && !on.requests_stopped());
        assert!(accepts_requests(&shares.share(api.id).unwrap()));

        let off = update_share(
            &store,
            &shares,
            &ShareUpdate {
                shared: Some(false),
                ..update(api.id)
            },
        )
        .unwrap();
        assert!(off.unshared() && off.requests_stopped());
        let stored = shares.share(api.id).unwrap();
        assert_eq!(
            require_requests(&stored).unwrap_err().code,
            ErrorCode::AgentNotShared
        );
        // The owner's choices survive unsharing, for when it is shared again.
        assert!(stored.accepts_requests);

        let again = update_share(&store, &shares, &update(api.id)).unwrap();
        assert!(!again.changed());
    }

    #[test]
    fn turning_requests_off_stops_requests_only() {
        let api = agent("api", "API developer");
        let store = store_with(&[&api]);
        let shares = Shares::default();
        shares
            .put_share(&ShareSettings {
                shared: true,
                accepts_requests: true,
                ..ShareSettings::private(api.id)
            })
            .unwrap();
        let change = update_share(
            &store,
            &shares,
            &ShareUpdate {
                accepts_requests: Some(false),
                ..update(api.id)
            },
        )
        .unwrap();
        assert!(change.requests_stopped() && !change.unshared());
        assert_eq!(
            require_requests(&change.after).unwrap_err().code,
            ErrorCode::RequestsNotAccepted
        );
    }

    #[test]
    fn unknown_agents_cannot_be_shared_or_previewed() {
        let store = MemoryStore::default();
        let shares = Shares::default();
        let id = AgentId::generate();
        let err = update_share(&store, &shares, &update(id)).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        let err = share_preview(MemberId::generate(), &store, &shares, id).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        assert!(shares.shares().unwrap().is_empty());
    }

    #[test]
    fn private_agents_never_appear() {
        let owner = MemberId::generate();
        let (api, web, ops) = (
            agent("api", "API developer"),
            agent("web", "Frontend developer"),
            agent("ops", "Operations"),
        );
        let shares = vec![
            ShareSettings {
                shared: true,
                ..ShareSettings::private(api.id)
            },
            ShareSettings::private(web.id),
            // `ops` has no stored settings: private.
        ];
        let agents = [api.clone(), web.clone(), ops.clone()];
        let visible = visible_agents(owner, &agents, &shares);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].agent_id, api.id);
        assert_eq!(visible[0].owner, owner);
        assert!(!visible[0].accepts_requests);

        let pool = shared_agents(&agents, &shares);
        assert_eq!(pool.len(), 1);
        assert_eq!(pool[0].0.id, api.id);

        // A share stored for another agent never makes this one visible.
        assert!(shared_view(owner, &web, &shares[0]).is_none());
    }

    #[test]
    fn the_preview_shows_a_private_agent_as_teammates_would_see_it() {
        let owner = MemberId::generate();
        let api = agent("api", "API developer");
        let store = store_with(&[&api]);
        let shares = Shares::default();
        let preview = share_preview(owner, &store, &shares, api.id).unwrap();
        assert_eq!(preview.view.agent_id, api.id);
        assert_eq!(preview.view.role, "API developer");
        assert_eq!(preview.visibility, Visibility::AnswerExcerpts);
        assert!(shows_excerpts(preview.visibility));
        // Previewing changes nothing.
        assert!(!shares.share(api.id).unwrap().shared);

        shares
            .put_share(&ShareSettings {
                visibility: Visibility::Answer,
                ..ShareSettings::private(api.id)
            })
            .unwrap();
        let preview = share_preview(owner, &store, &shares, api.id).unwrap();
        assert_eq!(preview.visibility, Visibility::Answer);
        assert!(!shows_excerpts(preview.visibility));
    }
}
