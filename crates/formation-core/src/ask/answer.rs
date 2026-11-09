//! Post-processing of a forked answer before it leaves the owner's Mac (tech
//! spec, ask pipeline step 5; FR-012, NFR-003, NFR-005, D-025).
//!
//! - The "Messages used:" list becomes excerpts: each item that quotes a user
//!   or assistant message of the session becomes that message. Items that quote
//!   nothing in the session (tool output, the side conversation's own earlier
//!   questions, invented text) are dropped. The list never stays in the answer
//!   text, so an `answer`-only agent does not leak excerpts through it.
//! - `as_of` is the time of the newest message used; when the answer cites none,
//!   the newest message the fork could see.
//! - `possibly_stale` is set when `as_of` is more than
//!   [`STALE_AFTER_MINUTES`] before the answer.
//! - Excerpts are dropped unless the agent's visibility is `answer_excerpts`.
//! - The answer and every excerpt are masked ([`Masker`]).

use formation_proto::ask::{Answer, Excerpt, ExcerptRole, STALE_AFTER_MINUTES};
use formation_proto::common::{AgentId, AnswerId, Timestamp};
use formation_proto::harness::ConversationItem;
use formation_proto::policy::Visibility;

use super::excerpts::{cited_pieces, split_messages_used, words};
use super::mask::Masker;

/// Longest excerpt, in characters. A longer message is excerpted by the parts
/// of it the answer quoted.
pub const MAX_EXCERPT_CHARS: usize = 400;

/// Most excerpts kept per answer, newest first.
pub const MAX_EXCERPTS: usize = 6;

/// A user or assistant message of the session, the only kinds an excerpt may show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMessage {
    pub role: ExcerptRole,
    pub text: String,
    pub at: Timestamp,
}

/// The session's user and assistant messages up to `until`, oldest first. Tool
/// steps, approvals, requests, and notices are left out: their content may hold
/// command output, and excerpts show conversation only.
pub fn source_messages(items: &[ConversationItem], until: Timestamp) -> Vec<SourceMessage> {
    let mut messages: Vec<SourceMessage> = items
        .iter()
        .filter_map(|item| match item {
            ConversationItem::UserMessage { at, text, .. } => Some((ExcerptRole::User, text, *at)),
            ConversationItem::AssistantText { at, text, .. } => {
                Some((ExcerptRole::Assistant, text, *at))
            }
            _ => None,
        })
        .filter(|(_, text, at)| *at <= until && !text.trim().is_empty())
        .map(|(role, text, at)| SourceMessage {
            role,
            text: text.clone(),
            at,
        })
        .collect();
    messages.sort_by_key(|message| message.at);
    messages
}

/// Whether the agent was mid-turn when the session was read: its newest item is
/// not a reply. A fork then sees the owner's newest message as unanswered
/// (TASK-006), so the question carries a note about it.
pub fn mid_turn(items: &[ConversationItem]) -> bool {
    items
        .iter()
        .rev()
        .find(|item| !matches!(item, ConversationItem::Notice { .. }))
        .is_some_and(|item| !matches!(item, ConversationItem::AssistantText { .. }))
}

/// Whether an answer built from content of `as_of` is possibly out of date at
/// `answered_at` (FR-012: more than an hour older).
pub fn is_possibly_stale(as_of: Timestamp, answered_at: Timestamp) -> bool {
    (answered_at - as_of).num_seconds() > STALE_AFTER_MINUTES * 60
}

/// Keeps excerpts only when the visibility allows them (D-025).
pub fn apply_visibility(visibility: Visibility, excerpts: Vec<Excerpt>) -> Vec<Excerpt> {
    match visibility {
        Visibility::AnswerExcerpts => excerpts,
        Visibility::Answer => Vec::new(),
    }
}

/// What post-processing needs besides the fork's raw answer.
#[derive(Debug, Clone)]
pub struct AnswerContext<'a> {
    pub agent_id: AgentId,
    /// The session's messages the fork could see ([`source_messages`]).
    pub sources: &'a [SourceMessage],
    pub visibility: Visibility,
    pub masker: &'a Masker,
    pub answered_at: Timestamp,
    /// `as_of` when the answer cites nothing, such as the previous answer's
    /// `as_of` in a follow-up; without it, the newest of `sources`.
    pub fallback_as_of: Option<Timestamp>,
}

