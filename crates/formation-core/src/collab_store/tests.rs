//! The same behaviors as `MemoryCollabStore`'s tests, against SQLite.

use std::sync::Arc;
use std::time::Duration;

use formation_proto::ask::{Answer, AskTarget, QuestionStatus};
use formation_proto::common::{AnswerId, ChainId, InvitationId};
use formation_proto::policy::{Chain, RuleScope, ToolCall, Visibility};
use formation_proto::request::{QueueSource, RequestStatus};
use formation_proto::team::{MemberRole, TeamId};

use super::*;
use crate::ports::{AgentRepo, CollabStore};
use crate::store::MIGRATIONS;

fn store() -> Arc<dyn CollabStore> {
    let sqlite = SqliteStore::in_memory(&[MIGRATIONS, COLLAB_MIGRATIONS]).unwrap();
    Arc::new(SqliteCollabStore::new(Arc::new(sqlite)))
}

fn now() -> Timestamp {
    chrono::Utc::now()
}

fn later(at: Timestamp, by: Duration) -> Timestamp {
    at + chrono::Duration::from_std(by).unwrap()
}

fn member(login: &str, role: MemberRole) -> Member {
    Member {
        id: MemberId::generate(),
        github_login: login.into(),
        display_name: login.into(),
        tailscale_login: format!("{login}@github"),
        tailscale_node: format!("{login}-mbp"),
        role,
        joined_at: now(),
        online: true,
        last_seen_at: Some(now()),
    }
}

fn work_request(
    requester: MemberId,
    owner: MemberId,
    agent_id: AgentId,
    prompt: &str,
    idempotency_key: &str,
) -> WorkRequest {
    let at = now();
    WorkRequest {
        id: RequestId::generate(),
        requester,
        requester_agent: None,
        chain: Chain {
            id: ChainId::generate(),
            depth: 0,
        },
        owner,
        agent_id,
        prompt: prompt.into(),
        idempotency_key: idempotency_key.into(),
        status: RequestStatus::Received,
        result: None,
        error: None,
        created_at: at,
        updated_at: at,
    }
}

#[test]
fn migrations_are_in_b_range_and_rerun_is_a_no_op() {
    assert!(
        COLLAB_MIGRATIONS
            .iter()
            .all(|m| crate::ports::MIGRATIONS_B.contains(&m.version))
    );
    let sqlite = SqliteStore::in_memory(&[MIGRATIONS, COLLAB_MIGRATIONS]).unwrap();
    assert_eq!(sqlite.migrate(&[MIGRATIONS, COLLAB_MIGRATIONS]).unwrap(), 0);
}

#[test]
fn team_record_and_me_round_trip_and_survive_a_reopen() {
    let path = std::env::temp_dir().join(format!("formation-collab-{}.db", AgentId::generate()));
    let creator = member("dohyeon", MemberRole::Creator);
    let record = TeamRecord {
        id: TeamId::generate(),
        creator_host: "dohyeon-mbp.tail0000.ts.net".into(),
        creator_port: 7413,
        roster_version: 3,
        open_invitation: Some("7F3K-Q9PX".into()),
    };
    {
        let sqlite = Arc::new(SqliteStore::open(&path, &[MIGRATIONS, COLLAB_MIGRATIONS]).unwrap());
        let store = SqliteCollabStore::new(sqlite.clone());
        assert_eq!(store.team().unwrap(), None);
        store.put_member(&creator).unwrap();
        store.set_me(creator.id).unwrap();
        store.put_team(Some(&record)).unwrap();
        // The same store is A's `Store` too.
        assert!(sqlite.agents().unwrap().is_empty());
    }
    let sqlite = SqliteStore::open(&path, &[MIGRATIONS, COLLAB_MIGRATIONS]).unwrap();
    let store = SqliteCollabStore::new(Arc::new(sqlite));
    assert_eq!(store.me().unwrap(), Some(creator.id));
    assert_eq!(store.team().unwrap(), Some(record.clone()));
    assert_eq!(store.members().unwrap(), vec![creator]);
    let bumped = TeamRecord {
        roster_version: 4,
        open_invitation: None,
        ..record
    };
    store.put_team(Some(&bumped)).unwrap();
    assert_eq!(store.team().unwrap(), Some(bumped));
    store.put_team(None).unwrap();
    assert_eq!(store.team().unwrap(), None);
    drop(store);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
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

    let removed_at = now();
    store.remove_member(minsu.id, removed_at).unwrap();
    store
        .remove_member(minsu.id, later(removed_at, Duration::from_secs(5)))
        .unwrap();
    assert_eq!(store.members().unwrap(), vec![dohyeon.clone()]);
    assert_eq!(store.member_by_tailscale("minsu@github").unwrap(), None);
    assert!(store.member(minsu.id).unwrap().is_some());
    // A removed member's login can be taken again.
    store.put_member(&impostor).unwrap();
    let missing = store
        .remove_member(MemberId::generate(), now())
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);

    // Storing a removed member again (rejoining) clears the removal.
    let mut dohyeon_left = dohyeon.clone();
    store.remove_member(dohyeon.id, now()).unwrap();
    assert!(
        store
            .member_by_tailscale("dohyeon@github")
            .unwrap()
            .is_none()
    );
    dohyeon_left.online = false;
    store.put_member(&dohyeon_left).unwrap();
    assert_eq!(
        store.member_by_tailscale("dohyeon@github").unwrap(),
        Some(dohyeon_left)
    );
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
        link: "formation://join/fmn1-abcd?host=h&port=7413".into(),
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
    assert_eq!(store.invitation_by_code("fmn1-abcd").unwrap(), Some(used));
}

