# glslint

A luma.gl / deck.gl-aware GLSL checker and language server. Stock GLSL tools choke on these shaders because they aren't standalone translation units — they reference UBO instances (`wind.*`, `blit.*`) declared in separate module fragments and deck builtins (`project_position_to_clipspace`) injected at link time. glslint **assembles** the modules + deck stubs into a complete unit, validates it with the Khronos **glslangValidator** reference compiler, and **maps diagnostics back to the original file and line**.

## Requirements

glslint shells out to `glslangValidator` (the Khronos GLSL reference compiler):

```sh
brew install glslang     # provides glslangValidator (and the newer `glslang`)
```

glslint finds it on `PATH` (trying `glslangValidator`, then `glslang`); set `GLSLINT_GLSLANG` to point at a specific binary.

## Usage

```sh
cargo build
./target/debug/glslint check path/to/shader.frag.glsl   # one-shot, exit 1 on errors
./target/debug/glslint lsp                               # language server over stdio
```

`check` resolves config by walking up for a `glsl-lsp.toml`; with none, it uses a built-in deck `project32` prelude and auto-discovers sibling `*Uniforms.glsl` module fragments next to the target.

The faithful form mirrors luma's own model — name the modules once, then bind each shader to the modules it actually uses (the `new Model({modules: [...]})` call in JS). Each shader then gets exactly its modules, so referencing a uniform block the shader doesn't have is flagged instead of silently resolved:

```toml
# glsl-lsp.toml
[[module]]
name = "windUniforms"
source = "src/shaders/windUniforms.glsl"
types  = "src/modules.ts"  # optional: cross-check the UBO block vs JS uniformTypes

[[module]]
name = "project32"
builtin = true            # deck project32 (baked-in stub for now)

[[shader]]
match   = "draw.*.glsl"   # first matching binding wins
modules = ["project32", "windUniforms"]

[[shader]]
match   = "blit.*.glsl"
modules = ["blitUniforms"]
```

Without `[[shader]]` bindings it accepts a legacy global list (`preludes` / `modules` / `builtin_prelude`) applied to every shader. With **no `glsl-lsp.toml` at all**, it first tries to auto-derive each shader's modules from the project's JS/TS `new Model({ modules })` calls (see `derive.rs`), and only falls back to zero-config sibling discovery if that finds nothing.

## Editor integration

A minimal VS Code / Cursor extension lives in [`editors/vscode/`](editors/vscode/) — it's a thin LSP client that launches `glslint lsp` and shows its diagnostics on GLSL files. See that folder's README to run it (`F5` dev host, or `vsce package`). Point `glslint.path` at the built binary (e.g. `target/debug/glslint`) if it isn't on the editor's PATH.

## How it works

