// Minimal LSP client: launch the `glslint` binary in `lsp` mode and let it drive
// diagnostics/hover/completion/etc. for GLSL documents. Works in VS Code and
// Cursor (same extension API).
//
// Binary resolution, in order: an explicit `glslint.path` setting; a locally
// installed binary (~/.cargo/bin or PATH); else a prebuilt binary downloaded once
// from this repo's GitHub Release and cached in the extension's storage.

const { workspace, window, ProgressLocation } = require("vscode");
const { LanguageClient } = require("vscode-languageclient/node");
const os = require("os");
const path = require("path");
const fs = require("fs");
const https = require("https");

const REPO = "johncarmack1984/glslint";
// Binary version to download: this extension's own version (release-please keeps
// package.json in lockstep with the crate), prefixed with `v` for the release tag.
const VERSION = `v${require("./package.json").version}`;

let client;

/// node platform/arch -> the Rust target triple used in the release asset names.
function rustTarget() {
  if (process.platform === "darwin") {
    return process.arch === "arm64" ? "aarch64-apple-darwin" : "x86_64-apple-darwin";
  }
  if (process.platform === "linux" && process.arch === "x64") return "x86_64-unknown-linux-gnu";
  if (process.platform === "win32" && process.arch === "x64") return "x86_64-pc-windows-msvc";
  return null;
}

function exeName() {
  return process.platform === "win32" ? "glslint.exe" : "glslint";
}

/// A locally installed binary, if any: explicit setting, then ~/.cargo/bin, then PATH.
function localBinary() {
  const configured = workspace.getConfiguration("glslint").get("path");
  if (configured) return configured;
  const cargoBin = path.join(os.homedir(), ".cargo", "bin", exeName());
  if (fs.existsSync(cargoBin)) return cargoBin;
  for (const dir of (process.env.PATH || "").split(path.delimiter)) {
    if (dir && fs.existsSync(path.join(dir, exeName()))) return path.join(dir, exeName());
  }
  return null;
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

async function resolveCommand(context) {
  const local = localBinary();
  if (local) return local;
  return downloadBinary(context); // throws if unavailable; caller falls back
}

async function activate(context) {
  let command = "glslint";
  try {
    command = await resolveCommand(context);
  } catch (err) {
    window.showWarningMessage(`glslint: ${err.message}. Falling back to \`glslint\` on PATH.`);
  }

  const serverOptions = { command, args: ["lsp"] };
  const clientOptions = { documentSelector: [{ scheme: "file", language: "glsl" }] };

  client = new LanguageClient("glslint", "glslint", serverOptions, clientOptions);
  client.start().catch((err) => {
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
