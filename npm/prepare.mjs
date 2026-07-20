#!/usr/bin/env node
// Lay out the npm packages for a release.
//
// Generates one package per platform from `glslint/platforms.json` (the single
// source of truth for the target table) and stamps every version from the
// release tag, so the wrapper and the binaries it pins can never disagree.
// The generated packages are build output, not source: `npm/@glslint/` is
// gitignored.
//
//   node npm/prepare.mjs --version 0.3.0 --binaries dist
//   node npm/prepare.mjs --version 0.3.0 --dry-run
//
// --dry-run needs no binaries and writes nothing that git tracks, so it is safe
// to run anywhere: CI uses it to keep the drift guard honest on every PR.
//
// `--binaries` is the directory holding the release assets, named as the
// release workflow uploads them: `glslint-<rust-target>`, plus `.exe` on
// Windows.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const NPM_DIR = path.dirname(fileURLToPath(import.meta.url));
const REPO_DIR = path.dirname(NPM_DIR);
const WRAPPER_DIR = path.join(NPM_DIR, 'glslint');
const OUT_DIR = path.join(NPM_DIR, '@glslint');
const LICENSES = ['LICENSE-MIT', 'LICENSE-APACHE'];

function fail(message) {
  console.error(`prepare: ${message}`);
  process.exit(1);
}

function parseArgs(argv) {
  const args = { dryRun: false };
  for (let i = 0; i < argv.length; i++) {
    switch (argv[i]) {
      case '--version':
        args.version = argv[++i];
        break;
      case '--binaries':
        args.binaries = argv[++i];
        break;
      case '--dry-run':
        args.dryRun = true;
        break;
      default:
        fail(`unknown argument ${argv[i]}`);
    }
  }
  if (!args.version) fail('--version is required');
  // Tags arrive as v0.3.0; accept either form.
  args.version = args.version.replace(/^v/, '');
  if (!/^\d+\.\d+\.\d+/.test(args.version)) fail(`not a version: ${args.version}`);
  if (!args.binaries && !args.dryRun) fail('--binaries is required (or pass --dry-run)');
  return args;
}

const readJson = (p) => JSON.parse(fs.readFileSync(p, 'utf8'));
const writeJson = (p, value) => fs.writeFileSync(p, `${JSON.stringify(value, null, 2)}\n`);

const args = parseArgs(process.argv.slice(2));
const platforms = readJson(path.join(WRAPPER_DIR, 'platforms.json'));
const wrapperPath = path.join(WRAPPER_DIR, 'package.json');
const wrapper = readJson(wrapperPath);

// Drift guard: the wrapper's optionalDependencies are the checked-in copy of
// the platform table. If the two disagree, an install would silently miss a
// platform, so fail here instead of publishing something broken.
const expected = Object.keys(platforms).map((key) => `@glslint/${key}`).sort();
const declared = Object.keys(wrapper.optionalDependencies ?? {}).sort();
if (expected.join(',') !== declared.join(',')) {
  fail(
    `npm/glslint/package.json optionalDependencies do not match platforms.json.\n` +
      `  platforms.json: ${expected.join(', ')}\n` +
      `  package.json:   ${declared.join(', ') || '(none)'}`,
  );
}

// Stamp the wrapper: its own version, and an exact pin on each platform package.
// This rewrites a checked-in file, which is why --dry-run leaves it alone: only
// a release build should touch it, and only in an ephemeral checkout.
wrapper.version = args.version;
for (const dep of expected) wrapper.optionalDependencies[dep] = args.version;
if (args.dryRun) {
  console.log(`prepare: glslint@${args.version} (wrapper, dry run: not written)`);
} else {
  writeJson(wrapperPath, wrapper);
  for (const license of LICENSES) {
    fs.copyFileSync(path.join(REPO_DIR, license), path.join(WRAPPER_DIR, license));
  }
  console.log(`prepare: glslint@${args.version} (wrapper)`);
}

fs.rmSync(OUT_DIR, { recursive: true, force: true });

for (const [key, platform] of Object.entries(platforms)) {
  const name = `@glslint/${key}`;
  const dir = path.join(OUT_DIR, key);
  const isWindows = platform.os === 'win32';
  const binName = isWindows ? 'glslint.exe' : 'glslint';
  fs.mkdirSync(path.join(dir, 'bin'), { recursive: true });

  writeJson(path.join(dir, 'package.json'), {
    name,
    version: args.version,
    description: `The glslint binary for ${key}`,
    homepage: wrapper.homepage,
    bugs: wrapper.bugs,
    repository: { ...wrapper.repository, directory: `npm/@glslint/${key}` },
    license: wrapper.license,
    engines: wrapper.engines,
    os: [platform.os],
    cpu: [platform.cpu],
    files: ['bin/', 'LICENSE-MIT', 'LICENSE-APACHE'],
  });

  fs.writeFileSync(
    path.join(dir, 'README.md'),
    `# ${name}\n\n` +
      `The prebuilt glslint binary for ${key} (Rust target \`${platform.target}\`).\n\n` +
      `Do not install this package directly. It is an optional dependency of ` +
      `[glslint](https://www.npmjs.com/package/glslint), which resolves the right ` +
      `binary for your platform.\n`,
  );

  for (const license of LICENSES) {
    fs.copyFileSync(path.join(REPO_DIR, license), path.join(dir, license));
  }

  const asset = path.join(args.binaries ?? '', `glslint-${platform.target}${isWindows ? '.exe' : ''}`);
  if (args.dryRun) {
    console.log(`prepare: ${name}@${args.version} (dry run, would copy ${asset})`);
    continue;
  }
  if (!fs.existsSync(asset)) fail(`missing release binary ${asset}`);
  const dest = path.join(dir, 'bin', binName);
  fs.copyFileSync(asset, dest);
  // npm preserves the executable bit through pack/publish; the downloaded
  // release asset does not carry one, so set it here.
  fs.chmodSync(dest, 0o755);
  console.log(`prepare: ${name}@${args.version} (${asset})`);
}

console.log(`prepare: done, packages in ${path.relative(REPO_DIR, OUT_DIR)}/`);
