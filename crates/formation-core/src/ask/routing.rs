//! Routing a person-level question to one of the owner's shared agents (FR-010,
//! D-027; tech spec, ask pipeline step 3).
//!
//! A [`Scorer`] rates each shared agent from its role, the first lines of its
//! `AGENTS.md`, and recent session text. [`decide`] then routes confidently only
//! when the best score reaches [`RoutingThresholds::confident`] and leads the
//! next one by at least [`RoutingThresholds::margin`]; otherwise the asker gets
//! one choice prompt with the best guess first, the other candidates, and "ask
//! the owner". Only shared agents ever reach a scorer; callers build the pool
//! with [`crate::policy::sharing::shared_agents`].
//!
//! Candidate reasons are shown to the asker, so they are masked and kept short
//! like an answer (they summarize session content but never quote it at length).

use async_trait::async_trait;

use formation_proto::agent::Agent;
use formation_proto::ask::RoutingCandidate;
use formation_proto::common::{AgentId, ApiError, ErrorCode};
use formation_proto::harness::ConversationItem;

use super::answer::truncate_chars;
use super::mask::Masker;
use crate::ports::{Harness, Result};

/// What a scorer knows about one shared agent. It stays on the owner's Mac.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingProfile {
    pub agent_id: AgentId,
    pub name: String,
    pub role: String,
    /// The first lines of the agent's `AGENTS.md` ([`instructions_head`]).
    pub instructions: String,
    /// The newest user and assistant messages of its session ([`recent_text`]).
    pub recent: String,
}

/// One agent's score from a [`Scorer`].
#[derive(Debug, Clone, PartialEq)]
pub struct AgentScore {
    pub agent_id: AgentId,
    /// 0.0 to 1.0: how well the agent's own work can answer the question.
    pub score: f32,
    /// One short sentence for the asker.
    pub reason: String,
}

/// Rates how well each agent can answer a question. The real scorer is a short,
/// isolated Claude Code call with tools disabled ([`super::scorer::ClaudeScorer`]).
#[async_trait]
pub trait Scorer: Send + Sync {
    async fn score(&self, question: &str, agents: &[RoutingProfile]) -> Result<Vec<AgentScore>>;
}

/// When routing is confident (assumed values, like the other thresholds in the
/// PRD's assumptions).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoutingThresholds {
    /// The best score must be at least this.
    pub confident: f32,
    /// And at least this much above the second best.
    pub margin: f32,
}

impl Default for RoutingThresholds {
    fn default() -> Self {
        Self {
            confident: 0.6,
            margin: 0.15,
        }
    }
}

/// At most this many candidates in the choice prompt.
pub const MAX_CANDIDATES: usize = 5;

/// Longest reason shown to the asker, in characters.
pub const MAX_REASON_CHARS: usize = 160;

/// Lines of `AGENTS.md` a scorer sees.
pub const INSTRUCTION_LINES: usize = 20;
/// Characters of `AGENTS.md` a scorer sees.
pub const INSTRUCTION_CHARS: usize = 1_500;
/// Characters of recent session text a scorer sees.
pub const RECENT_CHARS: usize = 2_000;
/// Items read from the end of a session for its recent text.
pub const RECENT_ITEMS: u32 = 40;

#[derive(Debug, Clone, PartialEq)]
pub enum RoutingDecision {
    /// The agent answers, and the answer names it.
    Confident { agent_id: AgentId, score: f32 },
    /// The asker chooses: candidates best first, then "ask the owner" (D-027).
    Unsure { candidates: Vec<RoutingCandidate> },
}

/// Scores the shared agents and decides. A scorer that fails leaves the choice to
/// the asker with every agent unscored, rather than failing the question.
/// `agent_not_shared` when the owner shares no agent.
pub async fn route(
    scorer: &dyn Scorer,
    question: &str,
    profiles: &[RoutingProfile],
    thresholds: RoutingThresholds,
    masker: &Masker,
) -> Result<RoutingDecision> {
    if profiles.is_empty() {
        return Err(ApiError::new(
            ErrorCode::AgentNotShared,
            "the member shares no agent",
        ));
    }
    let scores = scorer.score(question, profiles).await.unwrap_or_default();
    Ok(decide(profiles, &scores, thresholds, masker))
}

