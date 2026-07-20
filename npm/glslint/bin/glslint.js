#!/usr/bin/env node
'use strict';

// Thin shim: hand argv to the real binary and mirror its exit status.
//
// `stdio: 'inherit'` passes the parent's file descriptors straight through, so
// `glslint lsp` speaks its stdio protocol to the editor with no relaying here.

const { spawnSync } = require('node:child_process');
const { binaryPath } = require('../index.js');

let bin;
try {
  bin = binaryPath();
} catch (err) {
  console.error(err.message);
  process.exit(1);
}

const result = spawnSync(bin, process.argv.slice(2), { stdio: 'inherit' });

if (result.error) {
  console.error(`glslint: could not run ${bin}: ${result.error.message}`);
  process.exit(1);
}
// A signal-killed child reports status null; surface that as a failure.
process.exit(result.status === null ? 1 : result.status);
