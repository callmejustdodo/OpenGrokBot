use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;

use formation_proto::agent::{Agent, AgentState, Avatar, HarnessKind};
use formation_proto::ask::AskTarget;
use formation_proto::common::{AgentId, ChainId, ErrorCode, MemberId, Timestamp};
use formation_proto::policy::{
    AllowReason, Caller, Chain, MAX_CALLS_PER_MINUTE, PolicyVerdict, ShareSettings, Visibility,
};
use formation_proto::team::{Member, MemberRole};

use super::*;
use crate::ports::{AgentRepo, ShareRepo, TeamRepo, ToolContext};
use crate::testkit::MemoryStore;
use crate::testkit::collab::{MemoryCollabStore, samples};

struct World {
    policy: TeamPolicy,
    collab: Arc<MemoryCollabStore>,
    me: Member,
    teammate: Member,
    agent: Agent,
}

fn agent(folder: &str) -> Agent {
    Agent {
        id: AgentId::generate(),
        slug: "api".into(),
        name: "api".into(),
        role: "Backend".into(),
        harness: HarnessKind::ClaudeCode,
        folder_path: folder.into(),
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

fn world() -> World {
    let collab = Arc::new(MemoryCollabStore::new());
    let store = Arc::new(MemoryStore::default());
    let me = samples::member("dohyeon", MemberRole::Creator);
    let teammate = samples::member("minsu", MemberRole::Member);
    collab.put_member(&me).unwrap();
    collab.put_member(&teammate).unwrap();
    collab.set_me(me.id).unwrap();
    let agent = agent("/agents/api");
    store.insert_agent(&agent).unwrap();
    World {
        policy: TeamPolicy::new(collab.clone(), store),
        collab,
        me,
        teammate,
        agent,
    }
}

fn member(id: MemberId, depth: u8) -> Caller {
    Caller::Member {
        member_id: id,
        via_agent: None,
        chain: Chain {
            id: ChainId::generate(),
            depth,
        },
    }
}

impl World {
    fn share(&self, shared: bool, accepts_requests: bool) {
        self.collab
            .put_share(&ShareSettings {
                agent_id: self.agent.id,
                shared,
                visibility: Visibility::Answer,
                accepts_requests,
            })
            .unwrap();
    }

    fn caller(&self) -> Caller {
        member(self.teammate.id, 0)
    }
}

fn code<T: std::fmt::Debug>(result: crate::ports::Result<T>) -> ErrorCode {
    result.unwrap_err().code
}

#[tokio::test]
async fn authenticates_current_members_by_login_and_node() {
    let w = world();
    let whois = samples::whois_of(&w.teammate);
    assert_eq!(w.policy.authenticate(&whois).await.unwrap(), w.teammate.id);
    let mut other_node = whois.clone();
    other_node.node = "someone-else".into();
    assert_eq!(
        code(w.policy.authenticate(&other_node).await),
        ErrorCode::NotMember
    );
    // This Mac's own member is refused.
    let mine = samples::whois_of(&w.me);
    assert_eq!(
        code(w.policy.authenticate(&mine).await),
        ErrorCode::NotMember
    );
    // A removed member is refused.
    w.collab
        .remove_member(w.teammate.id, Timestamp::default())
        .unwrap();
    assert_eq!(
        code(w.policy.authenticate(&whois).await),
        ErrorCode::NotMember
    );
    assert_eq!(
        code(
            w.policy
                .check_ask(&w.caller(), &AskTarget::Person { member_id: w.me.id })
                .await
        ),
        ErrorCode::NotMember
    );
}

#[tokio::test]
async fn private_agents_are_never_listed_asked_or_sent_requests() {
    let w = world();
    let target = AskTarget::Agent {
        member_id: w.me.id,
        agent_id: w.agent.id,
    };
    assert!(
        w.policy
            .visible_agents(&w.caller())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        code(w.policy.check_ask(&w.caller(), &target).await),
        ErrorCode::AgentNotShared
    );
    assert_eq!(
        code(w.policy.check_request(&w.caller(), w.agent.id).await),
        ErrorCode::AgentNotShared
    );
    w.share(true, false);
    let views = w.policy.visible_agents(&w.caller()).await.unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].agent_id, w.agent.id);
    w.policy.check_ask(&w.caller(), &target).await.unwrap();
    assert_eq!(
        code(w.policy.check_request(&w.caller(), w.agent.id).await),
        ErrorCode::RequestsNotAccepted
    );
    w.share(true, true);
    w.policy
        .check_request(&w.caller(), w.agent.id)
        .await
        .unwrap();
    assert_eq!(
        w.policy.visibility(w.agent.id).await.unwrap(),
        Visibility::Answer
    );
    // An unknown agent is not shared.
    assert_eq!(
        code(
            w.policy
                .check_request(&w.caller(), AgentId::generate())
                .await
        ),
        ErrorCode::AgentNotShared
    );
}