- `assemble.rs` — hoists the target's own `#version` to the top, injects default precision (so the deck prelude's `float`/`vec*` are well-formed before the shader's own `precision` line), then the prelude + module blocks, recording a per-line map back to the originals. Source is passed through **verbatim** — glslangValidator validates GLSL ES natively, so there are no source transforms. Stage is inferred from the filename (`*.vert.glsl` / `*.frag.glsl` / `*.comp.glsl`); bare module fragments are wrapped in a dummy shell for syntax-only checking.
- `check.rs` — runs `glslangValidator --stdin -S <stage>` over the assembled unit, parses its `ERROR: 0:LINE:` / `WARNING:` output, collapses glslang's per-line error cascades to the root cause, and translates each line back to the original file:line via the map (refining the column from the offending token when glslang names one). **This mapping is the hard part and it works** — errors land on `draw.vert.glsl:4`, and an error inside an injected module lands on `windUniforms.glsl:3`, not the assembled unit.
- `lints.rs` — opinionated, zero-false-positive rules (currently: GLSL ES 1.00 builtins/qualifiers removed in `#version 300 es`). Runs alongside the validator, so a `varying` declaration draws both the raw glslang error and a friendlier migration hint.
- `drift.rs` — when a module declares a `types` JS file (see config), cross-checks its GLSL UBO block against that file's `uniformTypes` and warns on drift (a member on one side only, or a type mismatch). luma keeps these two in sync by hand; nothing else sees both at once. Conservative — silent unless it can confidently read both sides.
- `symbols.rs` — a line-based symbol scanner over the assembled unit (UBO/interface blocks, top-level `uniform`/`in`/`out`, function definitions, deck builtins), each symbol carrying its original `Loc`. Powers hover, go-to-definition, completion, and the document outline — including the cross-module jump: `wind.uMin` in `draw.vert.glsl` resolves to its declaration in `windUniforms.glsl`.
- `deck.rs` — resolves deck.gl's `project` builtins from `node_modules` instead of a hand-written stub. deck ships the module GLSL as a JS template whose bodies interpolate constants and depend on a `geometry`/`project` UBO, so the bodies can't be spliced; but the function *signatures* are clean GLSL. So it extracts the real signatures, generates empty-body stubs so any project function validates (no dependency graph needed), and records each declaration site — so hover shows the true signature and go-to-definition jumps into the deck source. Falls back to a baked-in 4-function stub when deck isn't installed.
- `derive.rs` — when there's no `glsl-lsp.toml`, recovers each shader's module bindings the way luma already encodes them: by reading the project's `new Model({ vs, fs, modules })` calls. It finds the shader's `?raw` import, the `Model` call that references it, and follows each module identifier to its GLSL source (a local module's `vs:` import, e.g. `windUniforms` → `windUniforms.glsl`) or to the deck builtins (a package import like `project32`). A heuristic scan, not a JS parser — handles multi-line imports, conservative enough to fall back to sibling discovery when it can't confidently resolve. The payoff: deck-wind-layer's shaders validate with their faithful per-shader modules and **no config file** — `blit` bound to `blitUniforms` only, `draw` to `project32` + `windUniforms`.
- `lsp.rs` — tower-lsp; `publishDiagnostics` on open/change/save, plus hover, go-to-definition, completion (`wind.` → member list), and document symbols, filtered to the edited document. Edits are debounced and the (subprocess-spawning) check runs off the async runtime, with a per-document generation guard so a slow check can't clobber a newer edit.

## Why glslangValidator

The validation engine was originally **naga** (`front::glsl`), chosen for in-process Rust with no external dependency. Spikes proved naga's GLSL frontend is a **Vulkan-GLSL** frontend, not a WebGL/OpenGL one:

| Construct | naga `front::glsl` | glslangValidator |
|-----------|--------------------|------------------|
| `#version 300 es` | rejected (accepts only `440/450/460 core`) | **OK** |
| `precision highp float;` | rejected | **OK** |
| `layout(std140) uniform {…}` w/o `binding` | rejected (requires `binding=`) | **OK** |
| combined `sampler2D u;` decl | rejected (wants separate `texture2D` + `sampler`) | **OK** |
| **`sampler2D` as a function parameter** | **rejected** — `Expected RightParen` | **OK** |

That last row was the blocker: deck-wind-layer's two main shaders use

```glsl
vec2 windAt(sampler2D windTex, vec2 pos) { … texture(windTex, pos) … }
```

which naga can't express, and supporting it would have meant rewriting function signatures, internal `texture()` calls, and every call site — scope-aware compiler work. glslangValidator is the Khronos ES reference compiler: it validates `#version 300 es` + combined samplers authoritatively, with **zero source transforms**. The cost is an external binary on `PATH`; the assembler, the diagnostic mapping, the CLI, the LSP, and the lints are all backend-independent and carried over unchanged.

### glslang quirks worth knowing

- All diagnostics go to **stdout** (not stderr), as `ERROR: 0:LINE: 'token' : message`. The `0` is the source-string index (always 0 for our single stdin unit); there's no column, so glslint derives one from the named token.
- glslang does **not** stop at the first error. For a semantic failure it emits the root cause and then a string of *derived* errors (sometimes exact duplicates) on the same source line, and it inlines whole type definitions into some messages (an entire `uniform block{...}` for a bad UBO-member access). glslint therefore keeps the **first diagnostic per source line** (glslang emits the root cause first) and truncates over-long messages. The `compilation terminated` line (a *parse*-phase cascade only) and the `N compilation errors` summary are filtered out separately.
- GLSL ES fragment shaders have **no default `float` precision**, so the assembler injects `precision highp float; precision highp int;` right after `#version`; re-declaring them later (as the shaders do) is legal.

## License

Dual-licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
