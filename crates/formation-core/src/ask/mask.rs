//! Secret masking for everything that leaves the owner's Mac from a side
//! conversation: answers, excerpts, and routing reasons (NFR-003).
//!
//! Masking is a backup, not a guarantee: it supplements the owner's sharing
//! choices and preview (FR-009). It covers private key blocks, well-known token
//! formats, credentials in URLs and authorization headers, values assigned to
//! password, secret, token, and key names, and the owner's own email addresses,
//! which Claude Code attaches to every turn of the transcript (TASK-006).
//!
//! Masking the same text twice gives the same result.

use std::sync::LazyLock;

use regex::{Captures, Regex};

/// What a masked secret is replaced with.
pub const MASK: &str = "[masked]";

/// A private key block, up to its end line or, when the text was cut, to the end.
static PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----.*?(?:-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----|\z)",
    )
    .expect("valid regex")
});

/// Tokens recognizable by their documented prefix or shape.
static TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        // Anthropic and OpenAI keys (`sk-ant-api03-…`, `sk-proj-…`).
        r"\bsk-[A-Za-z0-9_-]{20,}",
        // GitHub tokens and fine-grained personal access tokens.
        r"|\bgh[pousr]_[A-Za-z0-9]{20,}",
        r"|\bgithub_pat_[A-Za-z0-9_]{20,}",
        // GitLab, Slack, npm, Hugging Face, Tailscale.
        r"|\bglpat-[A-Za-z0-9_-]{16,}",
        r"|\bxox[abposre]-[A-Za-z0-9-]{10,}",
        r"|\bnpm_[A-Za-z0-9]{36}\b",
        r"|\bhf_[A-Za-z0-9]{30,}",
        r"|\btskey-[A-Za-z0-9-]{16,}",
        // AWS access key IDs, Google API keys, Stripe secret and restricted keys.
        r"|\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
        r"|\bAIza[0-9A-Za-z_-]{35}",
        r"|\b[rs]k_(?:live|test)_[0-9A-Za-z]{16,}",
        // JSON Web Tokens.
        r"|\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
    ))
    .expect("valid regex")
});

/// `Bearer <token>` and `Basic <credentials>`.
static AUTH_SCHEME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(bearer|basic)([ \t]+)[A-Za-z0-9_.~+/-]{12,}=*").expect("valid regex")
});

/// `scheme://user:password@host`.
static URL_CREDENTIALS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b([a-z][a-z0-9+.-]{0,30}://)[^\s/@:]+:[^\s/@]+@").expect("valid regex")
});

/// `--password value`, `--api-key value`.
static CREDENTIAL_FLAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(^|\s)(--?[a-z0-9-]*(?:password|passwd|passphrase|secret|token|api-?key|access-?key|credentials?)[a-z0-9-]*)([ \t]+)([^\s-]\S*)",
    )
    .expect("valid regex")
});

/// `password: value`, `DB_PASSWORD=value`, `"api_key": "value"`.
static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b([A-Za-z0-9_.-]*(?:password|passwd|passphrase|secret|token|api[_-]?key|apikey|access[_-]?key|private[_-]?key|credentials?)[A-Za-z0-9_.-]*)(["']?[ \t]*[:=][ \t]*)("[^"\n]*"|'[^'\n]*'|[^\s,;]+)"#,
    )
    .expect("valid regex")
});

/// Masks secret-like strings and the owner's email addresses.
#[derive(Debug, Clone, Default)]
pub struct Masker {
    owner_emails: Option<Regex>,
}

impl Masker {
    /// Masks secret-like strings only.
    pub fn new() -> Self {
        Self::default()
    }

    /// Also masks these addresses, ignoring case: the owner's Claude Code account
    /// and any other address of theirs that a transcript may carry.
    pub fn with_owner_emails<I, S>(emails: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let alternatives: Vec<String> = emails
            .into_iter()
            .map(|email| email.as_ref().trim().to_owned())
            .filter(|email| email.contains('@'))
            .map(|email| regex::escape(&email))
            .collect();
        let owner_emails = (!alternatives.is_empty()).then(|| {
            Regex::new(&format!(
                r"(?i)(^|[^A-Za-z0-9._%+-])(?:{})($|[^A-Za-z0-9.-]|\.[^A-Za-z0-9-]|\.$)",
                alternatives.join("|")
            ))
            .expect("escaped addresses form a valid regex")
        });
        Self { owner_emails }
    }

