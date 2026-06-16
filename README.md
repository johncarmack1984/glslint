# glslint

A luma.gl / deck.gl-aware GLSL checker and language server. Stock GLSL tools
choke on these shaders because they aren't standalone translation units — they
reference UBO instances (`wind.*`, `blit.*`) declared in separate module
fragments and deck builtins (`project_position_to_clipspace`) injected at link
time. glslint **assembles** the modules + deck stubs into a complete unit,
validates it, and **maps diagnostics back to the original file and line**.

> **Status: PARKED (2026-06-16).** The architecture works; it's blocked on the
> validation backend. See [Why it's parked](#why-its-parked).

## Usage

```sh
cargo build
./target/debug/glslint check path/to/shader.frag.glsl   # one-shot, exit 1 on errors
./target/debug/glslint lsp                               # language server over stdio
```

`check` resolves config by walking up for a `glsl-lsp.toml`; with none, it uses a
built-in deck `project32` prelude and auto-discovers sibling `*Uniforms.glsl`
module fragments next to the target.

```toml
# glsl-lsp.toml (optional)
preludes = ["preludes/deck.glsl"]
modules  = ["src/shaders/windUniforms.glsl", "src/shaders/blitUniforms.glsl"]
builtin_prelude = true
```

## How it works

- `assemble.rs` — hoists `#version`, injects prelude + module blocks, records a
  per-line map back to the originals. Also normalizes ES→core spellings (version,
  `precision`, `binding=`) for the current naga backend. Stage is inferred from
  the filename (`*.vert.glsl` / `*.frag.glsl` / `*.comp.glsl`); bare module
  fragments are wrapped in a dummy shell for syntax-only checking.
- `check.rs` — runs the validator and translates spans back to original
  file:line via the map. **This mapping is the hard part and it works** — verified
  errors land on `draw.vert.glsl:4`, not the assembled unit.
- `lints.rs` — opinionated, zero-false-positive rules (currently: GLSL ES 1.00
  builtins/qualifiers removed in `#version 300 es`).
- `lsp.rs` — tower-lsp; `publishDiagnostics` on open/change/save, filtered to the
  edited document.

## Why it's parked

The validation engine is **naga** (`front::glsl`), chosen for in-process Rust
with no external dependency. Spikes proved naga's GLSL frontend is a
**Vulkan-GLSL** frontend, not a WebGL/OpenGL one:

| Construct | naga `front::glsl` |
|-----------|--------------------|
| `#version 300 es` | rejected (accepts only `440/450/460 core`) |
| `precision highp float;` | rejected (`NotImplemented: variable qualifier`) |
| `layout(std140) uniform {…}` w/o `binding` | rejected (requires `binding=`) |
| combined `sampler2D u;` decl | rejected (wants separate `texture2D` + `sampler`) |
| `sampler2D(tex, samp)` expr + `texelFetch`/`textureSize` | **OK** |
| **`sampler2D` as a function parameter** | **rejected** — `Expected RightParen` |

The first three are handled by an ES→core normalization pass in `assemble.rs`,
and with it **5 of deck-wind-layer's 8 shaders validate clean**. The last row is
the blocker: deck-wind-layer's two main shaders use

```glsl
vec2 windAt(sampler2D windTex, vec2 pos) { … texture(windTex, pos) … }
```

and naga cannot express a combined-sampler parameter. Supporting it would require
rewriting function signatures (split `sampler2D` → `texture2D` + `sampler`),
internal `texture()` calls, and every call site — scope-aware compiler work.

## Resume options (decision pending)

1. **Swap engine to `glslangValidator` (recommended).** Keep everything; replace
   the naga call in `check.rs` with a subprocess (`glslangValidator --stage frag`
   on the assembled source), parse its `ERROR: 0:LINE:` output, map back via the
   existing line map. Authoritative `#version 300 es` + combined samplers, **zero
   source transforms**. Needs `brew install glslang`. Drop the ES→core
   normalization (glslang takes ES natively).
2. **Hybrid.** Prefer glslang when on `PATH`; fall back to in-process naga
   (limited — noisy on the `windAt` shaders).
3. **Push through with naga.** Implement the combined→separate sampler transform
   above. In-process, no dep, but fragile.

Backend selection is the single open decision; the assembler, mapping, CLI, LSP,
and lints are reusable as-is under any backend.