/// Builds the teammate's [`Answer`] from the fork's raw answer text.
pub fn build_answer(raw: &str, context: &AnswerContext<'_>) -> Answer {
    let used = split_messages_used(raw);
    let normalized: Vec<String> = context.sources.iter().map(|m| words(&m.text)).collect();

    // Message index -> the pieces of it the answer quoted.
    let mut cited: Vec<(usize, Vec<String>)> = Vec::new();
    for item in &used.items {
        let Some(pieces) = cited_pieces(item, &normalized) else {
            continue;
        };
        for (piece, index) in pieces {
            match cited.iter_mut().find(|(i, _)| *i == index) {
                Some((_, quoted)) => quoted.push(piece),
                None => cited.push((index, vec![piece])),
            }
        }
    }
    cited.sort_by_key(|(index, _)| *index);

    let as_of = cited
        .last()
        .map(|(index, _)| context.sources[*index].at)
        .or(context.fallback_as_of)
        .or_else(|| context.sources.last().map(|m| m.at))
        .unwrap_or(context.answered_at);

    let excerpts: Vec<Excerpt> = cited
        .iter()
        .rev()
        .take(MAX_EXCERPTS)
        .rev()
        .map(|(index, quoted)| {
            let message = &context.sources[*index];
            Excerpt {
                role: message.role,
                text: context.masker.mask(&excerpt_text(&message.text, quoted)),
                at: message.at,
            }
        })
        .collect();

    Answer {
        id: AnswerId::generate(),
        agent_id: Some(context.agent_id),
        text: context.masker.mask(&used.body),
        excerpts: apply_visibility(context.visibility, excerpts),
        as_of,
        possibly_stale: is_possibly_stale(as_of, context.answered_at),
        answered_at: context.answered_at,
    }
}

/// The whole message when it is short; otherwise the parts the answer quoted,
/// joined with `…`, cut to [`MAX_EXCERPT_CHARS`].
fn excerpt_text(message: &str, quoted: &[String]) -> String {
    let message = message.trim();
    if message.chars().count() <= MAX_EXCERPT_CHARS {
        return message.to_owned();
    }
    let text = quoted
        .iter()
        .map(|piece| piece.trim_matches(['"', '“', '”', ' ']))
        .collect::<Vec<_>>()
        .join(" … ");
    truncate_chars(&text, MAX_EXCERPT_CHARS)
}

