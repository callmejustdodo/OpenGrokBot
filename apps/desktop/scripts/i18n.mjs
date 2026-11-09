// Extracts the UI copy from `t("key", "English default", ...)` calls into
// src/i18n/en.json and checks that src/i18n/ko.json translates every key and
// no other (TASK-040). Keys are nested on "." as i18next expects.
//
//   node scripts/i18n.mjs          write en.json, then check ko.json
//   node scripts/i18n.mjs --check  also fail when en.json is out of date
//
// Uses the TypeScript compiler already in devDependencies to read the source.
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const root = fileURLToPath(new URL("..", import.meta.url));
const srcDir = join(root, "src");
export const enPath = join(srcDir, "i18n", "en.json");
export const koPath = join(srcDir, "i18n", "ko.json");

// Keys built at run time from a value, enumerated from the generated IPC types.
// `t(\`history.status.${status}\`, humanize(status))` in views/history.
const DYNAMIC = [
  {
    prefix: "history.status.",
    types: ["QuestionStatus", "RequestStatus", "ApprovalStatus", "ApprovalChoice"],
    english: (value) => {
      const words = value.replace(/_/g, " ");
      return words.charAt(0).toUpperCase() + words.slice(1);
    },
  },
];

function sourceFiles(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return entry.name === "generated" ? [] : sourceFiles(path);
    return /\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name) ? [path] : [];
  });
}

/** The text of a string literal, a substitution-free template, or a `+` chain of them; otherwise null. */
function literal(node) {
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) return node.text;
  if (ts.isParenthesizedExpression(node)) return literal(node.expression);
  if (ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.PlusToken) {
    const left = literal(node.left);
    const right = literal(node.right);
    return left === null || right === null ? null : left + right;
  }
  return null;
}

