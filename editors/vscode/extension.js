// Minimal LSP client: launch the `glslint` binary in `lsp` mode and let it drive
// diagnostics/hover/completion/etc. for GLSL documents. Works in VS Code and
// Cursor (same extension API).

const { workspace, window } = require("vscode");
const { LanguageClient } = require("vscode-languageclient/node");
const os = require("os");
const path = require("path");
const fs = require("fs");

let client;

// Resolve the glslint binary. An explicit `glslint.path` setting wins; otherwise
// prefer ~/.cargo/bin (where `cargo install --path .` puts it) by absolute path —
// so it's found even when a GUI-launched editor didn't inherit the shell PATH —
// then fall back to `glslint` on PATH.
function resolveCommand() {
  const configured = workspace.getConfiguration("glslint").get("path");
  if (configured) return configured;
  const exe = process.platform === "win32" ? "glslint.exe" : "glslint";
  const cargoBin = path.join(os.homedir(), ".cargo", "bin", exe);
  if (fs.existsSync(cargoBin)) return cargoBin;
  return "glslint"; // PATH
}

function activate() {
  const command = resolveCommand();
  const serverOptions = { command, args: ["lsp"] };
  const clientOptions = {
    documentSelector: [{ scheme: "file", language: "glsl" }],
  };

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
