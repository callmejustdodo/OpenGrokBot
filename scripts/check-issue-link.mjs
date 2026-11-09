#!/usr/bin/env node
// Checks that a pull request links the issue it belongs to, as AGENTS.md requires.
//
//   gh pr view <N> --json body,closingIssuesReferences | node scripts/check-issue-link.mjs
//   node scripts/check-issue-link.mjs pull-request.json   the same JSON, read from a file
//
// Passes when GitHub reports a linked issue (a closing keyword or the Development sidebar), when
// the description uses a closing keyword (close, fix or resolve in any tense) or "Part of" before
// an issue reference on the same line, or when the description says the pull request is a hotfix.
// HTML comments and code are ignored, and so are task-list items when looking for "hotfix",
// because the pull request template mentions `Closes #N` and hotfixes in them. Exits 1 with the
// reason when no link is found, and 2 when the input cannot be read. Needs Node.js 18+ and no packages.
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const ISSUE = String.raw`(?:(?:[\w.-]+\/[\w.-]+)?#\d+|GH-\d+|https:\/\/github\.com\/[\w.-]+\/[\w.-]+\/issues\/\d+)\b`;
const CLOSING = new RegExp(String.raw`\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)[ \t]*:?[ \t]+${ISSUE}`, 'iu');
const PART_OF = new RegExp(String.raw`\bpart[ \t]+of[ \t]*:?[ \t]+${ISSUE}`, 'iu');
const HOTFIX = /\bhot-?fix\b/iu;
const FENCE_OPEN = /^ {0,3}(`{3,}|~{3,})/u;
const FENCE_CLOSE = /^ {0,3}(`{3,}|~{3,})[ \t]*$/u;
const TASK_ITEM = /^\s*(?:[-*+]|\d+[.)])\s+\[[ xX]\]/u;

// Removes inline code and HTML comments from one line, in reading order, so that `<!--` inside
// code does not open a comment. Returns the kept text and whether a comment is still open.
function stripInline(line, inComment) {
  let rest = line;
  let kept = '';
  while (rest) {
    if (inComment) {
      const end = rest.indexOf('-->');
      if (end === -1) return { kept, inComment };
      rest = rest.slice(end + 3);
      inComment = false;
      continue;
    }
    const token = rest.match(/<!--|`+/u);
    if (!token) return { kept: kept + rest, inComment };
    kept += rest.slice(0, token.index);
    rest = rest.slice(token.index + token[0].length);
    if (token[0] === '<!--') {
      inComment = true;
      continue;
    }
    // A code span closes at the next run of exactly as many backticks; an unmatched run is literal.
    const close = rest.search(new RegExp('(?<!`)`{' + token[0].length + '}(?!`)', 'u'));
    if (close === -1) {
      kept += token[0];
    } else {
      rest = rest.slice(close + token[0].length);
    }
  }
  return { kept, inComment };
}

// The description's lines without HTML comments, fenced code blocks or inline code.
export function proseLines(markdown) {
  const lines = [];
  let fence = null;
  let inComment = false;
  for (const line of markdown.split(/\r?\n/u)) {
    if (fence) {
      const marker = line.match(FENCE_CLOSE)?.[1];
      if (marker && marker[0] === fence[0] && marker.length >= fence.length) fence = null;
      continue;
    }
    const opening = inComment ? undefined : line.match(FENCE_OPEN)?.[1];
    if (opening) {
      fence = opening;
      continue;
    }
    const result = stripInline(line, inComment);
    inComment = result.inComment;
    lines.push(result.kept);
  }
  return lines;
}

// Decides whether a pull request, shaped like `gh pr view --json body,closingIssuesReferences`, links its issue.
export function issueLink(pullRequest) {
  const { body, closingIssuesReferences } = pullRequest ?? {};
  const linked = (closingIssuesReferences ?? []).map((issue) => `#${issue.number}`);
  if (linked.length) return { ok: true, reason: `GitHub links ${linked.join(', ')} to this pull request.` };

  const lines = proseLines(body ?? '');
  const text = lines.join('\n');
  const reference = text.match(CLOSING) ?? text.match(PART_OF);
  if (reference) return { ok: true, reason: `The description says "${reference[0].replace(/\s+/gu, ' ')}".` };

  if (lines.some((line) => !TASK_ITEM.test(line) && HOTFIX.test(line))) {
    return { ok: true, reason: 'The description says this pull request is a hotfix.' };
  }

  return {
    ok: false,
    reason: 'This pull request links no issue. Write `Closes #N` in the description when it finishes the issue, '
      + '`Part of #N` when more pull requests follow, or say that it is a hotfix. '
      + 'An issue linked only in the Development sidebar counts, but linking it there does not re-run this check: '
      + 'edit the description or re-run the check afterwards. See "Git workflow and pull requests" in AGENTS.md.',
  };
}

function main() {
  const pullRequest = JSON.parse(readFileSync(process.argv[2] ?? 0, 'utf8'));
  if (typeof pullRequest !== 'object' || pullRequest === null || Array.isArray(pullRequest)) {
    throw new Error('expected a JSON object from `gh pr view --json body,closingIssuesReferences`');
  }
  const { ok, reason } = issueLink(pullRequest);
  if (ok) {
    console.log(reason);
  } else {
    console.error(reason);
    process.exitCode = 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    main();
  } catch (error) {
    console.error(`Could not read the pull request: ${error.message}`);
    process.exitCode = 2;
  }
}
