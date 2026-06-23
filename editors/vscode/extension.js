// Minimal LSP client: launch the `glslint` binary in `lsp` mode and let it drive
// diagnostics for GLSL documents. Works in VS Code and Cursor (same extension API).

const { workspace, window } = require("vscode");
const { LanguageClient } = require("vscode-languageclient/node");

let client;

function activate() {
  const command = workspace.getConfiguration("glslint").get("path") || "glslint";

  // `glslint lsp` speaks LSP over stdio.
  const serverOptions = { command, args: ["lsp"] };
  const clientOptions = {
    documentSelector: [{ scheme: "file", language: "glsl" }],
  };

  client = new LanguageClient("glslint", "glslint", serverOptions, clientOptions);
  client.start().catch((err) => {
    window.showErrorMessage(
      `glslint: failed to start "${command} lsp" — set "glslint.path" or install the binary. ${err}`,
    );
  });
}

function deactivate() {
  return client ? client.stop() : undefined;
}

module.exports = { activate, deactivate };
