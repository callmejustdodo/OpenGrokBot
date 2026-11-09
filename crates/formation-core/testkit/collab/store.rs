//! Owner: B. `MemoryCollabStore`: B's repositories in memory, until B's SQLite
//! repositories exist (TASK-017, TASK-019, TASK-023, TASK-025).

use std::collections::HashMap;
use std::sync::Mutex;

use formation_proto::ask::Question;
use formation_proto::common::{
    AgentId, ApprovalId, ErrorCode, MemberId, MessageId, QuestionId, RequestId, RuleId, SessionId,
    Timestamp,
};
use formation_proto::policy::{Approval, ApprovalStatus, Rule, ShareSettings};
use formation_proto::request::{QueueItem, WorkRequest};
use formation_proto::team::{Invitation, Member};

use crate::ports::{
    ApprovalRepo, QuestionRepo, RequestRepo, Result, RuleRepo, ShareRepo, TeamRepo,
};

use super::{error, lock};

/// A [`CollabStore`](crate::ports::CollabStore) in memory.
///
/// It keeps the rules the repositories document: sharing is private until stored;
/// `members` and `member_by_tailscale` skip removed members (`member` still finds
/// them, for names in history); `insert_request` returns the stored request for a
/// repeated (requester, idempotency key). It also keeps the uniqueness a SQLite
/// schema would: GitHub login and Tailscale login among current members,
/// invitation codes, and idempotency keys, with `conflict` on a clash. Deleting
/// something that is not there gives `not_found`. Lists come back oldest first
/// (by `joined_at`, `created_at`, or `queued_at`).
#[derive(Debug, Default)]
pub struct MemoryCollabStore {
    state: Mutex<StoreState>,
}

#[derive(Debug, Default)]
struct StoreState {
    me: Option<MemberId>,
    team: Option<crate::ports::TeamRecord>,
    members: Vec<Member>,
    removed: HashMap<MemberId, Timestamp>,
    invitations: Vec<Invitation>,
    shares: Vec<ShareSettings>,
    questions: Vec<Question>,
    fork_sessions: HashMap<QuestionId, SessionId>,
    requests: Vec<WorkRequest>,
    queue: Vec<QueueItem>,
    approvals: Vec<Approval>,
    rules: Vec<Rule>,
}

impl MemoryCollabStore {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Replaces the item with the same key, or appends it.
fn upsert<T, K: PartialEq>(items: &mut Vec<T>, item: T, key: impl Fn(&T) -> K) {
    let k = key(&item);
    match items.iter_mut().find(|existing| key(existing) == k) {
        Some(existing) => *existing = item,
        None => items.push(item),
    }
}

fn sorted<T: Clone, K: Ord>(items: impl Iterator<Item = T>, key: impl Fn(&T) -> K) -> Vec<T> {
    let mut items: Vec<T> = items.collect();
    items.sort_by_key(|item| key(item));
    items
}

impl StoreState {
    fn is_current(&self, member: &Member) -> bool {
        !self.removed.contains_key(&member.id)
    }
}

impl TeamRepo for MemoryCollabStore {
    fn me(&self) -> Result<Option<MemberId>> {
        Ok(lock(&self.state).me)
    }

    fn set_me(&self, id: MemberId) -> Result<()> {
        lock(&self.state).me = Some(id);
        Ok(())
    }

    fn team(&self) -> Result<Option<crate::ports::TeamRecord>> {
        Ok(lock(&self.state).team.clone())
    }

    fn put_team(&self, team: Option<&crate::ports::TeamRecord>) -> Result<()> {
        lock(&self.state).team = team.cloned();
        Ok(())
    }

    fn members(&self) -> Result<Vec<Member>> {
        let state = lock(&self.state);
        let current = state
            .members
            .iter()
            .filter(|m| state.is_current(m))
            .cloned();
        Ok(sorted(current, |m| m.joined_at))
    }

    fn member(&self, id: MemberId) -> Result<Option<Member>> {
        Ok(lock(&self.state)
            .members
            .iter()
            .find(|m| m.id == id)
            .cloned())
    }

    fn member_by_tailscale(&self, login: &str) -> Result<Option<Member>> {
        let state = lock(&self.state);
        Ok(state
            .members
            .iter()
            .find(|m| m.tailscale_login == login && state.is_current(m))
            .cloned())
    }

