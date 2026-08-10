// Minimal LSP client: launch the `glslint` binary in `lsp` mode and let it drive
// diagnostics/hover/completion/etc. for GLSL documents. Works in VS Code and
// Cursor (same extension API).
//
// Binary resolution keeps the LSP pinned to this extension's exact version.
// An explicit `glslint.path` is a dev override, honored verbatim (a version
// mismatch only warns). Otherwise an auto-detected binary (~/.cargo/bin or PATH)
// is used ONLY when its `--version` matches this extension — so a stale
// `cargo install glslint` can never silently shadow the pinned LSP. With no
// matching local binary, the extension downloads its own version-pinned binary
// from this repo's GitHub Release and caches it; that download is now the normal
// path and upgrades automatically on every extension update. If the download
// fails (offline/404) it falls back to the best local binary it can find,
// mismatch and all — a slightly-stale LSP beats none.

const { workspace, window, ProgressLocation } = require("vscode");
const { LanguageClient } = require("vscode-languageclient/node");
const os = require("os");
const path = require("path");
const fs = require("fs");
const https = require("https");
const cp = require("child_process");

const REPO = "johncarmack1984/glslint";
const pkg = require("./package.json");
// Binary version to download: this extension's own version (release-please keeps
// package.json in lockstep with the crate), prefixed with `v` for the release tag.
const VERSION = `v${pkg.version}`;

// node `platform-arch` -> the Rust target triple in the release asset names. A
// local copy the packaged .vsix carries (it can't read npm's copy at install
// time); a CI drift guard (check-platforms.mjs) pins it to the single source of
// truth, npm/glslint/platforms.json.
const TARGETS = require("./targets.json");

let client;

/// node platform/arch -> the Rust target triple used in the release asset names.
function rustTarget() {
  return TARGETS[`${process.platform}-${process.arch}`] || null;
}

function exeName() {
  return process.platform === "win32" ? "glslint.exe" : "glslint";
}

/// Run `<cmd> --version` and return the reported semver (`glslint <semver>`, see
/// src/main.rs), or null if it can't be run or the output doesn't parse. Used to
/// gate auto-detected binaries so only an exact version match is accepted.
function binaryVersion(cmd) {
  try {
    const res = cp.spawnSync(cmd, ["--version"], { timeout: 3000, encoding: "utf8" });
    if (res.error || res.status !== 0 || !res.stdout) return null;
    const m = res.stdout.trim().match(/^glslint (\S+)$/);
    return m ? m[1] : null;
  } catch {
    return null;
  }
}

/// Auto-detected candidate binaries, best-first: ~/.cargo/bin, then each PATH dir.
/// Excludes the explicit `glslint.path` setting, which is handled separately.
function localCandidates() {
  const candidates = [];
  const cargoBin = path.join(os.homedir(), ".cargo", "bin", exeName());
  if (fs.existsSync(cargoBin)) candidates.push(cargoBin);
  for (const dir of (process.env.PATH || "").split(path.delimiter)) {
    if (!dir) continue;
    const p = path.join(dir, exeName());
    if (fs.existsSync(p)) candidates.push(p);
  }
  return candidates;
}

function download(url, dest) {
  return new Promise((resolve, reject) => {
    const get = (u) =>
      https
        .get(u, { headers: { "User-Agent": "glslint-vscode" } }, (res) => {
          if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
            res.resume();
            return get(res.headers.location); // GitHub redirects to a CDN
          }
          if (res.statusCode !== 200) {
            res.resume();
            return reject(new Error(`HTTP ${res.statusCode} downloading ${u}`));
          }
          const file = fs.createWriteStream(dest);
          res.pipe(file);
          file.on("finish", () => file.close(resolve));
          file.on("error", (e) => reject(e));
        })
        .on("error", reject);
    get(url);
  });
}

/// Download (and cache) the prebuilt binary for this platform from the Release.
async function downloadBinary(context) {
  const target = rustTarget();
  if (!target) throw new Error(`no prebuilt glslint for ${process.platform}/${process.arch}`);
  const asset = `glslint-${target}${process.platform === "win32" ? ".exe" : ""}`;
  const dir = path.join(context.globalStorageUri.fsPath, VERSION);
  const dest = path.join(dir, asset);
  if (fs.existsSync(dest)) return dest;

  fs.mkdirSync(dir, { recursive: true });
  const url = `https://github.com/${REPO}/releases/download/${VERSION}/${asset}`;
  await window.withProgress(
    { location: ProgressLocation.Notification, title: `Downloading glslint ${VERSION}…` },
    () => download(url, dest),
  );
  if (process.platform !== "win32") fs.chmodSync(dest, 0o755);
  return dest;
}

