//! Always-allowed reads (D-028, FR-015): a read-only tool call whose target
//! resolves inside the agent folder or one of its assigned repositories runs
//! without asking. Every other call needs the owner's approval, and so does a
//! read whose target cannot be resolved.
//!
//! A target is resolved the way the file system will see it: relative paths
//! against the agent folder (the run's working directory), `~` and `~/…` against
//! the home folder, `..` and symlinks through [`std::fs::canonicalize`]. The
//! result must sit under the canonical form of an allowed root, compared by
//! path component, so `/a/api-old` is not inside `/a/api`. A path that does not
//! exist, `~user`, an empty path, a path with a NUL byte, or a field of the wrong
//! type is unresolvable. On macOS, Windows drive letters, backslashes and
//! `file://` URLs are ordinary file names, so they resolve only if such a file
//! exists inside the folder.
//!
//! Known limits:
//! - The check runs before the tool does. A symlink swapped in between the two
//!   is not seen; creating one needs a write, which asks first.
//! - Glob and Grep search a directory tree. The check covers the search root and
//!   the pattern's literal prefix, not symlinks met during the search; whether
//!   Claude Code's tools follow them is to be checked with the gate (TASK-044).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use formation_proto::policy::{AllowReason, PolicyVerdict, ToolCall};

use crate::ports::ToolContext;

/// Characters that start the wildcard part of a Glob pattern. A backslash is
/// included so an escaped character is never taken as literal.
const GLOB_META: &[char] = &['*', '?', '[', ']', '{', '}', '\\'];

/// `Policy::evaluate` for what this task covers: an always-allowed read is
/// allowed, anything else needs approval. Saved rules (TASK-028) come between
/// the two.
pub fn evaluate(context: &ToolContext, call: &ToolCall) -> PolicyVerdict {
    if ReadScope::from_context(context).allows(call) {
        PolicyVerdict::Allow {
            reason: AllowReason::AlwaysAllowedRead,
        }
    } else {
        PolicyVerdict::NeedsApproval
    }
}

/// Where reads run without asking: the agent folder and its assigned
/// repositories, resolved once.
#[derive(Debug, Clone)]
pub struct ReadScope {
    /// The run's working directory, for relative paths.
    cwd: PathBuf,
    /// Canonical allowed roots. A root that cannot be resolved is left out.
    roots: Vec<PathBuf>,
    home: Option<PathBuf>,
}

impl ReadScope {
    /// Relative repository paths are taken from the folder.
    pub fn new(folder: &Path, repositories: &[PathBuf], home: Option<PathBuf>) -> Self {
        let roots = std::iter::once(folder.to_path_buf())
            .chain(repositories.iter().map(|repo| folder.join(repo)))
            .filter_map(|root| fs::canonicalize(root).ok())
            .collect();
        Self {
            cwd: folder.to_path_buf(),
            roots,
            home,
        }
    }