    /// `conflict` when another current member has the same GitHub or Tailscale login.
    fn put_member(&self, member: &Member) -> Result<()> {
        let mut state = lock(&self.state);
        let clash = state.members.iter().find(|m| {
            m.id != member.id
                && state.is_current(m)
                && (m.github_login == member.github_login
                    || m.tailscale_login == member.tailscale_login)
        });
        if let Some(other) = clash {
            return Err(error(
                ErrorCode::Conflict,
                format!("member {} has the same login", other.id),
            ));
        }
        upsert(&mut state.members, member.clone(), |m| m.id);
        Ok(())
    }

    /// Keeps the first removal time when called again.
    fn remove_member(&self, id: MemberId, at: Timestamp) -> Result<()> {
        let mut state = lock(&self.state);
        if !state.members.iter().any(|m| m.id == id) {
            return Err(error(ErrorCode::NotFound, format!("no member {id}")));
        }
        state.removed.entry(id).or_insert(at);
        Ok(())
    }

    /// `conflict` when another invitation has the same code.
    fn put_invitation(&self, invitation: &Invitation) -> Result<()> {
        let mut state = lock(&self.state);
        if state
            .invitations
            .iter()
            .any(|i| i.id != invitation.id && i.code == invitation.code)
        {
            return Err(error(ErrorCode::Conflict, "invitation code in use"));
        }
        upsert(&mut state.invitations, invitation.clone(), |i| i.id);
        Ok(())
    }

    fn invitation_by_code(&self, code: &str) -> Result<Option<Invitation>> {
        Ok(lock(&self.state)
            .invitations
            .iter()
            .find(|i| i.code == code)
            .cloned())
    }
}

impl ShareRepo for MemoryCollabStore {
    fn share(&self, agent_id: AgentId) -> Result<ShareSettings> {
        Ok(lock(&self.state)
            .shares
            .iter()
            .find(|s| s.agent_id == agent_id)
            .cloned()
            .unwrap_or_else(|| ShareSettings::private(agent_id)))
    }

    fn shares(&self) -> Result<Vec<ShareSettings>> {
        Ok(lock(&self.state).shares.clone())
    }

    fn put_share(&self, share: &ShareSettings) -> Result<()> {
        upsert(&mut lock(&self.state).shares, share.clone(), |s| s.agent_id);
        Ok(())
    }
}

impl QuestionRepo for MemoryCollabStore {
    fn question(&self, id: QuestionId) -> Result<Option<Question>> {
        Ok(lock(&self.state)
            .questions
            .iter()
            .find(|q| q.id == id)
            .cloned())
    }

    fn questions(&self) -> Result<Vec<Question>> {
        let state = lock(&self.state);
        Ok(sorted(state.questions.iter().cloned(), |q| q.created_at))
    }

    fn put_question(&self, question: &Question) -> Result<()> {
        upsert(&mut lock(&self.state).questions, question.clone(), |q| q.id);
        Ok(())
    }

    fn delete_question(&self, id: QuestionId) -> Result<()> {
        let mut state = lock(&self.state);
        let before = state.questions.len();
        state.questions.retain(|q| q.id != id);
        if state.questions.len() == before {
            return Err(error(ErrorCode::NotFound, format!("no question {id}")));
        }
        Ok(())
    }

    fn fork_session(&self, thread_id: QuestionId) -> Result<Option<SessionId>> {
        Ok(lock(&self.state).fork_sessions.get(&thread_id).cloned())
    }

    fn put_fork_session(&self, thread_id: QuestionId, session_id: &SessionId) -> Result<()> {
        lock(&self.state)
            .fork_sessions
            .insert(thread_id, session_id.clone());
        Ok(())
    }
}

impl RequestRepo for MemoryCollabStore {
    /// `conflict` when the ID exists under another idempotency key.
    fn insert_request(&self, request: &WorkRequest) -> Result<(WorkRequest, bool)> {
        let mut state = lock(&self.state);
        if let Some(stored) = state.requests.iter().find(|r| {
            r.requester == request.requester && r.idempotency_key == request.idempotency_key
        }) {
            return Ok((stored.clone(), false));
        }
        if state.requests.iter().any(|r| r.id == request.id) {
            return Err(error(
                ErrorCode::Conflict,
                format!("request {} exists with another idempotency key", request.id),
            ));
        }
        state.requests.push(request.clone());
        Ok((request.clone(), true))
    }

    fn request(&self, id: RequestId) -> Result<Option<WorkRequest>> {
        Ok(lock(&self.state)
            .requests
            .iter()
            .find(|r| r.id == id)
            .cloned())
    }

