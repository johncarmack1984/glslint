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

```toml
# glsl-lsp.toml (optional)
preludes = ["preludes/deck.glsl"]
modules  = ["src/shaders/windUniforms.glsl", "src/shaders/blitUniforms.glsl"]
builtin_prelude = true
```

## How it works

- `assemble.rs` — hoists the target's own `#version` to the top, injects default precision (so the deck prelude's `float`/`vec*` are well-formed before the shader's own `precision` line), then the prelude + module blocks, recording a per-line map back to the originals. Source is passed through **verbatim** — glslangValidator validates GLSL ES natively, so there are no source transforms. Stage is inferred from the filename (`*.vert.glsl` / `*.frag.glsl` / `*.comp.glsl`); bare module fragments are wrapped in a dummy shell for syntax-only checking.
- `check.rs` — runs `glslangValidator --stdin -S <stage>` over the assembled unit, parses its `ERROR: 0:LINE:` / `WARNING:` output, collapses glslang's per-line error cascades to the root cause, and translates each line back to the original file:line via the map (refining the column from the offending token when glslang names one). **This mapping is the hard part and it works** — errors land on `draw.vert.glsl:4`, and an error inside an injected module lands on `windUniforms.glsl:3`, not the assembled unit.
- `lints.rs` — opinionated, zero-false-positive rules (currently: GLSL ES 1.00 builtins/qualifiers removed in `#version 300 es`). Runs alongside the validator, so a `varying` declaration draws both the raw glslang error and a friendlier migration hint.
- `lsp.rs` — tower-lsp; `publishDiagnostics` on open/change/save, filtered to the edited document. Edits are debounced and the (subprocess-spawning) check runs off the async runtime, with a per-document generation guard so a slow check can't clobber a newer edit.

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
