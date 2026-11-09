#!/usr/bin/env node
// Checks that local Markdown links and heading anchors resolve the way GitHub renders them.
//
//   node scripts/check-documentation.mjs                  every Markdown file Git tracks or would track
//   node scripts/check-documentation.mjs docs README.md   only these files or directories
//   node scripts/check-documentation.mjs --exclude ko     everything except this directory (repeatable),
//                                                          e.g. a translation mirror whose headings differ
//
// Skips external links, links inside code and HTML comments, symlinked Markdown files (such as
// CLAUDE.md -> AGENTS.md) and dot-directories other than .github. File names must match in case,
// because GitHub and Linux CI are case-sensitive even when the local disk is not. Links that climb
// out of the repository fail, except GitHub's own pages such as ../../issues from the root.
// Exits 1 and lists every broken link when one fails. Needs Node.js 18+ and no packages.
import { execFileSync } from 'node:child_process';
import { existsSync, lstatSync, readdirSync, readFileSync, statSync } from 'node:fs';
import path from 'node:path';

const MARKDOWN = /\.(md|markdown)$/iu;
const SKIPPED_DIRECTORIES = new Set(['node_modules', 'vendor', 'dist', 'build', 'out', 'coverage']);
// Repository pages a README reaches with ../../<route> from the repository root.
const GITHUB_ROUTES = new Set(['issues', 'pulls', 'pull', 'wiki', 'releases', 'actions', 'discussions', 'security',
  'tree', 'blob', 'compare', 'commits', 'commit', 'tags', 'branches', 'projects', 'milestones', 'labels', 'network', 'graphs', 'raw']);

function repositoryRoot() {
  try {
    return execFileSync('git', ['rev-parse', '--show-toplevel'], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim();
  } catch {
    return process.cwd();
  }
}

const root = repositoryRoot();
const relative = (file) => path.relative(root, file).split(path.sep).join('/');

// True when a directory on the way to this repository-relative path is one the check skips.
function skippedPath(relativePath) {
  return relativePath.split('/').slice(0, -1)
    .some((segment) => (segment.startsWith('.') && segment !== '.github') || SKIPPED_DIRECTORIES.has(segment));
}

function walk(directory) {
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const target = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === '.git' || skippedPath(`${relative(target)}/x`)) continue;
      files.push(...walk(target));
    } else if (entry.isFile() && MARKDOWN.test(entry.name)) {
      files.push(target);
    }
  }
  return files;
}

function gitFiles() {
  try {
    const output = execFileSync('git', ['ls-files', '-z', '--cached', '--others', '--exclude-standard', '--', ':(icase)*.md', ':(icase)*.markdown'],
      { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] });
    return output.split('\0').filter(Boolean).map((file) => path.join(root, file));
  } catch {
    return null;
  }
}

function parseArguments(argv) {
  const paths = [];
  const excluded = [];
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--exclude' || arg.startsWith('--exclude=')) {
      const value = arg === '--exclude' ? argv[++index] : arg.slice('--exclude='.length);
      if (!value) throw new Error('--exclude needs a path');
      excluded.push(path.resolve(value));
    } else {
      paths.push(arg);
    }
  }
  return { paths, excluded };
}

function markdownFiles(args, excluded) {
  const isExcluded = (file) => excluded.some((directory) => file === directory || file.startsWith(directory + path.sep));
  let files;
  if (args.length) {
    files = args.flatMap((arg) => {
      const target = path.resolve(arg);
      if (!existsSync(target)) throw new Error(`No such file or directory: ${arg}`);
      return statSync(target).isDirectory() ? walk(target) : [target];
    });
  } else {
    files = (gitFiles() ?? walk(root)).filter((file) => !skippedPath(relative(file)));
  }
  return [...new Set(files)]
    .filter((file) => MARKDOWN.test(file) && !isExcluded(file) && existsSync(file) && !lstatSync(file).isSymbolicLink())
    .sort();
}

