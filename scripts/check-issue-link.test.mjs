// Tests for check-issue-link.mjs. Run with `node --test scripts/*.test.mjs`.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { issueLink } from './check-issue-link.mjs';

const template = readFileSync(new URL('../.github/pull_request_template.md', import.meta.url), 'utf8');
const script = fileURLToPath(new URL('./check-issue-link.mjs', import.meta.url));
const passes = (body, closingIssuesReferences = []) => issueLink({ body, closingIssuesReferences }).ok;

test('closing keywords in every tense pass', () => {
  for (const keyword of ['Closes', 'close', 'closed', 'Fixes', 'fix', 'fixed', 'Resolves', 'resolve', 'resolved']) {
    assert.ok(passes(`## What this does\n\n${keyword} #12`), keyword);
  }
});

test('a colon, another repository, GH-N and a full issue URL pass', () => {
  assert.ok(passes('Fixes: #3'));
  assert.ok(passes('Resolves callmejustdodo/Shared#4'));
  assert.ok(passes('Closes GH-7'));
  assert.ok(passes('Closes https://github.com/callmejustdodo/Shared/issues/5'));
});

test('punctuation, several references and CRLF line endings pass', () => {
  assert.ok(passes('(Closes #12)'));
  assert.ok(passes('Closes #12, closes #13.'));
  assert.ok(passes('## What this does\r\n\r\nCloses #12\r\n'));
});

test('Part of passes', () => {
  assert.ok(passes('Part of #6'));
  assert.ok(passes('This is part of #6.'));
});

test('a keyword and an issue on different lines fail', () => {
  assert.equal(passes('Closes\n#12'), false);
  assert.equal(passes('This is part of\n#6'), false);
});

test('an issue linked by GitHub passes without any text', () => {
  assert.ok(passes('', [{ number: 15 }]));
  assert.match(issueLink({ body: null, closingIssuesReferences: [{ number: 15 }] }).reason, /#15/u);
  assert.ok(issueLink({ body: 'Closes #1', closingIssuesReferences: null }).ok);
});

test('a hotfix note passes', () => {
  assert.ok(passes('Hotfix: main fails to build after #20.'));
  assert.ok(passes('This is a hot-fix for the broken release.'));
});

test('the unfilled pull request template fails', () => {
  assert.equal(passes(template), false);
});

test('the template filled in with an issue link passes', () => {
  assert.ok(passes(template.replace('## What this does\n', '## What this does\n\nCloses #33\n')));
});

test('links only in comments or code fail', () => {
  assert.equal(passes('<!-- Closes #12 -->'), false);
  assert.equal(passes('<!--\nCloses #12\n-->'), false);
  assert.equal(passes('<!-- an unclosed comment\nCloses #12'), false);
  assert.equal(passes('Use `Closes #12` to link an issue.'), false);
  assert.equal(passes('Use ``Closes `#12` here`` to link an issue.'), false);
  assert.equal(passes('```\nCloses #12\n```'), false);
  assert.equal(passes('~~~md\nPart of #12\n~~~'), false);
  assert.equal(passes('````\n```\nCloses #12\n```\n````'), false);
});

test('comment markers inside code do not hide a later link', () => {
  assert.ok(passes('Use `<!--` to open a comment.\n\nCloses #12'));
  assert.ok(passes('```\n<!--\n```\nCloses #12'));
});

test('a fence line with trailing text does not close the code block', () => {
  assert.equal(passes('```\n``` js\nCloses #12\n```'), false);
});

test('a hotfix mentioned only in a task-list item fails', () => {
  assert.equal(passes('- [ ] The description links its issue, or says this is a hotfix.'), false);
});

test('words that only contain a keyword fail', () => {
  assert.equal(passes('This encloses #5 and adds a prefix #6.'), false);
  assert.equal(passes('Closes issue 12'), false);
  assert.equal(passes('Closes #12abc'), false);
  assert.equal(passes('Part of the #12 work'), false);
});

test('an empty or missing description fails', () => {
  assert.equal(passes(''), false);
  assert.equal(issueLink({ body: null }).ok, false);
  assert.equal(issueLink().ok, false);
  assert.equal(issueLink(null).ok, false);
});

test('the command exits 0 with a link, 1 without one, and 2 on unreadable input', () => {
  const run = (args, input) => {
    try {
      execFileSync(process.execPath, [script, ...args], { input, stdio: ['pipe', 'pipe', 'pipe'] });
      return 0;
    } catch (error) {
      return error.status;
    }
  };
  assert.equal(run([], JSON.stringify({ body: 'Closes #1', closingIssuesReferences: [] })), 0);
  assert.equal(run([], JSON.stringify({ body: template, closingIssuesReferences: [] })), 1);
  assert.equal(run([], 'not json'), 2);
  assert.equal(run([], 'null'), 2);

  const directory = mkdtempSync(path.join(tmpdir(), 'issue-link-'));
  try {
    const file = path.join(directory, 'pull-request.json');
    writeFileSync(file, JSON.stringify({ body: 'Part of #6', closingIssuesReferences: [] }));
    assert.equal(run([file], ''), 0);
    assert.equal(run([path.join(directory, 'missing.json')], ''), 2);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