/// Applies the thresholds. Scores for agents outside `profiles` are ignored;
/// agents without a score count as 0; scores are clamped to 0.0 to 1.0. Ties keep
/// the order of `profiles`. With no profiles the asker can only ask the owner.
pub fn decide(
    profiles: &[RoutingProfile],
    scores: &[AgentScore],
    thresholds: RoutingThresholds,
    masker: &Masker,
) -> RoutingDecision {
    let mut ranked: Vec<(&RoutingProfile, f32, &str)> = profiles
        .iter()
        .map(|profile| {
            let best = scores
                .iter()
                .filter(|s| s.agent_id == profile.agent_id)
                .map(|s| (clamp(s.score), s.reason.as_str()))
                .fold(None, |best: Option<(f32, &str)>, next| match best {
                    Some(b) if b.0 >= next.0 => Some(b),
                    _ => Some(next),
                });
            let (score, reason) = best.unwrap_or((0.0, ""));
            (profile, score, reason)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));

    let Some(&(_, top, _)) = ranked.first() else {
        return RoutingDecision::Unsure { candidates: vec![] };
    };
    let second = ranked.get(1).map_or(0.0, |r| r.1);
    if top >= thresholds.confident && top - second >= thresholds.margin {
        return RoutingDecision::Confident {
            agent_id: ranked[0].0.agent_id,
            score: top,
        };
    }
    RoutingDecision::Unsure {
        candidates: ranked
            .into_iter()
            .take(MAX_CANDIDATES)
            .map(|(profile, score, reason)| RoutingCandidate {
                agent_id: profile.agent_id,
                name: profile.name.clone(),
                role: profile.role.clone(),
                score,
                reason: truncate_chars(&masker.mask(reason.trim()), MAX_REASON_CHARS),
            })
            .collect(),
    }
}

fn clamp(score: f32) -> f32 {
    if score.is_nan() {
        0.0
    } else {
        score.clamp(0.0, 1.0)
    }
}

/// Builds a shared agent's profile: its `AGENTS.md` head from its folder and its
/// recent session text through the harness. Missing pieces are left empty, so a
/// stopped agent or one without instructions is still scored by its role.
pub async fn profile(harness: &dyn Harness, agent: &Agent) -> RoutingProfile {
    let instructions = std::fs::read_to_string(agent.folder_path.join("AGENTS.md"))
        .map(|text| instructions_head(&text))
        .unwrap_or_default();
    let recent = harness
        .conversation(agent.id, None, None, RECENT_ITEMS)
        .await
        .map(|page| recent_text(&page.items))
        .unwrap_or_default();
    RoutingProfile {
        agent_id: agent.id,
        name: agent.name.clone(),
        role: agent.role.clone(),
        instructions,
        recent,
    }
}

