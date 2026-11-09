# Agent guidance

Formation is a proposed open-source macOS app and CLI for collaborating through local Claude Code and Codex sessions; the repository holds the product documentation and an early scaffold of the Rust workspace and the desktop app. Keep changes small, reviewable and aligned with the agreed requirements.

## Repository layout

- `documentations/`: product planning documents (one-pager, product requirements, user stories, screens and flows, tech spec, frontend and backend architecture, implementation plan, product plan, competitive landscape) and HTML mockups.
- `.claude/skills/`: project-scoped Claude Code skills for macOS, SwiftUI and Swift development.
- `crates/`: the Rust workspace (`formation-proto`, `formation-core`, `formationd`, `formation-cli`), described in the [tech spec](documentations/4_techspec.md#code-layout).
- `apps/desktop/`: the Tauri 2 app, with the React and TypeScript UI in `src/` and the Rust side in `src-tauri/` (a member of the Cargo workspace), described in the [frontend architecture](documentations/5_frontend_architecture.md#code-layout).
- `THIRD_PARTY_NOTICES.md`: every file copied or generated from another project, with its source and license.
- `scripts/`: repository tooling: the documentation link checker and the pull request issue-link checker, with its tests.

## Checks

The documentation and issue-link checks need Node.js 18 or later and no packages; the issue-link check also needs the GitHub CLI. The Rust checks need a stable Rust toolchain with Clippy and rustfmt. The desktop checks need pnpm (the version is pinned in `apps/desktop/package.json`); run `pnpm -C apps/desktop install` first. CI runs them as the `documentation`, `rust` and `desktop` checks, and runs the `issue-link` check, with the script tests, on every pull request.

```sh
node scripts/check-documentation.mjs    # local Markdown links and heading anchors resolve
node --test scripts/*.test.mjs          # tests for the repository scripts
gh pr view <N> --json body,closingIssuesReferences | node scripts/check-issue-link.mjs  # pull request <N> links its issue
cargo test --workspace                  # Rust unit and integration tests, including the app's Rust side
cargo fmt --all -- --check              # Rust formatting; run `cargo fmt --all` to fix
cargo clippy --workspace -- -D warnings # Rust lint
pnpm -C apps/desktop lint               # ESLint
pnpm -C apps/desktop typecheck          # TypeScript
pnpm -C apps/desktop test               # Vitest
pnpm -C apps/desktop ipc:generate       # regenerate src/ipc/generated after changing formation-proto; commit the result
pnpm -C apps/desktop tauri dev          # run the app in development
```

## Working agreements

- Read the relevant files before editing, and do not guess the architecture or dependencies.
- Keep secrets, credentials and local environment files out of Git. Add an example file when configuration needs to be shared.
- Run the relevant checks for the files you change, and report any check that could not run.
- For every behavior or setup change, check whether product, technical, setup or API documentation needs updating. Update the owning document in the same change; if none needs updating, say why in the final report.

## Documentation

Keep documents complementary rather than repeating the same requirement in several places: each fact has one owning document, and other documents link to it. If two documents disagree, resolve the disagreement explicitly before implementing the affected behavior. Update the owning document and its links instead of silently choosing one interpretation or keeping conflicting copies.

`node scripts/check-documentation.mjs` checks that every local Markdown link and heading anchor resolves, with the exact file-name case GitHub needs. CI runs it as the `documentation` check.

## Git branch conventions

- `main` is the integration branch.
- Name new branches `<type>/<short-description>`, using the commit types below and lowercase letters, digits and hyphens within each segment. Examples: `fix/login-redirect` and `docs/branch-conventions`. Do not use tool prefixes such as `codex/` or `claude/`. Follow an explicit user-provided branch name when given.
- Keep each branch focused on one feature, fix or documentation task.
- Start new, independent work from the latest `origin/main` after fetching. Continue an existing task on its existing branch; use another base only when the task explicitly depends on it.
- Inspect the current branch and working tree before switching or creating branches. Preserve unrelated local changes; use a separate worktree when isolation is needed. Do not rename an existing branch just to match this convention.
- Keep shared branch history intact. Do not force-push or rebase a shared branch without explicit authorization.
- Delete a topic branch after its pull request is merged and its work is no longer needed. Do not delete branches with unmerged work or active worktrees.

## Git workflow and pull requests

The team uses GitHub Flow, starting from an issue: issue -> `main` -> short-lived working branch -> pull request -> checks -> merge -> branch deletion. Keep `main` ready to deploy, and use working branches for incomplete work.

- Before starting work or opening an issue, check whether the work is already tracked or under way: search open and closed issues (`gh issue list --state all --search "<words>"`) and open pull requests (`gh pr list --search "<words>"`). Reuse a matching open issue instead of opening a duplicate. When a closed issue or merged pull request already covers the work, confirm the problem remains before reopening the issue or opening a new one.
- Start every piece of work from an issue, so the team can see who is doing what and two people do not do the same work. Assign yourself when you start (`gh issue edit <N> --add-assignee @me`). If someone else holds the issue or has a pull request open for it, ask them before taking it over.
- Link every pull request to its issue with `Closes #N`, or with `Part of #N` when more pull requests follow. Only a hotfix, an urgent fix for a broken `main` or deployment, may go without an issue, and its description says it is a hotfix. The `issue-link` check fails a pull request that has none of these and no issue linked in its Development sidebar. It ignores HTML comments and code, so the template's own examples do not count. Linking an issue only in the sidebar does not re-run the check; edit the description or re-run the check afterwards.
- Do not commit directly to `main`; every change reaches it through a pull request. Merge once the relevant checks pass. Teammate review is welcome but not required, so the author may merge their own pull request.
- Keep pull requests small and separate unrelated changes. Describe the resulting behavior, rationale and validation, including any checks that could not run. Bring in new changes from `main` regularly and resolve conflicts before merging.
- Write the pull request description with the sections of [`.github/pull_request_template.md`](.github/pull_request_template.md). GitHub fills it in on the web, but `gh pr create --body` does not, so agents copy its sections themselves.
- Open issues with the forms in `.github/ISSUE_TEMPLATE/`; blank issues are disabled. Issues track ideas and work.
- Use **Squash and merge** by default, so each pull request becomes one commit on `main` whose title and message follow the commit format below. Use **Rebase and merge** only when the individual commits are already meaningful, independently reviewable and worth keeping.
- After a merge, delete the local branch and start the next task from the latest `main`, especially after a squash merge.

## Commit messages

Use Conventional Commit types with the Lore Commit Protocol. Start every commit message with `type(scope): intent`; the scope is optional. Keep the intent concise, use an imperative verb, and explain why the change is needed.

Types:

- `feat`: new functionality.
- `fix`: bug correction.
- `docs`: documentation.
- `refactor`: code restructuring without behavior changes.
- `test`: tests.
- `build`: dependencies or build tooling.
- `ci`: CI configuration.
- `chore`: other maintenance.
- `perf`: performance improvements.
- `revert`: reversing a commit.

Use a short scope naming the affected area. Example: `fix(auth): refresh an expired session before retrying`.

Add an optional body for rationale, and git-native Lore trailers when they give decision context:

```text
Constraint: <external constraint that shaped the decision>
Rejected: <alternative considered> | <reason for rejection>
Confidence: <low|medium|high>
Scope-risk: <narrow|moderate|broad>
Directive: <forward-looking warning for future modifiers>
Tested: <what was verified>
Not-tested: <known gaps in verification>
```

Use `Rejected:` to record alternatives future modifiers should not revisit and `Directive:` for warnings. Include only trailers that add useful context.
