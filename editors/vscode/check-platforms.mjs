#!/usr/bin/env node
// Drift guard for the extension's platform table.
//
// The extension ships its own copy of the platform table
// (editors/vscode/targets.json) because the packaged .vsix can't read npm's copy
// at install time. This asserts that copy still names the same Rust target for
// every platform as the single source of truth, npm/glslint/platforms.json, so
// the three hardcoded tables (npm, homebrew-via-npm, and the extension) can never
// disagree.
//
//   node editors/vscode/check-platforms.mjs
//
// CI runs it on every PR (see .github/workflows/ci.yml). It writes nothing and
// exits non-zero on drift.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_DIR = path.dirname(path.dirname(HERE));
const CANONICAL = path.join(REPO_DIR, 'npm', 'glslint', 'platforms.json');
const LOCAL = path.join(HERE, 'targets.json');

const readJson = (p) => JSON.parse(fs.readFileSync(p, 'utf8'));

// npm/glslint/platforms.json is `{ key: { os, cpu, target } }`; the extension
// only needs `{ key: target }`. Reduce the canonical table to that mapping and
// compare it order-independently.
const canonical = Object.fromEntries(
  Object.entries(readJson(CANONICAL)).map(([key, { target }]) => [key, target]),
);
const local = readJson(LOCAL);

const normalize = (map) => JSON.stringify(Object.entries(map).sort());
if (normalize(canonical) !== normalize(local)) {
  console.error(
    'check-platforms: editors/vscode/targets.json has drifted from the platform\n' +
      'table in npm/glslint/platforms.json. Update the extension copy to match.\n' +
      `  npm/glslint/platforms.json: ${JSON.stringify(canonical)}\n` +
      `  editors/vscode/targets.json: ${JSON.stringify(local)}`,
  );
  process.exit(1);
}
console.log('check-platforms: extension platform table matches npm/glslint/platforms.json');