#[test]
fn sharing_is_private_until_stored() {
    let store = store();
    let (agent, other) = (AgentId::generate(), AgentId::generate());
    assert_eq!(store.share(agent).unwrap(), ShareSettings::private(agent));
    assert!(store.shares().unwrap().is_empty());
    let shared = ShareSettings {
        shared: true,
        visibility: Visibility::Answer,
        accepts_requests: true,
        ..ShareSettings::private(agent)
    };
    store.put_share(&shared).unwrap();
    store.put_share(&ShareSettings::private(other)).unwrap();
    store.put_share(&shared).unwrap();
    assert_eq!(store.share(agent).unwrap(), shared);
    assert_eq!(
        store.shares().unwrap(),
        vec![shared, ShareSettings::private(other)]
    );
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
        text: "Is the login API done? 한글".into(),
        status: QuestionStatus::Received,
        candidates: vec![],
        agent_id: None,
        answer: None,
        error: None,
        created_at: now(),
        updated_at: now(),
    };
    store.put_question(&question).unwrap();
    let answered = Question {
        status: QuestionStatus::Answered,
        agent_id: Some(AgentId::generate()),
        answer: Some(Answer {
            id: AnswerId::generate(),
            agent_id: None,
            text: "Yes, merged this morning.".into(),
            excerpts: vec![],
            as_of: now(),
            possibly_stale: true,
            answered_at: now(),
        }),
        error: Some(ApiError::new(ErrorCode::Internal, "kept as JSON")),
        ..question.clone()
    };
    store.put_question(&answered).unwrap();
    assert_eq!(store.questions().unwrap(), vec![answered.clone()]);
    assert_eq!(store.question(id).unwrap(), Some(answered));

    assert_eq!(store.fork_session(id).unwrap(), None);
    let session = SessionId("fork-1".into());
    store.put_fork_session(id, &session).unwrap();
    let replaced = SessionId("fork-2".into());
    store.put_fork_session(id, &replaced).unwrap();
    assert_eq!(store.fork_session(id).unwrap(), Some(replaced.clone()));

    store.delete_question(id).unwrap();
    assert_eq!(store.question(id).unwrap(), None);
    assert_eq!(
        store.delete_question(id).unwrap_err().code,
        ErrorCode::NotFound
    );
    // The fork session outlives this Mac's copy of the question.
    assert_eq!(store.fork_session(id).unwrap(), Some(replaced));
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
    assert_eq!(
        store.requests().unwrap(),
        vec![request.clone(), someone_else.clone()]
    );

    let mut completed = request.clone();
    completed.status = RequestStatus::Completed;
    completed.result = Some("Done.".into());
    completed.chain.depth = 2;
    store.put_request(&completed).unwrap();
    assert_eq!(store.request(request.id).unwrap(), Some(completed));
    let mut stolen = someone_else.clone();
    stolen.requester = requester;
    assert_eq!(
        store.put_request(&stolen).unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(store.request(RequestId::generate()).unwrap(), None);
}

#[test]
fn queue_items_survive_in_order_per_agent() {
    let store = store();
    let (agent, other) = (AgentId::generate(), AgentId::generate());
    let t0 = now();
    let item = |agent_id, seconds, source| QueueItem {
        id: MessageId::generate(),
        agent_id,
        source,
        text: format!("at {seconds}"),
        queued_at: later(t0, Duration::from_secs(seconds)),
    };
    let request = QueueSource::Request {
        request_id: RequestId::generate(),
        requester: MemberId::generate(),
    };
    let (late, early, elsewhere) = (
        item(agent, 2, request),
        item(agent, 1, QueueSource::Owner),
        item(other, 0, QueueSource::Owner),
    );
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
        request_id: Some(RequestId::generate()),
        requester: Some(MemberId::generate()),
        requester_agent: Some("planner".into()),
        purpose: Some("run the tests".into()),
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
        expires_at: later(now(), Duration::from_secs(86_400)),
    };
    let denied = Approval {
        id: ApprovalId::generate(),
        status: ApprovalStatus::Denied,
        decided_at: Some(now()),
        ..pending.clone()
    };
    store.put_approval(&pending).unwrap();
    store.put_approval(&denied).unwrap();
    assert_eq!(
        store.approvals(None).unwrap(),
        vec![pending.clone(), denied.clone()]
    );
    assert_eq!(
        store.approvals(Some(ApprovalStatus::Pending)).unwrap(),
        vec![pending.clone()]
    );
    assert!(
        store
            .approvals(Some(ApprovalStatus::Expired))
            .unwrap()
            .is_empty()
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
    let other = Rule {
        id: RuleId::generate(),
        agent_id: AgentId::generate(),
        scope: RuleScope::Command {
            program: "npm".into(),
            arg_pattern: "test".into(),
            folder: "/tmp/repo".into(),
        },
        max_uses: None,
        expires_at: Some(now()),
        ..rule.clone()
    };
    store.put_rule(&rule).unwrap();
    store.put_rule(&other).unwrap();
    assert_eq!(store.rules(Some(agent)).unwrap(), vec![rule.clone()]);
    assert_eq!(
        store.rules(None).unwrap(),
        vec![rule.clone(), other.clone()]
    );
    assert!(store.rules(Some(AgentId::generate())).unwrap().is_empty());
    assert_eq!(store.record_rule_use(rule.id).unwrap().uses, 1);
    assert_eq!(store.record_rule_use(rule.id).unwrap().uses, 2);
    assert_eq!(store.rule(rule.id).unwrap().unwrap().uses, 2);
    assert_eq!(store.rule(other.id).unwrap(), Some(other));
    let missing = store.record_rule_use(RuleId::generate()).unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);
}