function optionDefaults(node) {
  const out = {};
  if (!node || !ts.isObjectLiteralExpression(node)) return out;
  for (const prop of node.properties) {
    if (!ts.isPropertyAssignment(prop)) continue;
    const name = prop.name.getText().replace(/^["']|["']$/g, "");
    if (!name.startsWith("defaultValue")) continue;
    const text = literal(prop.initializer);
    if (text !== null) out[name] = text;
  }
  return out;
}

function unionValues(typeName) {
  const text = readFileSync(join(srcDir, "ipc", "generated", `${typeName}.ts`), "utf8");
  return [...text.matchAll(/"([^"]+)"/g)].map((m) => m[1]);
}

/**
 * Every message the code asks for: a map from i18next key to its English text.
 * A plural call (`count` with `defaultValue_one`/`_other`) gives `key_one` and `key_other`.
 * `problems` lists calls the extractor cannot read and keys with two different defaults.
 */
export function extract() {
  const messages = new Map();
  const plurals = new Set();
  const problems = [];
  const add = (key, text, where) => {
    if (messages.has(key) && messages.get(key) !== text) {
      problems.push(`${where}: "${key}" has two defaults: "${messages.get(key)}" and "${text}"`);
      return;
    }
    messages.set(key, text);
  };

  for (const file of sourceFiles(srcDir)) {
    const source = ts.createSourceFile(file, readFileSync(file, "utf8"), ts.ScriptTarget.Latest, true);
    const visit = (node) => {
      if (ts.isCallExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === "t") {
        const where = `${relative(root, file)}:${source.getLineAndCharacterOfPosition(node.getStart()).line + 1}`;
        const [keyArg, second, third] = node.arguments;
        const key = keyArg && literal(keyArg);
        if (key === null || key === undefined) {
          const dynamic = keyArg && ts.isTemplateExpression(keyArg) && DYNAMIC.find((d) => d.prefix === keyArg.head.text);
          if (dynamic) {
            for (const value of dynamic.types.flatMap(unionValues)) add(dynamic.prefix + value, dynamic.english(value), where);
          } else {
            problems.push(`${where}: a t() key that is not a literal and not listed in DYNAMIC`);
          }
        } else {
          const text = second && literal(second);
          const options = optionDefaults(text === null || text === undefined ? second : third);
          if (options.defaultValue_one !== undefined || options.defaultValue_other !== undefined) {
            plurals.add(key);
            add(`${key}_one`, options.defaultValue_one ?? options.defaultValue_other, where);
            add(`${key}_other`, options.defaultValue_other ?? options.defaultValue_one, where);
          } else if (text !== null && text !== undefined) {
            add(key, text, where);
          } else if (options.defaultValue !== undefined) {
            add(key, options.defaultValue, where);
          } else {
            problems.push(`${where}: "${key}" has no English default`);
          }
        }
      }
      ts.forEachChild(node, visit);
    };
    visit(source);
  }
  return { messages, plurals, problems };
}

/** `{ "a.b": "x" }` → `{ a: { b: "x" } }`, sorted, reporting a key that is also a prefix of another. */
export function nest(messages, problems = []) {
  const out = {};
  for (const key of [...messages.keys()].sort()) {
    const parts = key.split(".");
    let node = out;
    for (const part of parts.slice(0, -1)) {
      if (typeof node[part] === "string") problems.push(`"${key}" nests under the message "${part}"`);
      node[part] ??= {};
      node = node[part];
    }
    const last = parts[parts.length - 1];
    if (typeof node[last] === "object") problems.push(`"${key}" is both a message and a group`);
    else node[last] = messages.get(key);
  }
  return out;
}

export function flatten(tree, prefix = "", out = new Map()) {
  for (const [name, value] of Object.entries(tree)) {
    const key = prefix + name;
    if (value !== null && typeof value === "object") flatten(value, `${key}.`, out);
    else out.set(key, value);
  }
  return out;
}

const placeholders = (text) => [...new Set([...String(text).matchAll(/\{\{\s*([\w.]+)[^}]*\}\}/g)].map((m) => m[1]))].sort().join(",");

/**
 * Compares ko.json with the extracted messages. Korean has one plural form, so a
 * plural key needs only `key_other`. Returns the missing keys, the keys no code
 * uses, and the keys whose `{{placeholders}}` differ from English.
 */
export function checkKorean(messages, plurals, ko) {
  const expected = new Map();
  for (const [key, text] of messages) {
    const base = key.replace(/_(one|other)$/, "");
    if (plurals.has(base) && key.endsWith("_one")) continue;
    expected.set(key, text);
  }
  const korean = flatten(ko);
  const missing = [...expected.keys()].filter((key) => !korean.has(key));
  const unused = [...korean.keys()].filter((key) => !expected.has(key));
  const placeholderMismatch = [...expected.keys()].filter(
    (key) => korean.has(key) && placeholders(korean.get(key)) !== placeholders(expected.get(key)),
  );
  return { missing, unused, placeholderMismatch };
}

function main() {
  const { messages, plurals, problems } = extract();
  const tree = nest(messages, problems);
  const json = `${JSON.stringify(tree, null, 2)}\n`;
  let failed = problems.length > 0;
  for (const problem of problems) console.error(problem);

  const current = readFileSync(enPath, "utf8");
  if (process.argv.includes("--check")) {
    if (current !== json) {
      console.error("src/i18n/en.json is out of date; run `pnpm i18n:extract`.");
      failed = true;
    }
  } else if (current !== json) {
    writeFileSync(enPath, json);
    console.log(`Wrote ${messages.size} messages to src/i18n/en.json.`);
  }

  const { missing, unused, placeholderMismatch } = checkKorean(messages, plurals, JSON.parse(readFileSync(koPath, "utf8")));
  for (const key of missing) console.error(`ko.json is missing "${key}"`);
  for (const key of unused) console.error(`ko.json has "${key}", which no code uses`);
  for (const key of placeholderMismatch) console.error(`ko.json "${key}" has different {{placeholders}} from English`);
  if (failed || missing.length || unused.length || placeholderMismatch.length) process.exit(1);
  console.log(`${messages.size} messages; ko.json translates every one.`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) main();