// Blanks out a byte-order mark, front matter, fenced code blocks (also inside block quotes), HTML
// comments and, unless kept for headings, inline code. Every character offset and line break stays,
// so line numbers are right and links or headings inside those parts are ignored.
function prose(markdown, { keepInlineCode = false } = {}) {
  const lines = markdown.replace(/^﻿/u, ' ').split('\n');
  const closing = lines.findIndex((line, index) => index > 0 && /^(---|\.\.\.)\s*$/u.test(line));
  let frontMatter = lines[0]?.trim() === '---' && closing > 0;
  let fence = null;
  const blankLine = (line) => ' '.repeat(line.length);
  const kept = lines.map((line, index) => {
    if (frontMatter) {
      if (index === closing) frontMatter = false;
      return blankLine(line);
    }
    const body = line.replace(/^(?:\s*>)+/u, '');
    const marker = body.match(/^\s*(`{3,}|~{3,})/u);
    if (fence) {
      if (marker && marker[1][0] === fence[0] && marker[1].length >= fence.length && !body.slice(marker[0].length).trim()) fence = null;
      return blankLine(line);
    }
    if (marker && !(marker[1][0] === '`' && body.slice(marker[0].length).includes('`'))) {
      fence = marker[1];
      return blankLine(line);
    }
    return line;
  });
  const blank = (match) => match.replace(/[^\n]/gu, ' ');
  const text = kept.join('\n');
  // A code span may wrap onto the next line, but not across a blank line.
  return (keepInlineCode ? text : text.replace(/(`+)(?!`)((?:(?!\n[ \t]*\n)[\s\S])*?[^`])\1(?!`)/gu, blank))
    .replace(/<!--[\s\S]*?-->/gu, blank);
}

const ENTITIES = { amp: '&', lt: '<', gt: '>', quot: '"', apos: "'", nbsp: ' ' };

function decodeEntity(entity, name) {
  if (ENTITIES[name]) return ENTITIES[name];
  // Other named entities are symbols or punctuation, which anchors drop anyway.
  if (!name.startsWith('#')) return '';
  const code = Number(name.slice(1).replace(/^x/iu, '0x'));
  return Number.isInteger(code) && code >= 0 && code <= 0x10ffff ? String.fromCodePoint(code) : entity;
}

// An HTML tag as CommonMark recognizes it; `HashMap<K, V>` is text, not a tag.
const TAG = /<\/?[A-Za-z][A-Za-z0-9-]*(?:\s+[A-Za-z_:][\w.:-]*(?:\s*=\s*(?:"[^"]*"|'[^']*'|[^\s"'=<>`]+))?)*\s*\/?>/gu;