    fn requests(&self) -> Result<Vec<WorkRequest>> {
        let state = lock(&self.state);
        Ok(sorted(state.requests.iter().cloned(), |r| r.created_at))
    }

    /// `conflict` when the change would give two requests the same (requester,
    /// idempotency key).
    fn put_request(&self, request: &WorkRequest) -> Result<()> {
        let mut state = lock(&self.state);
        if state.requests.iter().any(|r| {
            r.id != request.id
                && r.requester == request.requester
                && r.idempotency_key == request.idempotency_key
        }) {
            return Err(error(ErrorCode::Conflict, "idempotency key in use"));
        }
        upsert(&mut state.requests, request.clone(), |r| r.id);
        Ok(())
    }

    fn queue_items(&self, agent_id: AgentId) -> Result<Vec<QueueItem>> {
        let state = lock(&self.state);
        let items = state
            .queue
            .iter()
            .filter(|i| i.agent_id == agent_id)
            .cloned();
        Ok(sorted(items, |i| i.queued_at))
    }

    fn put_queue_item(&self, item: &QueueItem) -> Result<()> {
        upsert(&mut lock(&self.state).queue, item.clone(), |i| i.id);
        Ok(())
    }

    fn remove_queue_item(&self, id: MessageId) -> Result<()> {
        let mut state = lock(&self.state);
        let before = state.queue.len();
        state.queue.retain(|i| i.id != id);
        if state.queue.len() == before {
            return Err(error(
                ErrorCode::NotFound,
                format!("no queued message {id}"),
            ));
        }
        Ok(())
    }
}

impl ApprovalRepo for MemoryCollabStore {
    fn approval(&self, id: ApprovalId) -> Result<Option<Approval>> {
        Ok(lock(&self.state)
            .approvals
            .iter()
            .find(|a| a.id == id)
            .cloned())
    }

    fn approvals(&self, status: Option<ApprovalStatus>) -> Result<Vec<Approval>> {
        let state = lock(&self.state);
        let matching = state
            .approvals
            .iter()
            .filter(|a| status.is_none_or(|status| a.status == status))
            .cloned();
        Ok(sorted(matching, |a| a.created_at))
    }

    fn put_approval(&self, approval: &Approval) -> Result<()> {
        upsert(&mut lock(&self.state).approvals, approval.clone(), |a| a.id);
        Ok(())
    }
}

impl RuleRepo for MemoryCollabStore {
    fn rules(&self, agent_id: Option<AgentId>) -> Result<Vec<Rule>> {
        let state = lock(&self.state);
        let matching = state
            .rules
            .iter()
            .filter(|r| agent_id.is_none_or(|agent_id| r.agent_id == agent_id))
            .cloned();
        Ok(sorted(matching, |r| r.created_at))
    }

    fn rule(&self, id: RuleId) -> Result<Option<Rule>> {
        Ok(lock(&self.state).rules.iter().find(|r| r.id == id).cloned())
    }

    fn put_rule(&self, rule: &Rule) -> Result<()> {
        upsert(&mut lock(&self.state).rules, rule.clone(), |r| r.id);
        Ok(())
    }