/// The first [`INSTRUCTION_LINES`] non-empty lines, at most [`INSTRUCTION_CHARS`].
pub fn instructions_head(agents_md: &str) -> String {
    let head = agents_md
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .take(INSTRUCTION_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    truncate_chars(&head, INSTRUCTION_CHARS)
}

/// The newest user and assistant messages, oldest first, labeled by speaker and
/// at most [`RECENT_CHARS`] in total. Tool steps are left out.
pub fn recent_text(items: &[ConversationItem]) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut used = 0;
    for item in items.iter().rev() {
        let line = match item {
            ConversationItem::UserMessage { text, .. } => format!("User: {}", one_line(text)),
            ConversationItem::AssistantText { text, .. } => format!("Agent: {}", one_line(text)),
            _ => continue,
        };
        let remaining = RECENT_CHARS.saturating_sub(used);
        if remaining == 0 {
            break;
        }
        let line = truncate_chars(&line, remaining);
        used += line.chars().count() + 1;
        lines.push(line);
    }
    lines.reverse();
    lines.join("\n")
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Mutex;

    use super::*;

    /// Returns scripted scores and records what it was asked.
    pub struct ScriptedScorer {
        pub scores: Result<Vec<AgentScore>>,
        pub asked: Mutex<Vec<(String, Vec<AgentId>)>>,
    }

    impl ScriptedScorer {
        pub fn new(scores: Result<Vec<AgentScore>>) -> Self {
            Self {
                scores,
                asked: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl Scorer for ScriptedScorer {
        async fn score(
            &self,
            question: &str,
            agents: &[RoutingProfile],
        ) -> Result<Vec<AgentScore>> {
            self.asked.lock().unwrap().push((
                question.to_owned(),
                agents.iter().map(|a| a.agent_id).collect(),
            ));
            self.scores.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use formation_proto::common::{ItemId, Timestamp};
    use formation_proto::harness::{Author, Step, StepKind, StepStatus};

    use super::test_support::ScriptedScorer;
    use super::*;
    use crate::ask::mask::MASK;

    fn profiles(roles: &[&str]) -> Vec<RoutingProfile> {
        roles
            .iter()
            .map(|role| RoutingProfile {
                agent_id: AgentId::generate(),
                name: role.split(' ').next().unwrap().to_owned(),
                role: (*role).to_owned(),
                instructions: String::new(),
                recent: String::new(),
            })
            .collect()
    }

    fn scores(profiles: &[RoutingProfile], values: &[f32]) -> Vec<AgentScore> {
        profiles
            .iter()
            .zip(values)
            .map(|(p, &score)| AgentScore {
                agent_id: p.agent_id,
                score,
                reason: format!("{} work", p.name),
            })
            .collect()
    }

    fn decide_default(profiles: &[RoutingProfile], values: &[f32]) -> RoutingDecision {
        decide(
            profiles,
            &scores(profiles, values),
            RoutingThresholds::default(),
            &Masker::new(),
        )
    }

    #[test]
    fn a_clear_best_agent_is_routed_confidently() {
        let p = profiles(&["Backend developer", "Frontend developer"]);
        assert_eq!(
            decide_default(&p, &[0.9, 0.2]),
            RoutingDecision::Confident {
                agent_id: p[0].agent_id,
                score: 0.9
            }
        );
        // Exactly at both thresholds is still confident.
        assert!(matches!(
            decide_default(&p, &[0.2, 0.75]),
            RoutingDecision::Confident { agent_id, .. } if agent_id == p[1].agent_id
        ));
        // One shared agent: only the score threshold applies.
        let single = profiles(&["Backend developer"]);
        assert!(matches!(
            decide_default(&single, &[0.6]),
            RoutingDecision::Confident { .. }
        ));
    }

    #[test]
    fn a_low_best_score_asks_the_asker() {
        let p = profiles(&["Backend developer", "Frontend developer", "Designer"]);
        let RoutingDecision::Unsure { candidates } = decide_default(&p, &[0.3, 0.55, 0.1]) else {
            panic!("expected unsure");
        };
        let order: Vec<AgentId> = candidates.iter().map(|c| c.agent_id).collect();
        assert_eq!(order, [p[1].agent_id, p[0].agent_id, p[2].agent_id]);
        assert_eq!(candidates[0].role, "Frontend developer");
        assert_eq!(candidates[0].reason, "Frontend work");
        assert!((candidates[0].score - 0.55).abs() < f32::EPSILON);

        let single = profiles(&["Backend developer"]);
        assert!(matches!(
            decide_default(&single, &[0.59]),
            RoutingDecision::Unsure { candidates } if candidates.len() == 1
        ));
    }

    #[test]
    fn a_close_second_asks_the_asker() {
        let p = profiles(&["Backend developer", "Frontend developer"]);
        let RoutingDecision::Unsure { candidates } = decide_default(&p, &[0.8, 0.7]) else {
            panic!("expected unsure");
        };
        assert_eq!(candidates[0].agent_id, p[0].agent_id);
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn odd_scores_are_cleaned_up() {
        let p = profiles(&["Backend developer", "Frontend developer"]);
        let mut s = scores(&p, &[f32::NAN, 7.0]);
        // A score for an agent that is not shared is ignored.
        s.push(AgentScore {
            agent_id: AgentId::generate(),
            score: 1.0,
            reason: "private".into(),
        });
        // A duplicate keeps the higher score.
        s.push(AgentScore {
            agent_id: p[0].agent_id,
            score: 0.1,
            reason: "dup".into(),
        });
        assert_eq!(
            decide(&p, &s, RoutingThresholds::default(), &Masker::new()),
            RoutingDecision::Confident {
                agent_id: p[1].agent_id,
                score: 1.0
            }
        );
        assert_eq!(
            decide(&[], &s, RoutingThresholds::default(), &Masker::new()),
            RoutingDecision::Unsure { candidates: vec![] }
        );
        // No scores at all: unsure, in the pool's order.
        let RoutingDecision::Unsure { candidates } =
            decide(&p, &[], RoutingThresholds::default(), &Masker::new())
        else {
            panic!("expected unsure");
        };
        assert_eq!(candidates[0].agent_id, p[0].agent_id);
        assert_eq!(candidates[0].score, 0.0);
    }

    #[test]
    fn candidates_are_capped_and_reasons_masked() {
        let roles: Vec<String> = (0..8).map(|i| format!("Agent{i} role")).collect();
        let p = profiles(&roles.iter().map(String::as_str).collect::<Vec<_>>());
        let mut s = scores(&p, &[0.1; 8]);
        s[0].reason = format!("Used password: hunter2 while {}", "deploying ".repeat(40));
        let RoutingDecision::Unsure { candidates } =
            decide(&p, &s, RoutingThresholds::default(), &Masker::new())
        else {
            panic!("expected unsure");
        };
        assert_eq!(candidates.len(), MAX_CANDIDATES);
        assert!(candidates[0].reason.contains(MASK));
        assert!(!candidates[0].reason.contains("hunter2"));
        assert!(candidates[0].reason.chars().count() <= MAX_REASON_CHARS);
    }

    #[tokio::test]
    async fn route_needs_a_shared_agent_and_survives_a_failing_scorer() {
        let scorer = ScriptedScorer::new(Ok(vec![]));
        let err = route(
            &scorer,
            "How far is the login work?",
            &[],
            RoutingThresholds::default(),
            &Masker::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::AgentNotShared);
        assert!(scorer.asked.lock().unwrap().is_empty());

        let p = profiles(&["Backend developer", "Frontend developer"]);
        let failing = ScriptedScorer::new(Err(ApiError::new(
            ErrorCode::HarnessUnavailable,
            "not signed in",
        )));
        let decision = route(
            &failing,
            "How far is the login work?",
            &p,
            RoutingThresholds::default(),
            &Masker::new(),
        )
        .await
        .unwrap();
        assert!(
            matches!(decision, RoutingDecision::Unsure { candidates } if candidates.len() == 2)
        );
        let asked = failing.asked.lock().unwrap();
        assert_eq!(asked[0].0, "How far is the login work?");
        assert_eq!(asked[0].1, [p[0].agent_id, p[1].agent_id]);
    }

    #[test]
    fn instructions_and_recent_text_are_short() {
        let md = format!("# API agent\n\n{}", "You own the login API.\n".repeat(50));
        let head = instructions_head(&md);
        assert_eq!(head.lines().count(), INSTRUCTION_LINES);
        assert!(head.starts_with("# API agent\nYou own the login API."));

        let at = Timestamp::default();
        let items = vec![
            ConversationItem::UserMessage {
                id: ItemId("u".into()),
                at,
                author: Author::Owner,
                text: "Clean up\nthe login API".into(),
            },
            ConversationItem::Step {
                id: ItemId("s".into()),
                at,
                step: Step {
                    kind: StepKind::Command,
                    tool_name: "Bash".into(),
                    target: "cat .env".into(),
                    detail: Some("SECRET=1".into()),
                    status: StepStatus::Done,
                },
            },
            ConversationItem::AssistantText {
                id: ItemId("a".into()),
                at,
                text: "Done.".into(),
            },
        ];
        assert_eq!(
            recent_text(&items),
            "User: Clean up the login API\nAgent: Done."
        );

        let long: Vec<ConversationItem> = (0..100)
            .map(|i| ConversationItem::AssistantText {
                id: ItemId(format!("a{i}")),
                at,
                text: format!("message {i} {}", "x".repeat(100)),
            })
            .collect();
        let text = recent_text(&long);
        assert!(text.chars().count() <= RECENT_CHARS);
        assert!(
            text.ends_with(&"x".repeat(50)),
            "the newest messages are kept"
        );
        assert!(text.contains("message 99"));
    }
}