    /// The scope of a turn, with the home folder from `HOME`.
    pub fn from_context(context: &ToolContext) -> Self {
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from);
        Self::new(&context.folder, &context.repositories, home)
    }

    /// True when the call is one of Claude Code's read-only tools and everything
    /// it reads resolves inside the scope. Tool names match exactly, so MCP tools
    /// (`mcp__…`) and unknown tools are never reads.
    pub fn allows(&self, call: &ToolCall) -> bool {
        let input = &call.input;
        match call.tool_name.as_str() {
            "Read" => self.field_inside(input, "file_path"),
            "NotebookRead" => self.field_inside(input, "notebook_path"),
            "LS" => self.field_inside(input, "path"),
            "Grep" => self.search_roots(input).is_some(),
            "Glob" => self.glob_inside(input),
            _ => false,
        }
    }

    /// True when `raw`, as a tool would read it, resolves inside the scope.
    pub fn contains(&self, raw: &str) -> bool {
        self.contains_from(raw, &self.cwd)
    }

    fn contains_from(&self, raw: &str, base: &Path) -> bool {
        if raw.is_empty() || raw.contains('\0') {
            return false;
        }
        if let Some(rest) = raw.strip_prefix('~') {
            // `~` and `~/…` only; `~user` is not expanded.
            if !(rest.is_empty() || rest.starts_with('/')) {
                return false;
            }
            let Some(home) = &self.home else {
                return false;
            };
            let expanded = home.join(rest.trim_start_matches('/'));
            // A tool that does not expand `~` reads `<base>/~/…` instead; if that
            // exists, it must be inside too.
            let literal = base.join(raw);
            return self.inside(&expanded)
                && (fs::symlink_metadata(&literal).is_err() || self.inside(&literal));
        }
        self.inside(&base.join(raw))
    }

    /// True when the path exists and its canonical form is under an allowed root.
    fn inside(&self, path: &Path) -> bool {
        fs::canonicalize(path)
            .is_ok_and(|path| self.roots.iter().any(|root| path.starts_with(root)))
    }

    fn field_inside(&self, input: &Value, field: &str) -> bool {
        input
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|raw| self.contains(raw))
    }

    /// The directories a Grep or Glob may search: its optional `path`, or the
    /// working directory. A `~` path gives both readings that [`contains`]
    /// checked. `None` when it is outside the scope or unresolvable.
    ///
    /// [`contains`]: ReadScope::contains
    fn search_roots(&self, input: &Value) -> Option<Vec<PathBuf>> {
        match input.get("path") {
            None | Some(Value::Null) => self.inside(&self.cwd).then(|| vec![self.cwd.clone()]),
            Some(Value::String(raw)) if self.contains(raw) => {
                let literal = self.cwd.join(raw);
                Some(match (raw.strip_prefix('~'), &self.home) {
                    (Some(rest), Some(home)) => {
                        let mut roots = vec![home.join(rest.trim_start_matches('/'))];
                        if fs::symlink_metadata(&literal).is_ok() {
                            roots.push(literal);
                        }
                        roots
                    }
                    _ => vec![literal],
                })
            }
            Some(_) => None,
        }
    }

    /// A Glob reads its search root, extended by the pattern's literal leading
    /// components (which may be absolute, start with `~`, or pass through a
    /// symlink). The wildcard part may not contain `..` at all, so brace
    /// alternatives such as `{..,src}` cannot climb out either; a file name that
    /// contains `..` therefore asks first. Nor may a brace alternative start with
    /// `/`, `~` or `\`, since brace expansion would turn `{/etc,src}/*` into the
    /// absolute pattern `/etc/*`.
    fn glob_inside(&self, input: &Value) -> bool {
        let Some(pattern) = input.get("pattern").and_then(Value::as_str) else {
            return false;
        };
        if pattern.is_empty() {
            return false;
        }
        let Some(roots) = self.search_roots(input) else {
            return false;
        };
        let (literal, wildcard) = split_glob(pattern);
        if wildcard.contains("..") || brace_may_leave_root(wildcard) {
            return false;
        }
        literal.is_empty() || roots.iter().all(|root| self.contains_from(literal, root))
    }
}

/// Splits a Glob pattern into its leading components without wildcards and the
/// rest, at a `/`. `src/**/*.rs` gives `("src", "**/*.rs")`.
fn split_glob(pattern: &str) -> (&str, &str) {
    let Some(meta) = pattern.find(GLOB_META) else {
        return (pattern, "");
    };
    match pattern[..meta].rfind('/') {
        // A pattern such as `/*` keeps its root as the literal part.
        Some(0) => ("/", &pattern[1..]),
        Some(slash) => (&pattern[..slash], &pattern[slash + 1..]),
        None => ("", pattern),
    }
}