    /// Counts the use whatever the limits say; the policy checks them first.
    fn record_rule_use(&self, id: RuleId) -> Result<Rule> {
        let mut state = lock(&self.state);
        let rule = state
            .rules
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| error(ErrorCode::NotFound, format!("no rule {id}")))?;
        rule.uses = rule.uses.saturating_add(1);
        Ok(rule.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use formation_proto::ask::{AskTarget, QuestionStatus};
    use formation_proto::common::InvitationId;
    use formation_proto::policy::{RuleScope, ToolCall, Visibility};
    use formation_proto::request::QueueSource;
    use formation_proto::team::MemberRole;

    use super::super::samples::{member, work_request};
    use super::super::{later, now};
    use super::*;
    use crate::ports::CollabStore;

    fn store() -> Arc<dyn CollabStore> {
        Arc::new(MemoryCollabStore::new())
    }

    #[test]
    fn team_hides_removed_members_and_keeps_logins_unique() {
        let store = store();
        assert_eq!(store.me().unwrap(), None);
        let (dohyeon, minsu) = (
            member("dohyeon", MemberRole::Creator),
            member("minsu", MemberRole::Member),
        );
        store.set_me(dohyeon.id).unwrap();
        store.put_member(&dohyeon).unwrap();
        store.put_member(&minsu).unwrap();
        assert_eq!(store.me().unwrap(), Some(dohyeon.id));
        assert_eq!(
            store.members().unwrap(),
            vec![dohyeon.clone(), minsu.clone()]
        );
        assert_eq!(
            store.member_by_tailscale("minsu@github").unwrap(),
            Some(minsu.clone())
        );

        let mut impostor = member("minsu", MemberRole::Member);
        impostor.github_login = "someone-else".into();
        let clash = store.put_member(&impostor).unwrap_err();
        assert_eq!(clash.code, ErrorCode::Conflict);

        let mut renamed = minsu.clone();
        renamed.display_name = "Minsu Kim".into();
        store.put_member(&renamed).unwrap();
        assert_eq!(
            store.member(minsu.id).unwrap().unwrap().display_name,
            "Minsu Kim"
        );

        store.remove_member(minsu.id, now()).unwrap();
        assert_eq!(store.members().unwrap(), vec![dohyeon.clone()]);
        assert_eq!(store.member_by_tailscale("minsu@github").unwrap(), None);
        assert!(store.member(minsu.id).unwrap().is_some());
        // A removed member's login can be taken again.
        store.put_member(&impostor).unwrap();
        let missing = store
            .remove_member(MemberId::generate(), now())
            .unwrap_err();
        assert_eq!(missing.code, ErrorCode::NotFound);
    }

    #[test]
    fn invitations_are_found_by_unique_code() {
        let store = store();
        let creator = MemberId::generate();
        let invitation = Invitation {
            id: InvitationId::generate(),
            code: "fmn1-abcd".into(),
            created_by: creator,
            expires_at: later(now(), Duration::from_secs(3600)),
            used_by: None,
            used_at: None,
            link: String::new(),
        };
        store.put_invitation(&invitation).unwrap();
        assert_eq!(
            store.invitation_by_code("fmn1-abcd").unwrap(),
            Some(invitation.clone())
        );
        assert_eq!(store.invitation_by_code("nope").unwrap(), None);
        let duplicate = Invitation {
            id: InvitationId::generate(),
            ..invitation.clone()
        };
        assert_eq!(
            store.put_invitation(&duplicate).unwrap_err().code,
            ErrorCode::Conflict
        );
        let used = Invitation {
            used_by: Some(MemberId::generate()),
            used_at: Some(now()),
            ..invitation
        };
        store.put_invitation(&used).unwrap();
        assert!(
            store
                .invitation_by_code("fmn1-abcd")
                .unwrap()
                .unwrap()
                .used_by
                .is_some()
        );
    }

    #[test]
    fn sharing_is_private_until_stored() {
        let store = store();
        let agent = AgentId::generate();
        assert_eq!(store.share(agent).unwrap(), ShareSettings::private(agent));
        assert!(store.shares().unwrap().is_empty());
        let shared = ShareSettings {
            shared: true,
            visibility: Visibility::Answer,
            ..ShareSettings::private(agent)
        };
        store.put_share(&shared).unwrap();
        store.put_share(&shared).unwrap();
        assert_eq!(store.share(agent).unwrap(), shared);
        assert_eq!(store.shares().unwrap(), vec![shared]);
    }

    #[test]
    fn questions_and_fork_sessions() {
        let store = store();
        let id = QuestionId::generate();
        let question = Question {
            id,
            thread_id: id,
            asker: MemberId::generate(),
            target: AskTarget::Person {
                member_id: MemberId::generate(),
            },
            text: "Is the login API done?".into(),
            status: QuestionStatus::Received,
            candidates: vec![],
            agent_id: None,
            answer: None,
            error: None,
            created_at: now(),
            updated_at: now(),
        };
        store.put_question(&question).unwrap();
        let answering = Question {
            status: QuestionStatus::Answering,
            ..question.clone()
        };
        store.put_question(&answering).unwrap();
        assert_eq!(store.questions().unwrap(), vec![answering.clone()]);
        assert_eq!(store.question(id).unwrap(), Some(answering));

        assert_eq!(store.fork_session(id).unwrap(), None);
        let session = SessionId("fork-1".into());
        store.put_fork_session(id, &session).unwrap();
        assert_eq!(store.fork_session(id).unwrap(), Some(session));

        store.delete_question(id).unwrap();
        assert_eq!(store.question(id).unwrap(), None);
        assert_eq!(
            store.delete_question(id).unwrap_err().code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn a_retried_request_is_inserted_once() {
        let store = store();
        let (requester, owner, agent) = (
            MemberId::generate(),
            MemberId::generate(),
            AgentId::generate(),
        );
        let request = work_request(requester, owner, agent, "Add a health endpoint", "k1");
        assert_eq!(
            store.insert_request(&request).unwrap(),
            (request.clone(), true)
        );

        let retry = WorkRequest {
            id: RequestId::generate(),
            ..request.clone()
        };
        assert_eq!(
            store.insert_request(&retry).unwrap(),
            (request.clone(), false)
        );
        let other_key = WorkRequest {
            idempotency_key: "k2".into(),
            ..request.clone()
        };
        assert_eq!(
            store.insert_request(&other_key).unwrap_err().code,
            ErrorCode::Conflict
        );
        // Another requester may reuse the key.
        let someone_else = work_request(MemberId::generate(), owner, agent, "p", "k1");
        assert!(store.insert_request(&someone_else).unwrap().1);
        assert_eq!(store.requests().unwrap().len(), 2);

        let mut queued = request.clone();
        queued.status = formation_proto::request::RequestStatus::Queued;
        store.put_request(&queued).unwrap();
        assert_eq!(store.request(request.id).unwrap(), Some(queued));
        let mut stolen = someone_else.clone();
        stolen.requester = requester;
        assert_eq!(
            store.put_request(&stolen).unwrap_err().code,
            ErrorCode::Conflict
        );
    }

    #[test]
    fn queue_items_survive_in_order_per_agent() {
        let store = store();
        let (agent, other) = (AgentId::generate(), AgentId::generate());
        let t0 = now();
        let item = |agent_id, seconds| QueueItem {
            id: MessageId::generate(),
            agent_id,
            source: QueueSource::Owner,
            text: format!("at {seconds}"),
            queued_at: later(t0, Duration::from_secs(seconds)),
        };
        let (late, early, elsewhere) = (item(agent, 2), item(agent, 1), item(other, 0));
        for queued in [&late, &early, &elsewhere] {
            store.put_queue_item(queued).unwrap();
        }
        assert_eq!(
            store.queue_items(agent).unwrap(),
            vec![early.clone(), late.clone()]
        );
        store.remove_queue_item(early.id).unwrap();
        assert_eq!(store.queue_items(agent).unwrap(), vec![late]);
        assert_eq!(
            store.remove_queue_item(early.id).unwrap_err().code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn approvals_filter_by_status_and_rules_count_uses() {
        let store = store();
        let agent = AgentId::generate();
        let pending = Approval {
            id: ApprovalId::generate(),
            agent_id: agent,
            request_id: None,
            requester: None,
            requester_agent: None,
            purpose: None,
            call: ToolCall {
                tool_use_id: None,
                tool_name: "Bash".into(),
                input: serde_json::json!({"command": "npm test"}),
            },
            summary: "npm test".into(),
            proposed_rule: None,
            status: ApprovalStatus::Pending,
            choice: None,
            created_at: now(),
            decided_at: None,
            expires_at: now(),
        };
        let denied = Approval {
            id: ApprovalId::generate(),
            status: ApprovalStatus::Denied,
            ..pending.clone()
        };
        store.put_approval(&pending).unwrap();
        store.put_approval(&denied).unwrap();
        assert_eq!(store.approvals(None).unwrap().len(), 2);
        assert_eq!(
            store.approvals(Some(ApprovalStatus::Pending)).unwrap(),
            vec![pending.clone()]
        );
        assert_eq!(store.approval(denied.id).unwrap(), Some(denied));

        let rule = Rule {
            id: RuleId::generate(),
            requester: MemberId::generate(),
            agent_id: agent,
            scope: RuleScope::All,
            expires_at: None,
            max_uses: Some(1),
            uses: 0,
            created_at: now(),
            revoked_at: None,
        };
        store.put_rule(&rule).unwrap();
        assert_eq!(store.rules(Some(agent)).unwrap(), vec![rule.clone()]);
        assert!(store.rules(Some(AgentId::generate())).unwrap().is_empty());
        assert_eq!(store.record_rule_use(rule.id).unwrap().uses, 1);
        assert_eq!(store.rule(rule.id).unwrap().unwrap().uses, 1);
        let missing = store.record_rule_use(RuleId::generate()).unwrap_err();
        assert_eq!(missing.code, ErrorCode::NotFound);
    }
}
