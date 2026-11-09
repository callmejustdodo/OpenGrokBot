//! Reads the "Messages used:" list that the answer prompt asks for and finds
//! the conversation messages each item quotes (tech spec, ask pipeline steps 4
//! and 5). The TASK-006 spike checked live answers with these functions; the
//! answer post-processing ([`super::answer`]) turns the cited messages into
//! excerpts and `as_of`.
//!
//! An answer looks like this; the list is the last part of it:
//!
//! ```text
//! The login API cleanup is done. The conversation does not establish when the
//! expiry check will be finished.
//!
//! Messages used:
//! - "the login API cleanup is done"
//! - "Jordan's expiry tests are waiting for my approval"
//! ```

/// An answer split into its text for the teammate and its "Messages used:" items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UsedList {
    /// The answer without the heading and its list.
    pub body: String,
    pub items: Vec<String>,
}

/// Splits off the last "Messages used:" heading and its list: list items up to
/// the next blank line, plus any text after the heading's colon. Text after the
/// list stays in the body.
pub(crate) fn split_messages_used(answer: &str) -> UsedList {
    let lines: Vec<&str> = answer.lines().collect();
    let Some(heading) = lines.iter().rposition(|line| is_heading(line)) else {
        return UsedList {
            body: answer.trim().to_owned(),
            items: Vec::new(),
        };
    };
    let mut items = Vec::new();
    if let Some((_, rest)) = lines[heading].split_once(':') {
        let rest = rest.trim().trim_matches(['*', '_']).trim();
        if !rest.is_empty() {
            items.push(rest.to_owned());
        }
    }
    let mut end = lines.len();
    for (i, line) in lines.iter().enumerate().skip(heading + 1) {
        let line = line.trim();
        if line.is_empty() {
            if items.is_empty() {
                continue;
            }
            end = i;
            break;
        }
        items.push(strip_bullet(line).to_owned());
    }
    let before = lines[..heading].join("\n");
    let after = lines[end..].join("\n");
    let body = [before.trim(), after.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    UsedList { body, items }
}

/// The items listed after the answer's "Messages used:" line.
#[cfg(test)]
pub(crate) fn messages_used(answer: &str) -> Vec<String> {
    split_messages_used(answer).items
}

/// `Messages used:` or `**Messages used**` alone, or followed by a colon and
/// an inline item; not a sentence that happens to start with those words.
fn is_heading(line: &str) -> bool {
    let normalized = words(line);
    normalized == "messages used" || (normalized.starts_with("messages used") && line.contains(':'))
}

/// `- x`, `* x`, `• x`, `1. x` and `1) x` become `x`.
pub(crate) fn strip_bullet(line: &str) -> &str {
    for bullet in ["- ", "* ", "• "] {
        if let Some(rest) = line.strip_prefix(bullet) {
            return rest.trim();
        }
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        let rest = &line[digits..];
        if let Some(rest) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return rest.trim();
        }
    }
    line
}

/// Whether a "Messages used" item quotes the conversation; see [`cited_pieces`].
#[cfg(test)]
pub(crate) fn cited_from(item: &str, conversation: &[&str]) -> bool {
    let messages: Vec<String> = conversation.iter().map(|m| words(m)).collect();
    cited_pieces(item, &messages).is_some_and(|pieces| !pieces.is_empty())
}

/// The pieces of a "Messages used" item, each with the index of the newest
/// message it quotes. `messages` are normalized with [`words`], oldest first.
///
/// The quoted parts of the item (or, without quotes, the item after any short
/// `label:`) are split at `…` or `...`; each piece must appear in one message,
/// ignoring case and punctuation. A piece of four or more words may also join
/// runs of at least four words from one message; a shorter one may leave out
/// words between its own. `None` when any piece quotes no message, so an item
/// that mixes a real quote with an invented one cites nothing.
pub(crate) fn cited_pieces(item: &str, messages: &[String]) -> Option<Vec<(String, usize)>> {
    let quoted = quoted_parts(item);
    let parts = if quoted.is_empty() {
        vec![strip_label(item)]
    } else {
        quoted
    };
    let mut pieces = Vec::new();
    for part in parts {
        for piece in part.split('…').flat_map(|p| p.split("...")) {
            let normalized = words(piece);
            if normalized.is_empty() {
                continue;
            }
            let index = messages
                .iter()
                .rposition(|message| quotes(&normalized, message))?;
            pieces.push((piece.trim().to_owned(), index));
        }
    }
    Some(pieces)
}