/// True when a brace alternative (after `{` or `,`) starts with `/`, `~` or an
/// escape, so its expansion could start a pattern of its own outside the search
/// root. Errs on the side of asking: `*x,/y` is refused too.
fn brace_may_leave_root(wildcard: &str) -> bool {
    wildcard
        .as_bytes()
        .windows(2)
        .any(|pair| matches!(pair, [b'{' | b',', b'/' | b'~' | b'\\']))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use serde_json::json;

    use formation_proto::common::AgentId;
    use formation_proto::policy::Caller;

    use super::*;
    use crate::ask::test_support::TempDir;

    /// ```text
    /// <base>/
    ///   home/                      HOME
    ///     notes.txt
    ///     Agents/
    ///       api/                   the agent folder and working directory
    ///         AGENTS.md
    ///         src/main.rs
    ///         notebook.ipynb
    ///         link_in   -> src
    ///         link_out  -> <base>/outside
    ///         file_out  -> <base>/outside/secret.txt
    ///         ~/notes.txt          a folder literally named `~`
    ///       api-old/old.txt        a sibling whose name starts with the folder's
    ///   repo/README.md             an assigned repository
    ///   outside/secret.txt
    /// ```
    struct Fixture {
        dir: TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new("policy-reads");
            let base = dir.path();
            let api = base.join("home/Agents/api");
            for folder in [
                api.join("src"),
                api.join("~"),
                base.join("home/Agents/api-old"),
                base.join("repo"),
                base.join("outside"),
            ] {
                fs::create_dir_all(folder).unwrap();
            }
            for file in [
                base.join("home/notes.txt"),
                api.join("AGENTS.md"),
                api.join("src/main.rs"),
                api.join("notebook.ipynb"),
                api.join("~/notes.txt"),
                base.join("home/Agents/api-old/old.txt"),
                base.join("repo/README.md"),
                base.join("outside/secret.txt"),
            ] {
                fs::write(file, "x").unwrap();
            }
            symlink("src", api.join("link_in")).unwrap();
            symlink(base.join("outside"), api.join("link_out")).unwrap();
            symlink(base.join("outside/secret.txt"), api.join("file_out")).unwrap();
            Self { dir }
        }

        fn base(&self) -> &Path {
            self.dir.path()
        }

        fn folder(&self) -> PathBuf {
            self.base().join("home/Agents/api")
        }

        fn scope(&self) -> ReadScope {
            ReadScope::new(
                &self.folder(),
                &[self.base().join("repo")],
                Some(self.base().join("home")),
            )
        }

        /// Replaces `<base>` and `<folder>` in a test row.
        fn fill(&self, value: &Value) -> Value {
            let text = value
                .to_string()
                .replace("<folder>", self.folder().to_str().unwrap())
                .replace("<base>", self.base().to_str().unwrap());
            serde_json::from_str(&text).unwrap()
        }
    }

    fn call(tool_name: &str, input: Value) -> ToolCall {
        ToolCall {
            tool_use_id: Some("toolu_1".into()),
            tool_name: tool_name.into(),
            input,
        }
    }

    #[test]
    fn reads_inside_the_folder_and_repositories_are_allowed_and_escapes_ask() {
        let fixture = Fixture::new();
        let scope = fixture.scope();
        // (row, tool, input, allowed without asking)
        #[rustfmt::skip]
        let rows: Vec<(&str, &str, Value, bool)> = vec![
            ("absolute inside", "Read", json!({"file_path": "<folder>/AGENTS.md"}), true),
            ("relative inside", "Read", json!({"file_path": "src/main.rs"}), true),
            ("dot segments that stay inside", "Read", json!({"file_path": "./src/../AGENTS.md"}), true),
            ("offset and limit", "Read", json!({"file_path": "src/main.rs", "offset": 1, "limit": 5}), true),
            ("parent escape", "Read", json!({"file_path": "../../../outside/secret.txt"}), false),
            ("parent escape to a sibling with the same prefix", "Read", json!({"file_path": "src/../../api-old/old.txt"}), false),
            ("absolute sibling with the same prefix", "Read", json!({"file_path": "<base>/home/Agents/api-old/old.txt"}), false),
            ("absolute outside", "Read", json!({"file_path": "<base>/outside/secret.txt"}), false),
            ("system file", "Read", json!({"file_path": "/etc/passwd"}), false),
            ("absolute with dot segments out", "Read", json!({"file_path": "<folder>/../../../outside/secret.txt"}), false),
            ("tilde outside the folder", "Read", json!({"file_path": "~/notes.txt"}), false),
            ("tilde into the folder", "Read", json!({"file_path": "~/Agents/api/AGENTS.md"}), true),
            ("tilde with doubled slash", "Read", json!({"file_path": "~//Agents/api/AGENTS.md"}), true),
            ("tilde alone is the home folder", "Read", json!({"file_path": "~"}), false),
            ("tilde with dot segments out", "Read", json!({"file_path": "~/Agents/api/../../notes.txt"}), false),
            ("tilde of another user", "Read", json!({"file_path": "~root/.ssh/id_rsa"}), false),
            ("symlink that stays inside", "Read", json!({"file_path": "link_in/main.rs"}), true),
            ("symlinked folder out", "Read", json!({"file_path": "link_out/secret.txt"}), false),
            ("symlinked file out", "Read", json!({"file_path": "file_out"}), false),
            ("assigned repository", "Read", json!({"file_path": "<base>/repo/README.md"}), true),
            ("assigned repository by a relative path", "Read", json!({"file_path": "../../../repo/README.md"}), true),
            ("missing file", "Read", json!({"file_path": "src/missing.rs"}), false),
            ("empty path", "Read", json!({"file_path": ""}), false),
            ("path that is not a string", "Read", json!({"file_path": 7}), false),
            ("no path", "Read", json!({}), false),
            ("NUL byte", "Read", json!({"file_path": "src/main.rs\u{0}"}), false),
            ("file URL", "Read", json!({"file_path": "file:///etc/passwd"}), false),
            ("Windows drive path", "Read", json!({"file_path": "C:\\Windows\\win.ini"}), false),
            ("Windows-style parent escape", "Read", json!({"file_path": "..\\..\\..\\outside\\secret.txt"}), false),
            ("notebook inside", "NotebookRead", json!({"notebook_path": "notebook.ipynb"}), true),
            ("list the folder", "LS", json!({"path": "<folder>"}), true),
            ("list outside", "LS", json!({"path": "<base>/outside"}), false),
            ("grep the working directory", "Grep", json!({"pattern": "fn main"}), true),
            ("grep with a null path", "Grep", json!({"pattern": "fn main", "path": null}), true),
            ("grep a subfolder", "Grep", json!({"pattern": "x", "path": "src", "glob": "*.rs"}), true),
            ("grep a repository", "Grep", json!({"pattern": "x", "path": "<base>/repo"}), true),
            ("grep outside", "Grep", json!({"pattern": "x", "path": "/etc"}), false),
            ("grep through a symlink out", "Grep", json!({"pattern": "x", "path": "link_out"}), false),
            ("grep the home folder", "Grep", json!({"pattern": "x", "path": "~"}), false),
            ("glob the working directory", "Glob", json!({"pattern": "**/*.rs"}), true),
            ("glob under an absolute literal prefix", "Glob", json!({"pattern": "<folder>/src/*.rs"}), true),
            ("glob a subfolder", "Glob", json!({"pattern": "*.rs", "path": "src"}), true),
            ("glob with a parent escape", "Glob", json!({"pattern": "../**/*"}), false),
            ("glob with a parent escape after a wildcard", "Glob", json!({"pattern": "*/../../*"}), false),
            ("glob with a brace escape", "Glob", json!({"pattern": "src/{..,x}/*"}), false),
            ("glob with brace alternatives inside", "Glob", json!({"pattern": "{src,link_in}/{main,lib}.rs"}), true),
            ("glob with a brace alternative from the root", "Glob", json!({"pattern": "{/etc,src}/*"}), false),
            ("glob with an empty brace alternative before the root", "Glob", json!({"pattern": "{x,}{,/etc}/passwd"}), false),
            ("glob with a brace alternative from the home folder", "Glob", json!({"pattern": "{~,src}/*"}), false),
            ("glob with an escaped brace alternative", "Glob", json!({"pattern": "{\\/etc,src}/*"}), false),
            ("glob of an absolute folder outside", "Glob", json!({"pattern": "/etc/*"}), false),
            ("glob from the root", "Glob", json!({"pattern": "/*"}), false),
            ("glob through a symlink out", "Glob", json!({"pattern": "link_out/*"}), false),
            ("glob from the home folder", "Glob", json!({"pattern": "~/**"}), false),
            ("glob with a path outside", "Glob", json!({"pattern": "*.txt", "path": "<base>/outside"}), false),
            ("glob of a missing literal folder", "Glob", json!({"pattern": "missing/*.rs"}), false),
            ("empty glob", "Glob", json!({"pattern": ""}), false),
            ("command", "Bash", json!({"command": "cat AGENTS.md"}), false),
            ("write inside", "Write", json!({"file_path": "<folder>/new.txt", "content": "x"}), false),
            ("edit inside", "Edit", json!({"file_path": "<folder>/AGENTS.md", "old_string": "x", "new_string": "y"}), false),
            ("network", "WebFetch", json!({"url": "https://example.com", "prompt": "title"}), false),
            ("search", "WebSearch", json!({"query": "formation"}), false),
            ("tool names match exactly", "read", json!({"file_path": "src/main.rs"}), false),
            ("MCP read tool", "mcp__fs__read_file", json!({"path": "src/main.rs"}), false),
        ];
        let mut failures = Vec::new();
        for (row, tool, input, expected) in rows {
            if scope.allows(&call(tool, fixture.fill(&input))) != expected {
                failures.push(format!("{row}: expected allowed = {expected}"));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn a_folder_given_through_a_symlink_is_resolved() {
        let fixture = Fixture::new();
        let alias = fixture.base().join("alias");
        symlink(fixture.folder(), &alias).unwrap();
        let scope = ReadScope::new(&alias, &[], None);
        assert!(scope.contains("AGENTS.md"));
        assert!(scope.contains(fixture.folder().join("AGENTS.md").to_str().unwrap()));
        assert!(!scope.contains("../api-old/old.txt"));
    }

    #[test]
    fn without_a_home_folder_tilde_asks() {
        let fixture = Fixture::new();
        let scope = ReadScope::new(&fixture.folder(), &[], None);
        assert!(!scope.contains("~/Agents/api/AGENTS.md"));
        assert!(scope.contains("AGENTS.md"));
    }

    #[test]
    fn a_tilde_folder_inside_the_folder_must_also_be_inside() {
        let fixture = Fixture::new();
        let api = fixture.folder();
        // `~/Agents/api/AGENTS.md` expands inside, and `<folder>/~/Agents/...` does
        // not exist, so a tool reading either stays inside.
        assert!(fixture.scope().contains("~/Agents/api/AGENTS.md"));
        // Now make the literal reading point outside.
        fs::create_dir_all(api.join("~/Agents")).unwrap();
        symlink(fixture.base().join("outside"), api.join("~/Agents/api")).unwrap();
        fs::write(fixture.base().join("outside/AGENTS.md"), "x").unwrap();
        assert!(!fixture.scope().contains("~/Agents/api/AGENTS.md"));
    }

    #[test]
    fn a_glob_under_a_tilde_path_checks_both_readings() {
        let fixture = Fixture::new();
        let api = fixture.folder();
        let glob = call(
            "Glob",
            json!({"pattern": "src/*.rs", "path": "~/Agents/api"}),
        );
        assert!(fixture.scope().allows(&glob));
        // `<folder>/~/Agents/api` exists and stays inside, but its `src` points out.
        fs::create_dir_all(api.join("~/Agents/api")).unwrap();
        symlink(fixture.base().join("outside"), api.join("~/Agents/api/src")).unwrap();
        assert!(fixture.scope().contains("~/Agents/api"));
        assert!(!fixture.scope().allows(&glob));
    }

    #[test]
    fn a_missing_folder_allows_nothing() {
        let fixture = Fixture::new();
        let scope = ReadScope::new(&fixture.base().join("gone"), &[], None);
        assert!(!scope.allows(&call("Grep", json!({"pattern": "x"}))));
        assert!(!scope.contains("/etc/passwd"));
    }

    #[test]
    fn evaluate_allows_reads_and_asks_for_everything_else() {
        let fixture = Fixture::new();
        let context = ToolContext {
            agent_id: AgentId::generate(),
            folder: fixture.folder(),
            repositories: vec![fixture.base().join("repo")],
            caller: Caller::Owner,
            request_id: None,
        };
        assert_eq!(
            evaluate(&context, &call("Read", json!({"file_path": "AGENTS.md"}))),
            PolicyVerdict::Allow {
                reason: AllowReason::AlwaysAllowedRead
            }
        );
        assert_eq!(
            evaluate(&context, &call("Bash", json!({"command": "npm test"}))),
            PolicyVerdict::NeedsApproval
        );
        assert_eq!(
            evaluate(
                &context,
                &call("Read", json!({"file_path": "../../../outside/secret.txt"}))
            ),
            PolicyVerdict::NeedsApproval
        );
    }

    #[test]
    fn split_glob_keeps_the_literal_prefix() {
        assert_eq!(split_glob("src/**/*.rs"), ("src", "**/*.rs"));
        assert_eq!(split_glob("**/*.rs"), ("", "**/*.rs"));
        assert_eq!(split_glob("/etc/*"), ("/etc", "*"));
        assert_eq!(split_glob("/*"), ("/", "*"));
        assert_eq!(split_glob("src/main.rs"), ("src/main.rs", ""));
        assert_eq!(split_glob("a/b\\*c/d"), ("a", "b\\*c/d"));
    }
}