#[tokio::test]
async fn strangers_and_deep_chains_are_refused() {
    let w = world();
    let person = AskTarget::Person { member_id: w.me.id };
    assert_eq!(
        code(
            w.policy
                .check_ask(&member(MemberId::generate(), 0), &person)
                .await
        ),
        ErrorCode::NotMember
    );
    assert_eq!(
        code(w.policy.check_ask(&member(w.me.id, 0), &person).await),
        ErrorCode::NotMember,
        "this Mac's own member is no teammate"
    );
    w.policy
        .check_ask(&member(w.teammate.id, 3), &person)
        .await
        .unwrap();
    assert_eq!(
        code(w.policy.check_ask(&member(w.teammate.id, 4), &person).await),
        ErrorCode::RateLimited
    );
}

#[tokio::test]
async fn members_get_thirty_calls_a_minute_and_the_owner_is_never_limited() {
    let w = world();
    let person = AskTarget::Person { member_id: w.me.id };
    for _ in 0..MAX_CALLS_PER_MINUTE {
        w.policy.check_ask(&w.caller(), &person).await.unwrap();
    }
    assert_eq!(
        code(w.policy.check_ask(&w.caller(), &person).await),
        ErrorCode::RateLimited
    );
    for _ in 0..100 {
        w.policy.check_ask(&Caller::Owner, &person).await.unwrap();
    }
    // The window slides.
    let later = Instant::now() + Duration::from_secs(61);
    w.policy.count(w.teammate.id, later).unwrap();
}

#[tokio::test]
async fn tool_calls_allow_reads_inside_the_folder_and_ask_for_the_rest() {
    let w = world();
    let folder = std::env::temp_dir().join(format!("team-policy-{}", AgentId::generate()));
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("a.txt"), "x").unwrap();
    let context = ToolContext {
        agent_id: w.agent.id,
        folder: folder.clone(),
        repositories: vec![],
        caller: w.caller(),
        request_id: None,
    };
    let read = samples::tool_call("Read", json!({"file_path": folder.join("a.txt")}));
    assert_eq!(
        w.policy.evaluate(&context, &read).await.unwrap(),
        PolicyVerdict::Allow {
            reason: AllowReason::AlwaysAllowedRead
        }
    );
    let outside = samples::tool_call("Read", json!({"file_path": "/etc/hosts"}));
    assert_eq!(
        w.policy.evaluate(&context, &outside).await.unwrap(),
        PolicyVerdict::NeedsApproval
    );
    assert_eq!(
        w.policy
            .evaluate(&context, &samples::bash("npm test"))
            .await
            .unwrap(),
        PolicyVerdict::NeedsApproval
    );
    std::fs::remove_dir_all(folder).unwrap();
}

#[tokio::test]
async fn a_mac_without_a_team_lists_nothing() {
    let collab = Arc::new(MemoryCollabStore::new());
    let policy = TeamPolicy::new(collab, Arc::new(MemoryStore::default()));
    assert_eq!(
        code(policy.visible_agents(&Caller::Owner).await),
        ErrorCode::NotMember
    );
}