/// Whether `piece` quotes `message`; both are normalized with [`words`].
pub(crate) fn quotes(piece: &str, message: &str) -> bool {
    if format!(" {message} ").contains(&format!(" {piece} ")) {
        return true;
    }
    let piece: Vec<&str> = piece.split(' ').collect();
    let message: Vec<&str> = message.split(' ').collect();
    const RUN: usize = 4;
    if piece.len() >= RUN {
        let mut covered = vec![false; piece.len()];
        for start in 0..=piece.len() - RUN {
            let run = &piece[start..start + RUN];
            if message.windows(RUN).any(|w| w == run) {
                covered[start..start + RUN].fill(true);
            }
        }
        return covered.iter().all(|&c| c);
    }
    // Every word in order, within twice the piece's length.
    (0..message.len()).any(|start| {
        let window = &message[start..message.len().min(start + 2 * piece.len())];
        let mut rest = window.iter();
        piece.iter().all(|word| rest.any(|w| w == word))
    })
}

/// The text between double quotes (`"` or `“ ”`).
pub(crate) fn quoted_parts(item: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current: Option<String> = None;
    for c in item.chars() {
        match (c, current.as_mut()) {
            ('"' | '“' | '”', Some(_)) => parts.push(current.take().unwrap_or_default()),
            ('"' | '“', None) => current = Some(String::new()),
            (c, Some(text)) => text.push(c),
            _ => {}
        }
    }
    parts
}

/// Drops a leading label of up to four words followed by `:`, such as
/// `User:` or `Agent's reply:`.
pub(crate) fn strip_label(item: &str) -> String {
    match item.split_once(':') {
        Some((label, rest)) if label.split_whitespace().count() <= 4 && !rest.trim().is_empty() => {
            rest.trim().to_owned()
        }
        _ => item.to_owned(),
    }
}

/// Lowercase words separated by single spaces; everything else is dropped.
pub(crate) fn words(text: &str) -> String {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_is_split_from_the_answer() {
        let answer = "The login API cleanup is done.\n\nThe conversation does not establish \
            when it ships.\n\n**Messages used:**\n1. \"the login API cleanup is done\"\n2. \
            “Jordan's tests”\n\nAsk again after lunch.";
        let used = split_messages_used(answer);
        assert_eq!(
            used.body,
            "The login API cleanup is done.\n\nThe conversation does not establish when it \
             ships.\n\nAsk again after lunch."
        );
        assert_eq!(
            used.items,
            ["\"the login API cleanup is done\"", "“Jordan's tests”"]
        );
    }

    #[test]
    fn an_answer_without_a_list_keeps_its_text() {
        let used = split_messages_used("  Not established.\n");
        assert_eq!(used.body, "Not established.");
        assert!(used.items.is_empty());
        // A sentence that starts with the words is not the heading.
        let used = split_messages_used("Messages used to be kept for a week.");
        assert_eq!(used.body, "Messages used to be kept for a week.");
        assert!(used.items.is_empty());
    }

    #[test]
    fn the_last_heading_is_the_list() {
        let used = split_messages_used(
            "It quoted \"Messages used:\" before.\nMessages used:\n- \"the deploy\"",
        );
        assert_eq!(used.items, ["\"the deploy\""]);
        assert_eq!(used.body, "It quoted \"Messages used:\" before.");
    }

    #[test]
    fn pieces_point_at_the_newest_message_they_quote() {
        let messages: Vec<String> = [
            "Deploy the API",
            "Deployed the API to staging.",
            "Deploy the API again",
        ]
        .iter()
        .map(|m| words(m))
        .collect();
        assert_eq!(
            cited_pieces("\"deploy the API\"", &messages),
            Some(vec![("deploy the API".to_owned(), 2)])
        );
        assert_eq!(
            cited_pieces("\"to staging\" … \"again\"", &messages),
            Some(vec![("to staging".to_owned(), 1), ("again".to_owned(), 2)])
        );
        assert_eq!(cited_pieces("\"to production\"", &messages), None);
        assert_eq!(
            cited_pieces("\"staging\" … \"production\"", &messages),
            None
        );
        assert_eq!(cited_pieces("\"\"", &messages), Some(vec![]));
    }
}
