# Presets

A **preset** teaches glslint about one shader ecosystem — as data, not code. Every preset in this directory is compiled into the binary and applied automatically when its `[detect]` rules match. The linter core carries no ecosystem-specific knowledge; it all lives here.

The files in this directory (`maplibre.toml`, `shadertoy.toml`, `deck.toml`, …) use the **exact same schema** a project can drop at its own root as `glslint.toml`. So shipping a preset here and configuring a private project are the same act — the bundled presets are just examples you can copy. The schema is published at [`schema/glslint.schema.json`](../schema/glslint.schema.json); point your editor at it (via SchemaStore, keyed on the `glslint.toml` filename) for validation and autocomplete.

`deck.toml` is the smallest full example of the `deck = true` builtin path: its `prelude` is the stub fallback, while the real deck signatures are resolved dynamically from `node_modules` (in `src/deck.rs`) when available.

## Why a preset, when discovery is automatic

glslint already works on an unfamiliar project with **no preset**: it discovers the shared library (any sibling `.glsl` file with no `main`) and resolves it, so shared functions type-check with zero config. A preset adds the things discovery can't do or can't do fast:

- **A `#pragma` DSL.** maplibre's `#pragma maplibre: define lowp float opacity` is a maplibre invention with no GLSL standard behind it — glslint can only expand it if a preset says how. This is the main reason a preset exists.
- **Speed and precision.** Listing the shared-library files (`[library]`) and injected macros (`defines`) lets glslint skip the discovery scan and pin exactly the right files (e.g. mercator over globe).

## Fields

See [`schema/glslint.schema.json`](../schema/glslint.schema.json) for the full contract. In brief:

| Field | Purpose |
|-------|---------|
| `name` | Preset identifier (required). |
| `[detect]` `source_contains` / `sibling_files` | When the preset activates: a substring in the shader, or a file next to it. Any hit wins. |
| `[library]` `vertex` / `fragment` | Shared-library files spliced ahead of every shader, in order. Omit to let glslint discover no-`main` siblings. |
| `defines` | Names of build-injected macros to default (`#ifndef`-guarded). |
| `discover_defines` | Also scan the project's JS/TS for injected `#define NAME ${…}` macros. Off by default (it walks the repo). |
| `prelude` / `prelude_vertex` / `prelude_fragment` | GLSL prepended as implicit globals. |
| `epilogue` | GLSL appended when the source has no `main` (e.g. ShaderToy's driver `main`). |
| `[[expand]]` | Directive-expansion rules — the `#pragma` DSL. |

### `[[expand]]` rules

Each rule rewrites one `#pragma <ns>: <verb> <args…>` line, in place, into GLSL. Tokens after the verb bind positionally to `args`, and `{name}` placeholders are substituted (so `u_{name}` becomes `u_opacity`):

```toml
[[expand]]
pragma = ["maplibre", "mapbox"]   # one namespace, or a list to share the rule
verb   = "define"                 # omit to match the namespace alone
args   = ["prec", "type", "name"]
vertex   = "in {prec} {type} a_{name};\nout {prec} {type} {name};"
fragment = "in {prec} {type} {name};"
# `emit = "..."` sets a template for any stage without a stage-specific one
```

The load-bearing rule of thumb: an expansion only has to make symbols **resolve with the right type** — glslint isn't replicating the ecosystem's runtime. Keep a rule's `define` and `initialize` templates consistent (the attribute a `define` declares is the one an `initialize` reads) and the expansion can never be the source of a type error.

## TOML gotcha

Top-level keys (`name`, `defines`, `prelude*`, `epilogue`, `discover_defines`) must appear **before** any `[table]` header — TOML assigns a bare key to the most recent table otherwise. The schema uses `deny_unknown_fields`, so a misplaced key fails loudly with a clear message rather than being silently ignored.

## Contributing a preset

1. Copy `shadertoy.toml` (prelude-only) or `maplibre.toml` (the full works) and adapt it.
2. Add its `include_str!` line to `bundled()` in `src/preset.rs`.
3. Validate against real shaders: `glslint check 'path/to/shaders/**/*.glsl'` should come back clean, and a deliberately broken shader should still error (a preset must not become a blanket suppressor).
4. Add a parse assertion to `src/preset.rs`'s tests.