    /// The text with every recognized secret replaced by [`MASK`].
    pub fn mask(&self, text: &str) -> String {
        let text = PRIVATE_KEY.replace_all(text, MASK);
        let text = TOKENS.replace_all(&text, MASK);
        let text = AUTH_SCHEME.replace_all(&text, format!("${{1}}${{2}}{MASK}"));
        let text = URL_CREDENTIALS.replace_all(&text, format!("${{1}}{MASK}@"));
        let text = CREDENTIAL_FLAG.replace_all(&text, |caps: &Captures| {
            if is_secret_value(&caps[2], &caps[4]) {
                format!("{}{}{}{MASK}", &caps[1], &caps[2], &caps[3])
            } else {
                caps[0].to_owned()
            }
        });
        let text = ASSIGNMENT.replace_all(&text, |caps: &Captures| {
            let value = &caps[3];
            if !is_secret_value(&caps[1], value) {
                return caps[0].to_owned();
            }
            let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'');
            match quote {
                Some(q) => format!("{}{}{q}{MASK}{q}", &caps[1], &caps[2]),
                None => format!("{}{}{MASK}", &caps[1], &caps[2]),
            }
        });
        match &self.owner_emails {
            // Matches overlap at shared boundary characters, so mask until stable.
            Some(emails) => {
                let mut text = text.into_owned();
                loop {
                    let next = emails
                        .replace_all(&text, format!("${{1}}{MASK}${{2}}"))
                        .into_owned();
                    if next == text {
                        return text;
                    }
                    text = next;
                }
            }
            None => text.into_owned(),
        }
    }
}

