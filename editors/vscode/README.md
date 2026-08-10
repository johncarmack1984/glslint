# glslint — VS Code / Cursor extension

A thin LSP client that runs the `glslint` binary in `lsp` mode and surfaces its diagnostics on GLSL files — and on GLSL written inline in JS/TS `glsl`…`` tagged template literals. It also ships GLSL syntax highlighting (a TextMate grammar) plus bracket/comment editing config. Works in VS Code and Cursor (identical extension API).

## Setup

The extension manages its own `glslint` binary and keeps it on the extension's exact version, so you never have to remember to update it. It resolves the binary like so: an explicit `glslint.path` setting (a dev override, used verbatim) → an auto-detected local install (`~/.cargo/bin/glslint`, then PATH) **only if its version matches the extension** → otherwise it **downloads** the version-matched prebuilt binary for your platform from this repo's GitHub Release and caches it in the extension's storage (upgrading automatically whenever the extension updates). A stale `cargo install glslint` is therefore ignored rather than silently serving an old LSP. So the only hard requirement is `glslangValidator` (glslint shells out to it):

```sh
brew install glslang
```

To use a local build instead of the downloaded one — recommended while hacking on glslint — install it:

```sh
cargo install --path .   # release binary into ~/.cargo/bin/glslint
```

Then install the client's dependency:

```sh
cd editors/vscode && npm install
```

## Run it

- **Dev host (fastest):** open the `editors/vscode` folder in VS Code/Cursor and press `F5`. That launches an Extension Development Host; open your `deck-wind-layer` folder in it and open a shader (e.g. `src/shaders/draw.vert.glsl`).
- **Install for real:** `npx @vscode/vsce package` here, then install the resulting `.vsix` (`code --install-extension glslint-<version>.vsix`).

If you didn't `cargo install` (e.g. you want the debug binary), point the setting at it — `glslint.path` is the escape hatch for local builds and is always used verbatim, so it bypasses the version check (you get a one-time warning, not a fallback, if it differs from the extension's version):
```json
{ "glslint.path": "/absolute/path/to/glslint/target/debug/glslint" }
```

## What you should see

Open `draw.vert.glsl` and change a uniform-block member to a typo, e.g. `wind.maxSpeed` → `wind.maxSpeeed`. A red squiggle appears under it:

> `'maxSpeeed' : no such field in structure 'wind'`

That's the point of glslint: `wind` is declared in a *separate* `windUniforms.glsl` module that stock GLSL tools never see, so only glslint can validate the member access. Diagnostics are debounced and map back to the exact `file:line:col`, including into the injected module files when the error originates there.

**Hover** a uniform-block member like `wind.uMin` to see its type, and **cmd-click** (Go to Definition) to jump straight into `windUniforms.glsl` — the cross-module navigation that stock GLSL tooling can't do. Hover/jump also work for top-level `uniform`/`in`/`out` declarations and function definitions; hovering a built-in — deck's `project_position_to_clipspace` or core GLSL like `clamp`/`texture`/`mix` — shows its signature, and cmd-clicking a deck builtin jumps into the real deck source in `node_modules`.

Type `wind.` for **member completion** (the whole uniform block), and open the **Outline** view (or breadcrumbs) for the file's **document symbols** — its uniforms, functions, and any blocks declared in it.

## Shaders inside JS/TS

The extension also watches `.ts`/`.tsx`/`.js`/`.jsx` files. A shader written inline as a `glsl`…`` tagged template — or a `/* glsl */ `…`` marked template — is extracted and validated in place, with diagnostics mapped back to the exact line and column **in the `.ts` file**:

```ts
const fs = glsl`#version 300 es
precision highp float;
out vec4 fragColor;
void main() { fragColor = vec4(nope, 1.0); }  // ← red squiggle under `nope`
`;
```

A template that contains a `${…}` interpolation can't be reconstructed into a complete shader (the injected value may declare or use symbols glslint can't see), so it's checked with the source-level lints only and gets an informational note; templates with no interpolation are fully validated. The GLSL-only features (hover, go-to-definition, completion, outline) stay on `.glsl` files.