// GitHub's anchor for a heading: the rendered text, lowercased, keeping only letters, marks, decimal
// digits, connector punctuation, joiners, hyphens and spaces, with each space turned into a hyphen.
// Checked against GitHub's renderer in tests/check-documentation.test.mjs.
function slug(rawHeading) {
  const kept = [];
  const keep = (value) => `\u0000${kept.push(value) - 1}\u0000`;
  const text = rawHeading
    .replace(/(`+)(.+?)\1/gu, (_, __, content) => keep(content.trim()))
    .replace(/\\([!-/:-@[-`{-~])/gu, (_, character) => keep(character))
    .replace(/<([a-z][a-z\d+.-]*:[^\s<>]*)>/giu, (_, url) => keep(url))
    .replace(/!\[[^\]]*\]\([^)]*\)|!\[[^\]]*\]\[[^\]]*\]/gu, '')
    .replace(/\[([^\]]*)\]\([^)]*\)/gu, '$1')
    .replace(/\[([^\]]*)\]\[[^\]]*\]/gu, '$1')
    .replace(TAG, '')
    .replace(/&(#?\w+);/gu, decodeEntity)
    .replace(/(^|[^\p{L}\p{N}_])(_{1,3})(?=\S)(.*?\S)\2(?=$|[^\p{L}\p{N}_])/gu, '$1$3')
    .replace(/\u0000(\d+)\u0000/gu, (_, index) => kept[Number(index)]);
  return text.toLowerCase().replace(/[^\p{Alphabetic}\p{M}\p{Nd}\p{Pc}\p{Join_Control}\- ]/gu, '').replace(/ /gu, '-');
}

const ATX = /^ {0,3}#{1,6}(?:\s+(.*?))?(?:\s+#+)?\s*$/u;
const BLOCK_START = /^\s*([-*+]\s|[>|#<]|\d+[.)]\s)/u;

const anchorCache = new Map();
function anchors(file) {
  if (anchorCache.has(file)) return anchorCache.get(file);
  const text = prose(readFileSync(file, 'utf8'), { keepInlineCode: true });
  const lines = text.split('\n');
  const headings = [];
  let offset = 0;
  lines.forEach((line, index) => {
    // Headings may sit in block quotes and list items: "> ## Quoted", "- ## Listed".
    const body = line.replace(/^ {0,3}(?:>\s?)+/u, '').replace(/^ {0,3}(?:[-*+]|\d+[.)])\s+(?=#)/u, '');
    const atx = body.match(ATX);
    if (atx) {
      headings.push({ offset, text: atx[1] ?? '' });
    } else if (/^ {0,3}(=+|-+)\s*$/u.test(line) && lines[index - 1]?.trim()) {
      // Setext heading: the whole paragraph above the underline, unless it belongs to another block.
      let start = index - 1;
      while (start > 0 && lines[start - 1].trim() && !ATX.test(lines[start - 1])) start -= 1;
      const paragraph = lines.slice(start, index);
      if (!BLOCK_START.test(paragraph[0]) && !/^( {4}|\t)/u.test(paragraph[0])) {
        headings.push({ offset, text: paragraph.map((part) => part.trim()).join('\n') });
      }
    }
    offset += line.length + 1;
  });
  for (const match of text.matchAll(/<h([1-6])\b[^>]*>([\s\S]*?)<\/h\1\s*>/giu)) headings.push({ offset: match.index, text: match[2] });
  const found = new Set();
  for (const heading of headings.sort((a, b) => a.offset - b.offset)) {
    const base = slug(heading.text);
    let candidate = base;
    for (let count = 1; found.has(candidate); count += 1) candidate = `${base}-${count}`;
    found.add(candidate);
  }
  for (const match of text.matchAll(/<[a-z][^>]*?\s(?:id|name)\s*=\s*["']([^"']+)["']/giu)) found.add(match[1]);
  anchorCache.set(file, found);
  return found;
}

const directoryCache = new Map();
function entriesOf(directory) {
  if (!directoryCache.has(directory)) {
    let names = [];
    try {
      names = readdirSync(directory).map((name) => name.normalize('NFC'));
    } catch {
      // Missing, unreadable or not a directory: every lookup in it fails.
    }
    directoryCache.set(directory, names);
  }
  return directoryCache.get(directory);
}

// Resolves a path inside the repository segment by segment, so a case difference is reported even
// on macOS and Windows. Returns null when the target exists.
function missingReason(target) {
  const inside = path.relative(root, target);
  let current = root;
  for (const segment of inside.split(path.sep).filter(Boolean)) {
    const names = entriesOf(current);
    const wanted = segment.normalize('NFC');
    if (!names.includes(wanted)) {
      const other = names.find((name) => name.toLowerCase() === wanted.toLowerCase());
      return other ? `case differs from "${other}"` : 'not found';
    }
    current = path.join(current, segment);
  }
  return existsSync(target) ? null : 'not found';
}

function decode(value) {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

function linkTargets(text) {
  const targets = [];
  const add = (url, index) => targets.push({ url: url.trim().replace(/^<|>$/gu, ''), index });
  // Inline links and images, [text](target "title"), including nested ones such as badges and
  // destinations with balanced parentheses.
  for (const match of text.matchAll(/\]\(\s*(<[^>\n]*>|(?:[^\s()]|\([^\s()]*\))+)(?:\s+(?:"[^"\n]*"|'[^'\n]*'|\([^)\n]*\)))?\s*\)/gu)) {
    add(match[1], match.index);
  }
  // Reference definitions, [label]: target "title", alone on their line. Footnotes ([^1]:) and
  // lines such as "[TODO]: remember this" are text.
  for (const match of text.matchAll(/^ {0,3}\[(?!\^)[^\]\n]+\]:[ \t]*(?:\n[ \t]*)?(<[^>\n]*>|\S+)(?:[ \t]+(?:"[^"\n]*"|'[^'\n]*'|\([^)\n]*\)))?[ \t]*$/gmu)) {
    add(match[1], match.index);
  }
  // HTML links, images and <picture> sources.
  for (const tag of text.matchAll(/<(?:a|img|source)\b[^>]*>/giu)) {
    for (const attribute of tag[0].matchAll(/(?<![\w-])(href|src|srcset)\s*=\s*["']([^"']*)["']/giu)) {
      const urls = attribute[1].toLowerCase() === 'srcset' ? attribute[2].split(',').map((part) => part.trim().split(/\s+/u)[0]) : [attribute[2]];
      urls.forEach((url) => add(url, tag.index));
    }
  }
  return targets.sort((a, b) => a.index - b.index);
}

function check(file) {
  const text = prose(readFileSync(file, 'utf8'));
  const lineStarts = [0];
  for (let index = text.indexOf('\n'); index !== -1; index = text.indexOf('\n', index + 1)) lineStarts.push(index + 1);
  const lineOf = (offset) => {
    let low = 0;
    let high = lineStarts.length - 1;
    while (low < high) {
      const middle = Math.ceil((low + high) / 2);
      if (lineStarts[middle] <= offset) low = middle;
      else high = middle - 1;
    }
    return low + 1;
  };
  const errors = [];
  let checked = 0;
  for (const { url, index } of linkTargets(text)) {
    if (!url || /^[a-z][a-z\d+.-]*:/iu.test(url) || url.startsWith('//')) continue;
    const hash = url.indexOf('#');
    const pathPart = decode((hash === -1 ? url : url.slice(0, hash)).replace(/\?.*$/u, ''));
    const fragment = hash === -1 ? '' : decode(url.slice(hash + 1)).normalize('NFC').replace(/^user-content-/u, '');
    const target = !pathPart ? file : pathPart.startsWith('/') ? path.join(root, pathPart) : path.resolve(path.dirname(file), pathPart);
    const segments = path.relative(root, target).split(path.sep);
    let reason = null;
    if (segments[0] === '..' || path.isAbsolute(path.relative(root, target))) {
      if (segments[0] === '..' && segments[1] === '..' && GITHUB_ROUTES.has(segments[2])) continue;
      reason = 'outside the repository';
    }
    checked += 1;
    reason ??= missingReason(target);
    const anchorChecked = fragment && fragment.toLowerCase() !== 'top' && !/^L\d+(-L\d+)?$/u.test(fragment);
    if (!reason && anchorChecked && MARKDOWN.test(target) && statSync(target).isFile()) {
      const known = anchors(target);
      if (!known.has(fragment) && !known.has(fragment.toLowerCase())) reason = 'no such heading';
    }
    if (reason) errors.push({ file: relative(file), message: `${relative(file)}:${lineOf(index)}: ${url} (${reason})` });
  }
  return { checked, errors };
}

try {
  const { paths, excluded } = parseArguments(process.argv.slice(2));
  const files = markdownFiles(paths, excluded);
  let checked = 0;
  const errors = [];
  for (const file of files) {
    const result = check(file);
    checked += result.checked;
    errors.push(...result.errors);
  }
  if (errors.length) {
    const fileCount = new Set(errors.map((error) => error.file)).size;
    console.error(errors.map((error) => error.message).join('\n'));
    console.error(`\n${errors.length} broken link${errors.length === 1 ? '' : 's'} in ${fileCount} file${fileCount === 1 ? '' : 's'}.`);
    process.exitCode = 1;
  } else {
    console.log(`Checked ${checked} local links in ${files.length} Markdown files.`);
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 2;
}