/// Whether the value given to a credential-like name looks like a secret.
///
/// Passwords, passphrases, and secrets are masked whatever their value. Token,
/// key, and credential names also name counts and settings (`max_tokens=1000`,
/// `token: expired`), so their values are masked only when they look like
/// credentials: at least 8 characters with a digit, or a quoted value of that
/// length.
fn is_secret_value(name: &str, value: &str) -> bool {
    let unquoted = value.trim_matches(['"', '\'']);
    if unquoted.is_empty() || unquoted == MASK {
        return false;
    }
    let name = name.to_ascii_lowercase();
    if ["password", "passwd", "passphrase", "secret"]
        .iter()
        .any(|word| name.contains(word))
    {
        return true;
    }
    if unquoted.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let quoted = unquoted.len() != value.len();
    unquoted.chars().count() >= 8
        && (quoted || unquoted.chars().any(|c| c.is_ascii_digit()))
        && !unquoted.contains(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(text: &str) -> String {
        Masker::new().mask(text)
    }

    /// Each secret is gone and the mask took its place.
    fn assert_masked(text: &str, secret: &str) {
        let masked = mask(text);
        assert!(
            !masked.contains(secret),
            "{secret:?} survived in {masked:?}"
        );
        assert!(masked.contains(MASK), "{masked:?}");
    }

    #[test]
    fn known_token_formats_are_masked() {
        // Built at run time so secret scanners do not flag these test strings.
        let secrets: Vec<String> = [
            ("sk-ant-api03-", "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789"),
            ("sk-proj-", "AbCdEfGhIjKlMnOpQrStUvWx"),
            ("ghp_", "AbCdEfGhIjKlMnOpQrStUvWxYz012345"),
            ("github_pat_", "11ABCDEFG0123456789_abcdefghijklmnop"),
            ("glpat-", "AbCdEfGhIjKlMnOp1234"),
            ("xoxb-", "1234567890-abcdefghij"),
            ("AKIA", "IOSFODNN7EXAMPLE"),
            ("AIza", "SyA-1234567890abcdefghijklmnopqrstu"),
            ("sk_live_", "51HabcdEFGHijklMNOP"),
            ("tskey-auth-", "kAbCdEf1CNTRL-0123456789abcdef"),
            ("eyJ", "hbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U"),
        ]
        .iter()
        .map(|(prefix, rest)| format!("{prefix}{rest}"))
        .collect();
        for secret in &secrets {
            assert_masked(&format!("The key is {secret}, keep it safe."), secret);
        }
    }

    #[test]
    fn private_key_blocks_are_masked_even_when_cut() {
        let block = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\nQyNTUxOQAAACD\n-----END OPENSSH PRIVATE KEY-----";
        let masked = mask(&format!("Use this:\n{block}\nthen run deploy."));
        assert_eq!(masked, format!("Use this:\n{MASK}\nthen run deploy."));

        let cut = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA7…";
        assert_eq!(mask(&format!("Key: {cut}")), format!("Key: {MASK}"));
        assert_masked(
            "-----BEGIN PGP PRIVATE KEY BLOCK-----\nlQOYBF\n-----END PGP PRIVATE KEY BLOCK-----",
            "lQOYBF",
        );
    }

    #[test]
    fn passwords_and_secrets_are_masked_whatever_their_value() {
        for (text, secret) in [
            ("password: hunter2", "hunter2"),
            ("DB_PASSWORD=correct-horse", "correct-horse"),
            (
                "export AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI",
                "wJalrXUtnFEMI",
            ),
            (r#"{"password": "p@ss word"}"#, "p@ss word"),
            ("client_secret = 'abc'", "abc"),
            ("mysql --password s3cret -u root", "s3cret"),
            ("passphrase:letmein", "letmein"),
        ] {
            assert_masked(text, secret);
        }
        assert_eq!(mask("password: hunter2"), format!("password: {MASK}"));
        assert_eq!(
            mask(r#"{"password": "p@ss word"}"#),
            format!(r#"{{"password": "{MASK}"}}"#)
        );
    }

    #[test]
    fn tokens_and_keys_are_masked_when_they_look_like_credentials() {
        for (text, secret) in [
            ("GITHUB_TOKEN=abcd1234efgh5678", "abcd1234efgh5678"),
            ("api_key: 9f8e7d6c5b4a", "9f8e7d6c5b4a"),
            (r#""access_token": "opaque-value""#, "opaque-value"),
            ("curl --token 12ab34cd56ef", "12ab34cd56ef"),
            (
                "Authorization: Bearer abc.def-ghi_jkl012",
                "abc.def-ghi_jkl012",
            ),
            (
                "Authorization: Basic dXNlcjpwYXNzd29yZA==",
                "dXNlcjpwYXNzd29yZA",
            ),
            (
                "git clone https://dohyeon:hunter2@github.com/acme/api.git",
                "hunter2",
            ),
            ("postgres://svc:pw123@db.internal:5432/app", "pw123"),
        ] {
            assert_masked(text, secret);
        }
        assert_eq!(
            mask("https://dohyeon:hunter2@github.com/acme"),
            format!("https://{MASK}@github.com/acme")
        );
    }

    #[test]
    fn ordinary_prose_and_counts_are_kept() {
        for text in [
            "The login API cleanup is done; it's adding a token expiry check.",
            "max_tokens=1000 and input_tokens: 512",
            "The token: expired yesterday, so it asked again.",
            "Password reset emails go out from the queue.",
            "See https://github.com/acme/api and ssh://git@github.com/acme.",
            "The bearer of bad news.",
            "Run `npm test -- --token-file ./ci.json` locally.",
        ] {
            assert_eq!(mask(text), text);
        }
    }

    #[test]
    fn the_owners_emails_are_masked_and_others_kept() {
        let masker = Masker::with_owner_emails(["Dohyeon@Example.com", "not-an-email", ""]);
        assert_eq!(
            masker.mask("Signed in as dohyeon@example.com (dohyeon@example.com)."),
            format!("Signed in as {MASK} ({MASK}).")
        );
        assert_eq!(masker.mask("<DOHYEON@EXAMPLE.COM>"), format!("<{MASK}>"));
        // Longer addresses that merely contain the owner's are not the owner's.
        for text in [
            "Mail minsu@example.com or xdohyeon@example.com.",
            "dohyeon@example.community",
        ] {
            assert_eq!(masker.mask(text), text);
        }
        assert_eq!(
            Masker::new().mask("dohyeon@example.com"),
            "dohyeon@example.com"
        );
    }

    #[test]
    fn masking_twice_changes_nothing_more() {
        let masker = Masker::with_owner_emails(["dohyeon@example.com"]);
        let text = "password: hunter2, key sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWx, \
            https://u:p4ss@host/x, Bearer abcdefghijkl0123, dohyeon@example.com";
        let once = masker.mask(text);
        assert_eq!(masker.mask(&once), once);
        for secret in [
            "hunter2",
            "AbCdEfGh",
            "p4ss",
            "abcdefghijkl0123",
            "dohyeon@",
        ] {
            assert!(!once.contains(secret), "{secret:?} in {once:?}");
        }
    }
}
