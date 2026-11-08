# Agent guidance

Formation is a proposed open-source macOS app and CLI for collaborating through local Claude Code and Codex sessions; the repository is in the planning stage and holds product documentation only. Keep changes small, reviewable and aligned with the agreed requirements.

## Repository layout

- `documentations/`: product planning documents (one-pager, product plan, competitive landscape).
- `scripts/`: repository tooling, currently the documentation link checker.

## Checks

No application code exists yet, so the documentation check is the only check so far. It needs Node.js 18 or later and no packages:

```sh
node scripts/check-documentation.mjs  # local Markdown links and heading anchors resolve
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

This repository has one maintainer, who reviews and merges their own work. Keep `main` working.

- Small, self-contained changes may go directly to `main` once the relevant checks pass. Use a topic branch and a pull request for larger or risky changes, or to let CI run before merging.
- Issues are optional. Before opening one, search open and closed issues (`gh issue list --state all --search "<words>"`) and reuse a matching open issue. When an issue tracks the work, link the pull request to it with `Closes #N`, or with `Part of #N` when more pull requests follow.
- Write a pull request description with the sections of [`.github/pull_request_template.md`](.github/pull_request_template.md). GitHub fills it in on the web, but `gh pr create --body` does not, so agents copy its sections themselves.
- Use **Squash and merge** for pull requests by default, so each becomes one commit whose title and message follow the commit format below. Use **Rebase and merge** only when the individual commits are worth keeping.

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
