#!/usr/bin/env node
// Generate the Homebrew formula for a release.
//
// Renders `glslint.rb` from the release binaries: one url + sha256 per
// Homebrew-supported platform (macOS arm64/x64, Linux x64 — Homebrew has no
// Windows channel, so the win32 asset is deliberately skipped). The formula is
// build output the release workflow pushes to the tap repo
// (johncarmack1984/homebrew-tap); it is not tracked here.
//
// Homebrew loads a formula on every OS and CPU it supports when the tap is
// tapped (`brew tap`, `brew readall`), and a formula with no url on one of them
// fails the whole tap. Linux has no arm64 binary, so its `on_arm` block names
// the x64 binary with `depends_on arch: :x86_64`: the formula loads everywhere,
// and an install on arm64 Linux stops at the arch requirement instead of the
// tap breaking for everyone. Homebrew's style allows url and sha256 only inside
// a CPU block, so the stand-in gets one of its own. The formula sets no
// `version`: Homebrew reads it from the release urls, and `brew audit --strict`
// flags an explicit one as redundant.
//
// The platform table is read from `npm/glslint/platforms.json`, the same single
// source of truth the npm packaging uses, so the two channels can never name
// different Rust targets for the same platform.
//
//   node homebrew/prepare.mjs --version 0.4.0 --binaries dist --out glslint.rb
//   node homebrew/prepare.mjs --version 0.4.0 --dry-run
//
// --dry-run needs no binaries and writes nothing (it prints to stdout with
// placeholder checksums), so CI runs it on every PR to keep the platform table
// and the rendered formula honest before a release depends on them.

import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { fileURLToPath } from 'node:url';

const HOMEBREW_DIR = path.dirname(fileURLToPath(import.meta.url));
const REPO_DIR = path.dirname(HOMEBREW_DIR);
const PLATFORMS = path.join(REPO_DIR, 'npm', 'glslint', 'platforms.json');
const REPO = 'johncarmack1984/glslint';
const DESC = 'GLSL checker and language server for luma.gl/deck.gl shaders';
const HOMEPAGE = `https://github.com/${REPO}`;

// The Homebrew blocks each release asset maps into, keyed by its platforms.json
// entry. Homebrew has no Windows, so `win32-x64` is intentionally absent — its
// binary still ships on the GitHub Release and through npm.
const BREW_TARGETS = {
  'darwin-arm64': { os: 'macos', cpu: 'arm' },
  'darwin-x64': { os: 'macos', cpu: 'intel' },
  'linux-x64': { os: 'linux', cpu: 'intel' },
};

// The CPUs Homebrew supports on every OS, each with the `depends_on arch:`
// symbol an OS block needs when it has a binary for only that one.
const BREW_ARCH = { arm: 'arm64', intel: 'x86_64' };

function fail(message) {
  console.error(`homebrew/prepare: ${message}`);
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
      case '--out':
        args.out = argv[++i];
        break;
      case '--dry-run':
        args.dryRun = true;
        break;
      default:
        fail(`unknown argument ${argv[i]}`);
    }
  }
  if (!args.version) fail('--version is required');
  // Tags arrive as v0.4.0; accept either form.
  args.version = args.version.replace(/^v/, '');
  if (!/^\d+\.\d+\.\d+/.test(args.version)) fail(`not a version: ${args.version}`);
  if (!args.binaries && !args.dryRun) fail('--binaries is required (or pass --dry-run)');
  return args;
}

// A dry run has no binaries, so it renders a placeholder checksum: the point is
// to exercise the platform table and the template, not to produce a real
// formula.
const PLACEHOLDER_SHA = '0'.repeat(64);

function sha256(file) {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

// A CPU block. `requireArch` marks a stand-in: the OS's one binary, built for
// that arch, named on a CPU that has none so the formula still loads there.
function renderCpuBlock(cpu, url, sha, indent, requireArch) {
  const pad = ' '.repeat(indent);
  const inner = ' '.repeat(indent + 2);
  const lines = [`${pad}on_${cpu} do`];
  if (requireArch) {
    lines.push(
      `${inner}# No ${BREW_ARCH[cpu]} build for this OS: the ${requireArch} url only lets the`,
      `${inner}# formula load here, and the arch requirement stops an install.`,
    );
  }
  lines.push(`${inner}url "${url}"`, `${inner}sha256 "${sha}"`);
  if (requireArch) lines.push(`${inner}depends_on arch: :${requireArch}`);
  lines.push(`${pad}end`);
  return lines.join('\n');
}

function renderOsBlock(os, entries) {
  const byCpu = Object.fromEntries(entries.map((entry) => [entry.cpu, entry]));
  const missing = Object.keys(BREW_ARCH).filter((cpu) => !byCpu[cpu]);
  if (missing.length && entries.length !== 1) {
    fail(`on_${os} has binaries for ${Object.keys(byCpu).join(' and ')} but not for ${missing.join(' or ')}`);
  }
  const blocks = Object.keys(BREW_ARCH).map((cpu) => {
    const own = byCpu[cpu];
    if (own) return renderCpuBlock(cpu, own.url, own.sha, 4);
    const [only] = entries;
    return renderCpuBlock(cpu, only.url, only.sha, 4, BREW_ARCH[only.cpu]);
  });
  return [`  on_${os} do`, blocks.join('\n'), '  end'].join('\n');
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const platforms = JSON.parse(fs.readFileSync(PLATFORMS, 'utf8'));

  // Group the assets by OS, preserving the BREW_TARGETS order, so macOS's two
  // architectures land in one `on_macos` block.
  const byOs = {};
  for (const [key, { os, cpu }] of Object.entries(BREW_TARGETS)) {
    const entry = platforms[key];
    if (!entry) fail(`platforms.json is missing ${key}`);
    const asset = `glslint-${entry.target}`;
    const url = `${HOMEPAGE}/releases/download/v${args.version}/${asset}`;
    let sha = PLACEHOLDER_SHA;
    if (!args.dryRun) {
      const file = path.join(args.binaries, asset);
      if (!fs.existsSync(file)) fail(`missing release binary ${file}`);
      sha = sha256(file);
    }
    (byOs[os] ??= []).push({ cpu, url, sha });
  }

  const osBlocks = Object.entries(byOs)
    .map(([os, entries]) => renderOsBlock(os, entries))
    .join('\n\n');

  const formula = `# This file is generated by homebrew/prepare.mjs in johncarmack1984/glslint
# and pushed here by that repo's release workflow. Do not edit it by hand — edit
# the generator instead; a hand edit is overwritten on the next release.
class Glslint < Formula
  desc "${DESC}"
  homepage "${HOMEPAGE}"
  license any_of: ["MIT", "Apache-2.0"]

  # glslint shells out to glslangValidator (the Khronos GLSL reference compiler)
  # to validate assembled shaders, so it is a runtime dependency, not just a
  # build one.
  depends_on "glslang"

${osBlocks}

  def install
    # Exactly one asset is downloaded per platform; install it under the plain
    # command name.
    bin.install Dir["glslint-*"].first => "glslint"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/glslint --version")
  end
end
`;

  if (args.out) {
    fs.writeFileSync(args.out, formula);
    console.error(`homebrew/prepare: wrote ${args.out}`);
  } else {
    process.stdout.write(formula);
  }
}

main();