/// Delete cached downloads from previous extension versions; only VERSION's dir
/// is kept, so the cache can't grow unbounded across updates. Best-effort.
function pruneOldDownloads(context) {
  const root = context.globalStorageUri.fsPath;
  let entries;
  try {
    entries = fs.readdirSync(root);
  } catch {
    return; // storage dir not created yet — nothing cached
  }
  for (const name of entries) {
    // Only touch our own version-tagged dirs (`v<major>…`); never anything else
    // that might live under globalStorage.
    if (name === VERSION || !/^v\d/.test(name)) continue;
    try {
      fs.rmSync(path.join(root, name), { recursive: true, force: true });
    } catch {
      // ignore — a stale dir that won't delete isn't worth failing activation
    }
  }
}

/// Resolve the `glslint` command to launch. Returns { command, explicit } where
/// `explicit` marks a user-set `glslint.path` (checked for version match after
/// the LSP starts, not here). Throws only when nothing is usable, letting the
/// caller fall back to `glslint` on PATH.
async function resolveCommand(context) {
  // 1. Explicit override: honored verbatim. Its version is checked post-start
  //    via the LSP handshake (serverInfo.version) — no extra process spawn.
  const configured = workspace.getConfiguration("glslint").get("path");
  if (configured) return { command: configured, explicit: true };

  // 2. An auto-detected binary is used only when its version matches this
  //    extension, so a stale install can never shadow the pinned LSP.
  const candidates = localCandidates();
  for (const cand of candidates) {
    if (binaryVersion(cand) === pkg.version) return { command: cand, explicit: false };
  }

  // 3. No match: download this extension's pinned binary (the normal path).
  try {
    return { command: await downloadBinary(context), explicit: false };
  } catch (err) {
    // 4. Download failed (offline/404): a slightly-stale local LSP beats none.
    if (candidates.length) {
      window.showWarningMessage(
        `glslint: couldn't download ${VERSION} (${err.message}); using ${candidates[0]}, ` +
          `which may be a different version.`,
      );
      return { command: candidates[0], explicit: false };
    }
    throw err; // caller falls back to `glslint` on PATH
  }
}

async function activate(context) {
  // Tidy caches from older extension versions on every activation (cheap).
  pruneOldDownloads(context);

  let command = "glslint";
  let explicit = false;
  try {
    ({ command, explicit } = await resolveCommand(context));
  } catch (err) {
    window.showWarningMessage(`glslint: ${err.message}. Falling back to \`glslint\` on PATH.`);
  }

  const serverOptions = { command, args: ["lsp"] };
  // GLSL files, plus JS/TS hosts for shaders written inline in `glsl`…`` tagged
  // template literals. For JS/TS the server publishes diagnostics only (mapped to
  // the template's span); the symbol features stay GLSL-file-only.
  const clientOptions = {
    documentSelector: [
      { scheme: "file", language: "glsl" },
      { scheme: "file", language: "typescript" },
      { scheme: "file", language: "typescriptreact" },
      { scheme: "file", language: "javascript" },
      { scheme: "file", language: "javascriptreact" },
    ],
  };

  client = new LanguageClient("glslint", "glslint", serverOptions, clientOptions);
  client
    .start()
    .then(() => {
      // Only an explicit `glslint.path` can be a mismatched version at this point
      // (auto-detected binaries are version-gated before start, downloads are
      // pinned). serverInfo.version comes from the LSP handshake — no extra spawn.
      if (!explicit) return;
      const serverVersion = client.initializeResult?.serverInfo?.version;
      if (serverVersion && serverVersion !== pkg.version) {
        window.showWarningMessage(
          `glslint.path points at glslint ${serverVersion}, but this extension expects ` +
            `${pkg.version} — diagnostics may differ. Clear "glslint.path" to use the managed binary.`,
        );
      }
    })
    .catch((err) => {
      window.showErrorMessage(
        `glslint: couldn't start "${command} lsp". Install it with \`cargo install --path .\` ` +
          `in the glslint repo, or set "glslint.path". ${err}`,
      );
    });
}

function deactivate() {
  return client ? client.stop() : undefined;
}

module.exports = { activate, deactivate };
