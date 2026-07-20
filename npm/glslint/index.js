'use strict';

// Resolve the prebuilt glslint binary for the running platform.
//
// The binaries live in per-platform packages (`@glslint/darwin-arm64` and
// friends) listed as optionalDependencies. npm installs only the one matching
// the host's os/cpu, so a user downloads one binary rather than all four. This
// module is the seam: the `glslint` shim uses it, and an editor extension can
// require it to launch `glslint lsp` directly instead of paying for a wrapper
// process.

const path = require('node:path');
const fs = require('node:fs');

const PLATFORMS = require('./platforms.json');

const REPO = 'https://github.com/johncarmack1984/glslint';

/** Key into PLATFORMS for the running host, e.g. "darwin-arm64". */
function platformKey() {
  return `${process.platform}-${process.arch}`;
}

/**
 * Absolute path to the glslint binary for this platform.
 * Throws with the specific remedy when it cannot be found.
 */
function binaryPath() {
  const key = platformKey();

  if (!Object.prototype.hasOwnProperty.call(PLATFORMS, key)) {
    throw new Error(
      `glslint does not ship a prebuilt binary for ${key}. ` +
        `Supported: ${Object.keys(PLATFORMS).join(', ')}. ` +
        `Build from source with \`cargo install --git ${REPO}\`, ` +
        `or open an issue at ${REPO}/issues to request this platform.`,
    );
  }

  const pkg = `@glslint/${key}`;
  let pkgDir;
  try {
    // Resolve the package.json rather than the binary: it is always a real
    // resolvable path, while the binary has no extension on unix.
    pkgDir = path.dirname(require.resolve(`${pkg}/package.json`));
  } catch {
    throw new Error(
      `glslint could not find its platform package ${pkg}. ` +
        `It installs as an optional dependency, so this usually means the install ` +
        `skipped optional deps (\`--no-optional\` / \`--omit=optional\`) or ran with a ` +
        `different os/cpu than it is running on now. Reinstall with optional ` +
        `dependencies enabled, or build from source with \`cargo install --git ${REPO}\`.`,
    );
  }

  const bin = path.join(pkgDir, 'bin', process.platform === 'win32' ? 'glslint.exe' : 'glslint');
  if (!fs.existsSync(bin)) {
    throw new Error(
      `${pkg} is installed but its binary is missing at ${bin}. ` +
        `Reinstall glslint, or build from source with \`cargo install --git ${REPO}\`.`,
    );
  }
  return bin;
}

module.exports = { binaryPath, platformKey, PLATFORMS };
