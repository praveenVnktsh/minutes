#!/usr/bin/env bun
// Run every test file in its own `bun test` process.
//
// Why not a plain `bun test`: it runs every file in ONE process, and
// `mock.module()` is process-wide, so a module mock one file installs is still in
// place for every file that runs after it. The order files run in comes from the
// directory listing, which differs between macOS and Linux. The suite passed on
// macOS and failed on the Ubuntu CI runner for exactly that reason: a file that
// happened to run first there left a partial `next/navigation` mock and a fake
// Tauri `invoke` behind, and two unrelated files broke.
//
// Restoring every mock in `afterAll` fixes the cases found so far, but it cannot
// be enforced, and one missed restore reintroduces the failure only on whichever
// machine lists files in the unlucky order. A process per file gives every file
// the clean module registry it already assumes, so the result no longer depends
// on order at all.
//
// Usage: `pnpm test` (or `bun scripts/test-each.mjs [file ...]` to run a subset).

import { spawnSync } from 'node:child_process';
import { readdirSync } from 'node:fs';
import { join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

// fileURLToPath, not URL.pathname: on Windows the pathname is "/C:/...".
const root = fileURLToPath(new URL('..', import.meta.url));

// Bun's own default patterns: *.test.*, *_test.*, *.spec.*, *_spec.*.
const TEST_FILE = /[._](test|spec)\.[cm]?[jt]sx?$/;
// Build output, dependencies, and the Rust crate (its target/ is enormous and
// holds no JavaScript tests).
const SKIP_DIRS = new Set(['node_modules', '.next', 'out', 'src-tauri', '.git']);

function findTests(dir) {
  const found = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.isDirectory()) {
      if (!SKIP_DIRS.has(entry.name)) found.push(...findTests(join(dir, entry.name)));
    } else if (TEST_FILE.test(entry.name)) {
      found.push(join(dir, entry.name));
    }
  }
  return found;
}

const requested = process.argv.slice(2);
// Sorted, so the log reads the same on every machine even though each file now
// runs alone and the order no longer affects the result.
const files = (requested.length ? requested.map((f) => join(process.cwd(), f)) : findTests(root))
  .map((f) => relative(root, f))
  .sort();

if (files.length === 0) {
  // No tests is not a pass: a discovery pattern that matches nothing would
  // otherwise turn the whole gate green forever.
  console.error('test-each: no test files found');
  process.exit(1);
}

const failed = [];
for (const file of files) {
  console.log(`\n=== ${file}`);
  const run = spawnSync('bun', ['test', `./${file}`], { cwd: root, stdio: 'inherit' });
  if (run.error) {
    console.error(`test-each: could not run bun for ${file}: ${run.error.message}`);
    failed.push(file);
  } else if (run.status !== 0) {
    failed.push(file);
  }
}

console.log(`\ntest-each: ${files.length - failed.length} of ${files.length} test files passed`);
if (failed.length) {
  console.error(`test-each: failed:\n${failed.map((f) => `  ${f}`).join('\n')}`);
  process.exit(1);
}