/// At most `max` characters, ending with `…` when cut.
pub(crate) fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use formation_proto::common::ItemId;
    use formation_proto::harness::{Author, NoticeLevel, Step, StepKind, StepStatus};

    use super::*;
    use crate::ask::mask::MASK;

    /// 10:00 UTC plus `minute` minutes.
    fn at(minute: i64) -> Timestamp {
        let total = 10 * 60 + minute;
        format!("2026-11-09T{:02}:{:02}:00Z", total / 60, total % 60)
            .parse()
            .unwrap()
    }

    fn user(minute: i64, text: &str) -> ConversationItem {
        ConversationItem::UserMessage {
            id: ItemId(format!("u{minute}")),
            at: at(minute),
            author: Author::Owner,
            text: text.into(),
        }
    }

    fn reply(minute: i64, text: &str) -> ConversationItem {
        ConversationItem::AssistantText {
            id: ItemId(format!("a{minute}")),
            at: at(minute),
            text: text.into(),
        }
    }

    fn step(minute: i64, detail: &str) -> ConversationItem {
        ConversationItem::Step {
            id: ItemId(format!("s{minute}")),
            at: at(minute),
            step: Step {
                kind: StepKind::Command,
                tool_name: "Bash".into(),
                target: "cat .env".into(),
                detail: Some(detail.into()),
                status: StepStatus::Done,
            },
        }
    }

    fn transcript() -> Vec<ConversationItem> {
        vec![
            user(0, "Clean up the login API endpoints."),
            step(1, "API_TOKEN=abcd1234efgh5678"),
            reply(
                31,
                "Endpoints cleaned up. Next I am adding a token expiry check.",
            ),
            user(40, "Minsu's expiry tests need my approval, remind me."),
            reply(44, "auth/token.ts: adding the expiry check now."),
        ]
    }

    const RAW: &str = "The login API cleanup is done and the agent is adding a token expiry \
        check. The conversation does not establish when it ships.\n\nMessages used:\n\
        - \"Endpoints cleaned up\"\n- \"adding the expiry check now\"\n- \"API_TOKEN\"";

    fn context<'a>(
        sources: &'a [SourceMessage],
        masker: &'a Masker,
        visibility: Visibility,
        answered_at: Timestamp,
    ) -> AnswerContext<'a> {
        AnswerContext {
            agent_id: AgentId::generate(),
            sources,
            visibility,
            masker,
            answered_at,
            fallback_as_of: None,
        }
    }

    #[test]
    fn cited_messages_become_excerpts_and_set_as_of() {
        let sources = source_messages(&transcript(), at(50));
        let masker = Masker::new();
        let answer = build_answer(
            RAW,
            &context(&sources, &masker, Visibility::AnswerExcerpts, at(50)),
        );
        assert!(answer.text.starts_with("The login API cleanup is done"));
        assert!(!answer.text.contains("Messages used"));
        let excerpts: Vec<(&str, ExcerptRole, Timestamp)> = answer
            .excerpts
            .iter()
            .map(|e| (e.text.as_str(), e.role, e.at))
            .collect();
        // The tool output quoted as "API_TOKEN" is not a conversation message.
        assert_eq!(
            excerpts,
            [
                (
                    "Endpoints cleaned up. Next I am adding a token expiry check.",
                    ExcerptRole::Assistant,
                    at(31)
                ),
                (
                    "auth/token.ts: adding the expiry check now.",
                    ExcerptRole::Assistant,
                    at(44)
                ),
            ]
        );
        assert_eq!(answer.as_of, at(44));
        assert!(!answer.possibly_stale);
        assert_eq!(answer.answered_at, at(50));
    }

    #[test]
    fn answer_only_visibility_drops_excerpts_but_keeps_as_of() {
        let sources = source_messages(&transcript(), at(50));
        let masker = Masker::new();
        let answer = build_answer(RAW, &context(&sources, &masker, Visibility::Answer, at(50)));
        assert!(answer.excerpts.is_empty());
        assert_eq!(answer.as_of, at(44));
        // The quotes in the list never reach the teammate through the text either.
        assert!(!answer.text.contains("Endpoints cleaned up"));
        assert!(!answer.text.contains("Messages used"));
    }

    #[test]
    fn answers_older_than_an_hour_are_possibly_stale() {
        let sources = source_messages(&transcript(), at(500));
        let masker = Masker::new();
        let fresh = build_answer(
            RAW,
            &context(&sources, &masker, Visibility::AnswerExcerpts, at(44 + 60)),
        );
        assert!(!fresh.possibly_stale, "exactly an hour is not stale");
        let stale = build_answer(
            RAW,
            &context(&sources, &masker, Visibility::AnswerExcerpts, at(44 + 61)),
        );
        assert!(stale.possibly_stale);
        assert!(is_possibly_stale(at(0), at(61)));
        assert!(!is_possibly_stale(at(0), at(60)));
    }

    #[test]
    fn without_citations_as_of_is_the_newest_message_seen() {
        let sources = source_messages(&transcript(), at(41));
        let masker = Masker::new();
        let answer = build_answer(
            "Not established by the conversation.",
            &context(&sources, &masker, Visibility::AnswerExcerpts, at(41)),
        );
        assert_eq!(answer.as_of, at(40));
        assert!(answer.excerpts.is_empty());
        assert_eq!(answer.text, "Not established by the conversation.");

        let fallback = AnswerContext {
            fallback_as_of: Some(at(3)),
            ..context(&[], &masker, Visibility::AnswerExcerpts, at(70))
        };
        let answer = build_answer("Not established.", &fallback);
        assert_eq!(answer.as_of, at(3));
        assert!(answer.possibly_stale);
    }

    #[test]
    fn answers_and_excerpts_are_masked() {
        let items = vec![
            user(
                0,
                "Use the key sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWxYz for staging.",
            ),
            reply(1, "Noted, staging uses that key now."),
        ];
        let sources = source_messages(&items, at(2));
        let masker = Masker::with_owner_emails(["dohyeon@example.com"]);
        let raw = "Staging uses sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWxYz; ask dohyeon@example.com.\n\n\
            Messages used:\n- \"Use the key\"";
        let answer = build_answer(
            raw,
            &context(&sources, &masker, Visibility::AnswerExcerpts, at(2)),
        );
        assert_eq!(answer.text, format!("Staging uses {MASK}; ask {MASK}."));
        assert_eq!(answer.excerpts.len(), 1);
        assert_eq!(
            answer.excerpts[0].text,
            format!("Use the key {MASK} for staging.")
        );
    }

    #[test]
    fn long_messages_are_excerpted_by_the_quoted_parts() {
        let long = format!(
            "{} The deploy script now retries twice.",
            "Filler. ".repeat(80)
        );
        let items = vec![reply(0, &long)];
        let sources = source_messages(&items, at(1));
        let masker = Masker::new();
        let answer = build_answer(
            "It retries.\n\nMessages used:\n- \"The deploy script now retries twice\"",
            &context(&sources, &masker, Visibility::AnswerExcerpts, at(1)),
        );
        assert_eq!(
            answer.excerpts[0].text,
            "The deploy script now retries twice"
        );
        assert_eq!(truncate_chars("abcdef", 4), "abc…");
        assert_eq!(truncate_chars("abc", 4), "abc");
    }

    #[test]
    fn mid_turn_is_an_unanswered_newest_item() {
        assert!(!mid_turn(&transcript()));
        let mut items = transcript();
        items.push(user(45, "Also add a refresh token."));
        assert!(mid_turn(&items));
        items.push(step(46, "running"));
        assert!(mid_turn(&items));
        items.push(reply(47, "Added."));
        items.push(ConversationItem::Notice {
            id: ItemId("n".into()),
            at: at(48),
            level: NoticeLevel::Info,
            text: "Turn ended".into(),
        });
        assert!(!mid_turn(&items));
        assert!(!mid_turn(&[]));
    }

    #[test]
    fn sources_are_conversation_messages_up_to_the_fork() {
        let sources = source_messages(&transcript(), at(40));
        assert_eq!(sources.len(), 3);
        assert!(sources.iter().all(|m| m.at <= at(40)));
        assert!(!sources.iter().any(|m| m.text.contains("API_TOKEN")));
    }
}
