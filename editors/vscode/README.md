# glslint — VS Code / Cursor extension

A thin LSP client that runs the `glslint` binary in `lsp` mode and surfaces its diagnostics on GLSL files. It also ships GLSL syntax highlighting (a TextMate grammar) plus bracket/comment editing config. Works in VS Code and Cursor (identical extension API).

## Setup

1. Build (or install) the binary so the extension can find it:
   ```sh
   cd ../..            # the glslint repo root
   cargo install --path .     # puts `glslint` on PATH (~/.cargo/bin)
   # — or just `cargo build` and point glslint.path at ./target/debug/glslint
   ```
   `glslint` shells out to `glslangValidator`, so also: `brew install glslang`.
2. Install the client's dependency:
   ```sh
   cd editors/vscode && npm install
   ```

## Run it

- **Dev host (fastest):** open the `editors/vscode` folder in VS Code/Cursor and press `F5`. That launches an Extension Development Host; open your `deck-wind-layer` folder in it and open a shader (e.g. `src/shaders/draw.vert.glsl`).
- **Install for real:** `npx @vscode/vsce package` here, then install the resulting `.vsix` (`code --install-extension glslint-0.1.0.vsix`).

If the binary isn't on PATH, set it in your settings:
```json
{ "glslint.path": "/absolute/path/to/glslint/target/debug/glslint" }
```

## What you should see

Open `draw.vert.glsl` and change a uniform-block member to a typo, e.g. `wind.maxSpeed` → `wind.maxSpeeed`. A red squiggle appears under it:

> `'maxSpeeed' : no such field in structure 'wind'`

That's the point of glslint: `wind` is declared in a *separate* `windUniforms.glsl` module that stock GLSL tools never see, so only glslint can validate the member access. Diagnostics are debounced and map back to the exact `file:line:col`, including into the injected module files when the error originates there.

**Hover** a uniform-block member like `wind.uMin` to see its type, and **cmd-click** (Go to Definition) to jump straight into `windUniforms.glsl` — the cross-module navigation that stock GLSL tooling can't do. Hover/jump also work for top-level `uniform`/`in`/`out` declarations and function definitions; hovering a built-in — deck's `project_position_to_clipspace` or core GLSL like `clamp`/`texture`/`mix` — shows its signature.

Type `wind.` for **member completion** (the whole uniform block), and open the **Outline** view (or breadcrumbs) for the file's **document symbols** — its uniforms, functions, and any blocks declared in it.
